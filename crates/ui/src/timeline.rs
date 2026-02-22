use egui::{pos2, vec2, Color32, CornerRadius, CursorIcon, DragAndDrop, Rect, Sense, Stroke};
use wizard_state::clip::ClipId;
use wizard_state::project::{AppState, TrimEdge, TrimState};
use wizard_state::timeline::{TrackId, TrackKind};

use crate::theme;
use crate::waveform_gpu::waveform_paint_callback;
use crate::TextureLookup;

const TRACK_HEIGHT: f32 = 60.0;
const TRACK_HEADER_WIDTH: f32 = 70.0;
const RULER_HEIGHT: f32 = 24.0;
const SCROLLBAR_HEIGHT: f32 = 12.0;
const V_SCROLLBAR_WIDTH: f32 = 10.0;
const ZOOM_MIN: f32 = 20.0;
const ZOOM_MAX: f32 = 500.0;
const SNAP_THRESHOLD_PX: f32 = 10.0;
const THUMB_WIDTH: f32 = 50.0;
const TRIM_HANDLE_WIDTH: f32 = 12.0;
const MIN_CLIP_DURATION: f64 = 0.1;

struct TrackLayout {
    track_id: TrackId,
    kind: TrackKind,
    name: String,
    display_index: usize,
    pair_index: usize,
}

fn build_track_layout(state: &AppState) -> Vec<TrackLayout> {
    let mut layouts = Vec::new();
    let mut idx = 0;
    for (pair_i, track) in state.project.timeline.video_tracks.iter().enumerate().rev() {
        layouts.push(TrackLayout {
            track_id: track.id,
            kind: track.kind,
            name: track.name.clone(),
            display_index: idx,
            pair_index: pair_i,
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
        });
        idx += 1;
    }
    layouts
}

