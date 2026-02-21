use egui::{pos2, vec2, Color32, CornerRadius, DragAndDrop, Rect, Sense, Stroke};
use wizard_state::clip::ClipId;
use wizard_state::project::AppState;
use wizard_state::timeline::TrackKind;

use crate::browser::TextureLookup;
use crate::theme;

const TRACK_HEIGHT: f32 = 60.0;
const TRACK_HEADER_WIDTH: f32 = 60.0;
const RULER_HEIGHT: f32 = 24.0;
const SCROLLBAR_HEIGHT: f32 = 12.0;
const ZOOM_MIN: f32 = 20.0;
const ZOOM_MAX: f32 = 500.0;
const SNAP_THRESHOLD_PX: f32 = 5.0;
const THUMB_WIDTH: f32 = 50.0;

pub fn timeline_panel(ui: &mut egui::Ui, state: &mut AppState, textures: &dyn TextureLookup) {
    state.ui.timeline_scrubbing = None;
    ui.set_min_width(0.0);
    ui.set_min_height(0.0);

    ui.heading("Timeline");
    ui.separator();

    let available = ui.available_rect_before_wrap();
    let num_tracks = state.project.tracks.len() as f32;
    let content_height = RULER_HEIGHT + num_tracks * (TRACK_HEIGHT + 2.0) + SCROLLBAR_HEIGHT;
    let timeline_rect = Rect::from_min_size(available.min, vec2(available.width(), content_height));

    let content_left = timeline_rect.min.x + TRACK_HEADER_WIDTH;
    let content_width = timeline_rect.width() - TRACK_HEADER_WIDTH;

    handle_zoom_scroll(ui, state, timeline_rect, content_left);
    let pps = state.ui.timeline_zoom;
    let scroll = state.ui.timeline_scroll_offset;

    draw_ruler(
        ui,
        content_left,
        timeline_rect.min.y,
        content_width,
        pps,
        scroll,
    );

    let tracks_top = timeline_rect.min.y + RULER_HEIGHT;

    let mut pending_browser_drop: Option<(ClipId, usize, f64)> = None;

    for (i, track) in state.project.tracks.iter().enumerate() {
        let track_id = track.id;
        let y = tracks_top + i as f32 * (TRACK_HEIGHT + 2.0);

        let header_rect = Rect::from_min_size(
            pos2(timeline_rect.min.x, y),
            vec2(TRACK_HEADER_WIDTH, TRACK_HEIGHT),
        );
        ui.painter()
            .rect_filled(header_rect, CornerRadius::ZERO, theme::TRACK_HEADER_BG);
        ui.painter().text(
            header_rect.center(),
            egui::Align2::CENTER_CENTER,
            &track.name,
            egui::FontId::proportional(12.0),
            theme::TEXT_PRIMARY,
        );

        let track_rect =
            Rect::from_min_size(pos2(content_left, y), vec2(content_width, TRACK_HEIGHT));

        let track_response = ui.allocate_rect(track_rect, Sense::hover());

        if let Some(payload) = track_response.dnd_release_payload::<ClipId>() {
            if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                let t = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;
                pending_browser_drop = Some((*payload, i, t));
            }
        }

        ui.painter()
            .rect_filled(track_rect, CornerRadius::ZERO, theme::TRACK_BG);

        if track_response.dnd_hover_payload::<ClipId>().is_some() {
            ui.painter().rect_stroke(
                track_rect,
                CornerRadius::ZERO,
                Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Outside,
            );
        }

        let clip_color = match track.kind {
            TrackKind::Video => theme::CLIP_VIDEO,
            TrackKind::Audio => theme::CLIP_AUDIO,
        };

        for (clip_idx, tc) in track.clips.iter().enumerate() {
            let clip_x = content_left + tc.position as f32 * pps - scroll;
            let clip_w = tc.duration as f32 * pps;
            let clip_rect =
                Rect::from_min_size(pos2(clip_x, y + 2.0), vec2(clip_w, TRACK_HEIGHT - 4.0));

            if clip_rect.max.x < content_left || clip_rect.min.x > content_left + content_width {
                continue;
            }

            let is_selected = state
                .ui
                .selection
                .selected_timeline_clip
                .is_some_and(|(tid, idx)| tid == track_id && idx == clip_idx);

            let is_being_dragged = state
                .ui
                .timeline_dragging_clip
                .is_some_and(|(tid, idx)| tid == track_id && idx == clip_idx);

            ui.painter()
                .rect_filled(clip_rect, theme::ROUNDING_SM, clip_color);

            if track.kind == TrackKind::Video {
                let thumb_w = THUMB_WIDTH.min(clip_w);
                let thumb_rect =
                    Rect::from_min_size(pos2(clip_x, y + 2.0), vec2(thumb_w, TRACK_HEIGHT - 4.0));
                if let Some(tex) = textures.thumbnail(&tc.clip_id) {
                    let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(tex.id(), thumb_rect, uv, Color32::WHITE);
                }
            } else if track.kind == TrackKind::Audio {
                if let Some(peaks) = textures.waveform_peaks(&tc.clip_id) {
                    draw_waveform(ui, clip_rect, peaks, clip_color);
                }
            }

            if let Some(clip) = state.project.clips.get(&tc.clip_id) {
                let name = clip.display_name();
                let label = if name.len() > 15 {
                    format!("{}...", &name[..12])
                } else {
                    name.to_string()
                };
                let label_rect = Rect::from_min_size(
                    pos2(clip_rect.min.x, clip_rect.max.y - 16.0),
                    vec2(clip_w.min(clip_rect.width()), 16.0),
                );
                ui.painter().rect_filled(
                    label_rect,
                    CornerRadius::ZERO,
                    Color32::from_black_alpha(160),
                );
                ui.painter().text(
                    pos2(clip_rect.min.x + 4.0, clip_rect.max.y - 8.0),
                    egui::Align2::LEFT_CENTER,
                    label,
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
            }

            if state.project.starred.contains(&tc.clip_id) {
                ui.painter().text(
                    clip_rect.right_top() + vec2(-12.0, 2.0),
                    egui::Align2::CENTER_TOP,
                    "\u{2605}",
                    egui::FontId::proportional(10.0),
                    theme::STAR_COLOR,
                );
            }

            if is_selected {
                ui.painter().rect_stroke(
                    clip_rect,
                    theme::ROUNDING_SM,
                    Stroke::new(2.0, theme::ACCENT),
                    egui::StrokeKind::Outside,
                );
            }

            if is_being_dragged {
                ui.painter().rect_filled(
                    clip_rect,
                    theme::ROUNDING_SM,
                    Color32::from_white_alpha(30),
                );
            }

            let clip_response = ui.interact(
                clip_rect,
                egui::Id::new(("timeline_clip", track_id, clip_idx)),
                Sense::click_and_drag(),
            );

            if clip_response.clicked() {
                state.ui.selection.selected_timeline_clip = Some((track_id, clip_idx));
                state.ui.selection.selected_clip = Some(tc.clip_id);
            }

            if clip_response.drag_started() {
                state.ui.timeline_dragging_clip = Some((track_id, clip_idx));
            }
        }
    }

    if let Some((clip_id, track_index, position_seconds)) = pending_browser_drop {
        state
            .project
            .add_clip_to_track(clip_id, track_index, position_seconds);
    }

    draw_drag_ghosts(
        ui,
        state,
        tracks_top,
        content_left,
        content_width,
        pps,
        scroll,
        textures,
    );

    handle_clip_drag_drop(ui, state, tracks_top, content_left, pps, scroll);

    let playhead_time = state.project.playback.playhead;
    let mut playhead_x = content_left + playhead_time as f32 * pps - scroll;

    let scrub_rect = Rect::from_min_size(
        pos2(content_left, timeline_rect.min.y),
        vec2(content_width, RULER_HEIGHT),
    );
    let scrub_response = ui.allocate_rect(scrub_rect, Sense::click_and_drag());
    if scrub_response.dragged() || scrub_response.clicked() {
        if let Some(pointer) = scrub_response.interact_pointer_pos() {
            let mut t = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;

            let snap_threshold_time = (SNAP_THRESHOLD_PX / pps) as f64;
            let mut snapped = false;
            for track in &state.project.tracks {
                for tc in &track.clips {
                    let start = tc.position;
                    let end = tc.position + tc.duration;
                    if (t - start).abs() < snap_threshold_time {
                        t = start;
                        snapped = true;
                        break;
                    }
                    if (t - end).abs() < snap_threshold_time {
                        t = end;
                        snapped = true;
                        break;
                    }
                }
                if snapped {
                    break;
                }
            }

            state.project.playback.playhead = t;
            state.ui.timeline_scrubbing = Some(t);
            playhead_x = content_left + t as f32 * pps - scroll;

            if snapped {
                let snap_line_top = tracks_top;
                let snap_line_bottom =
                    tracks_top + state.project.tracks.len() as f32 * (TRACK_HEIGHT + 2.0);
                ui.painter().line_segment(
                    [
                        pos2(playhead_x, snap_line_top),
                        pos2(playhead_x, snap_line_bottom),
                    ],
                    Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.5)),
                );
            }
        }
    }

    let playhead_top = timeline_rect.min.y;
    let playhead_bottom = tracks_top + state.project.tracks.len() as f32 * (TRACK_HEIGHT + 2.0);
    ui.painter().line_segment(
        [
            pos2(playhead_x, playhead_top),
            pos2(playhead_x, playhead_bottom),
        ],
        Stroke::new(2.0, theme::PLAYHEAD_COLOR),
    );

    let playhead_head =
        Rect::from_center_size(pos2(playhead_x, playhead_top + 4.0), vec2(10.0, 8.0));
    ui.painter()
        .rect_filled(playhead_head, CornerRadius::same(2), theme::PLAYHEAD_COLOR);

    draw_scrollbar(
        ui,
        state,
        content_left,
        content_width,
        playhead_bottom + 4.0,
    );
}

