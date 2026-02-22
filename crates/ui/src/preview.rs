use egui::vec2;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

use crate::browser::TextureLookup;
use crate::theme;

pub fn preview_panel(ui: &mut egui::Ui, state: &AppState, textures: &dyn TextureLookup) {
    let available = ui.available_size();

    let is_active = state.project.playback.state != PlaybackState::Stopped
        || state.ui.timeline_scrubbing.is_some();

    if is_active {
        if let Some(tex) = textures.playback_frame() {
            show_frame_texture(ui, tex, available);
        } else if let Some(clip_id) = state.ui.selection.selected_clip {
            if let Some(tex) = textures.thumbnail(&clip_id) {
                show_frame_texture(ui, tex, available);
            }
        }

        ui.vertical_centered(|ui| {
            let playhead = state.project.playback.playhead;
            let total_secs = playhead;
            let minutes = (total_secs / 60.0).floor() as i32;
            let secs = total_secs % 60.0;
            let frames = ((secs.fract()) * 24.0).floor() as i32;
            let status = match state.project.playback.state {
                PlaybackState::Stopped => "Scrub",
                PlaybackState::Playing => "Play",
                PlaybackState::PlayingReverse => "Rev",
            };
            ui.colored_label(
                theme::TEXT_DIM,
                format!(
                    "{minutes}:{:02}.{frames:02}  |  {status}",
                    secs.floor() as i32
                ),
            );
        });
        return;
    }

    match state.ui.selection.selected_clip {
        Some(clip_id) => {
            if let Some(clip) = state.project.clips.get(&clip_id) {
                ui.vertical_centered(|ui| {
                    if let Some(tex) = textures.thumbnail(&clip_id) {
                        show_frame_texture(ui, tex, available);
                    } else {
                        let preview_rect = egui::Rect::from_center_size(
                            ui.available_rect_before_wrap().center(),
                            vec2(320.0, 180.0),
                        );
                        ui.painter()
                            .rect_filled(preview_rect, theme::ROUNDING, theme::BG_SURFACE);
                        ui.painter().text(
                            preview_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Preview",
                            egui::FontId::proportional(16.0),
                            theme::TEXT_DIM,
                        );
                        ui.allocate_rect(preview_rect, egui::Sense::hover());
                    }

                    ui.add_space(8.0);
                    ui.colored_label(theme::TEXT_PRIMARY, &clip.filename);

                    if let Some(dur) = clip.duration {
                        let m = (dur as i32) / 60;
                        let s = (dur as i32) % 60;
                        ui.colored_label(theme::TEXT_DIM, format!("Duration: {m}:{s:02}"));
                    }
                    if let Some((w, h)) = clip.resolution {
                        ui.colored_label(theme::TEXT_DIM, format!("{w}x{h}"));
                    }
                });
            }
        }
        None => {
            ui.vertical_centered(|ui| {
                ui.add_space(available.y / 2.0 - 20.0);
                ui.colored_label(theme::TEXT_DIM, "Import media to begin");
                ui.add_space(8.0);

                let status = match state.project.playback.state {
                    PlaybackState::Stopped => "Stopped",
                    PlaybackState::Playing => "Playing",
                    PlaybackState::PlayingReverse => "Reverse",
                };
                ui.colored_label(
                    theme::TEXT_DIM,
                    format!(
                        "Playhead: {:.2}s  |  {}",
                        state.project.playback.playhead, status
                    ),
                );
            });
        }
    }
}

fn show_frame_texture(ui: &mut egui::Ui, tex: &egui::TextureHandle, available: egui::Vec2) {
    let tex_size = tex.size_vec2();
    let scale = (available.x / tex_size.x)
        .min((available.y - 30.0) / tex_size.y)
        .min(1.0);
    let display_size = tex_size * scale;
    ui.vertical_centered(|ui| {
        ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
    });
}
