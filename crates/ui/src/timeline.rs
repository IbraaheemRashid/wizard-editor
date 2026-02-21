use egui::{pos2, vec2, Color32, CornerRadius, Rect, Sense, Stroke};
use wizard_state::clip::ClipId;
use wizard_state::project::AppState;
use wizard_state::timeline::TrackKind;

use crate::theme;

const TRACK_HEIGHT: f32 = 60.0;
const TRACK_HEADER_WIDTH: f32 = 60.0;
const RULER_HEIGHT: f32 = 24.0;
const PIXELS_PER_SECOND: f32 = 100.0;

pub fn timeline_panel(ui: &mut egui::Ui, state: &mut AppState) {
    state.ui.timeline_scrubbing = None;
    ui.set_min_width(0.0);
    ui.set_min_height(0.0);

    ui.heading("Timeline");
    ui.separator();

    let available = ui.available_rect_before_wrap();
    let content_height = RULER_HEIGHT + state.project.tracks.len() as f32 * (TRACK_HEIGHT + 2.0);
    let timeline_rect = Rect::from_min_size(available.min, vec2(available.width(), content_height));

    let content_left = timeline_rect.min.x + TRACK_HEADER_WIDTH;
    let content_width = timeline_rect.width() - TRACK_HEADER_WIDTH;

    draw_ruler(ui, content_left, timeline_rect.min.y, content_width);

    let tracks_top = timeline_rect.min.y + RULER_HEIGHT;
    let mut pending_drop: Option<(ClipId, usize, f64)> = None;
    for (i, track) in state.project.tracks.iter().enumerate() {
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
                let t = ((pointer.x - content_left) / PIXELS_PER_SECOND).max(0.0) as f64;
                pending_drop = Some((*payload, i, t));
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

        for tc in &track.clips {
            let clip_x = content_left + tc.position as f32 * PIXELS_PER_SECOND;
            let clip_w = tc.duration as f32 * PIXELS_PER_SECOND;
            let clip_rect =
                Rect::from_min_size(pos2(clip_x, y + 2.0), vec2(clip_w, TRACK_HEIGHT - 4.0));
            ui.painter()
                .rect_filled(clip_rect, theme::ROUNDING_SM, clip_color);

            if let Some(clip) = state.project.clips.get(&tc.clip_id) {
                let label = if clip.filename.len() > 15 {
                    format!("{}...", &clip.filename[..12])
                } else {
                    clip.filename.clone()
                };
                ui.painter().text(
                    clip_rect.left_center() + vec2(4.0, 0.0),
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
        }
    }

    if let Some((clip_id, track_index, position_seconds)) = pending_drop {
        state
            .project
            .add_clip_to_track(clip_id, track_index, position_seconds);
    }

    let playhead_x = content_left + state.project.playback.playhead as f32 * PIXELS_PER_SECOND;
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

    let scrub_rect = Rect::from_min_size(
        pos2(content_left, playhead_top),
        vec2(content_width, RULER_HEIGHT),
    );
    let scrub_response = ui.allocate_rect(scrub_rect, Sense::click_and_drag());
    if scrub_response.dragged() || scrub_response.clicked() {
        if let Some(pointer) = scrub_response.interact_pointer_pos() {
            let t = ((pointer.x - content_left) / PIXELS_PER_SECOND).max(0.0) as f64;
            state.project.playback.playhead = t;
            state.ui.timeline_scrubbing = Some(t);
        }
    }
}

fn draw_ruler(ui: &mut egui::Ui, left: f32, top: f32, width: f32) {
    let ruler_rect = Rect::from_min_size(pos2(left, top), vec2(width, RULER_HEIGHT));
    ui.painter()
        .rect_filled(ruler_rect, CornerRadius::ZERO, theme::RULER_BG);

    let total_seconds = (width / PIXELS_PER_SECOND) as i32 + 1;
    for s in 0..total_seconds {
        let x = left + s as f32 * PIXELS_PER_SECOND;
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
            let sub_x = x + sub as f32 * PIXELS_PER_SECOND / 4.0;
            if sub_x < left + width {
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
