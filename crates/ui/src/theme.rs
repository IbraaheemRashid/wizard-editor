use egui::{Color32, CornerRadius, Stroke, Style, Visuals};

pub const BG_DARK: Color32 = Color32::from_rgb(26, 26, 30);
pub const BG_PANEL: Color32 = Color32::from_rgb(33, 33, 36);
pub const BG_SURFACE: Color32 = Color32::from_rgb(42, 42, 46);
pub const ACCENT: Color32 = Color32::from_rgb(0, 149, 218);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(230, 230, 230);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 140, 145);
pub const TRACK_BG: Color32 = Color32::from_rgb(28, 28, 32);
pub const CLIP_VIDEO: Color32 = Color32::from_rgb(82, 120, 190);
pub const CLIP_AUDIO: Color32 = Color32::from_rgb(80, 160, 100);
pub const PLAYHEAD_COLOR: Color32 = Color32::from_rgb(235, 64, 52);
pub const STAR_COLOR: Color32 = Color32::from_rgb(255, 200, 50);
pub const TRACK_HEADER_BG: Color32 = Color32::from_rgb(38, 38, 42);
pub const RULER_BG: Color32 = Color32::from_rgb(24, 24, 28);
pub const RULER_TICK: Color32 = Color32::from_rgb(65, 65, 70);
pub const WAVEFORM_COLOR: Color32 = Color32::from_rgba_premultiplied(120, 200, 255, 230);
pub const BG_HOVER: Color32 = Color32::from_rgb(50, 50, 55);
pub const BORDER: Color32 = Color32::from_rgb(55, 55, 60);

pub const ROUNDING: CornerRadius = CornerRadius::same(4);
pub const ROUNDING_SM: CornerRadius = CornerRadius::same(3);

pub fn apply_theme(ctx: &egui::Context) {
    let mut style = Style::default();
    let mut visuals = Visuals::dark();

    visuals.panel_fill = BG_PANEL;
    visuals.window_fill = BG_DARK;
    visuals.extreme_bg_color = BG_DARK;
    visuals.faint_bg_color = BG_SURFACE;

    visuals.widgets.noninteractive.bg_fill = BG_SURFACE;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);

    visuals.widgets.inactive.bg_fill = BG_SURFACE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);

    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER);

    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.4);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(12);

    ctx.set_style(style);
}
