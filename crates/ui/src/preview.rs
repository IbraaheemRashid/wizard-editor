use egui::vec2;
use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

use crate::browser::TextureLookup;
use crate::theme;

pub fn preview_panel(ui: &mut egui::Ui, state: &mut AppState, textures: &dyn TextureLookup) {
    let available = ui.available_size();

    let is_active = state.project.playback.state != PlaybackState::Stopped
        || state.ui.timeline_scrubbing.is_some();

    let has_frame = textures.playback_frame().is_some();

    if has_frame {
        if let Some(tex) = textures.playback_frame() {
            show_frame_texture(ui, tex, available);
        }
    } else if !is_active {
        match state.ui.selection.selected_clip {
            Some(clip_id) => {
                if let Some(clip) = state.project.clips.get(&clip_id) {
                    ui.vertical_centered(|ui| {
                        ui.add_space(available.y / 2.0 - 40.0);
                        ui.colored_label(theme::TEXT_PRIMARY, &clip.filename);

                        if let Some(dur) = clip.duration {
                            let m = (dur as i32) / 60;
                            let s = (dur as i32) % 60;
                            ui.colored_label(theme::TEXT_DIM, format!("Duration: {m}:{s:02}"));
                        }
                        if let Some((w, h)) = clip.resolution {
                            ui.colored_label(theme::TEXT_DIM, format!("{w}x{h}"));
                        }
                        if let Some(codec) = &clip.codec {
                            ui.colored_label(theme::TEXT_DIM, codec.as_str());
                        }
                    });
                }
            }
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(available.y / 2.0 - 20.0);
                    ui.colored_label(theme::TEXT_DIM, "Import media to begin");
                });
            }
        }
    }

    ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
        ui.add_space(4.0);
        transport_bar(ui, state);
    });
}

fn transport_bar(ui: &mut egui::Ui, state: &mut AppState) {
    let playhead = state.project.playback.playhead;
    let total_secs = playhead;
    let minutes = (total_secs / 60.0).floor() as i32;
    let secs = total_secs % 60.0;
    let frames = ((secs.fract()) * 24.0).floor() as i32;
    let timecode = format!("{minutes}:{:02}.{frames:02}", secs.floor() as i32);

    let is_playing = state.project.playback.state == PlaybackState::Playing
        || state.project.playback.state == PlaybackState::PlayingReverse;

    ui.horizontal(|ui| {
        ui.add_space(ui.available_width() / 2.0 - 100.0);

        let btn_size = vec2(28.0, 22.0);

        if ui
            .add_sized(btn_size, egui::Button::new("|\u{25C0}"))
            .on_hover_text("Rewind 5s")
            .clicked()
        {
            state.project.playback.playhead = (state.project.playback.playhead - 5.0).max(0.0);
        }

        let play_label = if is_playing { "| |" } else { "\u{25B6}" };
        if ui
            .add_sized(btn_size, egui::Button::new(play_label))
            .on_hover_text(if is_playing { "Pause" } else { "Play" })
            .clicked()
        {
            state.project.playback.toggle_play();
        }

        if ui
            .add_sized(btn_size, egui::Button::new("\u{25B6}|"))
            .on_hover_text("Forward 5s")
            .clicked()
        {
            state.project.playback.playhead += 5.0;
        }

        if ui
            .add_sized(btn_size, egui::Button::new("\u{25FC}"))
            .on_hover_text("Stop")
            .clicked()
        {
            state.project.playback.stop();
            state.project.playback.playhead = 0.0;
        }

        ui.add_space(8.0);
        ui.colored_label(theme::TEXT_PRIMARY, timecode);
    });
}

fn show_frame_texture(ui: &mut egui::Ui, tex: &egui::TextureHandle, available: egui::Vec2) {
    let tex_size = tex.size_vec2();
    let scale = (available.x / tex_size.x)
        .min((available.y - 60.0) / tex_size.y)
        .min(1.0);
    let display_size = tex_size * scale;
    ui.vertical_centered(|ui| {
        ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
    });
}
