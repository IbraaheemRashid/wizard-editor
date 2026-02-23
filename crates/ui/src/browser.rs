use std::path::PathBuf;
use std::time::Duration;

use egui::{vec2, Color32, Id, Rect, Sense, Stroke, StrokeKind, Vec2};
use wizard_state::clip::ClipId;
use wizard_state::project::{AppState, SortMode};
use wizard_state::tag::Tag;

use crate::constants;
use crate::theme;
use crate::TextureLookup;

pub enum BrowserAction {
    None,
    ImportFolder(PathBuf),
}

pub fn browser_panel(
    ui: &mut egui::Ui,
    state: &mut AppState,
    textures: &dyn TextureLookup,
) -> BrowserAction {
    state.ui.selection.hovered_clip = None;
    state.ui.browser.hovered_scrub_t = None;
    state.ui.browser.visible_clips.clear();

    let mut action = BrowserAction::None;

    ui.horizontal(|ui| {
        ui.heading("Media Browser");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Import Folder").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    action = BrowserAction::ImportFolder(path);
                }
            }
        });
    });
    ui.separator();

    ui.horizontal(|ui| {
        let search_width = ui.available_width() - 80.0;
        ui.add(
            egui::TextEdit::singleline(&mut state.ui.browser.search_query)
                .desired_width(search_width),
        );
        let star_label = if state.ui.browser.starred_only {
            "\u{2605} Starred"
        } else {
            "\u{2606} Starred"
        };
        if ui
            .selectable_label(state.ui.browser.starred_only, star_label)
            .clicked()
        {
            state.ui.browser.starred_only = !state.ui.browser.starred_only;
        }
    });
    ui.separator();

    ui.horizontal_wrapped(|ui| {
        for tag in Tag::ALL {
            let is_selected = (state.ui.browser.tag_filter_mask & tag.bit()) != 0;
            if ui.selectable_label(is_selected, tag.label()).clicked() {
                state.ui.browser.tag_filter_mask ^= tag.bit();
            }
        }
        if state.ui.browser.tag_filter_mask != 0 && ui.button("Clear").clicked() {
            state.ui.browser.tag_filter_mask = 0;
        }
    });
    ui.horizontal(|ui| {
        let current_label = state.ui.browser.sort_mode.label();
        egui::ComboBox::from_id_salt("sort_mode")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                for &mode in SortMode::ALL {
                    ui.selectable_value(&mut state.ui.browser.sort_mode, mode, mode.label());
                }
            });
        let label = if state.ui.browser.sort_ascending {
            "A-Z"
        } else {
            "Z-A"
        };
        if ui
            .button(label)
            .on_hover_text("Toggle sort direction")
            .clicked()
        {
            state.ui.browser.sort_ascending = !state.ui.browser.sort_ascending;
        }
    });
    ui.separator();

    let filtered = state.filtered_clips();
    let clip_count = filtered.len();

    if clip_count == 0 {
        ui.colored_label(theme::TEXT_DIM, "No clips. Import a folder to begin.");
        return action;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
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
            egui::Grid::new(egui::Id::new("clip_grid").with(cols))
                .spacing(vec2(constants::GRID_SPACING, constants::GRID_SPACING))
                .show(ui, |ui| {
                    for (i, clip_id) in filtered.iter().enumerate() {
                        is_any_tile_hovered |=
                            clip_thumbnail(ui, *clip_id, thumb_size, state, textures, &filtered);
                        if (i + 1) % cols == 0 {
                            ui.end_row();
                        }
                    }
                });

            if !is_any_tile_hovered {
                state.ui.browser.hover_active_clip = None;
                state.ui.browser.hover_started_at = None;
            }
        });

    if let Some(pointer) = ui.ctx().pointer_interact_pos() {
        if let Some(payload) = egui::DragAndDrop::payload::<Vec<ClipId>>(ui.ctx()) {
            let dragged_ids = payload.as_ref();
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                Id::new("drag_preview"),
            ));
            let preview_size = vec2(80.0, 60.0);
            let preview_rect = Rect::from_min_size(pointer + vec2(8.0, 8.0), preview_size);
            let tint = Color32::WHITE.gamma_multiply(0.7);
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

            let first_id = dragged_ids.first();
            if let Some(first) = first_id {
                if let Some(tex) = textures.thumbnail(first) {
                    painter.image(tex.id(), preview_rect, uv, tint);
                } else {
                    painter.rect_filled(
                        preview_rect,
                        theme::ROUNDING,
                        theme::BG_SURFACE.gamma_multiply(0.7),
                    );
                }
            }

            if dragged_ids.len() > 1 {
                let badge_pos = egui::pos2(preview_rect.max.x - 4.0, preview_rect.min.y - 4.0);
                let badge_size = vec2(20.0, 16.0);
                let badge_rect = Rect::from_min_size(badge_pos, badge_size);
                painter.rect_filled(badge_rect, 8.0, theme::ACCENT);
                painter.text(
                    badge_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", dragged_ids.len()),
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
            } else if let Some(first) = first_id {
                if let Some(clip) = state.project.clips.get(first) {
                    let name = clip.display_name();
                    let label = if name.len() > 14 {
                        format!("{}...", &name[..11])
                    } else {
                        name.to_string()
                    };
                    painter.text(
                        egui::pos2(preview_rect.center().x, preview_rect.max.y + 2.0),
                        egui::Align2::CENTER_TOP,
                        label,
                        egui::FontId::proportional(10.0),
                        theme::TEXT_PRIMARY,
                    );
                }
            }
        }
    }

    action
}