fn handle_clip_drag_drop(
    ui: &egui::Ui,
    state: &mut AppState,
    tracks_top: f32,
    content_left: f32,
    pps: f32,
    scroll: f32,
) {
    let Some((src_track_id, src_clip_idx)) = state.ui.timeline_dragging_clip else {
        return;
    };

    let is_dragging = ui.input(|i| i.pointer.any_down());
    if is_dragging {
        return;
    }

    state.ui.timeline_dragging_clip = None;

    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
        return;
    };

    let src_track_index = state
        .project
        .tracks
        .iter()
        .position(|t| t.id == src_track_id);
    let Some(src_idx) = src_track_index else {
        return;
    };

    let dst_track_index = ((pointer.y - tracks_top) / (TRACK_HEIGHT + 2.0))
        .floor()
        .max(0.0) as usize;
    if dst_track_index >= state.project.tracks.len() {
        return;
    }

    let new_pos = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;

    if src_idx == dst_track_index {
        state
            .project
            .move_clip_on_track(src_idx, src_clip_idx, new_pos);
    } else {
        state
            .project
            .move_clip_across_tracks(src_idx, src_clip_idx, dst_track_index, new_pos);
    }
    state.ui.selection.selected_timeline_clip = None;
}

fn draw_waveform(ui: &mut egui::Ui, rect: Rect, peaks: &[(f32, f32)], base_color: Color32) {
    if peaks.is_empty() {
        return;
    }

    let wave_color = Color32::from_rgba_premultiplied(
        (base_color.r() as u16 * 180 / 255) as u8,
        (base_color.g() as u16 * 255 / 255) as u8,
        (base_color.b() as u16 * 180 / 255) as u8,
        220,
    );

    let center_y = rect.center().y;
    let half_h = rect.height() * 0.4;
    let num_bars = (rect.width() as usize).min(peaks.len()).max(1);
    let bar_width = rect.width() / num_bars as f32;

    for i in 0..num_bars {
        let peak_idx = (i as f32 / num_bars as f32 * peaks.len() as f32) as usize;
        let peak_idx = peak_idx.min(peaks.len() - 1);
        let (min_val, max_val) = peaks[peak_idx];

        let top = center_y - max_val.abs() * half_h;
        let bottom = center_y + min_val.abs() * half_h;
        let x = rect.min.x + i as f32 * bar_width;

        if (bottom - top).abs() > 0.5 {
            let bar_rect = Rect::from_min_max(pos2(x, top), pos2(x + bar_width.max(1.0), bottom));
            ui.painter()
                .rect_filled(bar_rect, CornerRadius::ZERO, wave_color);
        }
    }
}

