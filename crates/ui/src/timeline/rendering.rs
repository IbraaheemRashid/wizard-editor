use egui::{pos2, vec2, Color32, CornerRadius, Rect, Stroke};
use wizard_state::clip::ClipId;
use wizard_state::project::AppState;
use wizard_state::timeline::TrackKind;

use crate::theme;
use crate::TextureLookup;

use super::layout::*;

pub struct ClipGhostParams<'a> {
    pub clip_id: &'a ClipId,
    pub duration: f64,
    pub drop_time: f64,
    pub track_y: f32,
    pub content_left: f32,
    pub pps: f32,
    pub scroll: f32,
    pub clip_color: Color32,
    pub track_kind: TrackKind,
    pub tracks_top: f32,
    pub num_tracks: usize,
    pub state: &'a AppState,
    pub textures: &'a dyn TextureLookup,
}

pub fn draw_waveform(painter: &egui::Painter, rect: Rect, peaks: &[(f32, f32)]) {
    if peaks.is_empty() {
        return;
    }

    let wave_color = theme::WAVEFORM_COLOR;

    let center_y = rect.center().y;
    let half_h = rect.height() * 0.45;
    let num_bars = (rect.width() as usize).min(peaks.len()).max(1);
    let bar_width = rect.width() / num_bars as f32;
    let min_bar_half = 1.0_f32;

    for i in 0..num_bars {
        let peak_idx = (i as f32 / num_bars as f32 * peaks.len() as f32) as usize;
        let peak_idx = peak_idx.min(peaks.len() - 1);
        let (min_val, max_val) = peaks[peak_idx];

        let amp_top = max_val.abs().max(min_val.abs()).max(min_bar_half / half_h);
        let amp_bottom = min_val.abs().max(min_bar_half / half_h);

        let top = center_y - amp_top * half_h;
        let bottom = center_y + amp_bottom * half_h;
        let x = rect.min.x + i as f32 * bar_width;

        let bar_rect = Rect::from_min_max(pos2(x, top), pos2(x + bar_width.max(1.0), bottom));
        painter.rect_filled(bar_rect, CornerRadius::ZERO, wave_color);
    }
}

pub fn draw_ruler(ui: &mut egui::Ui, left: f32, top: f32, width: f32, pps: f32, scroll: f32) {
    let ruler_rect = Rect::from_min_size(pos2(left, top), vec2(width, RULER_HEIGHT));
    ui.painter()
        .rect_filled(ruler_rect, CornerRadius::ZERO, theme::RULER_BG);

    let ruler_painter = ui.painter().with_clip_rect(ruler_rect);

    let start_time = scroll / pps;
    let visible_duration = width / pps;
    let first_second = start_time.floor() as i32;
    let last_second = (start_time + visible_duration).ceil() as i32 + 1;

    for s in first_second..last_second {
        if s < 0 {
            continue;
        }
        let x = left + s as f32 * pps - scroll;
        if x < left - pps || x > left + width + pps {
            continue;
        }

        ruler_painter.line_segment(
            [
                pos2(x, top + RULER_HEIGHT - 8.0),
                pos2(x, top + RULER_HEIGHT),
            ],
            Stroke::new(1.0, theme::TEXT_DIM),
        );

        let minutes = s / 60;
        let secs = s % 60;
        let label = format!("{minutes}:{secs:02}");
        ruler_painter.text(
            pos2(x + 2.0, top + 2.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::monospace(9.0),
            theme::TEXT_DIM,
        );

        for sub in 1..4 {
            let sub_x = x + sub as f32 * pps / 4.0;
            if sub_x >= left && sub_x < left + width {
                ruler_painter.line_segment(
                    [
                        pos2(sub_x, top + RULER_HEIGHT - 4.0),
                        pos2(sub_x, top + RULER_HEIGHT),
                    ],
                    Stroke::new(0.5, theme::RULER_TICK),
                );
            }
        }
    }
}

pub fn draw_scrollbar(ui: &mut egui::Ui, state: &mut AppState, left: f32, width: f32, top: f32) {
    let scrollbar_rect = Rect::from_min_size(pos2(left, top), vec2(width, SCROLLBAR_HEIGHT));
    ui.painter()
        .rect_filled(scrollbar_rect, CornerRadius::ZERO, theme::RULER_BG);

    let pps = state.ui.timeline.zoom;
    let scroll = state.ui.timeline.scroll_offset;

    let mut total_duration: f64 = 10.0;
    for track in state.project.timeline.all_tracks() {
        for tc in &track.clips {
            let end = tc.timeline_start + tc.duration;
            if end > total_duration {
                total_duration = end;
            }
        }
    }
    total_duration += 5.0;

    let total_width = total_duration as f32 * pps;
    let visible_fraction = (width / total_width).clamp(0.05, 1.0);
    let scroll_fraction = scroll / (total_width - width).max(1.0);

    let thumb_w = (width * visible_fraction).max(20.0);
    let thumb_x = left + scroll_fraction * (width - thumb_w);
    let thumb_rect = Rect::from_min_size(
        pos2(thumb_x, top + 1.0),
        vec2(thumb_w, SCROLLBAR_HEIGHT - 2.0),
    );

    ui.painter()
        .rect_filled(thumb_rect, CornerRadius::same(3), theme::BG_SURFACE);

    let response = ui.interact(
        scrollbar_rect,
        egui::Id::new("timeline_scrollbar"),
        egui::Sense::click_and_drag(),
    );

    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frac = ((pointer.x - left) / width).clamp(0.0, 1.0);
            let max_scroll = (total_width - width).max(0.0);
            state.ui.timeline.scroll_offset = frac * max_scroll;
        }
    }
}