fn clip_thumbnail(
    ui: &mut egui::Ui,
    clip_id: ClipId,
    thumb_size: Vec2,
    state: &mut AppState,
    textures: &dyn TextureLookup,
    filtered: &[ClipId],
) -> bool {
    let clip = match state.project.clips.get(&clip_id) {
        Some(c) => c,
        None => return false,
    };
    let display_name_str = clip.display_name().to_string();
    let duration = clip.duration;
    let resolution = clip.resolution;
    let is_audio_only = clip.audio_only;
    let is_starred = state.project.starred.contains(&clip_id);
    let is_selected = state.ui.selection.is_clip_selected(clip_id);
    let is_primary = state.ui.selection.primary_clip() == Some(clip_id);

    let meta_height = 16.0;
    let (rect, response) = ui.allocate_exact_size(
        thumb_size + vec2(0.0, 20.0 + meta_height),
        Sense::click_and_drag(),
    );

    if is_selected {
        let payload: Vec<ClipId> = filtered
            .iter()
            .copied()
            .filter(|id| state.ui.selection.is_clip_selected(*id))
            .collect();
        response.dnd_set_drag_payload(payload);
    } else {
        response.dnd_set_drag_payload(vec![clip_id]);
    }

    let is_hovered = response.hovered();
    let now = ui.input(|i| i.time);
    if is_hovered && state.ui.browser.hover_active_clip != Some(clip_id) {
        state.ui.browser.hover_active_clip = Some(clip_id);
        state.ui.browser.hover_started_at = Some(now);
    }

    let hover_ready = !is_primary
        && is_hovered
        && state.ui.browser.hover_active_clip == Some(clip_id)
        && state
            .ui
            .browser
            .hover_started_at
            .is_some_and(|started_at| now - started_at >= constants::HOVER_SCRUB_DELAY_SECS);

    if !is_primary && is_hovered && state.ui.browser.hover_active_clip == Some(clip_id) {
        if let Some(started_at) = state.ui.browser.hover_started_at {
            let remaining = constants::HOVER_SCRUB_DELAY_SECS - (now - started_at);
            if remaining > 0.0 {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(remaining));
            }
        }
    }

    let multi_selected_count = state.ui.selection.selected_clips.len();
    let is_multi = multi_selected_count > 1;
    let right_clicked_in_multi = is_selected && is_multi;

    response.context_menu(|ui| {
        if right_clicked_in_multi {
            let all_starred = state
                .ui
                .selection
                .selected_clips
                .iter()
                .all(|id| state.project.starred.contains(id));
            let star_text = if all_starred {
                format!("Unstar {} clips", multi_selected_count)
            } else {
                format!("Star {} clips", multi_selected_count)
            };
            if ui.button(star_text).clicked() {
                let ids: Vec<ClipId> = state.ui.selection.selected_clips.iter().copied().collect();
                if all_starred {
                    for id in ids {
                        state.project.starred.remove(&id);
                    }
                } else {
                    for id in ids {
                        state.project.starred.insert(id);
                    }
                }
                ui.close_menu();
            }

            ui.separator();
            ui.label("Tags");
            for tag in Tag::ALL {
                let all_have_tag = state
                    .ui
                    .selection
                    .selected_clips
                    .iter()
                    .all(|id| (state.project.clip_tag_mask(*id) & tag.bit()) != 0);
                if ui.selectable_label(all_have_tag, tag.label()).clicked() {
                    let ids: Vec<ClipId> =
                        state.ui.selection.selected_clips.iter().copied().collect();
                    for id in ids {
                        if all_have_tag {
                            let entry = state.project.clip_tags.entry(id).or_insert(0);
                            *entry &= !tag.bit();
                        } else {
                            let entry = state.project.clip_tags.entry(id).or_insert(0);
                            *entry |= tag.bit();
                        }
                    }
                }
            }
        } else {
            let star_text = if is_starred { "Unstar" } else { "Star" };
            if ui.button(star_text).clicked() {
                state.project.toggle_star(clip_id);
                ui.close_menu();
            }

            if ui.button("Rename").clicked() {
                let current_name = state
                    .project
                    .clips
                    .get(&clip_id)
                    .map(|c| c.display_name().to_string())
                    .unwrap_or_default();
                state.ui.browser.renaming_clip = Some(clip_id);
                state.ui.browser.rename_buffer = current_name;
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
        }
    });

    if ui.is_rect_visible(rect) {
        state.ui.browser.visible_clips.push(clip_id);
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

        if response.clicked() {
            let modifiers = ui.input(|i| i.modifiers);
            if modifiers.command {
                state.ui.selection.toggle_clip(clip_id);
            } else if modifiers.shift {
                let anchor = state.ui.selection.last_selected_clip;
                state.ui.selection.select_range(anchor, clip_id, filtered);
            } else {
                state.ui.selection.select_single(clip_id);
            }

            let click_t = response
                .interact_pointer_pos()
                .map(|p| ((p.x - thumb_rect.left()) / thumb_rect.width()).clamp(0.0, 1.0));
            state.ui.selection.selected_scrub_t = hover_t.or(click_t);
        }

        let is_selected = state.ui.selection.is_clip_selected(clip_id);
        let is_primary = state.ui.selection.primary_clip() == Some(clip_id);
        let hover_ready = hover_ready && !is_primary;

        if hover_ready {
            state.ui.selection.hovered_clip = Some(clip_id);
            state.ui.browser.hovered_scrub_t = hover_t;
        }

        let selected_t = if is_primary {
            state.ui.selection.selected_scrub_t
        } else {
            None
        };

        let scrub_info = if let Some(frames) = preview_frames {
            if frames.is_empty() {
                None
            } else if is_primary {
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
                let safe_idx = idx.min(frames.len().saturating_sub(1));
                if !frames.is_empty() {
                    ui.painter()
                        .image(frames[safe_idx].id(), thumb_rect, uv, Color32::WHITE);
                }
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
        } else if is_audio_only {
            ui.painter()
                .rect_filled(thumb_rect, theme::ROUNDING, theme::BG_SURFACE);
            ui.painter().text(
                thumb_rect.center() - vec2(0.0, 8.0),
                egui::Align2::CENTER_CENTER,
                "\u{266B}",
                egui::FontId::proportional(24.0),
                theme::ACCENT,
            );
            ui.painter().text(
                thumb_rect.center() + vec2(0.0, 14.0),
                egui::Align2::CENTER_CENTER,
                "Audio",
                egui::FontId::proportional(10.0),
                theme::TEXT_DIM,
            );
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

        if is_primary {
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
        } else {
            ui.painter().rect_stroke(
                thumb_rect,
                theme::ROUNDING,
                Stroke::new(1.0, theme::BORDER),
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

        let label_rect = Rect::from_min_size(
            egui::pos2(rect.min.x, thumb_rect.max.y + 2.0),
            vec2(thumb_size.x, 16.0),
        );
        let is_renaming = state.ui.browser.renaming_clip == Some(clip_id);
        if is_renaming {
            let text_edit = egui::TextEdit::singleline(&mut state.ui.browser.rename_buffer)
                .desired_width(thumb_size.x)
                .font(egui::FontId::proportional(11.0));
            let re = ui.put(label_rect, text_edit);
            if re.lost_focus() {
                let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if !escaped {
                    let new_name = state.ui.browser.rename_buffer.trim().to_string();
                    if !new_name.is_empty() {
                        let tag_mask = state.project.clip_tag_mask(clip_id);
                        if let Some(clip) = state.project.clips.get_mut(&clip_id) {
                            clip.display_name = Some(new_name);
                            clip.rebuild_search_haystack(tag_mask);
                        }
                    }
                }
                state.ui.browser.renaming_clip = None;
                state.ui.browser.rename_buffer.clear();
            } else {
                re.request_focus();
            }
        } else {
            let truncated = if display_name_str.len() > 20 {
                format!("{}...", &display_name_str[..17])
            } else {
                display_name_str.clone()
            };
            ui.painter().text(
                label_rect.min,
                egui::Align2::LEFT_TOP,
                truncated,
                egui::FontId::proportional(11.0),
                theme::TEXT_PRIMARY,
            );
        }

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

        let has_some_frames = preview_frames.is_some_and(|f| !f.is_empty());
        if hover_ready && has_some_frames {
            ui.ctx().request_repaint();
        }

        if hover_ready && !has_some_frames && textures.is_preview_loading(&clip_id) {
            let overlay = Color32::from_black_alpha(120);
            ui.painter()
                .rect_filled(thumb_rect, theme::ROUNDING, overlay);
            let spinner_size = 16.0;
            let spinner_rect =
                Rect::from_center_size(thumb_rect.center(), vec2(spinner_size, spinner_size));
            egui::Spinner::new()
                .size(spinner_size)
                .paint_at(ui, spinner_rect);
            ui.ctx().request_repaint();
        }
    }

    is_hovered
}
