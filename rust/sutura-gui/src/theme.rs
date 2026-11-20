//! The visual design: tokens, the card frame, and the egui `Style` built from
//! them.
//!
//! Matched to the approved target image: a charcoal window holding one large
//! rounded panel, cards inside it, outline buttons, and a single green accent.
//! Values live here and nowhere else -- a widget that invents a colour is how the
//! earlier rounds ended up disagreeing with the design.
//!
//! Two rules learned the hard way, stated once:
//!   * every surface is opaque -- eframe's default `clear_color` has alpha 180,
//!     which is what made the window half-transparent on macOS;
//!   * every text style is pinned to a token. `.weak()` resolves through
//!     `weak_text_color`, which is `text_color * weak_text_alpha`; at the default
//!     0.6 that lands near black on a dark card, which a reviewer saw as "some
//!     text is black".

/// The window behind the panel.
pub const WINDOW: egui::Color32 = egui::Color32::from_rgb(0x16, 0x17, 0x18);
/// The one large rounded panel everything sits on.
pub const PANEL: egui::Color32 = egui::Color32::from_rgb(0x1f, 0x20, 0x23);
/// A card inside the panel -- deliberately darker than the panel, as in the
/// target.
pub const CARD: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x1b, 0x1d);
/// The unfilled track behind a score bar -- one step up from the card, so an
/// empty bar still reads as a bar.
pub const TRACK: egui::Color32 = egui::Color32::from_rgb(0x2e, 0x30, 0x34);
/// The width every score bar is drawn at, so four rows line up.
pub const SCORE_BAR_W: f32 = 112.0;
/// A control at rest; an outline button is transparent and shows the card.
pub const CONTROL: egui::Color32 = egui::Color32::from_rgb(0x24, 0x25, 0x28);
pub const CONTROL_HOVER: egui::Color32 = egui::Color32::from_rgb(0x2c, 0x2e, 0x32);
/// Card borders, 1 px.
pub const BORDER: egui::Color32 = egui::Color32::from_rgb(0x2e, 0x30, 0x34);
/// A button's outline, a touch lighter than a card border.
pub const BUTTON_BORDER: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3d, 0x42);

/// Primary text.
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(0xec, 0xed, 0xef);
/// Card titles.
pub const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(0xb8, 0xbb, 0xc0);
/// Table headers, hints, section labels -- one step further down.
pub const TEXT_FAINT: egui::Color32 = egui::Color32::from_rgb(0x8e, 0x92, 0x98);
/// Text on the accent fill.
pub const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x0f, 0x2e, 0x20);

/// The single accent. Green, not turquoise.
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x4c, 0xa3, 0x7a);
/// "Fixed" in the Result column.
pub const OK: egui::Color32 = egui::Color32::from_rgb(0x6b, 0xbf, 0x59);
/// A problem count.
pub const WARN: egui::Color32 = egui::Color32::from_rgb(0xe0, 0xa0, 0x30);
/// A failed repair.
pub const ERROR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x5c, 0x5c);

/// A selected row: navy, with an accent bar on its left.
pub const ROW_SELECTED: egui::Color32 = egui::Color32::from_rgb(0x1b, 0x22, 0x30);

// --- metrics ---------------------------------------------------------------

pub const RADIUS_PANEL: u8 = 16;
pub const RADIUS_CARD: u8 = 14;
/// Kept for call sites that predate the card/panel split.
pub const RADIUS: u8 = RADIUS_CARD;
pub const RADIUS_BUTTON: u8 = 12;
pub const BORDER_W: f32 = 1.0;
/// Inside a card.
pub const PAD_CARD: f32 = 20.0;
/// Inside the panel, around the cards.
pub const PAD_PANEL: f32 = 20.0;
/// Between cards.
pub const GAP_CARD: f32 = 16.0;
pub const GAP_8: f32 = 8.0;
pub const GAP_16: f32 = 16.0;
pub const GAP_24: f32 = 24.0;
/// A vertical rhythm helper for frames that must not add margin.
pub const GAP_0: f32 = 0.0;
/// Every button is this tall, so a row of them lines up.
pub const BUTTON_H: f32 = 36.0;
pub const BUTTON_PAD_X: f32 = 16.0;
/// A table row: generous, so the rows breathe.
pub const ROW_H: f32 = 40.0;
/// The label column in Options, so every control starts at one x.
pub const LABEL_W: f32 = 104.0;
/// The progress bar: thin, and not the default rounded knob.
pub const PROGRESS_H: f32 = 4.0;
/// The main panel is centred and stops growing here, so a wide display shows the
/// same layout rather than a stretched one.
pub const PANEL_MAX_W: f32 = 1400.0;
/// Above this width the layout is two columns; at or below it, one stacked one.
pub const TWO_COLUMN_MIN_W: f32 = 1100.0;
/// The header row's height; on macOS the traffic lights sit inside it.
pub const HEADER_H: f32 = 36.0;
/// Content is centred and never wider than this on a very wide screen.
pub const CONTENT_MAX_W: f32 = 1480.0;
/// The single centred column shown before any file is added.
pub const EMPTY_MAX_W: f32 = 820.0;
/// The 3D preview card never gets shorter than this.
pub const PREVIEW_MIN_H: f32 = 300.0;
/// The preview's height in the stacked (narrow) layout.
pub const PREVIEW_STACKED_H: f32 = 380.0;

pub const FONT_BODY: f32 = 16.0;
pub const FONT_TITLE: f32 = 20.0;
pub const FONT_CARD_TITLE: f32 = 15.0;
pub const FONT_TABLE_HEAD: f32 = 13.0;
pub const FONT_SECTION: f32 = 13.0;