pub fn draw_vertical_scrollbar(
    ui: &mut egui::Ui,
    state: &mut AppState,
    left: f32,
    top: f32,
    height: f32,
    total_track_height: f32,
) {
    let scrollbar_rect = Rect::from_min_size(pos2(left, top), vec2(V_SCROLLBAR_WIDTH, height));
    ui.painter()
        .rect_filled(scrollbar_rect, CornerRadius::ZERO, theme::RULER_BG);

    let visible_fraction = (height / total_track_height).clamp(0.05, 1.0);
    let max_v_scroll = (total_track_height - height).max(1.0);
    let scroll_fraction = state.ui.timeline.vertical_scroll_offset / max_v_scroll;

    let thumb_h = (height * visible_fraction).max(20.0);
    let thumb_y = top + scroll_fraction * (height - thumb_h);
    let thumb_rect = Rect::from_min_size(
        pos2(left + 1.0, thumb_y),
        vec2(V_SCROLLBAR_WIDTH - 2.0, thumb_h),
    );

    ui.painter()
        .rect_filled(thumb_rect, CornerRadius::same(3), theme::BG_SURFACE);

    let response = ui.interact(
        scrollbar_rect,
        egui::Id::new("timeline_v_scrollbar"),
        egui::Sense::click_and_drag(),
    );

    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frac = ((pointer.y - top) / height).clamp(0.0, 1.0);
            state.ui.timeline.vertical_scroll_offset = frac * max_v_scroll;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_drag_ghosts(
    ui: &mut egui::Ui,
    state: &AppState,
    tracks_top: f32,
    content_left: f32,
    pps: f32,
    scroll: f32,
    textures: &dyn TextureLookup,
    content_clip_rect: Rect,
) {
    let pointer = match ui.ctx().pointer_hover_pos() {
        Some(p) => p,
        None => return,
    };

    let track_layouts = build_track_layout(state);
    let num_tracks = track_layouts.len();
    if num_tracks == 0 {
        return;
    }

    let target_display_idx = ((pointer.y - tracks_top) / (TRACK_HEIGHT + 2.0))
        .floor()
        .max(0.0) as usize;
    if target_display_idx >= num_tracks {
        return;
    }

    let target_kind = track_layouts[target_display_idx].kind;
    let raw_drop_time = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;

    let has_timeline_drag = !state.ui.timeline.dragging_clips.is_empty();
    let grab_offset = state.ui.timeline.drag_grab_offset.unwrap_or(0.0);
    let unsnapped_drop_time = if has_timeline_drag {
        (raw_drop_time - grab_offset).max(0.0)
    } else {
        raw_drop_time
    };
    let exclude_clip = state.ui.timeline.drag_primary_clip;
    let (drop_time, _snapped) =
        snap_time_to_clip_boundaries(state, unsnapped_drop_time, pps, exclude_clip);

    let paired_track_id = track_layouts[target_display_idx].track_id;
    let paired_id = state.project.timeline.paired_track_id(paired_track_id);
    let paired_display_idx =
        paired_id.and_then(|pid| track_layouts.iter().position(|l| l.track_id == pid));

    if !has_timeline_drag {
        if let Some(payload) = egui::DragAndDrop::payload::<Vec<ClipId>>(ui.ctx()) {
            let clip_ids = payload.as_ref();
            let mut cursor = drop_time;
            for clip_id in clip_ids {
                let duration = state
                    .project
                    .clips
                    .get(clip_id)
                    .and_then(|c| c.duration)
                    .unwrap_or(3.0)
                    .max(0.1);
                let clip_color = match target_kind {
                    TrackKind::Video => theme::CLIP_VIDEO,
                    TrackKind::Audio => theme::CLIP_AUDIO,
                };
                let track_y = tracks_top + target_display_idx as f32 * (TRACK_HEIGHT + 2.0);
                draw_clip_ghost(
                    ui,
                    content_clip_rect,
                    &ClipGhostParams {
                        clip_id,
                        duration,
                        drop_time: cursor,
                        track_y,
                        content_left,
                        pps,
                        scroll,
                        clip_color,
                        track_kind: target_kind,
                        tracks_top,
                        num_tracks,
                        state,
                        textures,
                    },
                );
                if let Some(p_idx) = paired_display_idx {
                    let paired_kind = track_layouts[p_idx].kind;
                    let paired_color = match paired_kind {
                        TrackKind::Video => theme::CLIP_VIDEO,
                        TrackKind::Audio => theme::CLIP_AUDIO,
                    };
                    let paired_y = tracks_top + p_idx as f32 * (TRACK_HEIGHT + 2.0);
                    draw_clip_ghost(
                        ui,
                        content_clip_rect,
                        &ClipGhostParams {
                            clip_id,
                            duration,
                            drop_time: cursor,
                            track_y: paired_y,
                            content_left,
                            pps,
                            scroll,
                            clip_color: paired_color,
                            track_kind: paired_kind,
                            tracks_top,
                            num_tracks,
                            state,
                            textures,
                        },
                    );
                }
                cursor += duration;
            }
        }
    }

    if has_timeline_drag {
        let is_dragging = ui.input(|i| i.pointer.any_down());
        if is_dragging {
            let primary_id = state.ui.timeline.drag_primary_clip;
            let primary_start = primary_id
                .and_then(|id| state.project.timeline.find_clip(id))
                .map(|(_, _, tc)| tc.timeline_start)
                .unwrap_or(0.0);
            let delta = drop_time - primary_start;

            let dragging: Vec<_> = state.ui.timeline.dragging_clips.iter().copied().collect();
            for src_clip_id in dragging {
                if let Some((_, _, tc)) = state.project.timeline.find_clip(src_clip_id) {
                    let clip_id = tc.source_id;
                    let duration = tc.duration;
                    let clip_drop_time = (tc.timeline_start + delta).max(0.0);
                    let tc_track_id = tc.track_id;

                    if let Some(l_idx) =
                        track_layouts.iter().position(|l| l.track_id == tc_track_id)
                    {
                        let tk = track_layouts[l_idx].kind;
                        let clip_color = match tk {
                            TrackKind::Video => theme::CLIP_VIDEO,
                            TrackKind::Audio => theme::CLIP_AUDIO,
                        };
                        let track_y = tracks_top + l_idx as f32 * (TRACK_HEIGHT + 2.0);
                        draw_clip_ghost(
                            ui,
                            content_clip_rect,
                            &ClipGhostParams {
                                clip_id: &clip_id,
                                duration,
                                drop_time: clip_drop_time,
                                track_y,
                                content_left,
                                pps,
                                scroll,
                                clip_color,
                                track_kind: tk,
                                tracks_top,
                                num_tracks,
                                state,
                                textures,
                            },
                        );
                    }
                }
            }
        }
    }
}

fn draw_clip_ghost(ui: &mut egui::Ui, clip_rect: Rect, p: &ClipGhostParams<'_>) {
    let ghost_x = p.content_left + p.drop_time as f32 * p.pps - p.scroll;
    let ghost_w = p.duration as f32 * p.pps;
    let ghost_rect = Rect::from_min_size(
        pos2(ghost_x, p.track_y + 2.0),
        vec2(ghost_w, TRACK_HEIGHT - 4.0),
    );

    let painter = ui.painter().with_clip_rect(clip_rect);

    let ghost_color =
        Color32::from_rgba_unmultiplied(p.clip_color.r(), p.clip_color.g(), p.clip_color.b(), 100);
    painter.rect_filled(ghost_rect, theme::ROUNDING_SM, ghost_color);

    painter.rect_stroke(
        ghost_rect,
        theme::ROUNDING_SM,
        Stroke::new(1.0, Color32::from_white_alpha(80)),
        egui::StrokeKind::Outside,
    );

    if p.track_kind == TrackKind::Video {
        let thumb_w = THUMB_WIDTH.min(ghost_w);
        let thumb_rect = Rect::from_min_size(
            pos2(ghost_x, p.track_y + 2.0),
            vec2(thumb_w, TRACK_HEIGHT - 4.0),
        );
        if let Some(tex) = p.textures.thumbnail(p.clip_id) {
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            let tint = Color32::from_rgba_unmultiplied(255, 255, 255, 100);
            painter.image(tex.id(), thumb_rect, uv, tint);
        }
    }

    if let Some(clip) = p.state.project.clips.get(p.clip_id) {
        let name = clip.display_name();
        let label = if name.len() > 15 {
            format!("{}...", &name[..12])
        } else {
            name.to_string()
        };
        painter.text(
            pos2(ghost_rect.min.x + 4.0, ghost_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(10.0),
            Color32::from_white_alpha(180),
        );
    }

    let line_top = p.tracks_top;
    let line_bottom = p.tracks_top + p.num_tracks as f32 * (TRACK_HEIGHT + 2.0);
    painter.line_segment(
        [pos2(ghost_x, line_top), pos2(ghost_x, line_bottom)],
        Stroke::new(1.0, Color32::from_white_alpha(120)),
    );

    let total_secs = p.drop_time;
    let minutes = (total_secs / 60.0).floor() as i32;
    let secs = total_secs % 60.0;
    let time_label = format!("{minutes}:{secs:04.1}");
    painter.text(
        pos2(ghost_x + 2.0, p.track_y - 2.0),
        egui::Align2::LEFT_BOTTOM,
        time_label,
        egui::FontId::monospace(9.0),
        Color32::from_white_alpha(200),
    );
}