fn draw_scrollbar(ui: &mut egui::Ui, state: &mut AppState, left: f32, width: f32, top: f32) {
    let scrollbar_rect = Rect::from_min_size(pos2(left, top), vec2(width, SCROLLBAR_HEIGHT));
    ui.painter()
        .rect_filled(scrollbar_rect, CornerRadius::ZERO, theme::RULER_BG);

    let pps = state.ui.timeline_zoom;
    let scroll = state.ui.timeline_scroll_offset;

    let mut total_duration: f64 = 10.0;
    for track in &state.project.tracks {
        for tc in &track.clips {
            let end = tc.position + tc.duration;
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
        Sense::click_and_drag(),
    );

    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frac = ((pointer.x - left) / width).clamp(0.0, 1.0);
            let max_scroll = (total_width - width).max(0.0);
            state.ui.timeline_scroll_offset = frac * max_scroll;
        }
    }
}

fn handle_zoom_scroll(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline_rect: Rect,
    content_left: f32,
) {
    let hover_pos = ui.input(|i| i.pointer.hover_pos());
    let in_timeline = hover_pos.is_some_and(|p| timeline_rect.contains(p));
    if !in_timeline {
        return;
    }

    let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
    let cmd_held = ui.input(|i| i.modifiers.command);

    if cmd_held {
        let zoom_delta = scroll_delta.y;
        if zoom_delta.abs() > 0.1 {
            let old_zoom = state.ui.timeline_zoom;
            let factor = if zoom_delta > 0.0 { 1.1 } else { 1.0 / 1.1 };
            let new_zoom = (old_zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);

            if let Some(pointer) = hover_pos {
                let pointer_time =
                    (pointer.x - content_left + state.ui.timeline_scroll_offset) / old_zoom;
                state.ui.timeline_zoom = new_zoom;
                state.ui.timeline_scroll_offset =
                    (pointer_time * new_zoom - (pointer.x - content_left)).max(0.0);
            } else {
                state.ui.timeline_zoom = new_zoom;
            }
        }
    } else if scroll_delta.x.abs() > 0.1 || scroll_delta.y.abs() > 0.1 {
        state.ui.timeline_scroll_offset =
            (state.ui.timeline_scroll_offset - scroll_delta.x - scroll_delta.y).max(0.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_drag_ghosts(
    ui: &mut egui::Ui,
    state: &AppState,
    tracks_top: f32,
    content_left: f32,
    content_width: f32,
    pps: f32,
    scroll: f32,
    textures: &dyn TextureLookup,
) {
    let pointer = match ui.ctx().pointer_hover_pos() {
        Some(p) => p,
        None => return,
    };

    let num_tracks = state.project.tracks.len();
    if num_tracks == 0 {
        return;
    }

    let target_track_idx = ((pointer.y - tracks_top) / (TRACK_HEIGHT + 2.0))
        .floor()
        .max(0.0) as usize;
    if target_track_idx >= num_tracks {
        return;
    }

    let drop_time = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;

    let has_timeline_drag = state.ui.timeline_dragging_clip.is_some();

    if !has_timeline_drag {
        if let Some(payload) = DragAndDrop::payload::<ClipId>(ui.ctx()) {
            let clip_id = *payload;
            let duration = state
                .project
                .clips
                .get(&clip_id)
                .and_then(|c| c.duration)
                .unwrap_or(3.0)
                .max(0.1);
            let track_kind = state.project.tracks[target_track_idx].kind;
            let clip_color = match track_kind {
                TrackKind::Video => theme::CLIP_VIDEO,
                TrackKind::Audio => theme::CLIP_AUDIO,
            };
            let track_y = tracks_top + target_track_idx as f32 * (TRACK_HEIGHT + 2.0);
            draw_clip_ghost(
                ui,
                &clip_id,
                duration,
                drop_time,
                track_y,
                content_left,
                content_width,
                pps,
                scroll,
                clip_color,
                track_kind,
                tracks_top,
                num_tracks,
                state,
                textures,
            );
        }
    }

    if let Some((src_track_id, src_clip_idx)) = state.ui.timeline_dragging_clip {
        let is_dragging = ui.input(|i| i.pointer.any_down());
        if is_dragging {
            let src_track_index = state
                .project
                .tracks
                .iter()
                .position(|t| t.id == src_track_id);
            if let Some(src_idx) = src_track_index {
                if let Some(tc) = state.project.tracks[src_idx].clips.get(src_clip_idx) {
                    let clip_id = tc.clip_id;
                    let duration = tc.duration;
                    let track_kind = state.project.tracks[target_track_idx].kind;
                    let clip_color = match track_kind {
                        TrackKind::Video => theme::CLIP_VIDEO,
                        TrackKind::Audio => theme::CLIP_AUDIO,
                    };
                    let track_y = tracks_top + target_track_idx as f32 * (TRACK_HEIGHT + 2.0);
                    draw_clip_ghost(
                        ui,
                        &clip_id,
                        duration,
                        drop_time,
                        track_y,
                        content_left,
                        content_width,
                        pps,
                        scroll,
                        clip_color,
                        track_kind,
                        tracks_top,
                        num_tracks,
                        state,
                        textures,
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_clip_ghost(
    ui: &mut egui::Ui,
    clip_id: &ClipId,
    duration: f64,
    drop_time: f64,
    track_y: f32,
    content_left: f32,
    _content_width: f32,
    pps: f32,
    scroll: f32,
    clip_color: Color32,
    track_kind: TrackKind,
    tracks_top: f32,
    num_tracks: usize,
    state: &AppState,
    textures: &dyn TextureLookup,
) {
    let ghost_x = content_left + drop_time as f32 * pps - scroll;
    let ghost_w = duration as f32 * pps;
    let ghost_rect = Rect::from_min_size(
        pos2(ghost_x, track_y + 2.0),
        vec2(ghost_w, TRACK_HEIGHT - 4.0),
    );

    let ghost_color =
        Color32::from_rgba_unmultiplied(clip_color.r(), clip_color.g(), clip_color.b(), 100);
    ui.painter()
        .rect_filled(ghost_rect, theme::ROUNDING_SM, ghost_color);

    ui.painter().rect_stroke(
        ghost_rect,
        theme::ROUNDING_SM,
        Stroke::new(1.0, Color32::from_white_alpha(80)),
        egui::StrokeKind::Outside,
    );

    if track_kind == TrackKind::Video {
        let thumb_w = THUMB_WIDTH.min(ghost_w);
        let thumb_rect = Rect::from_min_size(
            pos2(ghost_x, track_y + 2.0),
            vec2(thumb_w, TRACK_HEIGHT - 4.0),
        );
        if let Some(tex) = textures.thumbnail(clip_id) {
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            let tint = Color32::from_rgba_unmultiplied(255, 255, 255, 100);
            ui.painter().image(tex.id(), thumb_rect, uv, tint);
        }
    }

    if let Some(clip) = state.project.clips.get(clip_id) {
        let name = clip.display_name();
        let label = if name.len() > 15 {
            format!("{}...", &name[..12])
        } else {
            name.to_string()
        };
        ui.painter().text(
            pos2(ghost_rect.min.x + 4.0, ghost_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(10.0),
            Color32::from_white_alpha(180),
        );
    }

    let line_top = tracks_top;
    let line_bottom = tracks_top + num_tracks as f32 * (TRACK_HEIGHT + 2.0);
    ui.painter().line_segment(
        [pos2(ghost_x, line_top), pos2(ghost_x, line_bottom)],
        Stroke::new(1.0, Color32::from_white_alpha(120)),
    );

    let total_secs = drop_time;
    let minutes = (total_secs / 60.0).floor() as i32;
    let secs = total_secs % 60.0;
    let time_label = format!("{minutes}:{secs:04.1}");
    ui.painter().text(
        pos2(ghost_x + 2.0, track_y - 2.0),
        egui::Align2::LEFT_BOTTOM,
        time_label,
        egui::FontId::monospace(9.0),
        Color32::from_white_alpha(200),
    );
}

fn draw_ruler(ui: &mut egui::Ui, left: f32, top: f32, width: f32, pps: f32, scroll: f32) {
    let ruler_rect = Rect::from_min_size(pos2(left, top), vec2(width, RULER_HEIGHT));
    ui.painter()
        .rect_filled(ruler_rect, CornerRadius::ZERO, theme::RULER_BG);

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

        ui.painter().line_segment(
            [
                pos2(x, top + RULER_HEIGHT - 8.0),
                pos2(x, top + RULER_HEIGHT),
            ],
            Stroke::new(1.0, theme::TEXT_DIM),
        );

        let minutes = s / 60;
        let secs = s % 60;
        let label = format!("{minutes}:{secs:02}");
        ui.painter().text(
            pos2(x + 2.0, top + 2.0),
            egui::Align2::LEFT_TOP,
            label,
            egui::FontId::monospace(9.0),
            theme::TEXT_DIM,
        );

        for sub in 1..4 {
            let sub_x = x + sub as f32 * pps / 4.0;
            if sub_x >= left && sub_x < left + width {
                ui.painter().line_segment(
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