fn snap_time_to_clip_boundaries(
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

pub fn timeline_panel(ui: &mut egui::Ui, state: &mut AppState, textures: &dyn TextureLookup) {
    state.ui.timeline.scrubbing = None;
    ui.set_min_width(0.0);
    ui.set_min_height(0.0);

    ui.heading("Timeline");
    ui.separator();

    let available = ui.available_rect_before_wrap();
    let timeline_rect = Rect::from_min_size(available.min, available.size());
    ui.allocate_rect(timeline_rect, Sense::hover());

    let content_left = timeline_rect.min.x + TRACK_HEADER_WIDTH;
    let content_width = timeline_rect.width() - TRACK_HEADER_WIDTH;

    let track_layouts = build_track_layout(state);
    let total_track_height = track_layouts.len() as f32 * (TRACK_HEIGHT + 2.0);
    let available_track_height = timeline_rect.height() - RULER_HEIGHT - SCROLLBAR_HEIGHT - 8.0;
    let needs_vertical_scroll = total_track_height > available_track_height;

    handle_zoom_scroll(
        ui,
        state,
        timeline_rect,
        content_left,
        needs_vertical_scroll,
        total_track_height,
        available_track_height,
    );
    let pps = state.ui.timeline.zoom;
    let scroll = state.ui.timeline.scroll_offset;
    let v_scroll = state.ui.timeline.vertical_scroll_offset;
    let ruler_top = timeline_rect.min.y;

    draw_ruler(ui, content_left, ruler_top, content_width, pps, scroll);

    let clip_area_top = ruler_top + RULER_HEIGHT;
    let clip_area_bottom = timeline_rect.max.y - SCROLLBAR_HEIGHT - 8.0;
    let available_track_area = clip_area_bottom - clip_area_top;

    let centered_offset = if needs_vertical_scroll {
        0.0
    } else {
        (available_track_area - total_track_height).max(0.0) * 0.5
    };

    let tracks_top = clip_area_top + centered_offset - v_scroll;

    let mut pending_browser_drop: Option<(ClipId, TrackId, f64)> = None;

    let content_clip_rect = Rect::from_min_max(
        pos2(content_left, clip_area_top),
        pos2(content_left + content_width, clip_area_bottom),
    );
    let content_painter = ui.painter().with_clip_rect(content_clip_rect);

    let header_clip_rect = Rect::from_min_max(
        pos2(timeline_rect.min.x, clip_area_top),
        pos2(content_left, clip_area_bottom),
    );
    let header_painter = ui.painter().with_clip_rect(header_clip_rect);

    let full_track_clip_rect = Rect::from_min_max(
        pos2(timeline_rect.min.x, clip_area_top),
        pos2(timeline_rect.max.x, clip_area_bottom),
    );
    let track_row_painter = ui.painter().with_clip_rect(full_track_clip_rect);

    let screen_size = ui.ctx().screen_rect().size();
    let gpu_waveforms_available = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(egui::Id::new("gpu_waveforms")))
        .unwrap_or(false);

    for layout in &track_layouts {
        let track_id = layout.track_id;
        let y = tracks_top + layout.display_index as f32 * (TRACK_HEIGHT + 2.0);

        let header_rect = Rect::from_min_size(
            pos2(timeline_rect.min.x, y),
            vec2(TRACK_HEADER_WIDTH, TRACK_HEIGHT),
        );
        header_painter.rect_filled(header_rect, CornerRadius::ZERO, theme::TRACK_HEADER_BG);
        header_painter.line_segment(
            [
                pos2(header_rect.max.x, header_rect.min.y),
                pos2(header_rect.max.x, header_rect.max.y),
            ],
            Stroke::new(1.0, theme::BORDER),
        );
        header_painter.text(
            header_rect.center(),
            egui::Align2::CENTER_CENTER,
            &layout.name,
            egui::FontId::proportional(12.0),
            theme::TEXT_PRIMARY,
        );

        let header_response = ui.interact(
            header_rect,
            egui::Id::new(("track_header", track_id)),
            Sense::click(),
        );

        let pair_index = layout.pair_index;
        let pair_count = state.project.timeline.pair_count();
        header_response.context_menu(|ui| {
            if ui.button("Add Track Pair").clicked() {
                state.project.timeline.add_track_pair();
                ui.close_menu();
            }
            let can_delete = pair_count > 1;
            if ui
                .add_enabled(can_delete, egui::Button::new("Delete Track Pair"))
                .clicked()
            {
                state.project.timeline.remove_track_pair(pair_index);
                ui.close_menu();
            }
        });

        let track_rect =
            Rect::from_min_size(pos2(content_left, y), vec2(content_width, TRACK_HEIGHT));

        let track_response = ui.interact(
            track_rect,
            egui::Id::new(("track_drop", track_id)),
            Sense::hover(),
        );

        if let Some(payload) = track_response.dnd_release_payload::<ClipId>() {
            if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                let drop_t = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;
                let (t, _) = snap_time_to_clip_boundaries(state, drop_t, pps, None);
                pending_browser_drop = Some((*payload, track_id, t));
            }
        }

        content_painter.rect_filled(track_rect, CornerRadius::ZERO, theme::TRACK_BG);
        track_row_painter.line_segment(
            [
                pos2(timeline_rect.min.x, y + TRACK_HEIGHT),
                pos2(timeline_rect.max.x, y + TRACK_HEIGHT),
            ],
            Stroke::new(1.0, theme::BORDER),
        );

        if track_response.dnd_hover_payload::<ClipId>().is_some() {
            content_painter.rect_stroke(
                track_rect,
                CornerRadius::ZERO,
                Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Outside,
            );
        }

        let clip_color = match layout.kind {
            TrackKind::Video => theme::CLIP_VIDEO,
            TrackKind::Audio => theme::CLIP_AUDIO,
        };

        let track = state.project.timeline.track_by_id(track_id);
        let clips: Vec<_> = track.map(|t| t.clips.iter().collect()).unwrap_or_default();

        for tc in &clips {
            let clip_x = content_left + tc.timeline_start as f32 * pps - scroll;
            let clip_w = tc.duration as f32 * pps;
            let clip_rect =
                Rect::from_min_size(pos2(clip_x, y + 2.0), vec2(clip_w, TRACK_HEIGHT - 4.0));

            if clip_rect.max.x < content_left || clip_rect.min.x > content_left + content_width {
                continue;
            }

            let tc_id = tc.id;
            let tc_source_id = tc.source_id;

            let is_selected = state
                .ui
                .selection
                .selected_timeline_clip
                .is_some_and(|id| id == tc_id);

            let is_being_dragged = state
                .ui
                .timeline
                .dragging_clip
                .is_some_and(|id| id == tc_id);

            let mut drew_gpu_waveform = false;

            if layout.kind == TrackKind::Audio && gpu_waveforms_available {
                if let Some(peaks) = textures.waveform_peaks(&tc_source_id) {
                    let visible_peaks = visible_peak_slice(
                        peaks,
                        tc.source_in,
                        tc.source_out,
                        state
                            .project
                            .clips
                            .get(&tc_source_id)
                            .and_then(|c| c.duration),
                    );
                    let wave_color = theme::WAVEFORM_COLOR;
                    content_painter.add(waveform_paint_callback(
                        clip_rect,
                        visible_peaks,
                        wave_color,
                        clip_color,
                        [screen_size.x, screen_size.y],
                    ));
                    drew_gpu_waveform = true;
                }
            }

            if !drew_gpu_waveform {
                content_painter.rect_filled(clip_rect, theme::ROUNDING_SM, clip_color);
            }

            if layout.kind == TrackKind::Video && clip_w >= 20.0 {
                if let Some(tex) = textures.thumbnail(&tc_source_id) {
                    let thumb_w = THUMB_WIDTH.min(clip_w);
                    let thumb_rect = Rect::from_min_size(
                        pos2(clip_x, y + 2.0),
                        vec2(thumb_w, TRACK_HEIGHT - 4.0),
                    );
                    let uv = center_crop_uv(tex, TRACK_HEIGHT - 4.0, thumb_w);
                    content_painter.image(tex.id(), thumb_rect, uv, Color32::WHITE);
                }
            } else if layout.kind == TrackKind::Audio && !drew_gpu_waveform {
                if let Some(peaks) = textures.waveform_peaks(&tc_source_id) {
                    let visible_peaks = visible_peak_slice(
                        peaks,
                        tc.source_in,
                        tc.source_out,
                        state
                            .project
                            .clips
                            .get(&tc_source_id)
                            .and_then(|c| c.duration),
                    );
                    draw_waveform(&content_painter, clip_rect, visible_peaks);
                }
            }

            content_painter.rect_stroke(
                clip_rect,
                theme::ROUNDING_SM,
                Stroke::new(1.0, clip_color.gamma_multiply(0.6)),
                egui::StrokeKind::Inside,
            );

            if clip_w >= 30.0 {
                if let Some(clip) = state.project.clips.get(&tc_source_id) {
                    let name = clip.display_name();
                    let label = if name.len() > 15 {
                        format!("{}...", &name[..12])
                    } else {
                        name.to_string()
                    };
                    let label_rect = Rect::from_min_size(
                        pos2(clip_rect.min.x, clip_rect.max.y - 14.0),
                        vec2(clip_w.min(clip_rect.width()), 14.0),
                    );
                    content_painter.rect_filled(
                        label_rect,
                        CornerRadius::ZERO,
                        Color32::from_black_alpha(160),
                    );
                    content_painter.text(
                        pos2(clip_rect.min.x + 4.0, clip_rect.max.y - 7.0),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(10.0),
                        Color32::WHITE,
                    );
                }
            }

            if clip_w > 25.0 && state.project.starred.contains(&tc_source_id) {
                let star_pos = clip_rect.left_top() + vec2(4.0, 2.0);
                let pill_rect = Rect::from_min_size(star_pos, vec2(16.0, 14.0));
                content_painter.rect_filled(
                    pill_rect,
                    CornerRadius::same(3),
                    Color32::from_black_alpha(140),
                );
                content_painter.text(
                    star_pos + vec2(8.0, 7.0),
                    egui::Align2::CENTER_CENTER,
                    "\u{2605}",
                    egui::FontId::proportional(12.0),
                    theme::STAR_COLOR,
                );
            }

            if is_selected {
                content_painter.rect_stroke(
                    clip_rect,
                    theme::ROUNDING_SM,
                    Stroke::new(2.0, theme::ACCENT),
                    egui::StrokeKind::Outside,
                );
            }

            if is_being_dragged {
                content_painter.rect_filled(
                    clip_rect,
                    theme::ROUNDING_SM,
                    Color32::from_white_alpha(30),
                );
            }

            let hover_pos = ui.ctx().pointer_hover_pos();
            let effective_trim_w = TRIM_HANDLE_WIDTH.min(clip_w / 3.0);
            let hover_on_left = hover_pos.is_some_and(|p| {
                p.y >= clip_rect.min.y
                    && p.y <= clip_rect.max.y
                    && (p.x - clip_rect.min.x).abs() < effective_trim_w
            });
            let hover_on_right = hover_pos.is_some_and(|p| {
                p.y >= clip_rect.min.y
                    && p.y <= clip_rect.max.y
                    && (p.x - clip_rect.max.x).abs() < effective_trim_w
            });

            let is_being_trimmed = state
                .ui
                .timeline
                .trimming_clip
                .as_ref()
                .is_some_and(|t| t.clip_id == tc_id);

            if hover_on_left || is_being_trimmed {
                let handle_rect = Rect::from_min_size(
                    pos2(clip_rect.min.x, clip_rect.min.y),
                    vec2(3.0, clip_rect.height()),
                );
                content_painter.rect_filled(
                    handle_rect,
                    CornerRadius::ZERO,
                    Color32::from_white_alpha(180),
                );
            }
            if hover_on_right || is_being_trimmed {
                let handle_rect = Rect::from_min_size(
                    pos2(clip_rect.max.x - 3.0, clip_rect.min.y),
                    vec2(3.0, clip_rect.height()),
                );
                content_painter.rect_filled(
                    handle_rect,
                    CornerRadius::ZERO,
                    Color32::from_white_alpha(180),
                );
            }

            if (hover_on_left || hover_on_right)
                && state.ui.timeline.trimming_clip.is_none()
                && state.ui.timeline.dragging_clip.is_none()
            {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }

            let clip_response = ui.interact(
                clip_rect,
                egui::Id::new(("timeline_clip", tc_id)),
                Sense::click_and_drag(),
            );

            if clip_response.drag_started() {
                let origin = clip_response.interact_pointer_pos().unwrap_or_default();
                let origin_on_left = origin.y >= clip_rect.min.y
                    && origin.y <= clip_rect.max.y
                    && (origin.x - clip_rect.min.x).abs() < effective_trim_w;
                let origin_on_right = origin.y >= clip_rect.min.y
                    && origin.y <= clip_rect.max.y
                    && (origin.x - clip_rect.max.x).abs() < effective_trim_w;

                if origin_on_left || origin_on_right {
                    let edge = if origin_on_left {
                        TrimEdge::Left
                    } else {
                        TrimEdge::Right
                    };
                    state.ui.timeline.trimming_clip = Some(TrimState {
                        clip_id: tc_id,
                        edge,
                        original_position: tc.timeline_start,
                        original_duration: tc.duration,
                        original_in_point: tc.source_in,
                        original_out_point: tc.source_out,
                    });
                } else {
                    let grab_time = ((origin.x - content_left + scroll) / pps).max(0.0) as f64;
                    let grab_offset = grab_time - tc.timeline_start;
                    state.ui.timeline.drag_grab_offset = Some(grab_offset);
                    state.ui.timeline.dragging_clip = Some(tc_id);
                }
            }

            if clip_response.clicked() && state.ui.timeline.trimming_clip.is_none() {
                state.ui.selection.selected_timeline_clip = Some(tc_id);
                state.ui.selection.selected_clip = Some(tc_source_id);
            }
        }
    }

    if let Some((clip_id, track_id, position_seconds)) = pending_browser_drop {
        state
            .project
            .add_clip_to_track(clip_id, track_id, position_seconds);
    }

    let total_tracks = state.project.timeline.track_count();

    draw_drag_ghosts(ui, state, tracks_top, content_left, pps, scroll, textures);

    handle_clip_trim(ui, state, content_left, pps, scroll, tracks_top);
    handle_clip_drag_drop(ui, state, tracks_top, content_left, pps, scroll);

    let playhead_time = state.project.playback.playhead;
    let mut playhead_x = content_left + playhead_time as f32 * pps - scroll;

    let scrub_rect = Rect::from_min_size(
        pos2(content_left, ruler_top),
        vec2(content_width, RULER_HEIGHT),
    );
    let scrub_response = ui.interact(
        scrub_rect,
        egui::Id::new("timeline_scrub"),
        Sense::click_and_drag(),
    );
    if scrub_response.dragged() || scrub_response.clicked() {
        if let Some(pointer) = scrub_response.interact_pointer_pos() {
            let raw_t = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;
            let (t, snapped) = snap_time_to_clip_boundaries(state, raw_t, pps, None);

            state.project.playback.playhead = t;
            state.ui.timeline.scrubbing = Some(t);
            playhead_x = content_left + t as f32 * pps - scroll;

            if snapped {
                let snap_line_top = tracks_top;
                let snap_line_bottom = tracks_top + total_tracks as f32 * (TRACK_HEIGHT + 2.0);
                let snap_clip = Rect::from_min_max(
                    pos2(content_left, snap_line_top),
                    pos2(content_left + content_width, snap_line_bottom),
                );
                ui.painter().with_clip_rect(snap_clip).line_segment(
                    [
                        pos2(playhead_x, snap_line_top),
                        pos2(playhead_x, snap_line_bottom),
                    ],
                    Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.5)),
                );
            }
        }
    }

    let playhead_top = ruler_top;
    let playhead_bottom = clip_area_bottom;
    let playhead_clip = Rect::from_min_max(
        pos2(content_left, playhead_top),
        pos2(content_left + content_width, playhead_bottom),
    );
    let playhead_painter = ui.painter().with_clip_rect(playhead_clip);
    playhead_painter.line_segment(
        [
            pos2(playhead_x, playhead_top),
            pos2(playhead_x, playhead_bottom),
        ],
        Stroke::new(1.5, theme::PLAYHEAD_COLOR),
    );

    let playhead_head =
        Rect::from_center_size(pos2(playhead_x, playhead_top + 4.0), vec2(10.0, 8.0));
    playhead_painter.rect_filled(playhead_head, CornerRadius::same(2), theme::PLAYHEAD_COLOR);

    if needs_vertical_scroll {
        draw_vertical_scrollbar(
            ui,
            state,
            timeline_rect.max.x - V_SCROLLBAR_WIDTH,
            clip_area_top,
            clip_area_bottom - clip_area_top,
            total_track_height,
        );
    }

    draw_scrollbar(
        ui,
        state,
        content_left,
        content_width,
        clip_area_bottom + 4.0,
    );
}

