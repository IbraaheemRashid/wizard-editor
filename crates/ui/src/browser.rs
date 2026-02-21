use std::path::PathBuf;
use std::time::Duration;

use egui::{vec2, Color32, Id, Rect, Sense, Stroke, StrokeKind, Vec2};
use wizard_state::clip::ClipId;
use wizard_state::project::AppState;
use wizard_state::tag::Tag;

use crate::constants;
use crate::theme;

pub enum BrowserAction {
    None,
    Collapse,
    ImportFolder(PathBuf),
}

pub trait TextureLookup {
    fn thumbnail(&self, id: &ClipId) -> Option<&egui::TextureHandle>;
    fn preview_frames(&self, id: &ClipId) -> Option<&Vec<egui::TextureHandle>>;
    fn is_pending(&self, id: &ClipId) -> bool;
}

pub fn browser_panel(
    ui: &mut egui::Ui,
    state: &mut AppState,
    textures: &dyn TextureLookup,
) -> BrowserAction {
    state.ui.selection.hovered_clip = None;
    state.ui.hovered_scrub_t = None;
    state.ui.visible_clips.clear();

    let mut action = BrowserAction::None;
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Media Browser");
                if ui
                    .button("\u{25C0}")
                    .on_hover_text("Collapse browser")
                    .clicked()
                {
                    action = BrowserAction::Collapse;
                }
            });
            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Import Folder").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        action = BrowserAction::ImportFolder(path);
                    }
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Search:");
                ui.text_edit_singleline(&mut state.ui.search_query);
                let star_label = if state.ui.starred_only {
                    "\u{2605} Starred"
                } else {
                    "\u{2606} Starred"
                };
                if ui
                    .selectable_label(state.ui.starred_only, star_label)
                    .clicked()
                {
                    state.ui.starred_only = !state.ui.starred_only;
                }
            });

            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Tags:");
                for tag in Tag::ALL {
                    let is_selected = (state.ui.tag_filter_mask & tag.bit()) != 0;
                    if ui.selectable_label(is_selected, tag.label()).clicked() {
                        state.ui.tag_filter_mask ^= tag.bit();
                    }
                }
                if state.ui.tag_filter_mask != 0 && ui.button("Clear").clicked() {
                    state.ui.tag_filter_mask = 0;
                }
            });
            ui.add_space(4.0);

            let filtered = state.filtered_clips();
            let clip_count = filtered.len();
            if clip_count == 0 {
                ui.colored_label(theme::TEXT_DIM, "No clips. Import a folder to begin.");
                return;
            }

            let available_width = ui.available_width().max(1.0);

            let cols = ((available_width + constants::GRID_SPACING)
                / (constants::MIN_TILE_W + constants::GRID_SPACING))
                .floor()
                .max(1.0) as usize;
            let cols = cols.min(filtered.len().max(1));

            let tile_w = (available_width
                - constants::GRID_SPACING * (cols.saturating_sub(1) as f32))
                / cols as f32;
            let thumb_h = tile_w * (constants::THUMB_SIZE.y / constants::THUMB_SIZE.x);
            let thumb_size = vec2(tile_w, thumb_h);

            let mut is_any_tile_hovered = false;
            egui::Grid::new("clip_grid")
                .spacing(vec2(constants::GRID_SPACING, constants::GRID_SPACING))
                .show(ui, |ui| {
                    for (i, clip_id) in filtered.iter().enumerate() {
                        is_any_tile_hovered |=
                            clip_thumbnail(ui, *clip_id, thumb_size, state, textures);
                        if (i + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });

            if !is_any_tile_hovered {
                state.ui.hover_active_clip = None;
                state.ui.hover_started_at = None;
            }
        });
    action
}

