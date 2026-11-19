//! The Qt v0.9.9 look, as an egui style.
//!
//! The values are read from `sutura/gui.py:2221` (`_dark_palette`), not
//! invented: a Rust UI that picks its own colours is exactly what made the first
//! one look unlike anything shipped before it.
//!
//! Qt's palette, verbatim:
//!   Window        #353535   Base          #2b2d30   Button        #3a3d42
//!   WindowText    #f2f4f6   Text          #f2f4f6   ButtonText    #f2f4f6
//!   BrightText    #ff5b5b   Link/Highlight #14b8a6  Placeholder   #8b949e
//!   HighlightedText #0b0f11 Disabled text  #6b7480
//!
//! Only three semantic colours are allowed beyond the neutral ramp (rule g):
//! error, warning, success. The accent is used for the one primary action.

use crate::app::{COLOR_ACCENT, COLOR_ERROR, COLOR_OK, COLOR_WARN};

/// Qt's `Window`.
const WINDOW: egui::Color32 = egui::Color32::from_rgb(0x35, 0x35, 0x35);
/// Qt's `Base`, which is what an editable field paints.
const BASE: egui::Color32 = egui::Color32::from_rgb(0x2b, 0x2d, 0x30);
/// Qt's `Button`.
const BUTTON: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3d, 0x42);
/// Qt's `WindowText` / `Text` / `ButtonText`.
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xf2, 0xf4, 0xf6);
/// Qt's `PlaceholderText`.
const PLACEHOLDER: egui::Color32 = egui::Color32::from_rgb(0x8b, 0x94, 0x9e);
/// Qt's disabled text.
const DISABLED: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x74, 0x80);
/// A selected row's tint: Qt's accent, dimmed, so selection reads without
/// competing with the one primary action.
const SELECTION: egui::Color32 = egui::Color32::from_rgb(0x1d, 0x4b, 0x46);
/// Qt's `HighlightedText`.
const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x0b, 0x0f, 0x11);

/// The 8 px grid (rule b): every gap is 8, 16 or 24, never a round number of
/// something else.
pub const SPACE_8: f32 = 8.0;
pub const SPACE_16: f32 = 16.0;
pub const SPACE_24: f32 = 24.0;

/// Apply the Qt palette and the typography rules.
///
/// Called once from the app constructor, and again after a language change --
/// the font sizes come from `style.text_styles`, which a language switch does not
/// otherwise touch.
pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    // --- text styles -------------------------------------------------------
    //
    // Rule f: headings one step larger and bold, one body size, numbers in a
    // monospace font. egui ships four text styles, which map exactly:
    // Heading (bold, larger), Body, Button, Small (muted).
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(18.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(12.0, FontFamily::Monospace),
        ),
    ]
    .into();

    // --- spacing -----------------------------------------------------------
    style.spacing.item_spacing = egui::vec2(SPACE_8, SPACE_8);
    style.spacing.button_padding = egui::vec2(SPACE_8, 4.0);
    style.spacing.menu_margin = egui::Margin::same(SPACE_8 as i8);
    style.spacing.indent = SPACE_16;
    style.spacing.window_margin = egui::Margin::same(SPACE_16 as i8);
    style.spacing.interact_size.y = 24.0;
    style.spacing.scroll.bar_width = SPACE_8;

    // --- colours -----------------------------------------------------------
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = WINDOW;
    visuals.window_fill = WINDOW;
    visuals.extreme_bg_color = BASE;
    visuals.faint_bg_color = BASE;
    visuals.override_text_color = Some(TEXT);
    visuals.window_stroke = egui::Stroke::new(1.0, BUTTON);
    visuals.selection.bg_fill = SELECTION;
    visuals.selection.stroke = egui::Stroke::new(1.0, ON_ACCENT);
    visuals.hyperlink_color = COLOR_ACCENT;

    let rounded = egui::CornerRadius::same(4);
    visuals.widgets.noninteractive.bg_fill = WINDOW;
    visuals.widgets.noninteractive.weak_bg_fill = WINDOW;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BUTTON);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.corner_radius = rounded;

    visuals.widgets.inactive.bg_fill = BUTTON;
    visuals.widgets.inactive.weak_bg_fill = BUTTON;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BUTTON);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = rounded;

    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(0x46, 0x4a, 0x50);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0x46, 0x4a, 0x50);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, COLOR_ACCENT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.corner_radius = rounded;

    visuals.widgets.active.bg_fill = COLOR_ACCENT;
    visuals.widgets.active.weak_bg_fill = COLOR_ACCENT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, COLOR_ACCENT);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, ON_ACCENT);
    visuals.widgets.active.corner_radius = rounded;

    visuals.widgets.open.bg_fill = BUTTON;
    visuals.widgets.open.weak_bg_fill = BUTTON;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.open.corner_radius = rounded;

    visuals.striped = false;
    style.visuals = visuals;

    // Semantic colours, so no widget invents one (rule g). They live in one
    // place because `verdict_color` and the banners both need them.
    let _ = (COLOR_ERROR, COLOR_WARN, COLOR_OK, PLACEHOLDER, DISABLED);

    ctx.set_style_of(egui::Theme::Dark, style);
}
