use std::sync::mpsc;

use egui::{vec2, Color32, Rect, Sense, Stroke, StrokeKind, Vec2};
use wizard_media::metadata::MediaMetadata;
use wizard_state::clip::ClipId;
use wizard_state::project::AppState;
use wizard_state::tag::Tag;

use crate::theme;

const THUMB_SIZE: Vec2 = vec2(140.0, 80.0);
const GRID_SPACING: f32 = 8.0;
const MIN_TILE_W: f32 = 140.0;
const MAX_TILE_W: f32 = 220.0;

pub fn browser_panel(
    ui: &mut egui::Ui,
    state: &mut AppState,
    thumb_tx: &mpsc::Sender<(ClipId, image::RgbaImage)>,
    meta_tx: &mpsc::Sender<(ClipId, MediaMetadata)>,
    preview_tx: &mpsc::Sender<(ClipId, Vec<image::RgbaImage>)>,
) {
    state.selection.hovered_clip = None;
    state.hovered_scrub_t = None;

    ui.heading("Media Browser");
    ui.separator();

    ui.horizontal(|ui| {
        if ui.button("Import Folder").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                import_folder(path, state, thumb_tx, meta_tx);
            }
        }
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Search:");
        ui.text_edit_singleline(&mut state.search_query);
        let star_label = if state.starred_only {
            "\u{2605} Starred"
        } else {
            "\u{2606} Starred"
        };
        if ui
            .selectable_label(state.starred_only, star_label)
            .clicked()
        {
            state.starred_only = !state.starred_only;
        }
    });

    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.label("Tags:");
        for tag in Tag::ALL {
            let is_selected = (state.tag_filter_mask & tag.bit()) != 0;
            if ui.selectable_label(is_selected, tag.label()).clicked() {
                state.toggle_tag_filter(tag);
            }
        }
        if state.tag_filter_mask != 0 && ui.button("Clear").clicked() {
            state.tag_filter_mask = 0;
        }
    });
    ui.add_space(4.0);

    let filtered = state.filtered_clips();
    let clip_count = filtered.len();

    egui::ScrollArea::vertical().show(ui, |ui| {
        if clip_count == 0 {
            ui.colored_label(theme::TEXT_DIM, "No clips. Import a folder to begin.");
            return;
        }

        let available_width = ui.available_width().max(1.0);

        let mut cols = ((available_width + GRID_SPACING) / (MIN_TILE_W + GRID_SPACING))
            .floor()
            .max(1.0) as usize;
        cols = cols.min(filtered.len().max(1));

        for _ in 0..16 {
            let tile_w =
                (available_width - GRID_SPACING * (cols.saturating_sub(1) as f32)) / cols as f32;

            if tile_w > MAX_TILE_W {
                let next_cols = cols + 1;
                if next_cols > filtered.len().max(1) {
                    break;
                }
                cols = next_cols;
                continue;
            }

            if tile_w < MIN_TILE_W && cols > 1 {
                cols -= 1;
                continue;
            }

            break;
        }

        let tile_w =
            (available_width - GRID_SPACING * (cols.saturating_sub(1) as f32)) / cols as f32;
        let thumb_h = tile_w * (THUMB_SIZE.y / THUMB_SIZE.x);
        let thumb_size = vec2(tile_w, thumb_h);

        let mut is_any_tile_hovered = false;
        egui::Grid::new("clip_grid")
            .spacing(vec2(GRID_SPACING, GRID_SPACING))
            .show(ui, |ui| {
                for (i, clip_id) in filtered.iter().enumerate() {
                    is_any_tile_hovered |=
                        clip_thumbnail(ui, *clip_id, thumb_size, state, preview_tx);
                    if (i + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
            });

        if !is_any_tile_hovered {
            state.hover_active_clip = None;
            state.hover_started_at = None;
        }
    });
}

fn clip_thumbnail(
    ui: &mut egui::Ui,
    clip_id: ClipId,
    thumb_size: Vec2,
    state: &mut AppState,
    preview_tx: &mpsc::Sender<(ClipId, Vec<image::RgbaImage>)>,
) -> bool {
    let clip = match state.clips.get(&clip_id) {
        Some(c) => c,
        None => return false,
    };
    let filename = clip.filename.clone();
    let duration = clip.duration;
    let resolution = clip.resolution;
    let clip_path = clip.path.clone();
    let is_starred = state.starred.contains(&clip_id);
    let is_selected = state.selection.selected_clip == Some(clip_id);

    let meta_height = 16.0;
    let (rect, response) = ui.allocate_exact_size(
        thumb_size + vec2(0.0, 20.0 + meta_height),
        Sense::click_and_drag(),
    );

    response.dnd_set_drag_payload(clip_id);

    if response.clicked() {
        state.selection.selected_clip = Some(clip_id);
    }

    let is_hovered = response.hovered();
    let now = ui.input(|i| i.time);
    if is_hovered {
        if state.hover_active_clip != Some(clip_id) {
            state.hover_active_clip = Some(clip_id);
            state.hover_started_at = Some(now);
        }
    }

    let hover_ready = is_hovered
        && state.hover_active_clip == Some(clip_id)
        && state
            .hover_started_at
            .is_some_and(|started_at| now - started_at >= 0.5);

    if hover_ready && !state.preview_requested.contains(&clip_id) {
        state.preview_requested.insert(clip_id);
        let tx = preview_tx.clone();
        std::thread::spawn(move || {
            let frames = wizard_media::thumbnail::extract_preview_frames(&clip_path, 10);
            let _ = tx.send((clip_id, frames));
        });
    }

    response.context_menu(|ui| {
        let star_text = if is_starred { "Unstar" } else { "Star" };
        if ui.button(star_text).clicked() {
            state.toggle_star(clip_id);
            ui.close_menu();
        }

        ui.separator();
        ui.label("Tags");
        for tag in Tag::ALL {
            let has_tag = (state.clip_tag_mask(clip_id) & tag.bit()) != 0;
            if ui.selectable_label(has_tag, tag.label()).clicked() {
                state.toggle_tag(clip_id, tag);
            }
        }
    });

    if ui.is_rect_visible(rect) {
        let thumb_rect = Rect::from_min_size(rect.min, thumb_size);
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

        let preview_frames = state.preview_frames.get(&clip_id);
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

        if hover_ready {
            state.selection.hovered_clip = Some(clip_id);
            state.hovered_scrub_t = hover_t;
        }

        let scrub_info = if hover_ready {
            if let Some(frames) = preview_frames {
                if !frames.is_empty() {
                    let t = hover_t.unwrap_or(0.0);
                    let idx = ((t * frames.len() as f32) as usize).min(frames.len() - 1);
                    Some((idx, t))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        if let Some((idx, _)) = scrub_info {
            let frames = state.preview_frames.get(&clip_id).unwrap();
            ui.painter()
                .image(frames[idx].id(), thumb_rect, uv, Color32::WHITE);
        } else if let Some(tex) = state.thumbnails.get(&clip_id) {
            ui.painter().image(tex.id(), thumb_rect, uv, Color32::WHITE);
        } else if state.pending_thumbnails.contains(&clip_id) {
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

        if let Some((_, t)) = scrub_info {
            let bar_height = 3.0;
            let bar_rect = Rect::from_min_size(
                egui::pos2(thumb_rect.left(), thumb_rect.bottom() - bar_height),
                vec2(thumb_rect.width() * t, bar_height),
            );
            ui.painter().rect_filled(bar_rect, 0.0, theme::ACCENT);
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

fn import_folder(
    path: std::path::PathBuf,
    state: &mut AppState,
    thumb_tx: &mpsc::Sender<(ClipId, image::RgbaImage)>,
    meta_tx: &mpsc::Sender<(ClipId, MediaMetadata)>,
) {
    let video_extensions = ["mp4", "mov", "avi", "mkv", "webm", "m4v", "mxf", "prores"];
    let entries = match std::fs::read_dir(&path) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if video_extensions.contains(&ext.to_lowercase().as_str()) {
                    let clip = wizard_state::clip::Clip::from_path(p.clone());
                    let clip_id = clip.id;
                    state.add_clip(clip);

                    let ttx = thumb_tx.clone();
                    let mtx = meta_tx.clone();
                    let clip_path = p;
                    std::thread::spawn(move || {
                        let meta = wizard_media::metadata::extract_metadata(&clip_path);
                        let _ = mtx.send((clip_id, meta));

                        if let Some(img) = wizard_media::thumbnail::extract_thumbnail(&clip_path) {
                            let _ = ttx.send((clip_id, img));
                        }
                    });
                }
            }
        }
    }
}
