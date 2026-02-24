use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crate::gst_frame_decoder::GstFrameDecoder;
use crate::gst_init::init_once;
use crate::pipeline::DecodedFrame;

const REVERSE_GOP_WINDOW: f64 = 4.0;

struct ReverseStreamClock {
    start_time: Instant,
    start_pts: Option<f64>,
    speed: f64,
}

impl ReverseStreamClock {
    fn new(speed: f64) -> Self {
        Self {
            start_time: Instant::now(),
            start_pts: None,
            speed: speed.max(0.01),
        }
    }

    fn reset(&mut self, pts: f64) {
        self.start_time = Instant::now();
        self.start_pts = Some(pts);
    }

    fn set_speed(&mut self, new_speed: f64) {
        if let Some(start_pts) = self.start_pts {
            let elapsed = self.start_time.elapsed().as_secs_f64();
            let current_pts = start_pts + elapsed * self.speed;
            self.start_pts = Some(current_pts);
            self.start_time = Instant::now();
        }
        self.speed = new_speed.max(0.01);
    }

    fn delay(&mut self, pts_seconds: f64) -> std::time::Duration {
        let start_pts = *self.start_pts.get_or_insert(pts_seconds);
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let target = (pts_seconds - start_pts) / self.speed;
        let diff = target - elapsed;
        if diff > 0.001 {
            std::time::Duration::from_secs_f64(diff)
        } else {
            std::time::Duration::ZERO
        }
    }
}

fn decode_gop_range_with(
    decoder: &mut GstFrameDecoder,
    gop_start: f64,
    gop_end: f64,
    target_w: u32,
    target_h: u32,
) -> Vec<DecodedFrame> {
    let expected_size = (target_w as usize) * (target_h as usize) * 4;
    let mut frames = Vec::new();

    match decoder.seek_and_decode(gop_start) {
        Some(img) => {
            let pts = decoder.last_decode_time().unwrap_or(gop_start);
            if pts <= gop_end + 0.05 {
                let mut rgba_data = img.into_raw();
                rgba_data.resize(expected_size, 0);
                frames.push(DecodedFrame {
                    pts_seconds: pts,
                    width: target_w,
                    height: target_h,
                    rgba_data,
                });
            }
        }
        None => return Vec::new(),
    }

    while let Some((img, pts)) = decoder.decode_next_frame_with_pts() {
        if pts > gop_end + 0.05 {
            break;
        }
        let mut rgba_data = img.into_raw();
        rgba_data.resize(expected_size, 0);
        frames.push(DecodedFrame {
            pts_seconds: pts,
            width: target_w,
            height: target_h,
            rgba_data,
        });
    }

    frames
}

pub struct GstReversePipelineHandle {
    frame_rx: mpsc::Receiver<DecodedFrame>,
    stop_tx: Option<mpsc::Sender<()>>,
    speed_tx: mpsc::Sender<f64>,
    first_frame_ready: Arc<AtomicBool>,
    _decode_handle: Option<JoinHandle<()>>,
    _pacer_handle: Option<JoinHandle<()>>,
}

impl GstReversePipelineHandle {
    pub fn start(
        path: &std::path::Path,
        start_time: f64,
        speed: f64,
        target_w: u32,
        target_h: u32,
    ) -> Result<Self, String> {
        init_once();

        let path_owned = path.to_path_buf();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (speed_tx, speed_rx) = mpsc::channel::<f64>();
        let (intermediate_tx, intermediate_rx) = mpsc::sync_channel::<DecodedFrame>(8);
        let (frame_tx, frame_rx) = mpsc::sync_channel::<DecodedFrame>(4);
        let first_frame_ready = Arc::new(AtomicBool::new(false));

        let decode_handle = std::thread::Builder::new()
            .name("gst-reverse-decode".into())
            .spawn(move || {
                let Ok(mut decoder) = GstFrameDecoder::open(&path_owned, target_w, target_h) else {
                    return;
                };

                let mut current_end = start_time;

                while current_end > 0.0 {
                    if let Ok(_) | Err(mpsc::TryRecvError::Disconnected) = stop_rx.try_recv() {
                        return;
                    }

                    let gop_start = (current_end - REVERSE_GOP_WINDOW).max(0.0);
                    let mut frames = decode_gop_range_with(
                        &mut decoder,
                        gop_start,
                        current_end,
                        target_w,
                        target_h,
                    );
                    frames.reverse();

                    for frame in frames {
                        if let Ok(_) | Err(mpsc::TryRecvError::Disconnected) = stop_rx.try_recv() {
                            return;
                        }
                        if intermediate_tx.send(frame).is_err() {
                            return;
                        }
                    }

                    current_end = gop_start;
                    if gop_start <= 0.0 {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn reverse decode thread: {e}"))?;

        let ffr = first_frame_ready.clone();
        let initial_speed = speed;
        let pacer_handle = std::thread::Builder::new()
            .name("gst-reverse-pacer".into())
            .spawn(move || {
                let mut clock = ReverseStreamClock::new(initial_speed);
                let mut last_pts: Option<f64> = None;
                let mut gop_base_pts: f64 = 0.0;
                let mut signaled_first = false;

                while let Ok(frame) = intermediate_rx.recv() {
                    while let Ok(s) = speed_rx.try_recv() {
                        clock.set_speed(s);
                    }

                    let is_gop_boundary = match last_pts {
                        Some(prev) => frame.pts_seconds > prev + 0.001,
                        None => true,
                    };

                    if is_gop_boundary {
                        clock.reset(0.0);
                        gop_base_pts = frame.pts_seconds;
                    }

                    let distance = (gop_base_pts - frame.pts_seconds).abs();
                    let delay = clock.delay(distance);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }

                    last_pts = Some(frame.pts_seconds);

                    if !signaled_first {
                        ffr.store(true, Ordering::Release);
                        signaled_first = true;
                    }

                    if frame_tx.send(frame).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("Failed to spawn reverse pacer thread: {e}"))?;

        Ok(Self {
            frame_rx,
            stop_tx: Some(stop_tx),
            speed_tx,
            first_frame_ready,
            _decode_handle: Some(decode_handle),
            _pacer_handle: Some(pacer_handle),
        })
    }

    pub fn is_first_frame_ready(&self) -> bool {
        self.first_frame_ready.load(Ordering::Acquire)
    }

    pub fn begin_playing(&self) -> Result<(), String> {
        Ok(())
    }

    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    pub fn update_speed(&self, speed: f64) {
        let _ = self.speed_tx.send(speed);
    }

    fn signal_stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for GstReversePipelineHandle {
    fn drop(&mut self) {
        self.signal_stop();
    }
}
