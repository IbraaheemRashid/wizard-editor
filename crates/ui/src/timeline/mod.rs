mod interaction;
mod layout;
mod rendering;

use egui::{pos2, vec2, Color32, CornerRadius, CursorIcon, Rect, Sense, Stroke};
use wizard_state::clip::ClipId;
use wizard_state::project::{AppState, TrimEdge, TrimState};
use wizard_state::timeline::TrackKind;

use crate::theme;
use crate::waveform_gpu::waveform_paint_callback;
use crate::TextureLookup;

use layout::*;
use rendering::*;

pub fn timeline_panel(ui: &mut egui::Ui, state: &mut AppState, textures: &dyn TextureLookup) {
    state.ui.timeline.scrubbing = None;
    ui.set_min_width(0.0);
    ui.set_min_height(0.0);

    let available = ui.available_rect_before_wrap();
    let timeline_rect = Rect::from_min_size(available.min, available.size());
    ui.allocate_rect(timeline_rect, Sense::hover());

    let content_left = timeline_rect.min.x + TRACK_HEADER_WIDTH;
    let content_width = timeline_rect.width() - TRACK_HEADER_WIDTH;

    let track_layouts = build_track_layout(state);
    let total_track_height = track_layouts.len() as f32 * (TRACK_HEIGHT + 2.0);
    let available_track_height = timeline_rect.height() - RULER_HEIGHT - SCROLLBAR_HEIGHT - 8.0;
    let needs_vertical_scroll = total_track_height > available_track_height;

    interaction::handle_zoom_scroll(
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

    let clip_area_top = ruler_top + RULER_HEIGHT;
    let clip_area_bottom = timeline_rect.max.y - SCROLLBAR_HEIGHT - 8.0;

    let tracks_top = clip_area_top - v_scroll;

    let mut pending_browser_drop: Option<(Vec<ClipId>, wizard_state::timeline::TrackId, f64)> =
        None;

    let content_clip_rect = Rect::from_min_max(
        pos2(content_left, clip_area_top),
        pos2(content_left + content_width, clip_area_bottom),
    );
    let content_painter = ui.painter().with_clip_rect(content_clip_rect);

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

        let track_rect =
            Rect::from_min_size(pos2(content_left, y), vec2(content_width, TRACK_HEIGHT));

        let track_response = ui.interact(
            track_rect,
            egui::Id::new(("track_drop", track_id)),
            Sense::hover(),
        );

        if let Some(payload) = track_response.dnd_release_payload::<Vec<ClipId>>() {
            if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                let drop_t = ((pointer.x - content_left + scroll) / pps).max(0.0) as f64;
                let (t, _) = snap_time_to_clip_boundaries(state, drop_t, pps, None);
                pending_browser_drop = Some((payload.as_ref().clone(), track_id, t));
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

        if track_response.dnd_hover_payload::<Vec<ClipId>>().is_some() {
            content_painter.rect_stroke(
                track_rect,
                CornerRadius::ZERO,
                Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Outside,
            );
        }

        let base_clip_color = match layout.kind {
            TrackKind::Video => theme::CLIP_VIDEO,
            TrackKind::Audio => theme::CLIP_AUDIO,
        };
        let clip_color = if layout.muted || !layout.visible {
            base_clip_color.gamma_multiply(0.3)
        } else {
            base_clip_color
        };

        let clips: Vec<_> = state
            .project
            .timeline
            .track_by_id(track_id)
            .map(|t| t.clips.clone())
            .unwrap_or_default();

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
                state.ui.selection.select_single(tc_source_id);
            }

            let is_starred = state.project.starred.contains(&tc_source_id);
            let has_linked = tc.linked_to.is_some();
            clip_response.context_menu(|ui| {
                if has_linked {
                    if ui.button("Delete").clicked() {
                        state.project.snapshot_for_undo();
                        state.project.timeline.remove_clip_single(tc_id);
                        if state.ui.selection.selected_timeline_clip == Some(tc_id) {
                            state.ui.selection.selected_timeline_clip = None;
                        }
                        ui.close_menu();
                    }
                    if ui.button("Delete Both").clicked() {
                        state.project.snapshot_for_undo();
                        state.project.timeline.remove_clip(tc_id);
                        if state.ui.selection.selected_timeline_clip == Some(tc_id) {
                            state.ui.selection.selected_timeline_clip = None;
                        }
                        ui.close_menu();
                    }
                } else if ui.button("Delete").clicked() {
                    state.project.snapshot_for_undo();
                    state.project.timeline.remove_clip_single(tc_id);
                    if state.ui.selection.selected_timeline_clip == Some(tc_id) {
                        state.ui.selection.selected_timeline_clip = None;
                    }
                    ui.close_menu();
                }
                let star_label = if is_starred { "Unstar" } else { "Star" };
                if ui.button(star_label).clicked() {
                    state.project.toggle_star(tc_source_id);
                    ui.close_menu();
                }
            });
        }
    }

    if let Some((clip_ids, track_id, position_seconds)) = pending_browser_drop {
        state.project.snapshot_for_undo();
        let mut cursor = position_seconds;
        for clip_id in clip_ids {
            state.project.add_clip_to_track(clip_id, track_id, cursor);
            let dur = state
                .project
                .clips
                .get(&clip_id)
                .and_then(|c| c.duration)
                .unwrap_or(3.0)
                .max(0.1);
            cursor += dur;
        }
    }

    let total_tracks = state.project.timeline.track_count();

    draw_drag_ghosts(
        ui,
        state,
        tracks_top,
        content_left,
        pps,
        scroll,
        textures,
        content_clip_rect,
    );

    interaction::handle_clip_trim(ui, state, content_left, pps, scroll, tracks_top);
    interaction::handle_clip_drag_drop(ui, state, tracks_top, content_left, pps, scroll);

    let header_clip_rect = Rect::from_min_max(
        pos2(timeline_rect.min.x, clip_area_top),
        pos2(content_left, clip_area_bottom),
    );
    let header_column_bg = Rect::from_min_max(
        pos2(timeline_rect.min.x, clip_area_top),
        pos2(content_left, clip_area_bottom),
    );
    ui.painter()
        .rect_filled(header_column_bg, CornerRadius::ZERO, theme::BG_PANEL);

    let header_painter = ui.painter().with_clip_rect(header_clip_rect);
    let track_layouts_for_headers = build_track_layout(state);
    for layout in &track_layouts_for_headers {
        let track_id = layout.track_id;
        let y = tracks_top + layout.display_index as f32 * (TRACK_HEIGHT + 2.0);

        let header_rect = Rect::from_min_size(
            pos2(timeline_rect.min.x, y),
            vec2(TRACK_HEADER_WIDTH, TRACK_HEIGHT),
        );
        let header_bg = if layout.muted || !layout.visible {
            theme::TRACK_HEADER_BG.gamma_multiply(0.6)
        } else {
            theme::TRACK_HEADER_BG
        };
        header_painter.rect_filled(header_rect, CornerRadius::ZERO, header_bg);
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
        let is_muted = layout.muted;
        let is_visible = layout.visible;
        header_response.context_menu(|ui| {
            let mute_label = if is_muted { "Unmute" } else { "Mute" };
            if ui.button(mute_label).clicked() {
                state.project.snapshot_for_undo();
                if let Some(track) = state.project.timeline.track_by_id_mut(track_id) {
                    track.muted = !track.muted;
                }
                ui.close_menu();
            }
            let vis_label = if is_visible { "Hide" } else { "Show" };
            if ui.button(vis_label).clicked() {
                state.project.snapshot_for_undo();
                if let Some(track) = state.project.timeline.track_by_id_mut(track_id) {
                    track.visible = !track.visible;
                }
                ui.close_menu();
            }
            ui.separator();
            let can_move_up = pair_index + 1 < pair_count;
            if ui
                .add_enabled(can_move_up, egui::Button::new("Move Up"))
                .clicked()
            {
                state.project.snapshot_for_undo();
                state
                    .project
                    .timeline
                    .move_track_pair(pair_index, pair_index + 1);
                ui.close_menu();
            }
            let can_move_down = pair_index > 0;
            if ui
                .add_enabled(can_move_down, egui::Button::new("Move Down"))
                .clicked()
            {
                state.project.snapshot_for_undo();
                state
                    .project
                    .timeline
                    .move_track_pair(pair_index, pair_index - 1);
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Add Track Pair").clicked() {
                state.project.snapshot_for_undo();
                state.project.timeline.add_track_pair();
                ui.close_menu();
            }
            let can_delete = pair_count > 1;
            if ui
                .add_enabled(can_delete, egui::Button::new("Delete Track Pair"))
                .clicked()
            {
                state.project.snapshot_for_undo();
                state.project.timeline.remove_track_pair(pair_index);
                ui.close_menu();
            }
        });
    }

    draw_ruler(ui, content_left, ruler_top, content_width, pps, scroll);

    let corner_rect = Rect::from_min_size(
        pos2(timeline_rect.min.x, ruler_top),
        vec2(TRACK_HEADER_WIDTH, RULER_HEIGHT),
    );
    ui.painter()
        .rect_filled(corner_rect, CornerRadius::ZERO, theme::RULER_BG);

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

            let previous_playhead = state.project.playback.playhead;
            state.project.playback.playhead = t;
            if scrub_response.dragged() || scrub_response.clicked() {
                state.ui.timeline.scrubbing = Some(t);
            }
            if scrub_response.clicked() {
                eprintln!(
                    "[SEEK-DEBUG] ruler click prev={previous_playhead:.3} next={t:.3} state={:?}",
                    state.project.playback.state
                );
            }
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

    let playhead_body_clip = Rect::from_min_max(
        pos2(content_left, clip_area_top),
        pos2(content_left + content_width, clip_area_bottom),
    );
    let playhead_body_painter = ui.painter().with_clip_rect(playhead_body_clip);
    playhead_body_painter.line_segment(
        [
            pos2(playhead_x, clip_area_top),
            pos2(playhead_x, clip_area_bottom),
        ],
        Stroke::new(1.5, theme::PLAYHEAD_COLOR),
    );

    let playhead_head_painter = ui.painter();
    let playhead_head = Rect::from_center_size(pos2(playhead_x, ruler_top + 4.0), vec2(10.0, 8.0));
    playhead_head_painter.rect_filled(playhead_head, CornerRadius::same(2), theme::PLAYHEAD_COLOR);

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