fn center_crop_uv(tex: &egui::TextureHandle, display_h: f32, display_w: f32) -> Rect {
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

fn visible_peak_slice(
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

fn handle_clip_trim(
    ui: &egui::Ui,
    state: &mut AppState,
    content_left: f32,
    pps: f32,
    scroll: f32,
    tracks_top: f32,
) {
    let Some(ref trim) = state.ui.timeline.trimming_clip else {
        return;
    };

    let is_dragging = ui.input(|i| i.pointer.any_down());
    if !is_dragging {
        let trim_clip_id = trim.clip_id;
        state.ui.timeline.trimming_clip = None;

        state.project.timeline.finalize_trim(trim_clip_id);
        return;
    }

    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);

    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
        return;
    };

    let pointer_time = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;
    let trim_clip_id = trim.clip_id;
    let edge = trim.edge;
    let original_position = trim.original_position;
    let original_duration = trim.original_duration;
    let original_in_point = trim.original_in_point;
    let original_out_point = trim.original_out_point;
    let source_clip_duration = state
        .project
        .timeline
        .find_clip(trim_clip_id)
        .and_then(|(_, _, tc)| state.project.clips.get(&tc.source_id))
        .and_then(|clip| clip.duration);
    let max_source_out = source_clip_duration
        .unwrap_or(original_out_point.max(original_in_point + original_duration));

    if let Some((track, clip_idx)) = state.project.timeline.find_clip_track_mut(trim_clip_id) {
        let tc = &mut track.clips[clip_idx];
        match edge {
            TrimEdge::Right => {
                let new_out = pointer_time - original_position + original_in_point;
                let clamped_out = new_out
                    .max(original_in_point + MIN_CLIP_DURATION)
                    .min(max_source_out);
                tc.source_out = clamped_out;
                tc.duration = clamped_out - tc.source_in;
            }
            TrimEdge::Left => {
                let delta = pointer_time - original_position;
                let max_delta = original_duration - MIN_CLIP_DURATION;
                let min_delta = -original_in_point;
                let clamped_delta = delta.clamp(min_delta, max_delta);
                tc.timeline_start = original_position + clamped_delta;
                tc.source_in = original_in_point + clamped_delta;
                tc.duration = original_duration - clamped_delta;
            }
        }

        let trim_x = match edge {
            TrimEdge::Left => content_left + tc.timeline_start as f32 * pps - scroll,
            TrimEdge::Right => {
                content_left + (tc.timeline_start + tc.duration) as f32 * pps - scroll
            }
        };
        let total_tracks = state.project.timeline.track_count();
        let line_top = tracks_top;
        let line_bottom = tracks_top + total_tracks as f32 * (TRACK_HEIGHT + 2.0);
        ui.painter().line_segment(
            [pos2(trim_x, line_top), pos2(trim_x, line_bottom)],
            Stroke::new(2.0, theme::ACCENT),
        );
    }
    state.project.timeline.sync_linked_clip(trim_clip_id, false);
}

