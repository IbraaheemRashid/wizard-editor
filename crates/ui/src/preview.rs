use wizard_state::playback::PlaybackState;
use wizard_state::project::AppState;

use crate::constants;
use crate::theme;
use crate::TextureLookup;

pub fn preview_panel(ui: &mut egui::Ui, state: &mut AppState, textures: &dyn TextureLookup) {
    let available = ui.available_size();

    let is_active = state.project.playback.state != PlaybackState::Stopped
        || state.ui.timeline.scrubbing.is_some();

    let has_frame = textures.playback_frame().is_some();

    let transport_height = 40.0;
    let video_area_height = available.y - transport_height;

    if has_frame {
        if let Some(tex) = textures.playback_frame() {
            show_frame_texture(ui, tex, egui::vec2(available.x, video_area_height));
        }
    } else if is_active {
        show_black_frame(ui, egui::vec2(available.x, video_area_height));
    } else if !is_active {
        match state.ui.selection.primary_clip() {
            Some(clip_id) => {
                if let Some(clip) = state.project.clips.get(&clip_id) {
                    ui.vertical_centered(|ui| {
                        ui.add_space(video_area_height / 2.0 - 40.0);
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
                    ui.add_space(video_area_height / 2.0 - 20.0);
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
    let minutes = (playhead / 60.0).floor() as i32;
    let secs = playhead % 60.0;
    let frames = ((secs.fract()) * 24.0).floor() as i32;
    let timecode = format!("{minutes}:{:02}.{frames:02}", secs.floor() as i32);

    let is_playing = state.project.playback.state == PlaybackState::Playing;

    ui.horizontal(|ui| {
        ui.add_space(ui.available_width() / 2.0 - 60.0);

        let btn = constants::TRANSPORT_BTN_SIZE;

        if ui
            .add_sized(btn, egui::Button::new("\u{23EE}"))
            .on_hover_text("Go to start")
            .clicked()
        {
            state.project.playback.playhead = 0.0;
        }

        let play_label = if is_playing { "\u{23F8}" } else { "\u{25B6}" };
        if ui
            .add_sized(btn, egui::Button::new(play_label))
            .on_hover_text(if is_playing { "Pause" } else { "Play" })
            .clicked()
        {
            state.project.playback.toggle_play();
        }

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(timecode)
                .font(egui::FontId::monospace(12.0))
                .color(theme::TEXT_PRIMARY),
        );
    });
}

fn show_black_frame(ui: &mut egui::Ui, available: egui::Vec2) {
    let aspect = 16.0 / 9.0;
    let video_h = available.y - 8.0;
    let w = (video_h * aspect).min(available.x);
    let h = w / aspect;
    let display_size = egui::vec2(w, h);
    let vertical_pad = (available.y - display_size.y) / 2.0;

    ui.vertical_centered(|ui| {
        ui.add_space(vertical_pad.max(0.0));
        let rect = egui::Rect::from_min_size(
            egui::pos2(
                ui.available_rect_before_wrap().center().x - display_size.x / 2.0,
                ui.cursor().min.y,
            ),
            display_size,
        );
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::BLACK);
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::ZERO,
            egui::Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Outside,
        );
        ui.allocate_space(display_size);
    });
}

fn show_frame_texture(ui: &mut egui::Ui, tex: &egui::TextureHandle, available: egui::Vec2) {
    let tex_size = tex.size_vec2();
    let video_h = available.y - 8.0;
    let scale = (available.x / tex_size.x)
        .min(video_h / tex_size.y)
        .min(1.0);
    let display_size = tex_size * scale;

    let vertical_pad = (available.y - display_size.y) / 2.0;

    ui.vertical_centered(|ui| {
        ui.add_space(vertical_pad.max(0.0));

        let frame_rect = egui::Rect::from_min_size(
            egui::pos2(
                ui.available_rect_before_wrap().center().x - display_size.x / 2.0,
                ui.cursor().min.y,
            ),
            display_size + egui::vec2(2.0, 2.0),
        );
        ui.painter().rect_stroke(
            frame_rect,
            egui::CornerRadius::ZERO,
            egui::Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Outside,
        );

        ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
    });
}
