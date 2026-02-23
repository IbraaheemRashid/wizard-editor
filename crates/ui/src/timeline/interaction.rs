use egui::{pos2, CursorIcon, Rect, Stroke};
use wizard_state::project::{AppState, TrimEdge};

use crate::theme;

use super::layout::*;

pub fn handle_clip_trim(
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

        state.project.snapshot_for_undo();
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

pub fn handle_clip_drag_drop(
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

    state.project.snapshot_for_undo();
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

pub fn handle_zoom_scroll(
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
