use egui::Rect;
use wizard_state::project::AppState;
use wizard_state::timeline::{TrackId, TrackKind};

pub const TRACK_HEIGHT: f32 = 60.0;
pub const TRACK_HEADER_WIDTH: f32 = 70.0;
pub const RULER_HEIGHT: f32 = 24.0;
pub const SCROLLBAR_HEIGHT: f32 = 12.0;
pub const V_SCROLLBAR_WIDTH: f32 = 10.0;
pub const ZOOM_MIN: f32 = 20.0;
pub const ZOOM_MAX: f32 = 500.0;
pub const SNAP_THRESHOLD_PX: f32 = 10.0;
pub const THUMB_WIDTH: f32 = 50.0;
pub const TRIM_HANDLE_WIDTH: f32 = 12.0;
pub const MIN_CLIP_DURATION: f64 = 0.1;

pub struct TrackLayout {
    pub track_id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub display_index: usize,
    pub pair_index: usize,
    pub muted: bool,
    pub visible: bool,
}

pub fn build_track_layout(state: &AppState) -> Vec<TrackLayout> {
    let mut layouts = Vec::new();
    let mut idx = 0;
    for (pair_i, track) in state.project.timeline.video_tracks.iter().enumerate().rev() {
        layouts.push(TrackLayout {
            track_id: track.id,
            kind: track.kind,
            name: track.name.clone(),
            display_index: idx,
            pair_index: pair_i,
            muted: track.muted,
            visible: track.visible,
        });
        idx += 1;
    }
    for (pair_i, track) in state.project.timeline.audio_tracks.iter().enumerate() {
        layouts.push(TrackLayout {
            track_id: track.id,
            kind: track.kind,
            name: track.name.clone(),
            display_index: idx,
            pair_index: pair_i,
            muted: track.muted,
            visible: track.visible,
        });
        idx += 1;
    }
    layouts
}

pub fn snap_time_to_clip_boundaries(
    state: &AppState,
    candidate_time: f64,
    pps: f32,
    exclude_clip: Option<wizard_state::timeline::TimelineClipId>,
) -> (f64, bool) {
    if pps <= 0.0 {
        return (candidate_time.max(0.0), false);
    }

    let snap_threshold_time = (SNAP_THRESHOLD_PX / pps) as f64;
    let mut best_time = candidate_time.max(0.0);
    let mut best_dist = f64::INFINITY;

    for track in state.project.timeline.all_tracks() {
        for tc in &track.clips {
            if exclude_clip.is_some_and(|id| id == tc.id) {
                continue;
            }
            let start = tc.timeline_start;
            let end = tc.timeline_start + tc.duration;

            let start_dist = (candidate_time - start).abs();
            if start_dist <= snap_threshold_time && start_dist < best_dist {
                best_dist = start_dist;
                best_time = start;
            }

            let end_dist = (candidate_time - end).abs();
            if end_dist <= snap_threshold_time && end_dist < best_dist {
                best_dist = end_dist;
                best_time = end;
            }
        }
    }

    if best_dist.is_finite() {
        (best_time.max(0.0), true)
    } else {
        (candidate_time.max(0.0), false)
    }
}

pub fn center_crop_uv(tex: &egui::TextureHandle, display_h: f32, display_w: f32) -> Rect {
    let tex_size = tex.size();
    let tex_w = tex_size[0] as f32;
    let tex_h = tex_size[1] as f32;
    if tex_w == 0.0 || tex_h == 0.0 || display_w == 0.0 || display_h == 0.0 {
        return Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    }
    let tex_aspect = tex_w / tex_h;
    let display_aspect = display_w / display_h;
    if display_aspect > tex_aspect {
        let crop_h = tex_aspect / display_aspect;
        let margin = (1.0 - crop_h) / 2.0;
        Rect::from_min_max(egui::pos2(0.0, margin), egui::pos2(1.0, 1.0 - margin))
    } else {
        let crop_w = display_aspect / tex_aspect;
        let margin = (1.0 - crop_w) / 2.0;
        Rect::from_min_max(egui::pos2(margin, 0.0), egui::pos2(1.0 - margin, 1.0))
    }
}

pub fn visible_peak_slice(
    peaks: &[(f32, f32)],
    source_in: f64,
    source_out: f64,
    source_duration: Option<f64>,
) -> &[(f32, f32)] {
    if let Some(total_dur) = source_duration {
        if total_dur > 0.0 {
            let in_frac = source_in / total_dur;
            let out_frac = source_out / total_dur;
            let start_idx = (in_frac * peaks.len() as f64) as usize;
            let end_idx = (out_frac * peaks.len() as f64) as usize;
            return &peaks[start_idx.min(peaks.len())..end_idx.min(peaks.len())];
        }
    }
    peaks
}