fn handle_clip_drag_drop(
    ui: &egui::Ui,
    state: &mut AppState,
    tracks_top: f32,
    content_left: f32,
    pps: f32,
    scroll: f32,
) {
    let Some(src_clip_id) = state.ui.timeline.dragging_clip else {
        return;
    };

    let is_dragging = ui.input(|i| i.pointer.any_down());
    if is_dragging {
        return;
    }

    let grab_offset = state.ui.timeline.drag_grab_offset.unwrap_or(0.0);
    state.ui.timeline.dragging_clip = None;
    state.ui.timeline.drag_grab_offset = None;

    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else {
        return;
    };

    let Some((src_track, _, _)) = state.project.timeline.find_clip(src_clip_id) else {
        return;
    };
    let src_track_id = src_track.id;

    let track_layouts = build_track_layout(state);
    let total_tracks = track_layouts.len();
    let dst_display_idx = ((pointer.y - tracks_top) / (TRACK_HEIGHT + 2.0))
        .floor()
        .max(0.0) as usize;
    if dst_display_idx >= total_tracks {
        return;
    }
    let dst_track_id = track_layouts[dst_display_idx].track_id;
    let dst_kind = track_layouts[dst_display_idx].kind;

    let src_kind = state.project.timeline.track_kind_for_clip(src_clip_id);
    if let Some(sk) = src_kind {
        if sk != dst_kind {
            return;
        }
    }

    let new_pos =
        (((pointer.x - content_left + scroll) / pps).max(0.0) as f64 - grab_offset).max(0.0);
    let (new_pos, _) = snap_time_to_clip_boundaries(state, new_pos, pps, Some(src_clip_id));

    if src_track_id == dst_track_id {
        state
            .project
            .timeline
            .move_clip_on_track(src_track_id, src_clip_id, new_pos);
    } else {
        state
            .project
            .timeline
            .move_clip_across_tracks(src_clip_id, dst_track_id, new_pos);
    }
    state.ui.selection.selected_timeline_clip = None;
}

