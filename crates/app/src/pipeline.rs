use std::path::{Path, PathBuf};
use std::sync::mpsc;

use wizard_media::gst_pipeline::{GstAudioOnlyHandle, GstPipelineHandle, GstReversePipelineHandle};
use wizard_media::pipeline::{DecodedFrame, ReversePipelineHandle as CpuReversePipelineHandle};
use wizard_state::clip::ClipId;
use wizard_state::timeline::TimelineClipId;

use crate::audio_mixer::AudioMixer;
use crate::constants::*;
use crate::debug_log;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PipelineStatus {
    StartingUp,
    Delivering,
    Stalled,
    LongStall,
}

impl PipelineStatus {
    pub fn is_stalled(self) -> bool {
        matches!(self, PipelineStatus::Stalled | PipelineStatus::LongStall)
    }
}

pub struct ForwardPipelineState {
    pub handle: GstPipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub pts_offset: Option<f64>,
    pub speed: f64,
    pub frame_delivered: bool,
    pub activated: bool,
    pub started_at: f64,
    pub last_frame_time: Option<f64>,
    pub age: u32,
}

impl ForwardPipelineState {
    pub fn status(&self, now: f64) -> PipelineStatus {
        if !self.frame_delivered {
            return PipelineStatus::StartingUp;
        }
        match self.last_frame_time {
            Some(t) if now - t > FRAME_GAP_LONG_STALL_S => PipelineStatus::LongStall,
            Some(t) if now - t > FRAME_GAP_STALL_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }

    pub fn stall_status(&self, now: f64) -> PipelineStatus {
        if !self.frame_delivered {
            return PipelineStatus::StartingUp;
        }
        match self.last_frame_time {
            Some(t) if now - t > PIPELINE_STALL_THRESHOLD_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }
}

pub struct ShadowPipelineState {
    pub handle: GstPipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub first_frame_ready: bool,
    pub buffered_frame: Option<DecodedFrame>,
    pub audio_sources: Vec<(GstAudioOnlyHandle, wizard_audio::output::AudioConsumer)>,
}

pub enum ReversePipelineHandle {
    Gst(GstReversePipelineHandle),
    Cpu(CpuReversePipelineHandle),
}

impl ReversePipelineHandle {
    pub fn is_first_frame_ready(&self) -> bool {
        match self {
            ReversePipelineHandle::Gst(handle) => handle.is_first_frame_ready(),
            ReversePipelineHandle::Cpu(_) => true,
        }
    }

    pub fn begin_playing(&self) -> Result<(), String> {
        match self {
            ReversePipelineHandle::Gst(handle) => handle.begin_playing(),
            ReversePipelineHandle::Cpu(_) => Ok(()),
        }
    }

    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        match self {
            ReversePipelineHandle::Gst(handle) => handle.try_recv_frame(),
            ReversePipelineHandle::Cpu(handle) => handle.try_recv_frame(),
        }
    }

    pub fn update_speed(&self, speed: f64) {
        match self {
            ReversePipelineHandle::Gst(handle) => handle.update_speed(speed),
            ReversePipelineHandle::Cpu(handle) => handle.update_speed(speed),
        }
    }
}

pub struct ReversePipelineState {
    pub handle: ReversePipelineHandle,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub pts_offset: Option<f64>,
    pub speed: f64,
    pub activated: bool,
    pub started_at: f64,
    pub last_frame_time: Option<f64>,
}

impl ReversePipelineState {
    pub fn status(&self, now: f64) -> PipelineStatus {
        match self.last_frame_time {
            None => PipelineStatus::StartingUp,
            Some(t) if now - t > FRAME_GAP_LONG_STALL_S => PipelineStatus::LongStall,
            Some(t) if now - t > FRAME_GAP_STALL_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }

    pub fn stall_status(&self, now: f64) -> PipelineStatus {
        match self.last_frame_time {
            None => PipelineStatus::StartingUp,
            Some(t) if now - t > PIPELINE_STALL_THRESHOLD_S => PipelineStatus::Stalled,
            _ => PipelineStatus::Delivering,
        }
    }
}

pub struct PendingPipeline {
    pub rx: mpsc::Receiver<Result<GstPipelineHandle, String>>,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub speed: f64,
    pub started_at: f64,
}

pub struct PendingReversePipeline {
    pub rx: mpsc::Receiver<Result<ReversePipelineHandle, String>>,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub speed: f64,
    pub started_at: f64,
}

impl PendingReversePipeline {
    fn should_use_cpu_reverse(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .is_some_and(|ext| matches!(ext.as_str(), "mp4" | "mov" | "m4v" | "mpg" | "mpeg"))
    }