/// A card: the surface every region sits on.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(egui::Stroke::new(BORDER_W, BORDER))
        .corner_radius(egui::CornerRadius::same(RADIUS_CARD))
        .inner_margin(egui::Margin::same(PAD_CARD as i8))
}

/// The large rounded panel the cards sit on.
pub fn panel() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(egui::Stroke::new(BORDER_W, BORDER))
        .corner_radius(egui::CornerRadius::same(RADIUS_PANEL))
        .inner_margin(egui::Margin::same(PAD_PANEL as i8))
}

/// A card title: secondary, one step up from the table headers.
pub fn card_title(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(
        egui::RichText::new(text.into())
            .size(FONT_CARD_TITLE)
            .color(TEXT_MUTED),
    );
}

/// A table column header: the faintest text in the card, and small.
pub fn table_header(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(
        egui::RichText::new(text.into())
            .size(FONT_TABLE_HEAD)
            .color(TEXT_FAINT),
    );
}

/// A section label: small, uppercase, faint. "ANALYSIS".
pub fn section_label(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(
        egui::RichText::new(text.into().to_uppercase())
            .size(FONT_SECTION)
            .color(TEXT_FAINT),
    );
}

/// A number: tabular figures, so a column of counts lines up.
pub fn number(text: impl Into<String>, colour: egui::Color32) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(FONT_BODY)
        .monospace()
        .color(colour)
}

/// The accent bar a selected row draws on its left edge.
pub fn selection_bar(painter: &egui::Painter, rect: egui::Rect) {
    let bar = egui::Rect::from_min_size(rect.left_top(), egui::vec2(3.0, rect.height()));
    painter.rect_filled(bar, 0.0, ACCENT);
}

/// Build the egui `Style` from the tokens.
pub fn apply(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    style.text_styles = [
        (TextStyle::Heading, FontId::new(FONT_CARD_TITLE, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(FONT_BODY, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(FONT_BODY, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(FONT_SECTION, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(FONT_BODY, FontFamily::Monospace)),
    ]
    .into();

    style.spacing.item_spacing = egui::vec2(GAP_8, GAP_8);
    style.spacing.button_padding = egui::vec2(BUTTON_PAD_X, 0.0);
    style.spacing.menu_margin = egui::Margin::same(GAP_8 as i8);
    style.spacing.indent = GAP_16;
    style.spacing.window_margin = egui::Margin::same(GAP_16 as i8);
    style.spacing.interact_size.y = BUTTON_H;
    style.spacing.scroll.bar_width = 7.0;
    style.spacing.scroll.bar_inner_margin = 2.0;
    style.spacing.scroll.bar_outer_margin = 2.0;
    // Solid, not floating: a floating bar allocates half the width, hides the
    // handle until the pointer is inside the area, and its trough competes with
    // the surface behind it. A screenshot has no pointer, so every capture showed
    // no bar at all.
    style.spacing.scroll.floating = false;
    // The handle takes `fg_stroke` when this is true, which is near-white.
    style.spacing.scroll.foreground_color = false;
    style.spacing.scroll.handle_min_length = 40.0;
    style.spacing.scroll.dormant_handle_opacity = 0.55;
    style.spacing.scroll.active_handle_opacity = 0.75;
    style.spacing.scroll.interact_handle_opacity = 1.0;
    style.spacing.scroll.dormant_background_opacity = 0.25;
    style.spacing.scroll.active_background_opacity = 0.35;

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = WINDOW;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = CONTROL;
    visuals.faint_bg_color = CARD;
    visuals.override_text_color = Some(TEXT);
    // `.weak()` labels -- table headers, hints, section labels -- resolve through
    // `weak_text_color`, which is `text_color * weak_text_alpha`. At the default
    // 0.6 that is near-black on a dark card, which read as "some text is black".
    visuals.weak_text_color = Some(TEXT_MUTED);
    visuals.weak_text_alpha = 1.0;
    visuals.window_stroke = egui::Stroke::new(BORDER_W, BORDER);
    visuals.selection.bg_fill = ROW_SELECTED;
    visuals.selection.stroke = egui::Stroke::new(BORDER_W, TEXT);
    visuals.hyperlink_color = ACCENT;

    let rounded = egui::CornerRadius::same(RADIUS_BUTTON);
    visuals.widgets.noninteractive.bg_fill = CARD;
    visuals.widgets.noninteractive.weak_bg_fill = CARD;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(BORDER_W, BORDER);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(BORDER_W, TEXT);
    visuals.widgets.noninteractive.corner_radius = rounded;

    // An outline button: transparent at rest, so the card shows through.
    visuals.widgets.inactive.bg_fill = CONTROL;
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(BORDER_W, BUTTON_BORDER);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(BORDER_W, TEXT);
    visuals.widgets.inactive.corner_radius = rounded;

    visuals.widgets.hovered.bg_fill = CONTROL_HOVER;
    visuals.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(BORDER_W, TEXT_FAINT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(BORDER_W, TEXT);
    visuals.widgets.hovered.corner_radius = rounded;

    visuals.widgets.active.bg_fill = CONTROL_HOVER;
    visuals.widgets.active.weak_bg_fill = CONTROL_HOVER;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(BORDER_W, ACCENT);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(BORDER_W, TEXT);
    visuals.widgets.active.corner_radius = rounded;

    visuals.widgets.open.bg_fill = CONTROL;
    visuals.widgets.open.weak_bg_fill = CONTROL;
    visuals.widgets.open.fg_stroke = egui::Stroke::new(BORDER_W, TEXT);
    visuals.widgets.open.corner_radius = rounded;

    visuals.striped = false;
    style.visuals = visuals;

    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(egui::Theme::Dark);
}