fn draw_waveform(painter: &egui::Painter, rect: Rect, peaks: &[(f32, f32)]) {
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

fn draw_scrollbar(ui: &mut egui::Ui, state: &mut AppState, left: f32, width: f32, top: f32) {
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
        Sense::click_and_drag(),
    );

    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frac = ((pointer.x - left) / width).clamp(0.0, 1.0);
            let max_scroll = (total_width - width).max(0.0);
            state.ui.timeline.scroll_offset = frac * max_scroll;
        }
    }
}

fn draw_vertical_scrollbar(
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
        Sense::click_and_drag(),
    );

    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let frac = ((pointer.y - top) / height).clamp(0.0, 1.0);
            state.ui.timeline.vertical_scroll_offset = frac * max_v_scroll;
        }
    }
}

fn handle_zoom_scroll(
    ui: &mut egui::Ui,
    state: &mut AppState,
    timeline_rect: Rect,
    content_left: f32,
    needs_vertical_scroll: bool,
    total_track_height: f32,
    available_track_height: f32,
) {
    let hover_pos = ui.input(|i| i.pointer.hover_pos());
    let in_timeline = hover_pos.is_some_and(|p| timeline_rect.contains(p));
    if !in_timeline {
        return;
    }

    let zoom_delta = ui.input(|i| i.zoom_delta());

    if zoom_delta != 1.0 {
        let old_zoom = state.ui.timeline.zoom;
        let new_zoom = (old_zoom * zoom_delta).clamp(ZOOM_MIN, ZOOM_MAX);

        if let Some(pointer) = hover_pos {
            let pointer_time =
                (pointer.x - content_left + state.ui.timeline.scroll_offset) / old_zoom;
            state.ui.timeline.zoom = new_zoom;
            state.ui.timeline.scroll_offset =
                (pointer_time * new_zoom - (pointer.x - content_left)).max(0.0);
        } else {
            state.ui.timeline.zoom = new_zoom;
        }
    } else {
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
        if scroll_delta.x.abs() > 0.1 {
            state.ui.timeline.scroll_offset =
                (state.ui.timeline.scroll_offset - scroll_delta.x).max(0.0);
        }

        if scroll_delta.y.abs() > 0.1 {
            if needs_vertical_scroll {
                let max_v_scroll = (total_track_height - available_track_height).max(0.0);
                state.ui.timeline.vertical_scroll_offset =
                    (state.ui.timeline.vertical_scroll_offset - scroll_delta.y)
                        .clamp(0.0, max_v_scroll);
            } else {
                state.ui.timeline.scroll_offset =
                    (state.ui.timeline.scroll_offset - scroll_delta.y).max(0.0);
            }
        }
    }
}