    pub fn spawn(
        path: &Path,
        source_time: f64,
        target_w: u32,
        target_h: u32,
        speed: f64,
        clip_id: ClipId,
        timeline_clip_id: TimelineClipId,
        now: f64,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let path_buf = path.to_path_buf();
        std::thread::Builder::new()
            .name("reverse-pipeline-spawn".into())
            .spawn(move || {
                let use_cpu = Self::should_use_cpu_reverse(&path_buf);
                // #region agent log
                debug_log::emit(
                    "H1",
                    "crates/app/src/pipeline.rs:PendingReversePipeline::spawn",
                    "reverse backend selected",
                    &format!(
                        "path={} extension={} backend={} sourceTime={:.3} speed={:.3}",
                        path_buf.display(),
                        path_buf.extension().and_then(|e| e.to_str()).unwrap_or(""),
                        if use_cpu { "cpu" } else { "gst" },
                        source_time,
                        speed
                    ),
                );
                // #endregion
                let result = if use_cpu {
                    CpuReversePipelineHandle::start(
                        &path_buf,
                        source_time,
                        speed,
                        target_w,
                        target_h,
                    )
                    .map(ReversePipelineHandle::Cpu)
                } else {
                    GstReversePipelineHandle::start(
                        &path_buf,
                        source_time,
                        speed,
                        target_w,
                        target_h,
                    )
                    .map(ReversePipelineHandle::Gst)
                };
                let _ = tx.send(result);
            })
            .expect("failed to spawn reverse-pipeline-spawn thread");
        Self {
            rx,
            clip: (clip_id, path.to_path_buf()),
            timeline_clip: timeline_clip_id,
            speed,
            started_at: now,
        }
    }

    pub fn try_recv(&self) -> Option<Result<ReversePipelineHandle, String>> {
        self.rx.try_recv().ok()
    }
}

pub struct ShadowAudioSourceRequest {
    pub path: PathBuf,
    pub source_time: f64,
}

pub struct ShadowPipelineBuild {
    pub handle: GstPipelineHandle,
    pub audio_sources: Vec<(GstAudioOnlyHandle, wizard_audio::output::AudioConsumer)>,
}

pub struct PendingShadowPipeline {
    pub rx: mpsc::Receiver<Result<ShadowPipelineBuild, String>>,
    pub clip: (ClipId, PathBuf),
    pub timeline_clip: TimelineClipId,
    pub speed: f64,
    pub started_at: f64,
}

impl PendingPipeline {
    pub fn spawn(
        path: &Path,
        source_time: f64,
        target_w: u32,
        target_h: u32,
        audio_sample_rate: u32,
        audio_channels: u16,
        speed: f64,
        clip_id: ClipId,
        timeline_clip_id: TimelineClipId,
        now: f64,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let path_buf = path.to_path_buf();
        std::thread::Builder::new()
            .name("pipeline-spawn".into())
            .spawn(move || {
                let result = GstPipelineHandle::start(
                    &path_buf,
                    source_time,
                    target_w,
                    target_h,
                    None,
                    audio_sample_rate,
                    audio_channels,
                    speed,
                );
                let _ = tx.send(result);
            })
            .expect("failed to spawn pipeline-spawn thread");

        Self {
            rx,
            clip: (clip_id, path.to_path_buf()),
            timeline_clip: timeline_clip_id,
            speed,
            started_at: now,
        }
    }

    pub fn try_recv(&self) -> Option<Result<GstPipelineHandle, String>> {
        self.rx.try_recv().ok()
    }
}

impl PendingShadowPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        path: &Path,
        source_time: f64,
        target_w: u32,
        target_h: u32,
        audio_sample_rate: u32,
        audio_channels: u16,
        speed: f64,
        clip_id: ClipId,
        timeline_clip_id: TimelineClipId,
        audio_requests: Vec<ShadowAudioSourceRequest>,
        now: f64,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let path_buf = path.to_path_buf();
        std::thread::Builder::new()
            .name("shadow-pipeline-spawn".into())
            .spawn(move || {
                let handle = match GstPipelineHandle::start(
                    &path_buf,
                    source_time,
                    target_w,
                    target_h,
                    None,
                    audio_sample_rate,
                    audio_channels,
                    speed,
                ) {
                    Ok(handle) => handle,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };

                let mut audio_sources = Vec::new();
                for req in audio_requests {
                    let (producer, consumer) = AudioMixer::create_source_producer();
                    let source_producer = std::sync::Arc::new(std::sync::Mutex::new(producer));
                    if let Ok(audio_handle) = GstAudioOnlyHandle::start(
                        &req.path,
                        req.source_time,
                        source_producer,
                        audio_sample_rate,
                        audio_channels,
                        speed,
                    ) {
                        audio_sources.push((audio_handle, consumer));
                    }
                }

                let _ = tx.send(Ok(ShadowPipelineBuild {
                    handle,
                    audio_sources,
                }));
            })
            .expect("failed to spawn shadow-pipeline-spawn thread");

        Self {
            rx,
            clip: (clip_id, path.to_path_buf()),
            timeline_clip: timeline_clip_id,
            speed,
            started_at: now,
        }
    }

    pub fn try_recv(&self) -> Option<Result<ShadowPipelineBuild, String>> {
        self.rx.try_recv().ok()
    }
}