fn clip_thumbnail(
    ui: &mut egui::Ui,
    clip_id: ClipId,
    thumb_size: Vec2,
    state: &mut AppState,
    textures: &dyn TextureLookup,
) -> bool {
    let clip = match state.project.clips.get(&clip_id) {
        Some(c) => c,
        None => return false,
    };
    let filename = clip.filename.clone();
    let duration = clip.duration;
    let resolution = clip.resolution;
    let is_starred = state.project.starred.contains(&clip_id);
    let is_selected = state.ui.selection.selected_clip == Some(clip_id);

    let meta_height = 16.0;
    let (rect, response) = ui.allocate_exact_size(
        thumb_size + vec2(0.0, 20.0 + meta_height),
        Sense::click_and_drag(),
    );

    if !is_selected {
        response.dnd_set_drag_payload(clip_id);
    }

    let is_hovered = response.hovered();
    let now = ui.input(|i| i.time);
    if is_hovered && state.ui.hover_active_clip != Some(clip_id) {
        state.ui.hover_active_clip = Some(clip_id);
        state.ui.hover_started_at = Some(now);
    }

    let hover_ready = !is_selected
        && is_hovered
        && state.ui.hover_active_clip == Some(clip_id)
        && state
            .ui
            .hover_started_at
            .is_some_and(|started_at| now - started_at >= constants::HOVER_SCRUB_DELAY_SECS);

    if !is_selected && is_hovered && state.ui.hover_active_clip == Some(clip_id) {
        if let Some(started_at) = state.ui.hover_started_at {
            let remaining = constants::HOVER_SCRUB_DELAY_SECS - (now - started_at);
            if remaining > 0.0 {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(remaining));
            }
        }
    }

    response.context_menu(|ui| {
        let star_text = if is_starred { "Unstar" } else { "Star" };
        if ui.button(star_text).clicked() {
            state.project.toggle_star(clip_id);
            ui.close_menu();
        }

        ui.separator();
        ui.label("Tags");
        for tag in Tag::ALL {
            let has_tag = (state.project.clip_tag_mask(clip_id) & tag.bit()) != 0;
            if ui.selectable_label(has_tag, tag.label()).clicked() {
                state.project.toggle_tag(clip_id, tag);
            }
        }
    });

    if ui.is_rect_visible(rect) {
        state.ui.visible_clips.push(clip_id);
        let thumb_rect = Rect::from_min_size(rect.min, thumb_size);
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

        let preview_frames = textures.preview_frames(&clip_id);
        let hover_t = if hover_ready {
            let pointer_x = ui.input(|i| {
                i.pointer
                    .hover_pos()
                    .map(|p| p.x)
                    .unwrap_or(thumb_rect.center().x)
            });
            Some(((pointer_x - thumb_rect.left()) / thumb_rect.width()).clamp(0.0, 1.0))
        } else {
            None
        };

        let was_selected = is_selected;
        if response.clicked() && !was_selected {
            state.ui.selection.selected_clip = Some(clip_id);

            let click_t = response
                .interact_pointer_pos()
                .map(|p| ((p.x - thumb_rect.left()) / thumb_rect.width()).clamp(0.0, 1.0));
            state.ui.selection.selected_scrub_t = hover_t.or(click_t);
        }

        let is_selected = state.ui.selection.selected_clip == Some(clip_id);
        let hover_ready = hover_ready && !is_selected;

        if hover_ready {
            state.ui.selection.hovered_clip = Some(clip_id);
            state.ui.hovered_scrub_t = hover_t;
        }

        let selected_t = if is_selected {
            state.ui.selection.selected_scrub_t
        } else {
            None
        };

        let scrub_info = if let Some(frames) = preview_frames {
            if frames.is_empty() {
                None
            } else if is_selected {
                selected_t.map(|t| {
                    let t = t.clamp(0.0, 1.0);
                    let idx = ((t * frames.len() as f32) as usize).min(frames.len() - 1);
                    (idx, t)
                })
            } else if hover_ready {
                let t = hover_t.unwrap_or(0.0).clamp(0.0, 1.0);
                let idx = ((t * frames.len() as f32) as usize).min(frames.len() - 1);
                Some((idx, t))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((idx, _)) = scrub_info {
            if let Some(frames) = textures.preview_frames(&clip_id) {
                ui.painter()
                    .image(frames[idx].id(), thumb_rect, uv, Color32::WHITE);
            }
        } else if let Some(tex) = textures.thumbnail(&clip_id) {
            ui.painter().image(tex.id(), thumb_rect, uv, Color32::WHITE);
        } else if textures.is_pending(&clip_id) {
            ui.painter()
                .rect_filled(thumb_rect, theme::ROUNDING, theme::BG_SURFACE);
            let spinner_size = 14.0;
            let spinner_rect =
                Rect::from_center_size(thumb_rect.center(), vec2(spinner_size, spinner_size));
            egui::Spinner::new()
                .size(spinner_size)
                .paint_at(ui, spinner_rect);
        } else {
            ui.painter()
                .rect_filled(thumb_rect, theme::ROUNDING, theme::BG_SURFACE);
            ui.painter().text(
                thumb_rect.center(),
                egui::Align2::CENTER_CENTER,
                "No Preview",
                egui::FontId::proportional(11.0),
                theme::TEXT_DIM,
            );
        }

        if is_selected {
            let bar_height = 3.0;
            let bar_track_rect = Rect::from_min_size(
                egui::pos2(thumb_rect.left(), thumb_rect.bottom() - bar_height),
                vec2(thumb_rect.width(), bar_height),
            );
            ui.painter()
                .rect_filled(bar_track_rect, 0.0, theme::ACCENT.gamma_multiply(0.25));

            if let Some((_, t)) = scrub_info {
                let bar_rect = Rect::from_min_size(
                    egui::pos2(thumb_rect.left(), thumb_rect.bottom() - bar_height),
                    vec2(thumb_rect.width() * t, bar_height),
                );
                ui.painter().rect_filled(bar_rect, 0.0, theme::ACCENT);
            }

            let bar_hit_h = 14.0;
            let bar_hit_rect = Rect::from_min_size(
                egui::pos2(thumb_rect.left(), thumb_rect.bottom() - bar_hit_h),
                vec2(thumb_rect.width(), bar_hit_h),
            );
            let bar_resp = ui
                .interact(
                    bar_hit_rect,
                    Id::new(("selected_scrub_bar", clip_id)),
                    Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);

            if bar_resp.clicked() || bar_resp.dragged() {
                if let Some(pointer) = bar_resp.interact_pointer_pos() {
                    let t = ((pointer.x - thumb_rect.left()) / thumb_rect.width()).clamp(0.0, 1.0);
                    state.ui.selection.selected_scrub_t = Some(t);
                    ui.ctx().request_repaint();
                }
            }
        }

        if is_selected {
            ui.painter().rect_stroke(
                thumb_rect,
                theme::ROUNDING,
                Stroke::new(2.0, theme::ACCENT),
                StrokeKind::Outside,
            );
        }

        if is_starred {
            ui.painter().text(
                thumb_rect.right_top() + vec2(-14.0, 4.0),
                egui::Align2::CENTER_TOP,
                "\u{2605}",
                egui::FontId::proportional(14.0),
                theme::STAR_COLOR,
            );
        }

        let label_pos = egui::pos2(rect.min.x, thumb_rect.max.y + 2.0);
        let truncated = if filename.len() > 20 {
            format!("{}...", &filename[..17])
        } else {
            filename
        };
        ui.painter().text(
            label_pos,
            egui::Align2::LEFT_TOP,
            truncated,
            egui::FontId::proportional(11.0),
            theme::TEXT_PRIMARY,
        );

        let meta_y = thumb_rect.max.y + 15.0;
        let mut meta_parts: Vec<String> = Vec::new();
        if let Some(dur) = duration {
            let m = (dur as i32) / 60;
            let s = (dur as i32) % 60;
            meta_parts.push(format!("{m}:{s:02}"));
        }
        if let Some((w, h)) = resolution {
            meta_parts.push(format!("{w}x{h}"));
        }
        if !meta_parts.is_empty() {
            ui.painter().text(
                egui::pos2(rect.min.x, meta_y),
                egui::Align2::LEFT_TOP,
                meta_parts.join("  "),
                egui::FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
        }

        if hover_ready && preview_frames.is_some_and(|f| !f.is_empty()) {
            ui.ctx().request_repaint();
        }
    }

    is_hovered
}