struct ClipGhostParams<'a> {
    clip_id: &'a ClipId,
    duration: f64,
    drop_time: f64,
    track_y: f32,
    content_left: f32,
    pps: f32,
    scroll: f32,
    clip_color: Color32,
    track_kind: TrackKind,
    tracks_top: f32,
    num_tracks: usize,
    state: &'a AppState,
    textures: &'a dyn TextureLookup,
}

fn draw_drag_ghosts(
    ui: &mut egui::Ui,
    state: &AppState,
    tracks_top: f32,
    content_left: f32,
    pps: f32,
    scroll: f32,
    textures: &dyn TextureLookup,
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

    let has_timeline_drag = state.ui.timeline.dragging_clip.is_some();
    let grab_offset = state.ui.timeline.drag_grab_offset.unwrap_or(0.0);
    let unsnapped_drop_time = if has_timeline_drag {
        (raw_drop_time - grab_offset).max(0.0)
    } else {
        raw_drop_time
    };
    let exclude_clip = state.ui.timeline.dragging_clip;
    let (drop_time, _snapped) =
        snap_time_to_clip_boundaries(state, unsnapped_drop_time, pps, exclude_clip);

    let paired_track_id = track_layouts[target_display_idx].track_id;
    let paired_id = state.project.timeline.paired_track_id(paired_track_id);
    let paired_display_idx =
        paired_id.and_then(|pid| track_layouts.iter().position(|l| l.track_id == pid));

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
            let clip_color = match target_kind {
                TrackKind::Video => theme::CLIP_VIDEO,
                TrackKind::Audio => theme::CLIP_AUDIO,
            };
            let track_y = tracks_top + target_display_idx as f32 * (TRACK_HEIGHT + 2.0);
            draw_clip_ghost(
                ui,
                &ClipGhostParams {
                    clip_id: &clip_id,
                    duration,
                    drop_time,
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
                    &ClipGhostParams {
                        clip_id: &clip_id,
                        duration,
                        drop_time,
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
        }
    }

    if let Some(src_clip_id) = state.ui.timeline.dragging_clip {
        let is_dragging = ui.input(|i| i.pointer.any_down());
        if is_dragging {
            if let Some((_, _, tc)) = state.project.timeline.find_clip(src_clip_id) {
                let clip_id = tc.source_id;
                let duration = tc.duration;
                let linked_id = tc.linked_to;
                let src_kind = state.project.timeline.track_kind_for_clip(src_clip_id);
                let kind_mismatch = src_kind.is_some_and(|sk| sk != target_kind);
                let clip_color = if kind_mismatch {
                    Color32::from_rgba_unmultiplied(255, 80, 80, 120)
                } else {
                    match target_kind {
                        TrackKind::Video => theme::CLIP_VIDEO,
                        TrackKind::Audio => theme::CLIP_AUDIO,
                    }
                };
                let track_y = tracks_top + target_display_idx as f32 * (TRACK_HEIGHT + 2.0);
                draw_clip_ghost(
                    ui,
                    &ClipGhostParams {
                        clip_id: &clip_id,
                        duration,
                        drop_time,
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
                if let Some(lid) = linked_id {
                    if let Some((linked_track, _, _)) = state.project.timeline.find_clip(lid) {
                        let lt_id = linked_track.id;
                        let lt_kind = linked_track.kind;
                        if let Some(l_idx) = track_layouts.iter().position(|l| l.track_id == lt_id)
                        {
                            let linked_color = match lt_kind {
                                TrackKind::Video => theme::CLIP_VIDEO,
                                TrackKind::Audio => theme::CLIP_AUDIO,
                            };
                            let linked_y = tracks_top + l_idx as f32 * (TRACK_HEIGHT + 2.0);
                            draw_clip_ghost(
                                ui,
                                &ClipGhostParams {
                                    clip_id: &clip_id,
                                    duration,
                                    drop_time,
                                    track_y: linked_y,
                                    content_left,
                                    pps,
                                    scroll,
                                    clip_color: linked_color,
                                    track_kind: lt_kind,
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
}

fn draw_clip_ghost(ui: &mut egui::Ui, p: &ClipGhostParams<'_>) {
    let ghost_x = p.content_left + p.drop_time as f32 * p.pps - p.scroll;
    let ghost_w = p.duration as f32 * p.pps;
    let ghost_rect = Rect::from_min_size(
        pos2(ghost_x, p.track_y + 2.0),
        vec2(ghost_w, TRACK_HEIGHT - 4.0),
    );

    let ghost_color =
        Color32::from_rgba_unmultiplied(p.clip_color.r(), p.clip_color.g(), p.clip_color.b(), 100);
    ui.painter()
        .rect_filled(ghost_rect, theme::ROUNDING_SM, ghost_color);

    ui.painter().rect_stroke(
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
            ui.painter().image(tex.id(), thumb_rect, uv, tint);
        }
    }

    if let Some(clip) = p.state.project.clips.get(p.clip_id) {
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

    let line_top = p.tracks_top;
    let line_bottom = p.tracks_top + p.num_tracks as f32 * (TRACK_HEIGHT + 2.0);
    ui.painter().line_segment(
        [pos2(ghost_x, line_top), pos2(ghost_x, line_bottom)],
        Stroke::new(1.0, Color32::from_white_alpha(120)),
    );

    let total_secs = p.drop_time;
    let minutes = (total_secs / 60.0).floor() as i32;
    let secs = total_secs % 60.0;
    let time_label = format!("{minutes}:{secs:04.1}");
    ui.painter().text(
        pos2(ghost_x + 2.0, p.track_y - 2.0),
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
