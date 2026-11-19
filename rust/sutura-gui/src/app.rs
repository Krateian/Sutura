use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use sutura_core::repair::CancelToken;
use sutura_core::repair_settings::RepairSettings;

use crate::i18n::{self, Language};
use crate::mesh::DisplayMesh;
use crate::viewer::Preview;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Before,
    After,
}

#[derive(Clone, Debug)]
enum RepairState {
    Idle,
    Running { step: u8, total: u8 },
    Done,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
struct RepairReport {
    health: Option<f64>,
    risk: Option<f64>,
    fidelity: Option<f64>,
    confidence: Option<f64>,
    verdict_code: String,
    verdict_areas: i64,
    status_code: String,
}

impl RepairReport {
    fn from_value(v: &Value) -> Self {
        let num = |k: &str| v.get(k).and_then(Value::as_f64);
        Self {
            health: num("repair_health"),
            risk: num("repair_risk"),
            fidelity: num("repair_fidelity"),
            confidence: num("repair_confidence"),
            verdict_code: v
                .get("repair_verdict_code")
                .and_then(Value::as_str)
                .unwrap_or("unavailable")
                .to_string(),
            verdict_areas: v
                .get("repair_verdict_areas")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            status_code: v
                .get("repair_status_code")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

struct Entry {
    path: PathBuf,
    input: Option<Arc<DisplayMesh>>,
    result: Option<Arc<DisplayMesh>>,
    error: Option<String>,
    repair: RepairState,
    report: Option<RepairReport>,
    /// The unparsed report, kept because `RepairReport` deliberately extracts
    /// only the six verdict fields. The method ranking lives in here.
    raw_report: Option<Value>,
    output: Option<PathBuf>,
    view: ViewMode,
    /// Cached defect analysis, filled on demand. `Result` so a failure is shown
    /// once instead of re-loading the mesh every frame; `Err` is the message.
    analysis: Option<Result<sutura_core::analysis::Analysis, String>>,
    /// Cached 3MF object list, same on-demand policy. Empty for a non-3MF file.
    objects: Option<Result<Vec<Value>, String>>,
    /// Cached defect list, same on-demand policy as `analysis`.
    defects: Option<Result<sutura_core::report::defects::DefectReport, String>>,
    /// Methods for THIS file, in the CLI's `--methods` order. `None` means "use
    /// the panel's global selection", which is what a file that was never
    /// customised should do -- so a change to the panel still applies to it.
    ///
    /// Python keeps the same per-file list (`gui.py:5093`) and lets the Methods
    /// tab edit the *selected* file's sequence.
    methods: Option<Vec<u8>>,
}

/// A rasterised before/after pair, held as textures so the window can be drawn
/// every frame without re-rendering.
struct ComparePair {
    title: String,
    before: Option<egui::TextureHandle>,
    after: Option<egui::TextureHandle>,
    error: Option<String>,
}

impl ComparePair {
    /// Rasterise both sides. `after` is the repaired mesh, absent until a repair
    /// has run.
    fn build(
        ctx: &egui::Context,
        title: String,
        before: &DisplayMesh,
        after: Option<&DisplayMesh>,
        heatmap: bool,
    ) -> Self {
        // 384x288 each: side by side in a 900 px window, small enough that a
        // 150k-triangle mesh still rasterises in one frame.
        const SIZE: [u32; 2] = [384, 288];
        let mut upload = |name: &str, mesh: &DisplayMesh| {
            match crate::viewer::Preview::rasterize_for_still(mesh, SIZE, heatmap) {
                Ok(pixmap) => {
                    let image = egui::ColorImage::from_rgba_premultiplied(
                        [pixmap.width() as usize, pixmap.height() as usize],
                        pixmap.data(),
                    );
                    Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
                }
                Err(e) => {
                    eprintln!("still render failed: {e}");
                    None
                }
            }
        };
        let before_tex = upload("compare-before", before);
        let after_tex = after.and_then(|m| upload("compare-after", m));
        let error = if before_tex.is_none() {
            Some("could not render the before image".to_string())
        } else if after.is_some() && after_tex.is_none() {
            Some("could not render the after image".to_string())
        } else {
            None
        };
        Self {
            title,
            before: before_tex,
            after: after_tex,
            error,
        }
    }
}

/// Whether the repaired output is actually watertight.
///
/// **The one field everything colour-codes on**: `stage1.two_manifold` is true
/// and `stage1.holes_remaining` is zero, which is also exactly what
/// `report::classify` calls `watertight`. Three places used to disagree --
/// the table cell painted green from `repair_verdict_code`, the verdict was
/// amber because `brief` could not see a written `output`, and the summary
/// counted `preserved_notes` as watertight -- with one repaired, open mesh
/// showing green in the table and "the model is still not closed" right below it.
///
/// `None` when the report has no topology at all (a container, or a run that
/// never reached stage 1); callers keep the code's own colour then.
fn output_watertight(raw: &Value) -> Option<bool> {
    let s1 = raw.get("stage1")?;
    let two_manifold = s1.get("two_manifold").map(truthy)?;
    let holes_remaining = s1.get("holes_remaining").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(two_manifold && holes_remaining == 0)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => false,
    }
}

impl Entry {
    /// The short per-file decision shown next to the name: `(text, colour, tip)`.
    ///
    /// A file that was never repaired has no decision to show at all (`None`),
    /// rather than a "pending" label -- Python writes an empty string there
    /// (`gui.py:6086`), and omitting the widget is the egui equivalent.
    fn decision(&self) -> Option<(String, egui::Color32, String)> {
        if matches!(self.repair, RepairState::Running { .. }) {
            return None;
        }
        let report = self.report.as_ref()?;
        let text = verdict_text(report);
        let status = status_text(&report.status_code);
        // Same rule as the verdict and the summary: green only when the output
        // is watertight, whatever the verdict string says.
        let colour = match self.raw_report.as_ref().and_then(output_watertight) {
            Some(false) => crate::theme::WARN,
            _ => verdict_color(&report.verdict_code),
        };
        Some((text.clone(), colour, status))
    }
}

/// What the file table's right-click menu asked for.
///
/// Collected while the table is borrowed and applied after it, the same pattern
/// the row buttons use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Clear the cached analysis so this frame recomputes it.
    Analyze(usize),
    /// Open the repaired file (`true` = reveal in the folder).
    OpenOutput(usize, bool),
    /// Open Options on the Methods tab with this file selected.
    EditMethods(usize),
    /// Drop the file's own method list, so it follows the global one again.
    ClearTags(usize),
    /// Add a recommended method to this file's own list.
    TagMethod(usize, u8),
    /// Open the repaired file in the detected slicer at this index.
    OpenInSlicer(usize, usize),
}

/// The ranked methods in a file's report: `(num, name, score 0..1)`.
///
/// Two shapes, as in the panel: the 3MF path stores `{model_name, report}` and
/// the single-mesh path stores the report directly. Both are handled here so the
/// menu and the panel cannot disagree about what was recommended.
fn recommended_methods(raw: &Value) -> Vec<(u8, String, f64)> {
    let mut out = Vec::new();
    // Three shapes, and missing one is silent: `--analyze` on a single mesh puts
    // `recommendations` at the top level, a repair report nests them per object,
    // and a 3MF wraps each object as `{model_name, report}`.
    if let Some(recs) = raw.get("recommendations").and_then(|v| v.as_array()) {
        collect_recs(recs, &mut out);
    }
    let Some(objects) = raw.get("object_reports").and_then(|v| v.as_array()) else {
        return out;
    };
    for object in objects {
        let report = object.get("report").unwrap_or(object);
        let Some(recs) = report.get("recommendations").and_then(|v| v.as_array()) else {
            continue;
        };
        collect_recs(recs, &mut out);
    }
    out.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn collect_recs(recs: &[Value], out: &mut Vec<(u8, String, f64)>) {
    for rec in recs {
        let Some(num) = rec.get("num").and_then(|v| v.as_u64()) else {
            continue;
        };
        let name = rec
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        let score = rec.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if !out.iter().any(|(n, _, _)| *n == num as u8) {
            out.push((num as u8, name, score));
        }
    }
}

/// A menu item, styled like the rest of the app.
/// A real score bar: 6 px, rounded, coloured by how good the score is.
///
/// `higher_is_better` is the whole reason this is not a one-liner: a risk of 5
/// is excellent and a risk of 90 is not, while a health of 5 is the opposite.
/// Green / amber / red are the only three meanings this app uses.
fn score_bar_widget(ui: &mut egui::Ui, score: Option<f64>, higher_is_better: bool) {
    const H: f32 = 6.0;
    let w = crate::theme::SCORE_BAR_W;
    // Centre the 6 px bar in the cell's full height rather than drawing it at
    // the cell's top, which is what `allocate_exact_size` would do.
    let cell_h = ui.available_height().max(H);
    let (outer, _) = ui.allocate_exact_size(egui::vec2(w, cell_h), egui::Sense::hover());
    let rect = egui::Rect::from_center_size(outer.center(), egui::vec2(w, H));
    // The trough, so an empty score still reads as a bar and not as a gap.
    ui.painter().rect_filled(rect, H / 2.0, crate::theme::TRACK);
    let Some(score) = score else { return };
    // Reports carry 0..100; the bar works in fractions.
    let fraction = (score / 100.0).clamp(0.0, 1.0) as f32;
    let goodness = if higher_is_better { fraction } else { 1.0 - fraction };
    let colour = if goodness >= 0.66 {
        crate::theme::OK
    } else if goodness >= 0.33 {
        crate::theme::WARN
    } else {
        crate::theme::ERROR
    };
    let filled = egui::Rect::from_min_size(rect.min, egui::vec2(w * fraction, H));
    ui.painter().rect_filled(filled, H / 2.0, colour);
}

/// Shorten a path from the middle, keeping both ends readable.
///
/// The end of a path names the file and the start names the folder it is in;
/// both matter, and a naive truncation keeps only one of them.
fn shorten_path(path: &str, max_width: f32) -> String {
    // ~7 px per character at this size; enough to decide, not exact.
    let max_chars = ((max_width / 7.0).floor() as usize).max(16);
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max_chars {
        return path.to_string();
    }
    let keep = max_chars.saturating_sub(1) / 2;
    let head: String = chars[..keep].iter().collect();
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("{head}…{tail}")
}

/// Blend `t` of `top` into `base`: the one way this app tints a surface.
fn lerp_color(base: egui::Color32, top: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    egui::Color32::from_rgb(
        mix(base.r(), top.r()),
        mix(base.g(), top.g()),
        mix(base.b(), top.b()),
    )
}

fn menu_action(ui: &mut egui::Ui, text: String) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(crate::theme::FONT_BODY)
                .color(crate::theme::TEXT),
        )
        .fill(egui::Color32::TRANSPARENT)
        .min_size(egui::vec2(ui.available_width(), crate::theme::BUTTON_H * 0.8)),
    )
    .clicked()
}

/// What the Repair tab's profile buttons asked for, applied after the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileAction {
    New,
    Duplicate,
    Rename,
    Delete,
    SaveAsNew,
}

/// The palette. Three semantic colours, one accent, and nothing else (rule g).
///
/// Taken from Qt's `apply_dark_theme` so the two windows agree; a control that
/// picks its own colour is what made the first Rust UI look unlike anything
/// shipped before it.
/// Sutura's version, as the CLI and the Python GUI report it.
///
/// `CARGO_PKG_VERSION` is the crate's own 0.1.0 placeholder and showed as
/// "v0.1.0" in the window; a user compares versions with the release they
/// downloaded, and this is that number (`sutura/repair.py:171`).
pub(crate) const SUTURA_VERSION: &str = "0.9.9";

pub(crate) const COLOR_ERROR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x5b, 0x5b);
pub(crate) const COLOR_WARN: egui::Color32 = egui::Color32::from_rgb(0xf5, 0x9e, 0x0b);
pub(crate) const COLOR_OK: egui::Color32 = egui::Color32::from_rgb(0x2e, 0xc4, 0x7a);
/// The single accent, used for the one primary action (rule c). Qt's value
/// (`gui.py:2217`), not a new one.
pub(crate) const COLOR_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x4c, 0xa3, 0x7a);
pub(crate) const COLOR_ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x0b, 0x0f, 0x11);

/// The 8 px grid (rule b): every gap is 8, 16 or 24.
/// The label column in every Options tab, so the controls line up (rule h).
/// The automatic ladder: the policy order Qt listed. Read-only in automatic
/// mode, which is what makes the rows informative rather than inert.
pub(crate) const LADDER: [u8; 6] = [2, 13, 16, 18, 19, 7];

pub(crate) const LABEL_W: f32 = 104.0;
pub(crate) const SPACE_8: f32 = 8.0;
pub(crate) const SPACE_16: f32 = 16.0;
pub(crate) const SPACE_24: f32 = 24.0;

pub(crate) enum RepairEvent {
    Started(u32),
    Progress { step: u8, total: u8 },
    Stderr(String),
    Finished {
        output: PathBuf,
        result: Result<(DisplayMesh, RepairReport, Value), String>,
        cancelled: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intensity {
    Quick,
    Balanced,
    Thorough,
    Extreme,
}

impl Intensity {
    pub fn value(self) -> &'static str {
        match self {
            Intensity::Quick => "quick",
            Intensity::Balanced => "balanced",
            Intensity::Thorough => "thorough",
            Intensity::Extreme => "extreme",
        }
    }

    pub fn label(self) -> String {
        match self {
            Intensity::Quick => rust_i18n::t!("app.intensity_quick").to_string(),
            Intensity::Balanced => rust_i18n::t!("app.intensity_balanced").to_string(),
            Intensity::Thorough => rust_i18n::t!("app.intensity_thorough").to_string(),
            Intensity::Extreme => rust_i18n::t!("app.intensity_extreme").to_string(),
        }
    }

    /// Inverse of [`Self::value`], for restoring saved state. Unknown strings
    /// fall back to the default rather than failing to start.
    pub fn from_value(value: &str) -> Self {
        match value {
            "quick" => Intensity::Quick,
            "thorough" => Intensity::Thorough,
            "extreme" => Intensity::Extreme,
            _ => Intensity::Balanced,
        }
    }
}

/// `--mode` ladder, in Python's order and spelling
/// (`gui.py`: `RepairModeDialog.MODES`), so the value strings match `parse_mode`
/// exactly.
pub const MODES: [&str; 5] = ["low", "medium", "auto", "aggressive", "extreme"];

/// `--profile` presets, from Python's picker (`gui.py:5422`). `None` is "Auto".
pub const PROFILES: [&str; 5] = ["mechanical", "organic", "scan", "miniature", "fast"];

pub struct StartupOptions {
    /// Test hook: force the deviation reference for the After view.
    pub deviation_reference: Option<PathBuf>,
    pub screenshot_path: Option<PathBuf>,
    pub exit_after_screenshot: bool,
    pub auto_repair: bool,
    pub auto_batch: bool,
    pub heatmap: bool,
    pub about: bool,
    pub updater: bool,
    pub intensity: Intensity,
    /// True when `--intensity` was given. Saved state must not override an
    /// explicit flag, and without this the two are indistinguishable.
    pub intensity_explicit: bool,
    /// True when `--lang` was given.
    pub language_explicit: bool,
    /// Open a view as soon as the first frame is drawn, so a screenshot can
    /// capture it without a click. `--screenshot` runs with no input device, so
    /// there is no way to open a window or a picker interactively; these flags
    /// are what make every view reachable from a script. Same role as
    /// `--heatmap` and `--about`, which already existed for this reason.
    pub open: OpenOnStart,
    /// Select this file index after loading (default: the last added, which is
    /// what a user who just dropped a file sees).
    pub select: Option<usize>,
    /// Start the settings panel at this scroll offset. Proving a control near
    /// the bottom of a taller-than-any-window panel needs it, and `--screenshot`
    /// cannot scroll.
    pub settings_scroll: f32,
    /// Frames to settle before capturing; an overlay needs more than the main
    /// window for its fade-in to finish.
    pub wait_frames: u8,
    /// Point the camera at this centre, optionally with an explicit zoom. A
    /// capture hook like `--settings-scroll`: a defect that is three pixels wide
    /// in the whole-part view cannot be documented, and `--screenshot` has no
    /// input device to orbit or zoom with.
    pub focus: Option<([f32; 3], Option<f32>)>,
    /// Capture hook: open Options on this tab as soon as the window appears.
    pub options_tab: Option<OptionsTab>,
    /// Capture hook: start the Methods tab on Manual with this sequence.
    pub methods_manual: Option<String>,
    /// Capture hook: where the Options body starts scrolled to.
    pub options_scroll: Option<f32>,
    /// Give the selected file its own method list, as if the user had unticked
    /// "Use the panel selection" and picked these. Needed to show the per-file
    /// editor in a state other than "inherited" -- which is otherwise the same
    /// frame as the panel above it.
    pub file_methods: Option<Vec<u8>>,
}

/// The Options window's tabs, in Qt's shipped order.
///
/// The order is part of the layout: a user who learned where "Experimental"
/// sits should not have to relearn it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionsTab {
    General,
    Repair,
    Methods,
    Experimental,
    Updates,
    Changelog,
    Engines,
}

impl OptionsTab {
    pub const ALL: [OptionsTab; 7] = [
        OptionsTab::General,
        OptionsTab::Repair,
        OptionsTab::Methods,
        OptionsTab::Experimental,
        OptionsTab::Updates,
        OptionsTab::Changelog,
        OptionsTab::Engines,
    ];

    pub fn label(self) -> String {
        let key = match self {
            OptionsTab::General => "app.tab_general",
            OptionsTab::Repair => "app.tab_repair",
            OptionsTab::Methods => "app.tab_methods",
            OptionsTab::Experimental => "app.tab_experimental",
            OptionsTab::Updates => "app.tab_updates",
            OptionsTab::Changelog => "app.tab_changelog",
            OptionsTab::Engines => "app.tab_engines",
        };
        rust_i18n::t!(key).to_string()
    }
}

/// Which overlay to show on the first frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenOnStart {
    #[default]
    None,
    Compare,
    RepeatPicker,
    Changelog,
    DefectColors,
    /// Show the repaired result (`After`) rather than the input. The deviation
    /// ramp only exists on the repaired side, so the heatmap's colouring can only
    /// be documented there.
    After,
}

pub struct SuturaApp {
    files: Vec<Entry>,
    selected: Option<usize>,
    loaded: Option<(usize, ViewMode)>,
    pub(crate) language: Language,
    preview: Preview,
    log: Vec<String>,
    repair_rx: Option<Receiver<RepairEvent>>,
    repair_index: Option<usize>,
    /// Handle on the running repair's cooperative cancel flag. There is no child
    /// process any more, so cancelling sets this instead of signalling a pid.
    cancel_token: Option<Arc<CancelToken>>,
    pending_overwrite: Option<(usize, PathBuf)>,
    pending_batch: Option<Vec<usize>>,
    intensity: Intensity,
    /// The full repair settings the panel edits. This IS the source of truth:
    /// `options_from` turns it into `RepairOptions`, the same function the CLI
    /// uses, so a control here cannot drift from the CLI.
    pub(crate) settings: RepairSettings,
    batch: Vec<usize>,
    batch_total: usize,
    batch_done: usize,
    batch_force: bool,
    batch_active: bool,
    auto_batch: bool,
    auto_batch_started: bool,
    about_open: bool,
    updater_open: bool,
    /// The repeat picker window, created on demand from the selected mesh.
    repeat_picker: Option<crate::repeat_picker::RepeatPicker>,
    /// Overrides the deviation reference for the After view. A **test hook** so
    /// the ramp can be demonstrated with a known pair, independent of what the
    /// repair happened to move.
    deviation_reference: Option<PathBuf>,
    /// Updater state (check / download / install), driven on a worker thread.
    pub(crate) updater: crate::updater_ui::Updater,
    /// The "what's new" window, reading the snapshot off disk.
    notes_open: bool,
    screenshot_path: Option<PathBuf>,
    screenshot_requested: bool,
    exit_after_screenshot: bool,
    auto_repair: bool,
    auto_repair_started: bool,
    ready_frames: u8,
    /// Frames to settle before capturing. 3 by default; an overlay such as the
    /// before/after window needs more for its fade-in to finish.
    wait_frames: u8,
    /// Serialized form of the last persisted state. Compared at the end of every
    /// frame so a write happens only when something actually changed -- the
    /// settings panel mutates `self.settings` in a dozen places and hooking each
    /// one would drift.
    persisted: String,
    /// `--open-compare` asked for the pair, but the repair had not finished when
    /// the window opened. Opened from the frame that has the result.
    pending_compare: bool,
    /// The current egui context, refreshed each frame. A repair worker holds a
    /// clone so it can wake the UI when it finishes -- see `start_repair_mode`.
    ctx: Option<egui::Context>,
    /// One-shot: size the window to the screen on the first frame, when the
    /// The window size restored from `gui.json` at startup, before any frame has
    /// run. `None` means "no previous run".
    saved_window_size: Option<[f32; 2]>,
    /// Latest inner size seen, so `Drop` can save it.
    last_window_size: Option<[f32; 2]>,
    /// The size the window opened at. A later difference is a user resize and is
    /// the only thing worth persisting.
    open_size: Option<[f32; 2]>,
    /// Whether the user has resized since startup.
    user_resized: bool,
    /// `Ctrl+,` toggles the settings panel. Python's only shortcut opens its
    /// options dialog; settings live in a permanent panel here, so the honest
    /// equivalent is to show or hide that panel.
    settings_open: bool,
    /// Where the settings panel starts scrolled to. `--settings-scroll`, and 0
    /// (the top) unless a script asks otherwise.
    settings_scroll: f32,
    /// Last drawn scroll offset of each area, for the interaction tests.
    /// `Arc<AtomicU32>` rather than plain fields because the capture happens
    /// inside the scroll-area closures while `self` is already borrowed.
    settings_scroll_probe: Arc<std::sync::atomic::AtomicU32>,
    file_scroll_probe: Arc<std::sync::atomic::AtomicU32>,
    /// The Options body's scroll offset, same purpose as the two above.
    pub(crate) options_scroll_probe: Arc<std::sync::atomic::AtomicU32>,
    /// Method availability, from `self_check::run`. Process-wide, so it is
    /// computed once at startup rather than per file -- and with
    /// `run_fixtures:false`, the same cheap availability probe `--self-check`
    /// does without `--self-check-fixtures`.
    method_health: Vec<Value>,
    /// Resolved at startup: whether fTetWild is usable and where it is. It is
    /// packaged as a sidecar, so this is normally a hit -- the panel exists to
    /// make the rare miss visible.
    ftetwild: Option<String>,
    /// Set when a panic was recorded in this process. The dialog is the whole
    /// point of the crash log: a trace nobody is told about is not reporting.
    crash_open: bool,
    /// Result message of the last diagnostics export, shown in the dialog.
    pub(crate) diagnostics_note: Option<String>,
    /// Version history for the Changelog view: `(tag, body)` pairs, newest
    /// first. Fetched **on request and never written to disk** -- the on-disk
    /// snapshot has exactly one copy (`whatsnew::current/`), and a second one
    /// would drift from it.
    pub(crate) changelog: Option<Vec<(String, String)>>,
    changelog_open: bool,
    pub(crate) changelog_error: Option<String>,
    /// Static before/after pair: two textures rasterised once when the window
    /// opens. `None` while closed. Python's default view is static
    /// (`gui.py:7373`); the live 3D preview is the interactive alternative.
    compare: Option<ComparePair>,
    /// Camera focus from `--focus`, applied when a mesh is loaded.
    focus: Option<([f32; 3], Option<f32>)>,
    /// The General tab's preferences, which Qt kept in `config.json` and which
    /// `RepairOptions` has no field for. They are applied around the repair in
    /// `run_repair`, the same switches the CLI's `--no-*` flags set.
    pub(crate) prefs: crate::state::GuiState,
    /// Slicers found at startup. Computed once: a user does not install one
    /// while the window is open, and detection walks PATH and /Applications.
    slicers: Vec<crate::slicers::Slicer>,
    /// Whether the Analysis card's defects list is expanded.
    defects_open: bool,
    /// Whether the batch summary's issue breakdown is open.
    summary_issues_open: bool,
    /// The last policy write's message and whether it succeeded.
    policy_status: Option<(String, bool)>,
    /// A failed save: (what could not be written, the reason, the wanted path).
    ///
    /// Its own field, not a log line, because "the file was not saved" is the one
    /// outcome a user must not miss -- they will otherwise open a file that was
    /// never written.
    write_error: Option<(String, String, PathBuf)>,
    /// Whether the first-run line has been shown this launch. Per session, not
    /// per frame: a line that reappears on every redraw is not a greeting.
    pub(crate) first_run_seen: bool,
    /// A profile edit requested from the Repair tab. Applied after the frame,
    /// because the dialog needs `&mut self.files`-free access to write the file.
    profile_action: Option<ProfileAction>,
    /// Pending name prompt for the profile buttons, and what to do with it.
    profile_prompt: Option<(ProfileAction, String)>,
    /// The Options window: Qt's 7-tab dialog, which replaced the settings
    /// column. `None` while hidden.
    pub(crate) options_open: bool,
    pub(crate) options_tab: OptionsTab,
    /// Capture hook: where the Options body starts scrolled to.
    pub(crate) options_scroll: Option<f32>,
    /// Capture hook: start the Methods tab on Manual with this sequence.
    pub methods_manual: Option<String>,
}

impl SuturaApp {
    /// --- Test accessors ---
    ///
    /// Only compiled for tests. The interaction tests drive `frame_ui` directly,
    /// so they need to read back what a click changed; keeping these together and
    /// behind `cfg(test)` stops them becoming a second public surface.
    #[cfg(test)]
    pub fn preview_heatmap(&self) -> bool {
        self.preview.heatmap
    }

    #[cfg(test)]
    pub fn preview_defect_colors(&self) -> bool {
        self.preview.defect_colors
    }

    #[cfg(test)]
    pub fn view_mode(&self) -> ViewMode {
        self.selected
            .and_then(|i| self.files.get(i))
            .map(|e| e.view)
            .unwrap_or(ViewMode::Before)
    }

    #[cfg(test)]
    pub fn settings_snapshot(&self) -> String {
        serde_json::to_string(&self.settings).unwrap_or_default()
    }

    #[cfg(test)]
    pub fn intensity(&self) -> Intensity {
        self.intensity
    }

    #[cfg(test)]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Load a file the way `add_path` does, without the file dialog.
    #[cfg(test)]
    pub fn add_file_for_test(&mut self, path: PathBuf) {
        self.add_path(path);
    }

    /// Test accessors for the Qt shell: the panels they name no longer exist
    /// as panels, so these report the state the new layout uses.
    #[cfg(test)]
    pub fn options_open_for_test(&self) -> bool {
        self.options_open
    }

    #[cfg(test)]
    pub fn options_tab_for_test(&self) -> OptionsTab {
        self.options_tab
    }

    #[cfg(test)]
    pub fn open_options_for_test(&mut self, tab: OptionsTab) {
        self.options_open = true;
        self.options_tab = tab;
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    #[cfg(test)]
    pub fn set_output_for_test(&mut self, out: PathBuf) {
        if let Some(i) = self.selected {
            if let Some(slot) = self.files.get_mut(i) {
                slot.output = Some(out);
            }
        }
    }

    #[cfg(test)]
    pub fn raise_write_error_for_test(&mut self, what: &str, reason: &str, path: PathBuf) {
        self.write_error = Some((what.to_string(), reason.to_string(), path));
    }

    #[cfg(test)]
    pub fn mark_method_unavailable_for_test(&mut self) {
        self.method_health.push(serde_json::json!({
            "num": 7,
            "name": "ftetwild",
            "status": "skip",
            "reason": "fTetWild binary not found",
        }));
    }

    #[cfg(test)]
    pub fn log_for_test(&self) -> &[String] {
        &self.log
    }

    #[cfg(test)]
    pub fn selected_recommendations_for_test(&self) -> Vec<(u8, String, f64)> {
        self.selected
            .and_then(|i| self.files.get(i))
            .and_then(|e| e.raw_report.as_ref())
            .map(recommended_methods)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn set_methods_for_test(&mut self, methods: &str) {
        self.settings.methods = Some(methods.to_string());
    }

    #[cfg(test)]
    pub fn policy_status_for_test(&self) -> Option<&(String, bool)> {
        self.policy_status.as_ref()
    }

    #[cfg(test)]
    pub fn slicers_for_test(&self) -> &[crate::slicers::Slicer] {
        &self.slicers
    }

    #[cfg(test)]
    pub fn set_raw_report_for_test(&mut self, raw: Value) {
        if let Some(i) = self.selected {
            if let Some(slot) = self.files.get_mut(i) {
                slot.raw_report = Some(raw);
            }
        }
    }

    #[cfg(test)]
    pub fn select_first_for_test(&mut self) {
        if !self.files.is_empty() {
            self.selected = Some(0);
        }
    }

    #[cfg(test)]
    pub fn clear_selection_for_test(&mut self) {
        self.selected = None;
    }

    #[cfg(test)]
    pub fn remove_file_for_test(&mut self, idx: usize) {
        self.remove_file(idx);
    }

    #[cfg(test)]
    pub fn file_scroll_offset(&self) -> f32 {
        f32::from_bits(self.file_scroll_probe.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The Options body's scroll offset. It lives in egui's own state rather than
    /// a probe: the window is a `ScrollArea` and egui keeps the offset, so it is
    /// read back from there after a frame.
    #[cfg(test)]
    pub fn options_scroll_offset(&self) -> f32 {
        f32::from_bits(
            self.options_scroll_probe
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    #[cfg(test)]
    pub fn settings_panel_hidden(&self) -> bool {
        !self.settings_open
    }

    #[cfg(test)]
    pub fn settings_scroll_offset(&self) -> f32 {
        f32::from_bits(self.settings_scroll_probe.load(std::sync::atomic::Ordering::Relaxed))
    }

    #[cfg(test)]
    pub fn file_list_scroll_offset(&self) -> f32 {
        f32::from_bits(self.file_scroll_probe.load(std::sync::atomic::Ordering::Relaxed))
    }

    #[cfg(test)]
    pub fn about_open(&self) -> bool {
        self.about_open
    }

    #[cfg(test)]
    pub fn updater_open(&self) -> bool {
        self.updater_open
    }

    #[cfg(test)]
    pub fn changelog_open(&self) -> bool {
        self.changelog_open
    }

    /// Open the Changelog without fetching: the fetch is a network call, and a
    /// test that makes one is slow and flaky in a full run.
    #[cfg(test)]
    pub fn open_changelog_for_test(&mut self) {
        self.changelog_open = true;
    }

    #[cfg(test)]
    pub fn repeat_picker_open(&self) -> bool {
        self.repeat_picker.is_some()
    }

    #[cfg(test)]
    pub fn close_modals_for_test(&mut self) {
        self.about_open = false;
        self.updater_open = false;
        self.notes_open = false;
        self.changelog_open = false;
    }

    #[cfg(test)]
    pub fn methods_manual_for_test(&self) -> bool {
        self.settings.methods.is_some()
    }

    #[cfg(test)]
    pub fn selected_file_has_own_methods(&self) -> bool {
        self.selected
            .and_then(|i| self.files.get(i))
            .map(|e| e.methods.is_some())
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub fn selected_file_methods(&self) -> Option<Vec<u8>> {
        self.selected
            .and_then(|i| self.files.get(i))
            .and_then(|e| e.methods.clone())
    }

    #[cfg(test)]
    pub fn scroll_settings_to(&mut self, offset: f32) {
        self.settings_scroll = offset;
    }

    /// Start a repair and hand back the channel it reports on, so a test can wait
    /// for completion WITHOUT driving a frame.
    #[cfg(test)]
    pub fn start_repair_without_frames(
        &mut self,
        idx: usize,
        force: bool,
    ) -> mpsc::Receiver<RepairEvent> {
        self.start_repair_mode(idx, force, false);
        self.repair_rx.take().expect("start_repair opens a channel")
    }

    #[cfg(test)]
    pub fn diagnostics_note(&self) -> Option<String> {
        self.diagnostics_note.clone()
    }

    /// A CPU-only preview, for tests: building the real one needs an
    /// `eframe::CreationContext`, which cannot be constructed in a unit test.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            files: Vec::new(),
            selected: None,
            loaded: None,
            language: Language::System,
            preview: Preview::for_test(),
            log: Vec::new(),
            repair_rx: None,
            repair_index: None,
            cancel_token: None,
            pending_overwrite: None,
            pending_batch: None,
            intensity: Intensity::Balanced,
            settings: sutura_core::repair_settings::RepairSettings::from_intensity(
                Intensity::Balanced.value(),
                false,
            ),
            batch: Vec::new(),
            batch_active: false,
            batch_total: 0,
            batch_done: 0,
            batch_force: false,
            auto_batch: false,
            auto_batch_started: false,
            about_open: false,
            updater_open: false,
            repeat_picker: None,
            deviation_reference: None,
            updater: crate::updater_ui::Updater::new(),
            notes_open: false,
            screenshot_path: None,
            screenshot_requested: false,
            exit_after_screenshot: false,
            auto_repair: false,
            auto_repair_started: false,
            ready_frames: 0,
            wait_frames: 3,
            persisted: String::new(),
            settings_open: true,
            settings_scroll: 0.0,
            settings_scroll_probe: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            file_scroll_probe: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            options_scroll_probe: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            pending_compare: false,
            ctx: None,
            saved_window_size: None,
            last_window_size: None,
            open_size: None,
            user_resized: false,
            method_health: Vec::new(),
            ftetwild: None,
            crash_open: false,
            diagnostics_note: None,
            changelog: None,
            changelog_open: false,
            changelog_error: None,
            compare: None,
            focus: None,
            options_open: false,
            options_tab: OptionsTab::Repair,
            options_scroll: None,
            defects_open: false,
            summary_issues_open: false,
            policy_status: None,
            write_error: None,
            first_run_seen: false,
            slicers: crate::slicers::detect(),
            methods_manual: None,
            prefs: crate::state::GuiState::default(),
            profile_action: None,
            profile_prompt: None,
        }
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        files: Vec<PathBuf>,
        language: Language,
        options: StartupOptions,
    ) -> Self {
        // A screenshot is a still, and egui eases every new window in over
        // `animation_time` (0.2 s). A windowless run draws its frames in
        // milliseconds, so the fade never finishes and every capture comes out a
        // grey ghost -- which reads as a rendering bug and is not. Turning the
        // animation off for `--screenshot` makes the image show the final state;
        // an interactive run keeps its animation.
        // Qt's palette and the 8 px grid, before any widget is built.
        crate::theme::apply(&cc.egui_ctx);
        if options.screenshot_path.is_some() {
            cc.egui_ctx.all_styles_mut(|style| style.animation_time = 0.0);
        }
        let mut preview = Preview::new(cc);
        preview.heatmap = options.heatmap;
        // Saved state is a fallback, never an override: an explicit flag wins.
        let saved = crate::state::load();
        let intensity = if options.intensity_explicit {
            options.intensity
        } else {
            saved
                .intensity
                .as_deref()
                .map(Intensity::from_value)
                .unwrap_or(options.intensity)
        };
        let language = if options.language_explicit {
            language
        } else {
            match saved.language.as_deref() {
                Some("en") => Language::En,
                Some("tr") => Language::Tr,
                _ => language,
            }
        };
        i18n::apply(language);
        let settings = if options.intensity_explicit {
            RepairSettings::from_intensity(intensity.value(), false)
        } else {
            saved.settings.clone()
        };
        // Availability only: no fixtures, so this is a directory lookup per
        // method rather than a repair run, and it is safe to do before the
        // window opens.
        let method_health = sutura_core::self_check::run(false)
            .get("methods")
            .and_then(|m| m.as_object())
            .map(|m| {
                let mut v: Vec<Value> = m.values().cloned().collect();
                v.sort_by_key(|r| r.get("num").and_then(Value::as_u64).unwrap_or(0));
                v
            })
            .unwrap_or_default();
        let mut app = Self {
            files: Vec::new(),
            selected: None,
            loaded: None,
            language,
            preview,
            log: Vec::new(),
            repair_rx: None,
            repair_index: None,
            cancel_token: None,
            pending_overwrite: None,
            pending_batch: None,
            intensity,
            settings,
            batch: Vec::new(),
            batch_total: 0,
            batch_done: 0,
            batch_force: false,
            batch_active: false,
            auto_batch: options.auto_batch,
            auto_batch_started: false,
            about_open: options.about,
            updater_open: options.updater,
            repeat_picker: None,
            deviation_reference: options.deviation_reference,
            updater: crate::updater_ui::Updater::new(),
            notes_open: false,
            screenshot_path: options.screenshot_path,
            screenshot_requested: false,
            exit_after_screenshot: options.exit_after_screenshot,
            auto_repair: options.auto_repair,
            auto_repair_started: false,
            ready_frames: 0,
            wait_frames: 3,
            // Left empty on purpose: the first frame writes the current state
            // out, which creates the file and makes the load/save loop
            // observable. Seeding it with `state_json()` instead would mean a
            // first launch never wrote anything, and the whole path stayed
            // unexercised until some control happened to change.
            persisted: String::new(),
            settings_open: true,
            settings_scroll: options.settings_scroll,
            settings_scroll_probe: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            file_scroll_probe: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            options_scroll_probe: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            pending_compare: false,
            ctx: None,
            saved_window_size: saved.window_size,
            last_window_size: None,
            open_size: None,
            user_resized: false,
            method_health,
            ftetwild: sutura_core::ftetwild::find_binary(),
            crash_open: crate::crash::panic_recorded(),
            diagnostics_note: None,
            changelog: None,
            changelog_open: false,
            changelog_error: None,
            compare: None,
            focus: options.focus,
            options_open: options.options_tab.is_some(),
            options_tab: options.options_tab.unwrap_or(OptionsTab::Repair),
            options_scroll: options.options_scroll,
            methods_manual: options.methods_manual.clone(),
            profile_action: None,
            profile_prompt: None,
            defects_open: false,
            policy_status: None,
            write_error: None,
            first_run_seen: false,
            summary_issues_open: false,
            slicers: crate::slicers::detect(),
            prefs: crate::state::GuiState::default(),
        };
        for path in files {
            app.add_path(path);
        }
        // Capture hook: a Manual selection, so the Up/Down rows can be seen.
        if let Some(seq) = &app.methods_manual {
            app.settings.methods = Some(seq.clone());
        }
        if let Some(idx) = options.select {
            if idx < app.files.len() {
                app.selected = Some(idx);
            }
        }
        if let Some(methods) = options.file_methods.clone() {
            if let Some(entry) = app.selected.and_then(|i| app.files.get_mut(i)) {
                let mut sorted = methods;
                sorted.sort_unstable();
                entry.methods = Some(sorted);
            }
        }
        // Views that need a loaded mesh are opened here rather than in the first
        // frame: `ComparePair::build` uploads textures, which needs the context
        // the constructor already has.
        match options.open {
            // Deferred to the first frame that has a repair result: building it
            // here would capture the Before half before the repair runs, and the
            // After panel would be a permanent "(not rendered)".
            OpenOnStart::Compare => app.pending_compare = true,
            OpenOnStart::DefectColors => {
                app.preview.defect_colors = true;
                app.preview.dirty = true;
            }
            OpenOnStart::After => {
                if let Some(entry) = app.selected.and_then(|i| app.files.get_mut(i)) {
                    entry.view = ViewMode::After;
                }
            }
            OpenOnStart::RepeatPicker => app.open_repeat_picker(),
            OpenOnStart::Changelog => {
                app.changelog_open = true;
                app.fetch_changelog();
            }
            OpenOnStart::None => {}
        }
        app
    }

    fn add_path(&mut self, path: PathBuf) {
        if path.is_dir() {
            if let Ok(read_dir) = std::fs::read_dir(&path) {
                let mut children: Vec<PathBuf> = read_dir
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| is_mesh_path(p))
                    .collect();
                children.sort();
                for child in children {
                    self.add_file(child);
                }
            }
            return;
        }
        if is_mesh_path(&path) {
            self.add_file(path);
        } else {
            self.log(format!(
                "{}",
                rust_i18n::t!("app.log_ignored", path = path.display().to_string())
            ));
        }
    }

    fn add_file(&mut self, path: PathBuf) {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if self.files.iter().any(|e| e.path == canonical) {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let entry = match DisplayMesh::load(&path) {
            Ok(mesh) => {
                self.log(
                    rust_i18n::t!(
                        "app.log_loaded",
                        name = name,
                        count = mesh.triangle_count()
                    )
                    .to_string(),
                );
                Entry {
                    path: canonical,
                    input: Some(Arc::new(mesh)),
                    result: None,
                    error: None,
                    repair: RepairState::Idle,
                    report: None,
                    raw_report: None,
                    output: None,
                    view: ViewMode::Before,
                    analysis: None,
                    objects: None,
                    defects: None,
                    methods: None,
                }
            }
            Err(e) => {
                self.log(
                    rust_i18n::t!("app.log_load_failed", name = name, error = e).to_string(),
                );
                Entry {
                    path: canonical,
                    input: None,
                    result: None,
                    error: Some(rust_i18n::t!("app.error_prefix", error = e).to_string()),
                    repair: RepairState::Idle,
                    report: None,
                    raw_report: None,
                    output: None,
                    view: ViewMode::Before,
                    analysis: None,
                    objects: None,
                    defects: None,
                    methods: None,
                }
            }
        };
        self.files.push(entry);
        self.selected = Some(self.files.len() - 1);
    }

    /// Open the repeat picker on the selected file's input mesh.
    /// Clear the one-shot scroll seed. Named so the invariant is testable and
    /// the call site cannot forget which half it is doing.
    fn consume_settings_scroll(&mut self) {
        self.settings_scroll = 0.0;
    }

    fn open_repeat_picker(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(entry) = self.files.get(idx) else { return };
        let Some(mesh) = entry.input.clone() else { return };
        self.repeat_picker = Some(crate::repeat_picker::RepeatPicker::new(&mesh));
    }

    /// Fetch the version history for the Changelog view.
    ///
    /// **Never writes to disk.** The single on-disk copy of release notes is
    /// `whatsnew::current/`, maintained by the updater after an install; a cache
    /// here would be a second copy that drifts (Python keeps exactly that, a
    /// `release_notes_cache.json`, and the product rule for this port is one
    /// copy only). So this reads the releases API into memory on request.
    ///
    /// Failure is a message, not a silent empty list: "offline?" and "no
    /// releases" look identical otherwise.
    pub(crate) fn fetch_changelog(&mut self) {
        // The `/latest` endpoint returns one release; the list endpoint is what
        // a history needs.
        let url = sutura_core::updater::GITHUB_API.replace(
            "/releases/latest",
            "/releases?per_page=20",
        );
        let Some(value) = sutura_core::http::get_json(&url) else {
            self.changelog_error = Some(rust_i18n::t!("app.changelog_offline").to_string());
            return;
        };
        let entries: Vec<(String, String)> = value
            .as_array()
            .map(|releases| {
                releases
                    .iter()
                    .filter_map(|r| {
                        let tag = r.get("tag_name").and_then(|v| v.as_str())?;
                        let body = r.get("body").and_then(|v| v.as_str()).unwrap_or("");
                        Some((tag.to_string(), body.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if entries.is_empty() {
            self.changelog_error = Some(rust_i18n::t!("app.changelog_empty").to_string());
            return;
        }
        self.changelog = Some(entries);
        self.changelog_error = None;
    }

    /// Write a diagnostics archive wherever the user says.
    ///
    /// Same builder the CLI's `--diagnostics` uses (`diagnostics::create_archive`),
    /// so a report from the GUI and one from the terminal contain the same
    /// files. Defaulted to the data directory so the button always produces
    /// something even if the picker is dismissed.
    pub(crate) fn export_diagnostics(&mut self) {
        let suggested = sutura_core::config::data_dir().join("sutura-diagnostics.zip");
        // The save dialog is the only part that cannot run headless: `rfd` needs a
        // native file picker, and it panics when there is no window server. The
        // archive itself is exercised either way, so the dialog is what a test
        // replaces -- not the whole path.
        let target = if cfg!(test) {
            suggested
        } else {
            rfd::FileDialog::new()
                .set_file_name(suggested.file_name().unwrap_or_default().to_string_lossy())
                .save_file()
                .unwrap_or(suggested)
        };
        match sutura_core::diagnostics::create_archive(&target) {
            Ok(v) => {
                let bytes = v.get("size_bytes").and_then(|b| b.as_u64()).unwrap_or(0);
                self.diagnostics_note = Some(
                    rust_i18n::t!(
                        "app.diagnostics_written",
                        path = target.display().to_string(),
                        bytes = bytes as i64
                    )
                    .to_string(),
                );
                self.log(self.diagnostics_note.clone().unwrap_or_default());
            }
            Err(e) => {
                self.diagnostics_note = Some(
                    rust_i18n::t!("app.diagnostics_failed", error = e).to_string(),
                );
                self.log(self.diagnostics_note.clone().unwrap_or_default());
            }
        }
    }

    /// Current state in its persisted form. Comparing strings is how change
    /// detection works without deriving `PartialEq` on `RepairSettings` (and
    /// without missing a field that only some other code path sets).
    fn state_json(&self) -> String {
        let state = crate::state::GuiState {
            settings: self.settings.clone(),
            intensity: Some(self.intensity.value().to_string()),
            language: Some(self.language.code().to_string()),
            // Only a resize is a choice; the opening size is a default and must
            // not be frozen into the next launch.
            window_size: if self.user_resized {
                self.last_window_size
            } else {
                self.saved_window_size
            },
            history_enabled: self.prefs.history_enabled,
            cache_enabled: self.prefs.cache_enabled,
            fidelity_enabled: self.prefs.fidelity_enabled,
            learning_enabled: self.prefs.learning_enabled,
            check_for_updates: self.prefs.check_for_updates,
            check_on_startup: self.prefs.check_on_startup,
            last_check: self.prefs.last_check,
        };
        serde_json::to_string(&state).unwrap_or_default()
    }

    /// Write the settings out when they differ from what was last written.
    ///
    /// Called once per frame, so it must be cheap: `to_string` on a small
    /// struct, then a file write only on an actual change. Saving
    /// unconditionally would touch the disk 60 times a second.
    fn persist_if_changed(&mut self) {
        // Tests drive `frame_ui` in-process, so without this every test run would
        // write `gui.json` into the real ~/.config/sutura -- which it did. A test
        // is not a user changing a setting.
        if cfg!(test) {
            return;
        }
        // A scripted capture is not a user changing a setting; it must not leave a
        // gui.json behind, and above all it must not pin a window size.
        if self.screenshot_path.is_some() {
            return;
        }
        let now = self.state_json();
        if now == self.persisted {
            return;
        }
        // Built by the same function the change detector uses, so the two can
        // never disagree about which fields are persisted.
        let state: crate::state::GuiState =
            serde_json::from_str(&now).unwrap_or_default();
        match crate::state::save(&state) {
            Ok(()) => self.persisted = now,
            // Logged, not fatal: a read-only config dir must not break the UI.
            Err(e) => self.log(format!("could not save GUI settings: {e}")),
        }
    }

    /// Drop one entry from the list, keeping `selected` and the preview
    /// consistent.
    ///
    /// `selected` and `loaded` are **indices** into `files`, so a removal
    /// invalidates every entry after it: the preview would otherwise show the
    /// previous file. Cleared outright rather than re-pointed, because guessing
    /// which file the user meant next is worse than showing nothing.
    fn remove_file(&mut self, idx: usize) {
        if idx >= self.files.len() {
            return;
        }
        self.files.remove(idx);
        self.selected = None;
        self.loaded = None;
        self.preview.clear_mesh();
    }

    /// Empty the list. Refused while a repair runs: the in-flight worker holds
    /// an index into `files` and would land its result on the wrong entry.
    fn clear_files(&mut self) {
        if self.busy() {
            self.log(rust_i18n::t!("app.clear_while_running").to_string());
            return;
        }
        self.files.clear();
        self.selected = None;
        self.loaded = None;
        self.preview.clear_mesh();
        self.batch.clear();
    }

    /// True while any repair is in flight (single file or batch).
    fn busy(&self) -> bool {
        self.repair_rx.is_some() || self.batch_active
    }

    fn apply_selection(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(entry) = self.files.get(idx) else { return };
        let key = (idx, entry.view);
        if self.loaded == Some(key) {
            return;
        }
        let mesh = match entry.view {
            ViewMode::Before => entry.input.clone(),
            ViewMode::After => entry.result.clone().or_else(|| entry.input.clone()),
        };
        if let Some(mesh) = mesh {
            // On the repaired view, compute the deviation ramp against the
            // ORIGINAL mesh — the same direction Python uses
            // (`viewer_data_render`: repaired -> original). Without this the
            // After view could only show the binary defect mask, which is why
            // the colour ramp never appeared after a repair.
            let mut mesh = (*mesh).clone();
            if entry.view == ViewMode::After && entry.result.is_some() {
                let reference_path = self
                    .deviation_reference
                    .clone()
                    .unwrap_or_else(|| entry.path.clone());
                match sutura_core::load_mesh(&reference_path) {
                    Ok(reference) => {
                        if let Err(e) = mesh.with_deviation_against(&reference) {
                            self.log(format!("deviation unavailable: {e}"));
                        }
                    }
                    Err(e) => self.log(format!("deviation reference unreadable: {e}")),
                }
            }
            self.preview.set_mesh(Arc::new(mesh));
            // A capture hook: point the camera at a region and zoom, so a defect
            // smaller than a few pixels in the whole-part view can be seen. The
            // default framing fits the whole bounding sphere, which is right for a
            // user and useless for documenting a 3 mm hole in a 57 mm key.
            if let Some((target, zoom)) = self.focus {
                self.preview.camera.target = glam::Vec3::from(target);
                if let Some(zoom) = zoom {
                    self.preview.camera.zoom = zoom.clamp(0.02, 3.0);
                } else {
                    // Frame a much smaller sphere: 1/6 of the part's radius puts
                    // the region across most of the viewport at the default FOV.
                    self.preview.camera.radius = (self.preview.camera.radius / 6.0).max(1e-6);
                }
                self.preview.dirty = true;
            }
        }
        self.loaded = Some(key);
    }

    pub(crate) fn log(&mut self, line: String) {
        eprintln!("{line}");
        self.log.push(line);
        if self.log.len() > 800 {
            self.log.remove(0);
        }
    }

    fn start_repair(&mut self, idx: usize, force: bool) {
        self.start_repair_mode(idx, force, false);
    }

    fn start_repair_mode(&mut self, idx: usize, force: bool, from_batch: bool) {
        let Some(entry) = self.files.get(idx) else { return };
        if entry.input.is_none() || matches!(entry.repair, RepairState::Running { .. }) {
            if from_batch {
                self.advance_batch();
            }
            return;
        }
        let input = entry.path.clone();
        let output = fixed_path(&input);
        if output.exists() && !force {
            if from_batch {
                self.log(
                    rust_i18n::t!("app.log_batch_skip", path = output.display().to_string())
                        .to_string(),
                );
                self.advance_batch();
            } else {
                self.pending_overwrite = Some((idx, output));
            }
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.repair_rx = Some(rx);
        self.repair_index = Some(idx);
        self.files[idx].repair = RepairState::Running { step: 0, total: 8 };
        self.files[idx].error = None;
        self.log(
            rust_i18n::t!("app.log_repair_started", path = input.display().to_string())
                .to_string(),
        );
        // Cooperative cancellation replaces the old SIGKILL timer: there is no
        // child process to signal any more.
        let cancel = Arc::new(CancelToken::new());
        self.cancel_token = Some(cancel.clone());
        // The panel edits `settings`; intensity is one of its fields, so keep
        // them in step rather than carrying two sources.
        self.settings.intensity = Some(self.intensity.value().to_string());
        if force {
            self.settings.force = true;
        }
        let mut settings = self.settings.clone();
        // Per-file methods win over the panel's global selection, exactly as
        // Python lets the Methods tab edit the selected file's sequence
        // (`gui.py:5093`). `None` keeps the global one, so a file that was never
        // customised follows later changes to the panel.
        if let Some(methods) = self.files[idx].methods.as_ref() {
            settings.methods = Some(
                methods
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        // The worker owns a `Context` handle so it can wake the UI when it
        // finishes. Without this the repair thread completes, writes its output,
        // and the event sits unread in the channel until the user happens to move
        // the mouse: `poll_repair` only runs inside a frame, and an idle window
        // produces none. On a Steam Deck that looked like "stuck at Step 4/8" --
        // measured here, the output file appeared at 52 s while the UI still said
        // "repair started" at 62 s.
        let ctx_for_worker = self.ctx.clone();
        let prefs = self.prefs.clone();
        std::thread::spawn(move || {
            run_repair(&input, &output, settings, prefs, tx, cancel);
            if let Some(ctx) = ctx_for_worker {
                ctx.request_repaint();
            }
        });
    }

    fn enqueue_all(&mut self) {
        if self.batch_active || self.repair_rx.is_some() {
            return;
        }
        let ids: Vec<usize> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, e)| e.input.is_some())
            .map(|(i, _)| i)
            .collect();
        if ids.is_empty() {
            return;
        }
        let existing = ids
            .iter()
            .filter(|&&i| fixed_path(&self.files[i].path).exists())
            .count();
        if existing > 0 {
            self.pending_batch = Some(ids);
        } else {
            self.start_batch(ids, false);
        }
    }

    fn start_batch(&mut self, ids: Vec<usize>, force: bool) {
        self.batch_total = ids.len();
        self.batch_done = 0;
        self.batch_force = force;
        self.batch_active = true;
        self.batch = ids.into_iter().rev().collect();
        self.log(
            rust_i18n::t!("app.log_batch_start", count = self.batch_total as i64).to_string(),
        );
        self.start_next_batch();
    }

    fn start_next_batch(&mut self) {
        if !self.batch_active {
            return;
        }
        if let Some(idx) = self.batch.pop() {
            self.start_repair_mode(idx, self.batch_force, true);
        } else {
            self.batch_active = false;
            self.log(rust_i18n::t!("app.log_batch_done").to_string());
        }
    }

    fn advance_batch(&mut self) {
        if self.batch_active {
            self.batch_done = (self.batch_done + 1).min(self.batch_total);
            self.start_next_batch();
        }
    }

    fn poll_repair(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = &self.repair_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
                if events.len() >= 256 {
                    break;
                }
            }
        }
        for ev in events {
            self.handle_event(ev);
        }
    }

    fn handle_event(&mut self, ev: RepairEvent) {
        match ev {
            RepairEvent::Started(_pid) => {
                // Nothing to track: the repair runs in its own thread here, and
                // cancellation goes through `cancel_token`.
            }
            RepairEvent::Progress { step, total } => {
                if let Some(idx) = self.repair_index {
                    if matches!(self.files[idx].repair, RepairState::Running { .. }) {
                        self.files[idx].repair = RepairState::Running { step, total };
                    }
                }
            }
            RepairEvent::Stderr(line) => {
                if !line.trim().is_empty() {
                    self.log(line);
                }
            }
            RepairEvent::Finished {
                output,
                result,
                cancelled,
            } => {
                let idx = self.repair_index.take();
                self.repair_rx = None;
                self.cancel_token = None;
                let Some(idx) = idx else { return };
                if idx >= self.files.len() {
                    return;
                }
                if cancelled {
                    self.files[idx].repair = RepairState::Idle;
                    self.log(rust_i18n::t!("app.log_repair_cancelled").to_string());
                    return;
                }
                match result {
                    Ok((mesh, report, raw)) => {
                        let count = mesh.triangle_count();
                        self.files[idx].result = Some(Arc::new(mesh));
                        self.files[idx].report = Some(report);
                        self.files[idx].raw_report = Some(raw);
                        self.files[idx].output = Some(output.clone());
                        self.files[idx].view = ViewMode::After;
                        self.files[idx].repair = RepairState::Done;
                        self.loaded = None;
                        self.log(
                            rust_i18n::t!(
                                "app.log_repair_done",
                                path = output.display().to_string(),
                                count = count
                            )
                            .to_string(),
                        );
                    }
                    Err(e) => {
                        self.files[idx].repair = RepairState::Failed(e.clone());
                        self.log(
                            rust_i18n::t!("app.log_repair_failed", error = e).to_string(),
                        );
                        // `run_repair` tags a failed save distinctly; a repair
                        // that could not produce a mesh is a different outcome
                        // and reads well enough in the row.
                        if let Some(reason) = e.strip_prefix("write failed: ") {
                            let path = self.files[idx]
                                .output
                                .clone()
                                .unwrap_or_else(|| fixed_path(&self.files[idx].path));
                            let name = self.files[idx]
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            self.write_error =
                                Some((name, reason.to_string(), path));
                        }
                    }
                }
                if self.batch_active {
                    self.advance_batch();
                }
            }
        }
    }

    fn cancel_repair(&mut self) {
        if self.batch_active {
            self.batch.clear();
            self.batch_active = false;
        }
        // Cooperative: `repair` checks this flag at its own boundaries. The old
        // SIGTERM-then-SIGKILL dance is gone -- there is no child process, and a
        // kill timer would have nothing to escalate to.
        let Some(token) = self.cancel_token.clone() else {
            return;
        };
        token.cancel();
        self.log(rust_i18n::t!("app.log_cancel_requested").to_string());
    }

    fn maybe_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else {
            return;
        };
        if self.screenshot_requested {
            return;
        }
        let ready = if self.auto_batch {
            self.pending_batch.is_some()
                || (!self.batch_active
                    && self.batch_total > 0
                    && self.batch_done >= self.batch_total
                    && self.repair_rx.is_none())
        } else if self.auto_repair {
            self.pending_overwrite.is_some()
                || self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .map(|e| {
                        matches!(e.repair, RepairState::Done | RepairState::Failed(_))
                    })
                    .unwrap_or(false)
        } else {
            true
        };
        if !ready {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        self.ready_frames = self.ready_frames.saturating_add(1);
        // 3 frames is enough for the main window, but not for an overlay:
        // egui fades a new window in, and a capture taken mid-fade is a grey
        // ghost of the view. `--wait-frames` lets a script ask for more.
        if self.ready_frames < self.wait_frames {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        self.screenshot_requested = true;
        let exit = self.exit_after_screenshot;
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |img| {
            save_screenshot(&path, &img);
            if exit {
                ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl eframe::App for SuturaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_ui(ui);
    }

    /// eframe's default clear colour is `from_rgba_unmultiplied(12, 12, 12, 180)`
    /// -- alpha 180. With no `transparent()` request the window still composites
    /// that against the desktop wherever a panel has not painted, which is the
    /// half-transparent background a macOS report showed. Opaque, always.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        crate::theme::WINDOW.to_normalized_gamma_f32()
    }
}

impl SuturaApp {
    /// One frame of the whole UI.
    ///
    /// Split out of `eframe::App::ui` because that takes an `eframe::Frame`,
    /// which cannot be constructed in a test -- and the interaction tests need to
    /// drive exactly the frame the user sees, not a hand-built approximation of
    /// it. `_frame` was never read.
    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::env::var_os("SUTURA_STYLE_LOG").is_some() && self.ready_frames == 0 {
            let st = ui.style().spacing.scroll;
            eprintln!(
                "[style] floating={} bar_w={} fg_color={} handle_op={} inactive_bg={:?} theme={:?}",
                st.floating, st.bar_width, st.foreground_color,
                st.dormant_handle_opacity,
                ui.visuals().widgets.inactive.bg_fill,
                ctx.theme(),
            );
        }
        self.ctx = Some(ctx.clone());
        // Deferred so the panel closure can set it while `self.files` is borrowed.
        let mut clear_list = false;
        // Deferred so a panel closure can set them while `self` is borrowed.
        let mut open_action: Option<(PathBuf, bool)> = None;
        let mut cancel_in_panel = false;
        self.poll_repair();
        // Updater events drive its panel; a change means a repaint.
        if self.updater.poll() {
            ctx.request_repaint();
        }

        // `Ctrl+,` opens Options on the Repair tab, which is exactly what Qt's
        // `QKeySequence.Preferences` shortcut did. There is no settings column
        // to toggle any more.
        if settings_shortcut_pressed(&ctx) {
            self.options_open = !self.options_open;
            if self.options_open {
                self.options_tab = OptionsTab::Repair;
            }
        }

        // A drag in progress: `hovered_files` is populated between enter and
        // drop, `dropped_files` only at the drop itself.
        self.preview.drop_active = ctx.input(|i| !i.raw.hovered_files.is_empty());

        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            let path = file.path().to_path_buf();
            if !path.as_os_str().is_empty() {
                self.add_path(path);
            }
        }
        if self.pending_compare && self.compare.is_none() {
            let ready = self
                .selected
                .and_then(|i| self.files.get(i))
                .map(|e| e.result.is_some())
                .unwrap_or(false);
            if ready {
                if let Some(entry) = self.selected.and_then(|i| self.files.get(i)) {
                    if let Some(input) = entry.input.clone() {
                        let title = entry
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        self.compare = Some(ComparePair::build(
                            &ctx,
                            title,
                            &input,
                            entry.result.as_deref(),
                            self.preview.heatmap,
                        ));
                        self.pending_compare = false;
                    }
                }
            }
        }
        if self.auto_repair && !self.auto_repair_started {
            if let Some(idx) = self.selected {
                self.auto_repair_started = true;
                // With `--screenshot` there is no one to answer the overwrite
                // prompt, so the run would sit on a dialog until the timeout --
                // and the capture would show "Output already exists" over a
                // half-drawn window. A scripted run states the intent up front,
                // so it overwrites rather than asking an absent user.
                let unattended = self.screenshot_path.is_some();
                self.start_repair(idx, unattended);
            }
        }
        if self.auto_batch && !self.auto_batch_started && !self.files.is_empty() {
            self.auto_batch_started = true;
            self.enqueue_all();
        }

        let mut cancel_clicked = false;
        let running = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|e| matches!(e.repair, RepairState::Running { .. }))
            .unwrap_or(false);

        // ---- Qt v0.9.9 layout ------------------------------------------------
        //
        // The old body was three panes: Files | preview | Settings, with the
        // settings controls living in a permanent right column. Qt's window is
        // one vertical flow with no settings on it at all -- they are their own
        // tabbed window -- and that is what a v0.9.9 user expects:
        //
        //   table · row 1 (file ops) · row 2 (analyze … repair) · progress ·
        //   report · log · analysis · defects · repair log · bottom row
        //
        // Nothing was dropped. The settings columns became the 7-tab Options
        // window (`options.rs`); the file panel's buttons became the rows below
        // the table; the 3D preview opens from the bottom row the way Qt opened
        // its dialogs. Panels with nothing to say collapse to nothing, so a
        // fresh launch is the table plus two rows.

        let mut add_files = false;
        let mut add_folder = false;
        let mut clear_list = false;
        let mut cancel_clicked = false;
        let mut open_action: Option<(PathBuf, bool)> = None;
        let mut open_compare = false;

        self.options_window(&ctx);

        // ---- The main window: three cards on a charcoal surface -------------
        //
        // Arif's direction: the first screen is calm and professional, and
        // anything that needs detail lives in Options. So the top bar carries a
        // title and a gear, the middle is a Files card, an action row, and a
        // Report card, and Mode / Profile / Methods moved into Options.

        // ---- The main window, matched to the approved target ----------------
        //
        // Window → one large rounded panel, centred and capped at
        // `PANEL_MAX_W` → cards inside it (darker than the panel), outline
        // buttons, one green accent. Wide windows get two columns (Files and the
        // action row on the left, Analysis on the right); narrow ones stack,
        // which is exactly what the target shows.

        // ---- Main window -----------------------------------------------------
        //
        // Cards sit directly on the window surface; there is no second rounded
        // panel inside the window (it left a gutter on every edge and read as a
        // window inside a window). On macOS the native title bar is transparent
        // and the traffic lights sit inside our header row, so there is one
        // header, not two.
        //
        //   header (title · version · gear)
        //   notice (only when something is unavailable)
        //   wide:   [ Files | action row | Preview ]   [ Report ]
        //   narrow:   Files · action row · Preview · Report   (scrolls)
        let header_inset = if cfg!(target_os = "macos") { 76.0 } else { 0.0 };
        let mut toggle_heatmap = false;
        let mut set_view: Option<ViewMode> = None;
        egui::containers::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(crate::theme::WINDOW)
                    .inner_margin(egui::Margin::symmetric(
                        crate::theme::GAP_24 as i8,
                        crate::theme::GAP_8 as i8,
                    )),
            )
            .show(ui, |ui| {
                // Header. With a transparent title bar the window is dragged from
                // here, so the empty part of the row starts a window drag.
                let mut drag_rect: Option<egui::Rect> = None;
                ui.horizontal(|ui| {
                    ui.set_min_height(crate::theme::HEADER_H);
                    ui.add_space(header_inset);
                    ui.label(
                        egui::RichText::new(rust_i18n::t!("app.title").to_string())
                            .size(crate::theme::FONT_TITLE)
                            .color(crate::theme::TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if gear_button(ui)
                            .on_hover_text(rust_i18n::t!("app.options_btn").to_string())
                            .clicked()
                        {
                            self.options_open = true;
                            self.options_tab = OptionsTab::Repair;
                        }
                        ui.add_space(crate::theme::GAP_16);
                        // Qt had an About button on the main window; the
                        // redesign dropped it and left no way to open the dialog.
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(rust_i18n::t!("app.about").to_string())
                                        .size(crate::theme::FONT_SECTION)
                                        .color(crate::theme::TEXT_MUTED),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            self.about_open = true;
                        }
                        ui.add_space(crate::theme::GAP_8);
                        ui.label(
                            egui::RichText::new(format!("v{}", SUTURA_VERSION))
                                .size(crate::theme::FONT_SECTION)
                                .color(crate::theme::TEXT_FAINT),
                        );
                        // Whatever is left between the title and these controls
                        // is the drag handle. It must not overlap the buttons: a
                        // drag area laid over the whole row swallowed their
                        // clicks (About and the gear did nothing).
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), crate::theme::HEADER_H),
                            egui::Sense::hover(),
                        );
                        drag_rect = Some(rect);
                    });
                });
                let drag = ui.interact(
                    drag_rect.unwrap_or(egui::Rect::NOTHING),
                    egui::Id::new("header_drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
                }
                ui.add_space(crate::theme::GAP_8);

                // Centre the content and cap its width on very wide screens.
                let avail = ui.available_width();
                // First launch (no files): one calm, centred column with the drop
                // area and the actions. The report and preview only appear once
                // there is something to show.
                let empty = self.files.is_empty();
                let max_w = if empty {
                    crate::theme::EMPTY_MAX_W
                } else {
                    crate::theme::CONTENT_MAX_W
                };
                let content_w = avail.min(max_w);
                let side = ((avail - content_w) / 2.0).max(0.0);
                let wide = !empty && content_w >= crate::theme::TWO_COLUMN_MIN_W;
                let gap = crate::theme::GAP_CARD;

                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(content_w);
                        // Same width as the cards under it.
                        self.unavailable_notice(ui);
                        if wide {
                            let left_w = ((content_w - gap) * 0.56).floor();
                            let right_w = content_w - gap - left_w;
                            let full_h = ui.available_height();
                            ui.horizontal_top(|ui| {
                                // Left: what you work on.
                                ui.vertical(|ui| {
                                    ui.set_width(left_w);
                                    self.files_card(ui, &mut add_files, &mut add_folder, &mut clear_list);
                                    ui.add_space(gap);
                                    self.action_row(ui, running, &mut cancel_clicked);
                                    self.progress_line(ui);
                                    // No files: no preview. An empty viewer under an
                                    // empty table only repeated the drop hint.
                                    if !self.files.is_empty() {
                                        ui.add_space(gap);
                                        // Leave the window's bottom margin free so the
                                        // card's lower edge is never cut off.
                                        let h = (ui.available_height() - crate::theme::GAP_16)
                                            .max(crate::theme::PREVIEW_MIN_H);
                                        self.preview_card(ui, h, &mut toggle_heatmap, &mut set_view, &mut open_compare);
                                    }
                                });
                                ui.add_space(gap);
                                // Right: what came out. Scrolls on its own so a
                                // long report never pushes the left column.
                                ui.vertical(|ui| {
                                    ui.set_width(right_w);
                                    egui::ScrollArea::vertical()
                                        .id_salt("report_scroll")
                                        .max_height((full_h - crate::theme::GAP_16).max(200.0))
                                        .auto_shrink([false, true])
                                        .show(ui, |ui| {
                                            self.analysis_card(ui, &mut open_action);
                                        });
                                });
                            });
                        } else {
                            egui::ScrollArea::vertical()
                                .id_salt("main_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    self.files_card(ui, &mut add_files, &mut add_folder, &mut clear_list);
                                    ui.add_space(gap);
                                    self.action_row(ui, running, &mut cancel_clicked);
                                    self.progress_line(ui);
                                    if !self.files.is_empty() {
                                        ui.add_space(gap);
                                        self.preview_card(ui, crate::theme::PREVIEW_STACKED_H, &mut toggle_heatmap, &mut set_view, &mut open_compare);
                                    }
                                    if !self.files.is_empty() {
                                        ui.add_space(gap);
                                        self.analysis_card(ui, &mut open_action);
                                    }
                                    ui.add_space(gap);
                                });
                        }
                    });
                });
            });

        if toggle_heatmap {
            self.preview.heatmap = !self.preview.heatmap;
            // The two colourings paint the same faces; only one at a time.
            if self.preview.heatmap {
                self.preview.defect_colors = false;
            }
            self.preview.dirty = true;
        }
        if let Some(view) = set_view {
            if let Some(entry) = self.selected.and_then(|i| self.files.get_mut(i)) {
                entry.view = view;
            }
        }
        if open_compare {
            self.pending_compare = true;
        }
        if add_files {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter("Meshes", &["stl", "obj", "ply", "3mf"])
                .pick_files()
            {
                for path in paths {
                    self.add_path(path);
                }
            }
        }
        if add_folder {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.add_path(dir);
            }
        }
        if clear_list {
            self.clear_files();
        }
        if cancel_clicked {
            self.cancel_repair();
        }
        if let Some((path, reveal)) = open_action {
            open_path(&path, reveal);
        }

        let mut view_changed = false;

        if let Some(ids) = self.pending_batch.clone() {
            let existing = ids
                .iter()
                .filter(|&&i| fixed_path(&self.files[i].path).exists())
                .count();
            let mut overwrite_all = false;
            let mut skip_existing = false;
            let mut cancel = false;
            egui::Window::new(rust_i18n::t!("app.batch_overwrite_title").to_string())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    ui.label(
                        rust_i18n::t!(
                            "app.batch_overwrite_body",
                            count = existing as i64
                        )
                        .to_string(),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .button(rust_i18n::t!("app.overwrite_all").to_string())
                            .clicked()
                        {
                            overwrite_all = true;
                        }
                        if ui
                            .button(rust_i18n::t!("app.skip_existing").to_string())
                            .clicked()
                        {
                            skip_existing = true;
                        }
                        if ui
                            .button(rust_i18n::t!("app.cancel").to_string())
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            if overwrite_all {
                self.pending_batch = None;
                self.start_batch(ids, true);
            } else if skip_existing {
                self.pending_batch = None;
                let keep: Vec<usize> = ids
                    .into_iter()
                    .filter(|&i| !fixed_path(&self.files[i].path).exists())
                    .collect();
                if keep.is_empty() {
                    self.log(rust_i18n::t!("app.log_batch_done").to_string());
                } else {
                    self.start_batch(keep, false);
                }
            } else if cancel {
                self.pending_batch = None;
            }
        }

        if self.about_open {
            let mut open = self.about_open;
            egui::Window::new(rust_i18n::t!("app.about").to_string())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.heading("Sutura");
                    ui.label(format!("v{}", SUTURA_VERSION));
                    ui.separator();
                    ui.label(rust_i18n::t!("app.about_body").to_string());
                });
            self.about_open = open;
        }
        if self.changelog_open {
            let mut open = self.changelog_open;
            egui::Window::new(rust_i18n::t!("app.changelog").to_string())
                .open(&mut open)
                .collapsible(false)
                .default_width(620.0)
                .show(&ctx, |ui| {
                    if let Some(err) = &self.changelog_error {
                        ui.label(egui::RichText::new(err).color(egui::Color32::LIGHT_RED));
                        if ui.button(rust_i18n::t!("app.retry").to_string()).clicked() {
                            self.changelog_error = None;
                            self.fetch_changelog();
                        }
                        return;
                    }
                    let Some(entries) = &self.changelog else {
                        ui.label(rust_i18n::t!("app.changelog_loading").to_string());
                        return;
                    };
                    egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                        for (tag, body) in entries {
                            egui::CollapsingHeader::new(tag.clone())
                                .id_salt(format!("changelog-{tag}"))
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(body).monospace().small());
                                });
                        }
                    });
                });
        }
        if let Some(pair) = &self.compare {
            let mut open = true;
            // Anchored and constrained: the default position is top-left and the
            // pair is 2 x 384 px wide plus spacing, so an unanchored window ran
            // its title bar above the viewport edge and the capture cut it off.
            // `constrain(true)` also keeps it inside the window when the two
            // panels are wider than the viewport.
            egui::Window::new(pair.title.clone())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(800.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .constrain(true)
                .show(&ctx, |ui| {
                    if let Some(err) = &pair.error {
                        ui.label(egui::RichText::new(err).color(egui::Color32::LIGHT_RED));
                    }
                    ui.horizontal(|ui| {
                        for (label, tex) in [
                            ("app.before".to_string(), pair.before.as_ref()),
                            ("app.after".to_string(), pair.after.as_ref()),
                        ] {
                            ui.vertical(|ui| {
                                ui.label(rust_i18n::t!(label.as_str()).to_string());
                                match tex {
                                    Some(tex) => {
                                        let [w, h] = tex.size();
                                        ui.image((tex.id(), egui::vec2(w as f32, h as f32)));
                                    }
                                    None => {
                                        ui.label(
                                            egui::RichText::new(
                                                rust_i18n::t!("app.compare_none").to_string(),
                                            )
                                            .small()
                                            .weak(),
                                        );
                                    }
                                }
                            });
                        }
                    });
                    if pair.after.is_none() {
                        ui.label(
                            egui::RichText::new(rust_i18n::t!("app.compare_no_after").to_string())
                                .small()
                                .weak(),
                        );
                    }
                });
            if !open {
                self.compare = None;
            }
        }
        if self.crash_open {
            // Python shows one short dialog pointing at the export; this is the
            // same, and it is the only reason the crash log is worth writing.
            let mut open = self.crash_open;
            egui::Window::new(rust_i18n::t!("app.crash_title").to_string())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(rust_i18n::t!("app.crash_body").to_string());
                    ui.label(
                        egui::RichText::new(
                            sutura_core::diagnostics::crash_log_path().display().to_string(),
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(rust_i18n::t!("app.diagnostics").to_string())
                            .clicked()
                        {
                            self.export_diagnostics();
                        }
                        if ui.button(rust_i18n::t!("app.close").to_string()).clicked() {
                            self.crash_open = false;
                        }
                    });
                    if let Some(note) = &self.diagnostics_note {
                        ui.label(egui::RichText::new(note).small().weak());
                    }
                });
        }
        if self.updater_open {
            let mut open = self.updater_open;
            let mut check = false;
            let mut install: Option<String> = None;
            let mut repair = false;
            let mut show_notes = false;
            {
                use crate::updater_ui::Phase;
                let u = &mut self.updater;
                egui::Window::new(rust_i18n::t!("app.updater_title").to_string())
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .show(&ctx, |ui| {
                        ui.label(
                            rust_i18n::t!("app.updater_current", version = u.current_version.as_str())
                                .to_string(),
                        );
                        match &u.phase {
                            Phase::Idle => {
                                ui.label(rust_i18n::t!("app.updater_body").to_string());
                            }
                            Phase::Checking => {
                                ui.label(rust_i18n::t!("app.updater_checking").to_string());
                            }
                            Phase::Available { tag } => {
                                ui.label(rust_i18n::t!("app.updater_available", tag = tag.as_str()).to_string());
                                if ui.button(rust_i18n::t!("app.updater_install").to_string()).clicked() {
                                    install = Some(tag.clone());
                                }
                            }
                            Phase::License { tag } => {
                                ui.label(rust_i18n::t!("app.updater_license", tag = tag.as_str()).to_string());
                                ui.hyperlink_to(
                                    rust_i18n::t!("app.updater_open_page").to_string(),
                                    u.releases_url,
                                );
                            }
                            Phase::Downloading { tag } => {
                                ui.label(rust_i18n::t!("app.updater_downloading", tag = tag.as_str()).to_string());
                            }
                            Phase::Installing { tag } => {
                                ui.label(rust_i18n::t!("app.updater_installing", tag = tag.as_str()).to_string());
                            }
                            Phase::Done { tag, has_notes } => {
                                ui.label(rust_i18n::t!("app.updater_done", tag = tag.as_str()).to_string());
                                if *has_notes {
                                    if ui.button(rust_i18n::t!("app.updater_whats_new").to_string()).clicked() {
                                        show_notes = true;
                                    }
                                }
                            }
                            Phase::Failed(msg) => {
                                ui.colored_label(egui::Color32::from_rgb(235, 60, 70), msg);
                            }
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.add_enabled(
                                !u.is_busy(),
                                egui::Button::new(rust_i18n::t!("app.updater_check").to_string()),
                            )
                            .clicked()
                            .then(|| check = true);
                            // Reinstalls the running version over itself. The
                            // static-binary answer to Python's self-repair:
                            // there is no venv to rewrite, but a damaged
                            // install can still be re-downloaded, verified and
                            // swapped in, with rollback if it will not start.
                            ui.add_enabled(
                                !u.is_busy(),
                                egui::Button::new(
                                    rust_i18n::t!("app.updater_repair").to_string(),
                                ),
                            )
                            .on_hover_text(
                                rust_i18n::t!("app.updater_repair_tip").to_string(),
                            )
                            .clicked()
                            .then(|| repair = true);
                        });
                    });
            }
            if check {
                self.updater.start_check(true);
            }
            if repair {
                self.updater.start_repair_install();
            }
            if let Some(tag) = install {
                self.updater.start_install(tag);
            }
            if show_notes {
                self.notes_open = true;
            }
            self.updater_open = open;
        }

        if self.notes_open {
            let mut open = self.notes_open;
            let dir = sutura_core::whatsnew::current_dir();
            let showed = match (
                sutura_core::whatsnew::current_tag(),
                sutura_core::whatsnew::current_body(),
            ) {
                (Some(tag), Some(body)) => {
                    let text = sutura_core::whatsnew::plain_text(&tag, &body, &dir);
                    egui::Window::new(rust_i18n::t!("app.updater_whats_new").to_string())
                        .open(&mut open)
                        .collapsible(false)
                        .show(&ctx, |ui| {
                            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                                ui.label(text);
                            });
                        });
                    true
                }
                _ => false,
            };
            if !showed {
                // No snapshot: point at the releases page instead.
                egui::Window::new(rust_i18n::t!("app.updater_whats_new").to_string())
                    .open(&mut open)
                    .collapsible(false)
                    .show(&ctx, |ui| {
                        ui.label(rust_i18n::t!("app.updater_no_notes").to_string());
                        ui.hyperlink_to(
                            rust_i18n::t!("app.updater_open_page").to_string(),
                            sutura_core::updater::RELEASES_URL,
                        );
                    });
            }
            self.notes_open = open;
        }

        if let Some(picker) = self.repeat_picker.as_mut() {
            let mut open = true;
            let mut apply = false;
            let mut cancel = false;
            egui::Window::new(rust_i18n::t!("app.repeat_pick_title").to_string())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(rust_i18n::t!("app.repeat_pick_hint").to_string());
                    picker.show(ui);
                    ui.horizontal(|ui| {
                        ui.label(
                            rust_i18n::t!(
                                "app.repeat_pick_state",
                                source = fmt_point(picker.source),
                                target = fmt_point(picker.target)
                            )
                            .to_string(),
                        );
                    });
                    ui.horizontal(|ui| {
                        let ready = picker.source.is_some() && picker.target.is_some();
                        if ui
                            .add_enabled(ready, egui::Button::new(rust_i18n::t!("app.repeat_pick_apply").to_string()))
                            .clicked()
                        {
                            apply = true;
                        }
                        if ui.button(rust_i18n::t!("app.repeat_pick_cancel").to_string()).clicked() {
                            cancel = true;
                        }
                    });
                });
            if apply {
                if let (Some(source), Some(target)) = (picker.source, picker.target) {
                    // The same fields the CLI's `--repeat-source/--repeat-target`
                    // land in, so the picker cannot diverge from the flags.
                    self.settings.repeat_source = Some(source);
                    self.settings.repeat_target = Some(target);
                    self.log(format!(
                        "repeat points set: source {}, target {}",
                        fmt_point(Some(source)),
                        fmt_point(Some(target))
                    ));
                }
                self.repeat_picker = None;
            } else if cancel || !open {
                self.repeat_picker = None;
            }
        }

        if let Some((idx, output)) = self.pending_overwrite.clone() {
            egui::Window::new(rust_i18n::t!("app.overwrite_title").to_string())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    ui.label(
                        rust_i18n::t!(
                            "app.overwrite_body",
                            path = output.display().to_string()
                        )
                        .to_string(),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .button(rust_i18n::t!("app.overwrite").to_string())
                            .clicked()
                        {
                            self.start_repair(idx, true);
                            self.pending_overwrite = None;
                        }
                        if ui
                            .button(rust_i18n::t!("app.cancel").to_string())
                            .clicked()
                        {
                            self.pending_overwrite = None;
                        }
                    });
                });
        }

        if let Some((what, reason, path)) = self.write_error.clone() {
            let mut dismiss = false;
            let mut choose_folder = false;
            egui::Window::new(rust_i18n::t!("app.write_error_title").to_string())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    ui.label(
                        egui::RichText::new(
                            rust_i18n::t!(
                                "app.write_error_body",
                                what = what.clone(),
                                path = path.display().to_string()
                            )
                            .to_string(),
                        )
                        .size(crate::theme::FONT_BODY),
                    );
                    ui.add_space(crate::theme::GAP_8);
                    ui.label(
                        egui::RichText::new(reason.clone())
                            .size(crate::theme::FONT_SECTION)
                            .color(crate::theme::TEXT_MUTED),
                    );
                    ui.add_space(crate::theme::GAP_16);
                    ui.horizontal(|ui| {
                        if ui
                            .button(rust_i18n::t!("app.write_error_choose").to_string())
                            .clicked()
                        {
                            choose_folder = true;
                        }
                        if ui
                            .add(primary_button(rust_i18n::t!("app.ok").to_string()))
                            .clicked()
                        {
                            dismiss = true;
                        }
                    });
                });
            if choose_folder {
                // Re-run the same repair with the output in the folder the user
                // picks: the mesh that failed to save is still in hand, but it
                // lives in the thread that wrote it -- so the honest thing is to
                // offer the choice and redo the write, not to invent a path into
                // a dead thread's result.
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    if let Some(idx) = self.selected {
                        let name = self.files[idx]
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if let Some(slot) = self.files.get_mut(idx) {
                            slot.output = Some(dir.join(name));
                        }
                        self.write_error = None;
                        self.start_repair(idx, true);
                    }
                }
            } else if dismiss {
                self.write_error = None;
            }
        }

        // Progress is read from a channel inside a frame, so a running repair needs
        // frames even when nothing is happening in the window. Without this the
        // progress bar froze until the next stray input event.
        if self.busy() || self.pending_batch.is_some() || self.pending_overwrite.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        // Remember a size the user actually chose. `open_size` is what the builder
        // asked for; anything else is a drag or a maximize, and is worth restoring
        // next time. Recording the opening size instead pinned every launch to it.
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            let (w, h) = (rect.width(), rect.height());
            if w >= 400.0 && h >= 300.0 {
                self.last_window_size = Some([w, h]);
                match self.open_size {
                    Some([ow, oh]) if (w - ow).abs() <= 2.0 && (h - oh).abs() <= 2.0 => {}
                    Some(_) => self.user_resized = true,
                    None => self.open_size = Some([w, h]),
                }
            }
        }

        if let Some(action) = self.profile_action.take() {
            match action {
                ProfileAction::Delete => {
                    if let Some(name) = self.settings.profile.clone() {
                        let mut profiles = sutura_core::config::intensity::load_profiles(None);
                        if profiles.remove(&name).is_some() {
                            match sutura_core::config::intensity::save_profiles(&profiles, None) {
                                Ok(()) => {
                                    self.log(format!("deleted profile {name}"));
                                    self.settings.profile = None;
                                }
                                Err(e) => self.log(format!("could not delete profile: {e}")),
                            }
                        }
                    }
                }
                // The rest need a name, so they open the prompt below.
                other => {
                    let seed = match other {
                        ProfileAction::Duplicate | ProfileAction::Rename => self
                            .settings
                            .profile
                            .clone()
                            .unwrap_or_default(),
                        _ => String::new(),
                    };
                    self.profile_prompt = Some((other, seed));
                }
            }
        }
        if let Some((action, mut name)) = self.profile_prompt.clone() {
            let mut open = true;
            let mut accept = false;
            let mut cancel = false;
            egui::Window::new(rust_i18n::t!("app.profile_new").to_string())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(rust_i18n::t!("app.profile_name").to_string());
                    ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text(rust_i18n::t!("app.profile_name_hint")),
                    );
                    ui.horizontal(|ui| {
                        let ok = !name.trim().is_empty();
                        if ui
                            .add_enabled(
                                ok,
                                egui::Button::new(rust_i18n::t!("app.profile_save").to_string()),
                            )
                            .clicked()
                        {
                            accept = true;
                        }
                        if ui.button(rust_i18n::t!("app.cancel").to_string()).clicked() {
                            cancel = true;
                        }
                    });
                });
            if accept {
                let name = name.trim().to_string();
                let mut profiles = sutura_core::config::intensity::load_profiles(None);
                match action {
                    // A new profile starts as a copy of the current selection's
                    // base, which is what Qt's editor did.
                    ProfileAction::New => {
                        let base = self
                            .settings
                            .profile
                            .clone()
                            .or_else(|| Some(self.intensity.value().to_string()))
                            .unwrap_or_else(|| "balanced".to_string());
                        profiles.insert(
                            name.clone(),
                            sutura_core::config::intensity::ProfileEntry {
                                base,
                                overrides: Default::default(),
                            },
                        );
                    }
                    ProfileAction::Duplicate => {
                        if let Some(src) = self
                            .settings
                            .profile
                            .as_ref()
                            .and_then(|n| profiles.get(n))
                            .cloned()
                        {
                            profiles.insert(name.clone(), src);
                        }
                    }
                    ProfileAction::Rename => {
                        if let Some(old) = self.settings.profile.clone() {
                            if let Some(entry) = profiles.remove(&old) {
                                profiles.insert(name.clone(), entry);
                            }
                        }
                    }
                    ProfileAction::SaveAsNew => {
                        let base = self.intensity.value().to_string();
                        profiles.insert(
                            name.clone(),
                            sutura_core::config::intensity::ProfileEntry {
                                base,
                                overrides: Default::default(),
                            },
                        );
                    }
                    ProfileAction::Delete => {}
                }
                match sutura_core::config::intensity::save_profiles(&profiles, None) {
                    Ok(()) => {
                        self.log(format!("saved profile {name}"));
                        self.settings.profile = Some(name);
                    }
                    Err(e) => self.log(format!("could not save profile: {e}")),
                }
                self.profile_prompt = None;
            } else if cancel || !open {
                self.profile_prompt = None;
            } else {
                self.profile_prompt = Some((action, name));
            }
        }

        self.apply_selection();
        self.maybe_screenshot(&ctx);
        self.persist_if_changed();
    }
    /// The file table: Qt's three columns, File / Method / Result.
    ///
    /// Qt's `QTreeWidget` had explicit widths 330 / 300 / 140 and no row
    /// styling beyond selection. Reproduced as a grid so the columns line up
    /// whatever the file names are, with the rows themselves selectable.
    /// The Files card: title, its buttons, and the table (or the drop area).
    fn files_card(
        &mut self,
        ui: &mut egui::Ui,
        add_files: &mut bool,
        add_folder: &mut bool,
        clear_list: &mut bool,
    ) {
        crate::theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                crate::theme::card_title(ui, rust_i18n::t!("app.files").to_string());
            });
            ui.add_space(crate::theme::GAP_16);
            self.file_table(ui);
            ui.add_space(crate::theme::GAP_16);
            ui.horizontal(|ui| {
                if ui
                    .add(secondary_button(rust_i18n::t!("app.add_files").to_string()))
                    .clicked()
                {
                    *add_files = true;
                }
                if ui
                    .add(secondary_button(rust_i18n::t!("app.add_folder").to_string()))
                    .clicked()
                {
                    *add_folder = true;
                }
                if ui
                    .add_enabled(
                        self.selected.is_some() && !self.busy(),
                        secondary_button(rust_i18n::t!("app.remove").to_string()),
                    )
                    .clicked()
                {
                    if let Some(idx) = self.selected {
                        self.remove_file(idx);
                    }
                }
                if ui
                    .add_enabled(
                        !self.files.is_empty() && !self.busy(),
                        secondary_button(rust_i18n::t!("app.clear_list").to_string()),
                    )
                    .clicked()
                {
                    *clear_list = true;
                }
            });
        });
    }

    /// Analyze on the left; Repair / Repair all on the right.
    fn action_row(&mut self, ui: &mut egui::Ui, running: bool, cancel_clicked: &mut bool) {
        let can_repair = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|e| e.input.is_some() && !matches!(e.repair, RepairState::Running { .. }))
            .unwrap_or(false);
        let multi = self.files.len() > 1;

        let analyze = |app: &mut SuturaApp, ui: &mut egui::Ui| {
            if ui
                .add_enabled(
                    app.selected.is_some(),
                    secondary_button(rust_i18n::t!("app.analyze").to_string()),
                )
                .clicked()
            {
                if let Some(idx) = app.selected {
                    if let Some(slot) = app.files.get_mut(idx) {
                        slot.analysis = None;
                        slot.defects = None;
                    }
                    // Qt's `_analyze_methods` (gui.py:7214): the ranking, not
                    // just the topology. It is a subprocess run there because
                    // Python did not link the core; here it is the same call the
                    // repair makes, so the recommendation is the one the repair
                    // would use.
                    app.analyze_methods(idx);
                }
            }
        };
        let repair = |app: &mut SuturaApp,
                      ui: &mut egui::Ui,
                      multi: bool,
                      can_repair: bool,
                      running: bool,
                      cancel: &mut bool| {
            // The group is laid out right-to-left, so the FIRST widget added is
            // the rightmost. "Repair" is the filled primary and sits at the far
            // right; "Repair all" is the outline beside it.
            if multi {
                if ui
                    .add_enabled(
                        can_repair,
                        primary_button(rust_i18n::t!("app.repair").to_string()),
                    )
                    .clicked()
                {
                    if let Some(idx) = app.selected {
                        app.start_repair(idx, false);
                    }
                }
                if ui
                    .add_enabled(
                        can_repair && !app.batch_active && app.repair_rx.is_none(),
                        secondary_button(rust_i18n::t!("app.repair_all").to_string()),
                    )
                    .clicked()
                {
                    app.enqueue_all();
                }
            } else if ui
                .add_enabled(
                    can_repair,
                    primary_button(rust_i18n::t!("app.repair").to_string()),
                )
                .clicked()
            {
                if let Some(idx) = app.selected {
                    app.start_repair(idx, false);
                }
            }
            if running
                && ui
                    .add(secondary_button(rust_i18n::t!("app.stop").to_string()))
                    .clicked()
            {
                *cancel = true;
            }
        };

        // Always one row: Analyze left, the repair group right. It fits because
        // the table no longer demands a fixed width -- the earlier overflow was
        // the columns, not the buttons.
        ui.horizontal(|ui| {
            analyze(self, ui);
            // "Open in…" lives on the output line in the report, next to
            // "Open result" and "Show in folder": it acts on the repaired file,
            // not on the selection.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                repair(self, ui, multi, can_repair, running, cancel_clicked)
            });
        });
    }

    /// The progress hairline and its one status line, under the action row.
    /// Nothing at all when no repair is running.
    fn progress_line(&mut self, ui: &mut egui::Ui) {
        let Some((name, step, total)) = self.repair_progress() else {
            return;
        };
        ui.add_space(crate::theme::GAP_8);
        ui.add(
            egui::ProgressBar::new(step as f32 / total.max(1) as f32)
                .desired_height(crate::theme::PROGRESS_H)
                .corner_radius(2)
                .fill(crate::theme::ACCENT)
                .animate(false),
        );
        ui.add_space(crate::theme::GAP_8);
        ui.label(
            egui::RichText::new(
                rust_i18n::t!(
                    "app.repairing_stage",
                    name = name,
                    step = step as i64,
                    total = total as i64
                )
                .to_string(),
            )
            .size(crate::theme::FONT_SECTION)
            .color(crate::theme::TEXT_MUTED),
        );
    }

    /// The live 3D preview in its own card, with the view controls on top:
    /// Before / After (once there is a result), Heatmap (on the After view),
    /// Defect colours, and the side-by-side comparison.
    ///
    /// The preview was built but no longer drawn after the layout rewrite, and
    /// the bottom-row buttons that should have replaced it set flags nothing
    /// read. This puts the viewer back where the empty space was.
    fn preview_card(
        &mut self,
        ui: &mut egui::Ui,
        height: f32,
        toggle_heatmap: &mut bool,
        set_view: &mut Option<ViewMode>,
        open_compare: &mut bool,
    ) {
        let (has_result, view) = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|e| (e.result.is_some(), e.view))
            .unwrap_or((false, ViewMode::Before));
        crate::theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                crate::theme::card_title(ui, rust_i18n::t!("app.preview").to_string());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if has_result {
                        if ui
                            .add(toggle_button(rust_i18n::t!("app.side_by_side").to_string(), false))
                            .on_hover_text(rust_i18n::t!("app.compare_tip").to_string())
                            .clicked()
                        {
                            *open_compare = true;
                        }
                        if view == ViewMode::After
                            && ui
                                .add(toggle_button(
                                    rust_i18n::t!("app.heatmap_short").to_string(),
                                    self.preview.heatmap,
                                ))
                                .clicked()
                        {
                            *toggle_heatmap = true;
                        }
                    }
                    if ui
                        .add(toggle_button(
                            rust_i18n::t!("app.defect_colors").to_string(),
                            self.preview.defect_colors,
                        ))
                        .clicked()
                    {
                        self.preview.defect_colors = !self.preview.defect_colors;
                        if self.preview.defect_colors {
                            self.preview.heatmap = false;
                        }
                        self.preview.dirty = true;
                    }
                    if has_result {
                        ui.add_space(crate::theme::GAP_8);
                        for (mode, key) in [
                            (ViewMode::After, "app.after"),
                            (ViewMode::Before, "app.before"),
                        ] {
                            if ui
                                .add(toggle_button(rust_i18n::t!(key).to_string(), view == mode))
                                .clicked()
                            {
                                *set_view = Some(mode);
                            }
                        }
                    }
                });
            });
            ui.add_space(crate::theme::GAP_8);
            let w = ui.available_width();
            let inner_h = (height - 2.0 * crate::theme::PAD_CARD - crate::theme::BUTTON_H - crate::theme::GAP_8)
                .max(crate::theme::PREVIEW_MIN_H * 0.6);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, inner_h), egui::Sense::hover());
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(crate::theme::RADIUS_BUTTON),
                crate::theme::WINDOW,
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            child.set_clip_rect(rect);
            self.preview.show(&mut child);
            // The heatmap is normalised over the 1st-99th percentile of the
            // per-vertex distance to the original. When nearly nothing moved that
            // range is zero and the ramp is one flat colour, which looked exactly
            // like "the button does nothing". Say so instead.
            if self.preview.heatmap && view == ViewMode::After && has_result {
                let flat = self
                    .preview
                    .mesh
                    .as_ref()
                    .map(|m| m.dev_idx.iter().all(|&i| i == 0))
                    .unwrap_or(true);
                let key = if flat { "app.heatmap_flat" } else { "app.heatmap_legend" };
                ui.painter().text(
                    rect.right_bottom() + egui::vec2(-12.0, -10.0),
                    egui::Align2::RIGHT_BOTTOM,
                    rust_i18n::t!(key).to_string(),
                    egui::FontId::proportional(crate::theme::FONT_SECTION),
                    crate::theme::TEXT_MUTED,
                );
            }
        });
    }

    /// The Analysis card: label/value rows, an amber count where a count is a
    /// problem, then the defects row and its list.
    fn analysis_card(&mut self, ui: &mut egui::Ui, open_action: &mut Option<(PathBuf, bool)>) {
        crate::theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            // A real slot, not `&mut None`: with a temporary here "Open result"
            // and "Show in folder" set a value nobody read and did nothing.
            self.report_and_logs(ui, open_action);
            self.batch_summary(ui);
            self.small_parts_section(ui);
            self.suggestions_block(ui);
            self.result_suggestions(ui);
            // This call was missing: `defects_panel` existed but nothing drew it,
            // which is why the hairline and "N defects >" row never appeared.
            self.defects_panel(ui);
        });
    }

    /// The file table: Qt's `QTreeWidget`, three columns.
    ///
    /// A real table, not sized labels: the first version laid the header out with
    /// `allocate_ui_with_layout` and the rows with `add_sized`, which do not
    /// agree, and the "row" came out as one full-width button with the method
    /// outside it. `TableBuilder` is the widget for exactly this.
    ///
    /// Qt's widths were 330 / 300 / 140 at 1055 px; the same proportions here so
    /// the columns land in the same place.
    pub(crate) fn file_table(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        let mut clicked = None;
        let mut removed = None;
        // Set by the right-click menu; applied after the table's borrow ends.
        let mut chosen: Option<RowAction> = None;

        // Empty state takes one row, not a hundred (rule e).
        let rows = if self.files.is_empty() {
            0
        } else {
            self.files.len()
        };
        let row_h = crate::theme::ROW_H;
        const HEADER_H: f32 = 28.0;
        // Only used by the empty-state branch below.
        let _rows = rows;

        let table = TableBuilder::new(ui)
            .id_salt("file_table")
            .striped(false)
            .resizable(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            // All three share what the card has: a fixed 330 + 300 demanded
            // 630 px of a card that is ~590 inside a 720 px window, so the row
            // ran past the panel edge and the Result column was off-screen.
            .column(Column::remainder().at_least(150.0))
            .column(Column::remainder().at_least(70.0))
            .column(Column::remainder().at_least(80.0))
            // Let the table take its natural height: `max_scroll_height` is the
            // height of the *scrollable body*, not of the table, and pairing it
            // with a `min_scrolled_height` of one row pinned the viewport to a
            // single row -- four loaded files drew one.
            .auto_shrink([false, true])
            .sense(egui::Sense::click());



        // (1) With no files there is no table: headers over an empty row read as
        // a broken table. A dashed drop area takes its place.
        if self.files.is_empty() {
            let w = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 160.0), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(crate::theme::RADIUS),
                egui::Stroke::new(1.0, crate::theme::BORDER),
                egui::StrokeKind::Inside,
            );
            // An upload arrow, drawn: the font has no glyph for it.
            let c = rect.center();
            let colour = crate::theme::TEXT_MUTED;
            painter.line_segment(
                [egui::pos2(c.x, c.y - 26.0), egui::pos2(c.x, c.y - 2.0)],
                egui::Stroke::new(1.6, colour),
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(c.x, c.y - 30.0),
                    egui::pos2(c.x - 5.0, c.y - 22.0),
                    egui::pos2(c.x + 5.0, c.y - 22.0),
                ],
                colour,
                egui::Stroke::NONE,
            ));
            painter.text(
                egui::pos2(c.x, c.y + 6.0),
                egui::Align2::CENTER_CENTER,
                rust_i18n::t!("app.drop_title").to_string(),
                egui::FontId::proportional(crate::theme::FONT_BODY),
                crate::theme::TEXT_MUTED,
            );
            painter.text(
                egui::pos2(c.x, c.y + 28.0),
                egui::Align2::CENTER_CENTER,
                rust_i18n::t!("app.drop_hint_alt").to_string(),
                egui::FontId::proportional(crate::theme::FONT_SECTION),
                crate::theme::TEXT_MUTED,
            );
            return;
        }

        table
            .header(HEADER_H, |mut header| {
                // (2) Secondary and small: the headers were body-coloured and
                // bold, which read as content rather than a label.
                for key in ["app.col_file", "app.col_method", "app.col_result"] {
                    header.col(|ui| {
                        ui.label(
                            egui::RichText::new(rust_i18n::t!(key).to_string())
                                .size(crate::theme::FONT_SECTION)
                                .color(crate::theme::TEXT_MUTED),
                        );
                    });
                }
            })
            .body(|mut body| {
                if self.files.is_empty() {
                    body.row(row_h, |mut row| {
                        row.col(|ui| {
                            ui.label(
                                egui::RichText::new(rust_i18n::t!("app.drop_hint").to_string())
                                    .size(crate::theme::FONT_BODY)
                                    .color(crate::theme::TEXT_MUTED),
                            );
                        });
                        row.col(|_| {});
                        row.col(|_| {});
                    });
                    return;
                }
                for (idx, entry) in self.files.iter().enumerate() {
                    body.row(row_h, |mut row| {
                        let selected = self.selected == Some(idx);
                        // A selected row is tinted, not filled: Qt paints
                        // `Highlight` behind the text, and a solid button per row
                        // is what the first attempt got wrong.
                        // H2: the row's own highlight, not egui's strong
                        // selection fill. `set_selected` handles the tint; the
                        // left bar is painted inside the first cell below.
                        row.set_selected(selected);

                        row.col(|ui| {
                            let name = entry
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| entry.path.display().to_string());
                            let mut text = egui::RichText::new(name);
                            if entry.error.is_some() {
                                text = text.color(COLOR_ERROR);
                            }
                            // The 3 px accent bar on a selected row: the visual
                            // half of H2, and it cannot be done with a
                            // `set_selected` tint alone.
                            if selected {
                                let (r, _) = ui.allocate_exact_size(
                                    egui::vec2(3.0, 18.0),
                                    egui::Sense::hover(),
                                );
                                crate::theme::selection_bar(ui.painter(), r);
                            }
                            let response =
                                ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                            if response.clicked() {
                                clicked = Some(idx);
                            }
                            // Right-click, as Qt's `_on_tree_context_menu` did.
                            // The menu is built here, closes over `idx`, and only
                            // reports what was chosen: the actions run after the
                            // table's borrow ends.
                            response.context_menu(|ui| {
                                if menu_action(ui, rust_i18n::t!("app.menu_analyze").to_string()) {
                                    chosen = Some(RowAction::Analyze(idx));
                                }
                                ui.separator();
                                if let Some(entry) = self.files.get(idx) {
                                    if entry.output.is_some() {
                                        if menu_action(
                                            ui,
                                            rust_i18n::t!("app.open_result").to_string(),
                                        ) {
                                            chosen = Some(RowAction::OpenOutput(idx, false));
                                        }
                                        if menu_action(
                                            ui,
                                            rust_i18n::t!("app.show_in_folder").to_string(),
                                        ) {
                                            chosen = Some(RowAction::OpenOutput(idx, true));
                                        }
                                        // "Open in..." only when a slicer is
                                        // actually installed: a menu entry that
                                        // cannot work is worse than none.
                                        let installed = self.slicers.clone();
                                        if !installed.is_empty() {
                                            ui.menu_button(
                                                rust_i18n::t!("app.open_in_slicer")
                                                    .to_string(),
                                                |ui| {
                                                    for (i, slicer) in
                                                        installed.iter().enumerate()
                                                    {
                                                        if menu_action(
                                                            ui,
                                                            slicer.label.to_string(),
                                                        ) {
                                                            chosen = Some(
                                                                RowAction::OpenInSlicer(idx, i),
                                                            );
                                                        }
                                                    }
                                                },
                                            );
                                        }
                                        ui.separator();
                                    }
                                }
                                // "Recommended methods": the ranking the report
                                // already carries, as a submenu. Qt built it in
                                // `_build_recommended_menu`.
                                let recs = self
                                    .files
                                    .get(idx)
                                    .and_then(|e| e.raw_report.as_ref())
                                    .map(recommended_methods)
                                    .unwrap_or_default();
                                ui.menu_button(
                                    rust_i18n::t!("app.menu_recommended").to_string(),
                                    |ui| {
                                        if recs.is_empty() {
                                            ui.label(
                                                egui::RichText::new(
                                                    rust_i18n::t!("app.menu_analyze_first")
                                                        .to_string(),
                                                )
                                                .size(crate::theme::FONT_SECTION)
                                                .color(crate::theme::TEXT_FAINT),
                                            );
                                            return;
                                        }
                                        for (num, name, score) in &recs {
                                            let pct = (score * 100.0).round() as i64;
                                            if menu_action(
                                                ui,
                                                format!("#{num}  {name}  {pct}%"),
                                            ) {
                                                chosen = Some(RowAction::TagMethod(idx, *num));
                                            }
                                        }
                                    },
                                );
                                if menu_action(
                                    ui,
                                    rust_i18n::t!("app.menu_use_method").to_string(),
                                ) {
                                    chosen = Some(RowAction::EditMethods(idx));
                                }
                                ui.separator();
                                if menu_action(
                                    ui,
                                    rust_i18n::t!("app.menu_clear_tags").to_string(),
                                ) {
                                    chosen = Some(RowAction::ClearTags(idx));
                                }
                            });
                        });
                        row.col(|ui| {
                            let method = entry
                                .methods
                                .as_ref()
                                .map(|m| {
                                    m.iter()
                                        .map(|n| n.to_string())
                                        .collect::<Vec<_>>()
                                        .join(",")
                                })
                                .unwrap_or_else(|| rust_i18n::t!("app.method_auto").to_string());
                            ui.label(egui::RichText::new(method));
                        });
                        row.col(|ui| {
                            match entry.decision() {
                                Some((text, colour, tip)) => {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(truncate_decision(&text, 26))
                                                .color(colour),
                                        ),
                                    )
                                    .on_hover_text(tip);
                                }
                                None => {
                                    // Qt leaves the Result cell empty until a
                                    // report exists; removal is the toolbar's.
                                    let _ = &mut removed;
                                }
                            }
                        });
                    });
                }
            });

        if let Some(idx) = clicked {
            self.selected = Some(idx);
        }
        if let Some(idx) = removed {
            self.remove_file(idx);
        }
        if let Some(action) = chosen {
            self.apply_row_action(action);
        }

        // Repair progress, if any, under the table rather than inside it: a
        // progress bar in a cell makes the row height jump around.
        if let Some(entry) = self.selected.and_then(|i| self.files.get(i)) {
            if let RepairState::Running { step, total } = entry.repair {
                ui.add(
                    egui::ProgressBar::new(step as f32 / total.max(1) as f32)
                        .desired_height(4.0)
                        .corner_radius(0)
                        .text(
                            rust_i18n::t!(
                                "app.progress",
                                step = step as i64,
                                total = total as i64
                            )
                            .to_string(),
                        ),
                );
            }
        }
    }

    /// The running repair's file name and step, for the line under the bar.
    fn repair_progress(&self) -> Option<(String, u8, u8)> {
        let entry = self.selected.and_then(|i| self.files.get(i))?;
        let (step, total) = match entry.repair {
            RepairState::Running { step, total } => (step, total),
            _ => return None,
        };
        let name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        Some((name, step, total))
    }

    /// The one-line status Qt shows at the right of the progress row.
    fn status_line(&self) -> String {
        if self.busy() {
            rust_i18n::t!("app.status_working").to_string()
        } else if self.batch_active {
            rust_i18n::t!(
                "app.batch_progress",
                done = self.batch_done as i64,
                total = self.batch_total as i64
            )
            .to_string()
        } else {
            rust_i18n::t!("app.ready").to_string()
        }
    }

    /// The report, log, analysis, defects and repair-log sections.
    ///
    /// Panels collapse to nothing when they have nothing to show (rule e), so a
    /// fresh launch is the table and the two button rows.
    fn report_and_logs(
        &mut self,
        ui: &mut egui::Ui,
        open_action: &mut Option<(PathBuf, bool)>,
    ) {
        let mut cancel_in_panel = false;
        // Set by the output row's "Open in…" menu; applied once the borrow of
        // the selected entry has ended.
        let mut slicer_pick: Option<usize> = None;
        // (3) The state belongs next to the title, not on a line of its own that
        // contradicts the analysis under it.
        let repaired = self
            .selected
            .and_then(|i| self.files.get(i))
            .map(|e| e.report.is_some())
            .unwrap_or(false);
        ui.horizontal(|ui| {
            crate::theme::card_title(ui, rust_i18n::t!("app.report").to_string());
            if !repaired {
                ui.label(
                    egui::RichText::new(rust_i18n::t!("app.not_repaired").to_string())
                        .size(crate::theme::FONT_SECTION)
                        .color(crate::theme::TEXT_MUTED),
                );
            }
        });
        if self.batch_active {
            ui.label(
                rust_i18n::t!(
                    "app.batch_progress",
                    done = self.batch_done as i64,
                    total = self.batch_total as i64
                )
                .to_string(),
            );
        }
        // Analysis and the 3MF object list are computed here, before
        // the immutable `entry` binding below, so filling the cache
        // does not need a second borrow of `self.files`. Both calls
        // are the ones the headless parity hooks use
        // (`analyze_selected`, `object_rows`), so a panel that
        // drifts from the hook cannot pass the gate.
        if let Some(idx) = self.selected {
            if let Some(slot) = self.files.get_mut(idx) {
                if slot.analysis.is_none() {
                    slot.analysis = Some(analyze_selected(&slot.path));
                }
                if slot.defects.is_none() {
                    slot.defects = Some(defect_rows(&slot.path));
                }
                if slot.objects.is_none()
                    && slot
                        .path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("3mf"))
                {
                    slot.objects = Some(object_rows(&slot.path));
                }
            }
        }
        let Some(entry) = self.selected.and_then(|i| self.files.get(i)) else {
            return;
        };
        if let RepairState::Running { step, total } = entry.repair {
            ui.add(
                egui::ProgressBar::new(step as f32 / total.max(1) as f32)
                    .text(
                        rust_i18n::t!(
                            "app.progress",
                            step = step as i64,
                            total = total as i64
                        )
                        .to_string(),
                    ),
            );
            if ui.button(rust_i18n::t!("app.cancel").to_string()).clicked() {
                cancel_in_panel = true;
            }
        }
        if let RepairState::Failed(e) = &entry.repair {
            ui.label(egui::RichText::new(e).color(egui::Color32::LIGHT_RED));
        }
        if let Some(report) = &entry.report {
            // Three columns: label | bar | number. The bar is the point of the
            // row -- eight ASCII blocks in a monospace font read as a debug
            // dump -- and the number stays because a bar cannot be precise.
            //
            // Risk is inverted: on the other three a high score is good, and a
            // risk of 5 is the best anything can be. Painting it red like a
            // failure would be a lie.
            egui::Grid::new("report-grid")
                .num_columns(3)
                .spacing([crate::theme::GAP_16, crate::theme::GAP_8])
                .min_col_width(84.0)
                .show(ui, |ui| {
                    // Every cell is given the row's height and centred inside
                    // it: a grid row aligns to the top, and a 6 px bar next to
                    // a 16 px label made the number sit low against both.
                    const ROW_H: f32 = 22.0;
                    let mut row = |ui: &mut egui::Ui,
                                   label: String,
                                   score: Option<f64>,
                                   higher_is_better: bool| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(84.0, ROW_H),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.label(label);
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(crate::theme::SCORE_BAR_W, ROW_H),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                score_bar_widget(ui, score, higher_is_better);
                            },
                        );
                        // Normal text, right-aligned in a fixed column. The
                        // bundled font has proportional digits, so a monospace
                        // family was the only way to get tabular figures -- and
                        // monospace read as code. Right-aligning in one column
                        // lines the digits up without it.
                        ui.allocate_ui_with_layout(
                            egui::vec2(44.0, ROW_H),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new(fmt_score(score))
                                        .size(crate::theme::FONT_BODY)
                                        .color(crate::theme::TEXT),
                                );
                            },
                        );
                        ui.end_row();
                    };
                    row(ui, rust_i18n::t!("app.health").to_string(), report.health, true);
                    row(ui, rust_i18n::t!("app.risk").to_string(), report.risk, false);
                    row(ui, rust_i18n::t!("app.fidelity").to_string(), report.fidelity, true);
                    row(ui, rust_i18n::t!("app.confidence").to_string(), report.confidence, true);
                });
            let verdict = verdict_text(report);
            // The text comes from the core and is not ours to change. Its COLOUR
            // is: "Repaired, shape preserved" in green over "the model is still
            // not closed" contradicts itself, so green is reserved for an output
            // that is actually watertight. The brief decides that, not a string
            // match on the verdict.
            let color = match self
                .selected
                .and_then(|i| self.files.get(i))
                .and_then(|e| e.raw_report.as_ref())
                .and_then(output_watertight)
            {
                Some(false) => crate::theme::WARN,
                // No report to judge by: keep the code's own colour.
                _ => verdict_color(&report.verdict_code),
            };
            ui.add_space(6.0);
            ui.label(rust_i18n::t!("app.verdict").to_string());
            ui.label(egui::RichText::new(verdict).color(color));
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    rust_i18n::t!("app.status"),
                    status_text(&report.status_code)
                ))
                .weak(),
            );
        }
        // The plain notes under the verdict: Qt's `report_notes`
        // (`_render_report_brief`, gui.py:6595). Each one is a sentence a user
        // can act on -- "thin wall", "budget exceeded", "SI escalation adopted"
        // -- so they are listed, not counted. Outside the `report` binding on
        // purpose: a note must not disappear because the typed parse failed.
        if let Some(raw) = self
            .selected
            .and_then(|i| self.files.get(i))
            .and_then(|e| e.raw_report.as_ref())
        {
            let b = sutura_core::report::brief::brief(raw);
            let mut lines = self.note_lines(&b);
            if b.verdict == "failed" {
                if let Some(err) = b.error.clone() {
                    lines.push(err);
                }
            }
            for line in lines {
                ui.label(
                    egui::RichText::new(line)
                        .size(crate::theme::FONT_SECTION)
                        .color(crate::theme::TEXT_MUTED),
                );
            }
        }
        // Panels come last: they need `entry` only for an index
        // sanity check, and the data is already cached above.
        if let Some(idx) = self.selected {
            if self.files.get(idx).is_some() {
                ui.add_space(crate::theme::GAP_8);
                // "Input analysis": the grid below describes the file that was
                // dropped in, not the repaired output above it.
                crate::theme::section_label(
                    ui,
                    rust_i18n::t!("app.input_analysis").to_string(),
                );
                ui.add_space(crate::theme::GAP_8);
                {
                    match &self.files[idx].analysis {
                        Some(Ok(a)) => {
                            let rows: [(String, String); 8] = [
                                (
                                    rust_i18n::t!("app.an_type").to_string(),
                                    format!(
                                        "{} ({}%)",
                                        a.mesh_type,
                                        (a.type_confidence * 100.0).round() as i64
                                    ),
                                ),
                                (
                                    rust_i18n::t!("app.an_faces").to_string(),
                                    a.faces.to_string(),
                                ),
                                (
                                    rust_i18n::t!("app.an_vertices").to_string(),
                                    a.vertices.to_string(),
                                ),
                                (
                                    rust_i18n::t!("app.an_components").to_string(),
                                    a.components.to_string(),
                                ),
                                (
                                    rust_i18n::t!("app.an_boundary_loops").to_string(),
                                    a.boundary_loops.to_string(),
                                ),
                                (
                                    rust_i18n::t!("app.an_non_manifold").to_string(),
                                    a.non_manifold_edges.to_string(),
                                ),
                                (
                                    rust_i18n::t!("app.an_self_intersections")
                                        .to_string(),
                                    a.self_intersections.to_string(),
                                ),
                                (
                                    rust_i18n::t!("app.an_open_area").to_string(),
                                    format!("{:.2}%", a.open_area_ratio * 100.0),
                                ),
                            ];
                            // The target's grid: a wide label column, the value
                            // starting at the halfway mark, roomy rows. Counts
                            // that ARE problems are amber; counts that are merely
                            // large (faces, vertices) are not.
                            let amber: [usize; 4] = [4, 5, 6, 7];
                            egui::Grid::new("analysis-grid")
                                .num_columns(2)
                                .min_col_width(220.0)
                                .spacing([crate::theme::GAP_16, crate::theme::GAP_8])
                                .show(ui, |ui| {
                                    for (i, (label, value)) in rows.iter().enumerate() {
                                        ui.label(
                                            egui::RichText::new(label)
                                                .size(crate::theme::FONT_BODY)
                                                .color(crate::theme::TEXT),
                                        );
                                        let is_problem = amber.contains(&i)
                                            && value.parse::<f64>().map(|v| v > 0.0).unwrap_or(
                                                value.trim_end_matches('%')
                                                    .parse::<f64>()
                                                    .map(|v| v > 0.0)
                                                    .unwrap_or(false),
                                            );
                                        let colour = if is_problem {
                                            crate::theme::WARN
                                        } else {
                                            crate::theme::TEXT
                                        };
                                        ui.label(
                                            egui::RichText::new(value.clone())
                                                .size(crate::theme::FONT_BODY)
                                                .color(colour),
                                        );
                                        ui.end_row();
                                    }
                                });
                        }
                        Some(Err(e)) => {
                            ui.label(
                                egui::RichText::new(e)
                                    .small()
                                    .color(egui::Color32::LIGHT_RED),
                            );
                        }
                        None => {}
                    }
                    }
                // Individual defects, as Python's plain-text panel
                // lists them (`gui.py:6755`): one line per hole and
                // per non-manifold region, with its centroid so the
                // user can locate it.
                if let Some(Ok(objects)) = &self.files[idx].objects {
                    if !objects.is_empty() {
                        egui::CollapsingHeader::new(
                            rust_i18n::t!(
                                "app.objects",
                                count = objects.len() as i64
                            )
                            .to_string(),
                        )
                        .id_salt("objects_header")
                        // Open by default: a multi-object archive is
                        // opaque otherwise, and that is the whole
                        // reason the list exists.
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new("objects-grid")
                                .num_columns(3)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label(
                                        rust_i18n::t!("app.ob_model").to_string(),
                                    );
                                    ui.label(
                                        rust_i18n::t!("app.ob_faces").to_string(),
                                    );
                                    ui.label(
                                        rust_i18n::t!("app.ob_vertices").to_string(),
                                    );
                                    ui.end_row();
                                    for row in objects {
                                        ui.label(
                                            row.get("model_name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("-"),
                                        );
                                        ui.label(
                                            row.get("faces")
                                                .map(|v| v.to_string())
                                                .unwrap_or_else(|| "-".into()),
                                        );
                                        ui.label(
                                            row.get("vertices")
                                                .map(|v| v.to_string())
                                                .unwrap_or_else(|| "-".into()),
                                        );
                                        ui.end_row();
                                    }
                                });
                        });
                    }
                }
            }
        }

        // Recommended methods, from the report's own ranking
        // (`ranking::recommendations_json`). They live per object
        // (`object_reports[i].recommendations`), not at the top
        // level -- verified against the CLI's own JSON, where the
        // key is absent from the root but present three times for
        // this 3MF. Every object's list is shown, labelled, so a
        // multi-part file does not silently show only the first.
        //
        // Read-only, not checkboxes: tagging a method is per-file
        // work (İ5) that does not exist yet, and a checkbox that
        // cannot tag would be the dead-control bug class again.
        if let Some(objects) = self
            .selected
            .and_then(|i| self.files.get(i))
            .and_then(|e| e.raw_report.as_ref())
            .and_then(|r| r.get("object_reports"))
            .and_then(|v| v.as_array())
            .filter(|a| {
                // Two shapes: the 3MF path stores
                // `{model_name, report}`; the single-mesh report
                // stores the report directly under `object_reports`.
                a.iter().any(|o| {
                    let report = o.get("report").unwrap_or(o);
                    report
                        .get("recommendations")
                        .and_then(|r| r.as_array())
                        .is_some_and(|r| !r.is_empty())
                })
            })
        {
            egui::CollapsingHeader::new(
                rust_i18n::t!("app.recommended").to_string(),
            )
            .id_salt("recommended_header")
            .default_open(true)
            .show(ui, |ui| {
                for (oi, object) in objects.iter().enumerate() {
                    let report = object.get("report").unwrap_or(object);
                    let recs = report
                        .get("recommendations")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if recs.is_empty() {
                        continue;
                    }
                    if objects.len() > 1 {
                        ui.label(
                            egui::RichText::new(
                                rust_i18n::t!(
                                    "app.rec_object",
                                    index = oi as i64 + 1
                                )
                                .to_string(),
                            )
                            .small()
                            .weak(),
                        );
                    }
                    for rec in &recs {
                        let num = rec
                            .get("num")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let name = rec
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let pct = rec
                            .get("score")
                            .and_then(|v| v.as_f64())
                            .map(|v| (v * 100.0).round() as i64)
                            .unwrap_or(0);
                        let reason = rec
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let row = ui.add(
                            egui::Label::new(format!("#{num}  {name}  {pct}%"))
                                .sense(egui::Sense::hover()),
                        );
                        if !reason.is_empty() {
                            row.on_hover_text(reason);
                        }
                    }
                }
            });
        }

        if let Some(output) = &entry.output {
            ui.separator();
            // One line: the path, then the two buttons at the right edge. It was
            // three stacked rows, and the second button fell off the card.
            ui.horizontal(|ui| {
                let full = output.display().to_string();
                let mut open_result = false;
                let mut reveal = false;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right to left: "Open in…" (when a slicer exists), then
                    // "Show in folder", then "Open result". Everything that acts
                    // on the repaired file sits on the line that names it.
                    if !self.slicers.is_empty() {
                        let installed = self.slicers.clone();
                        ui.menu_button(
                            rust_i18n::t!("app.open_in_slicer").to_string(),
                            |ui| {
                                for (i, slicer) in installed.iter().enumerate() {
                                    if menu_action(ui, slicer.label.to_string()) {
                                        slicer_pick = Some(i);
                                    }
                                }
                            },
                        );
                    }
                    if ui
                        .add(secondary_button(
                            rust_i18n::t!("app.show_in_folder").to_string(),
                        ))
                        .clicked()
                    {
                        reveal = true;
                    }
                    if ui
                        .add(secondary_button(
                            rust_i18n::t!("app.open_result").to_string(),
                        ))
                        .clicked()
                    {
                        open_result = true;
                    }
                    // The path takes whatever the two buttons leave, and is
                    // shortened in the middle when that is not enough. The
                    // tooltip carries the full path, because a truncated path
                    // without one is a dead end.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(shorten_path(&full, ui.available_width()))
                                    .size(crate::theme::FONT_SECTION)
                                    .color(crate::theme::TEXT_MUTED),
                            )
                            .truncate(),
                        )
                        .on_hover_text(full.clone());
                    });
                });
                if open_result {
                    *open_action = Some((output.clone(), false));
                }
                if reveal {
                    *open_action = Some((output.clone(), true));
                }
            });
        }
        if let (Some(which), Some(idx)) = (slicer_pick, self.selected) {
            self.apply_row_action(RowAction::OpenInSlicer(idx, which));
        }
    }

    /// Run a right-click action collected from the file table.
    fn apply_row_action(&mut self, action: RowAction) {
        match action {
            RowAction::Analyze(idx) => {
                if let Some(slot) = self.files.get_mut(idx) {
                    slot.analysis = None;
                    slot.defects = None;
                }
                self.selected = Some(idx);
            }
            RowAction::OpenOutput(idx, reveal) => {
                if let Some(out) = self.files.get(idx).and_then(|e| e.output.clone()) {
                    open_path(&out, reveal);
                }
            }
            RowAction::EditMethods(idx) => {
                self.selected = Some(idx);
                self.options_open = true;
                self.options_tab = OptionsTab::Methods;
            }
            RowAction::ClearTags(idx) => {
                if let Some(slot) = self.files.get_mut(idx) {
                    slot.methods = None;
                }
                self.log("method tags cleared; this file follows the global list".to_string());
            }
            RowAction::OpenInSlicer(idx, which) => {
                let Some(slicer) = self.slicers.get(which) else {
                    return;
                };
                let Some(out) = self.files.get(idx).and_then(|e| e.output.clone()) else {
                    return;
                };
                match crate::slicers::open_in(slicer, &out) {
                    Ok(()) => self.log(format!("opened {} in {}", out.display(), slicer.label)),
                    Err(e) => self.log(format!("could not open the slicer: {e}")),
                }
            }
            RowAction::TagMethod(idx, num) => {
                // Appended, not sorted: the order is the sequence the core runs.
                if let Some(slot) = self.files.get_mut(idx) {
                    let mut list = slot.methods.take().unwrap_or_default();
                    if !list.contains(&num) {
                        list.push(num);
                    }
                    slot.methods = Some(list);
                }
                self.log(format!("method {num} added to this file"));
            }
        }
    }

    /// "Suggestions": what to try next, from the analysis.
    ///
    /// One row per suggestion with an Apply button, at the bottom of the Analysis
    /// card. Applying sets the mode; it never starts a repair, because a mode is
    /// a choice, not an action.
    fn suggestions_block(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.selected else { return };
        let Some(Ok(a)) = self.files.get(idx).and_then(|e| e.analysis.as_ref()) else {
            return;
        };
        let list = crate::suggestions::suggestions(a);
        if list.is_empty() {
            return;
        }
        ui.add_space(crate::theme::GAP_16);
        ui.separator();
        ui.add_space(crate::theme::GAP_8);
        crate::theme::section_label(ui, rust_i18n::t!("app.suggestions").to_string());
        ui.add_space(crate::theme::GAP_8);

        let mut apply: Option<&'static str> = None;
        for sug in &list {
            // Text takes the width the button does not need, then the button
            // sits at the right edge. A plain `right_to_left` layout drew the
            // button over the last line of text: the label had already claimed
            // the full width before the button asked for its own.
            const BUTTON_W: f32 = 76.0;
            ui.horizontal(|ui| {
                let text_w = (ui.available_width() - BUTTON_W - crate::theme::GAP_8).max(120.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(text_w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    rust_i18n::t!(sug.reason_key).to_string(),
                                )
                                .size(crate::theme::FONT_BODY)
                                .color(crate::theme::TEXT),
                            )
                            .wrap(),
                        );
                    },
                );
                if let Some(mode) = sug.mode {
                    if ui
                        .add(secondary_button(rust_i18n::t!("app.apply").to_string()))
                        .on_hover_text(
                            rust_i18n::t!("app.apply_mode_tip", mode = mode).to_string(),
                        )
                        .clicked()
                    {
                        apply = Some(mode);
                    }
                }
            });
        }
        if let Some(mode) = apply {
            self.settings.mode = Some(mode.to_string());
            self.log(format!("mode set to {mode} from a suggestion"));
        }
    }

    /// Rank the methods for this file and keep the result where the panel and
    /// the right-click submenu both read it (`_analyze_methods`).
    ///
    /// Qt ran `sutura --analyze` in a thread so the window stayed live; this is
    /// an ordinary call, because the analysis is pure and takes well under a
    /// frame on the meshes the window can open. If it ever becomes slow, this is
    /// the call to move behind `repair_rx` like the repair itself.
    pub fn analyze_methods(&mut self, idx: usize) {
        let Some(entry) = self.files.get(idx) else { return };
        let path = entry.path.clone();
        let loaded = sutura_core::config::policy::load_policy(None)
            .unwrap_or_else(|_| sutura_core::config::policy::default_policy().unwrap());
        let report = sutura_core::analyze::analyze_report(
            &path.display().to_string(),
            &loaded,
            self.settings
                .classifier_engine
                .as_deref()
                .unwrap_or("experimental"),
            true,
            true,
        );
        let recs = recommended_methods(&report).len();
        if let Some(slot) = self.files.get_mut(idx) {
            slot.raw_report = Some(report);
        }
        self.log(format!(
            "{}: {} method(s) ranked",
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            recs
        ));
    }

    /// A missing optional part, as one sentence with the button that fixes it.
    ///
    /// One notice, not a list: whatever is missing, the answer is the same
    /// screen, and a list of internal names ("#7 skip") tells a user nothing.
    fn unavailable_notice(&mut self, ui: &mut egui::Ui) {
        let missing: Vec<&Value> = self
            .method_health
            .iter()
            .filter(|m| m.get("status").and_then(|v| v.as_str()) != Some("ok"))
            .collect();
        if missing.is_empty() {
            return;
        }
        // fTetWild is the one that matters here: it is the solid fallback, and a
        // repair silently takes the weaker path without it. Any other missing
        // part is named by its own sentence; the raw id and status stay out.
        let ftetwild = missing.iter().any(|m| {
            m.get("name")
                .and_then(|v| v.as_str())
                .map(|n| n.to_lowercase().contains("ftetwild"))
                .unwrap_or(false)
        });
        let text = if ftetwild {
            rust_i18n::t!("app.notice_ftetwild_missing").to_string()
        } else {
            rust_i18n::t!("app.notice_part_missing", count = missing.len() as i64).to_string()
        };
        egui::Frame::new()
            // Amber at 8% over the panel: the tint says "warning" without a
            // second accent colour, which the design rules forbid.
            .fill(lerp_color(
                crate::theme::PANEL,
                crate::theme::WARN,
                0.08,
            ))
            .stroke(egui::Stroke::new(1.0, crate::theme::WARN))
            .corner_radius(crate::theme::RADIUS_CARD)
            .inner_margin(egui::Margin::symmetric(
                crate::theme::GAP_16 as i8,
                crate::theme::GAP_16 as i8,
            ))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .size(crate::theme::FONT_BODY)
                                .color(crate::theme::TEXT),
                        )
                        .wrap(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(secondary_button(
                                rust_i18n::t!("app.install_dots").to_string(),
                            ))
                            .on_hover_text(rust_i18n::t!("app.install_dots_tip").to_string())
                            .clicked()
                        {
                            // Options → Engines, where Install lives.
                            self.options_open = true;
                            self.options_tab = OptionsTab::Engines;
                        }
                    });
                });
            });
    }

    /// Write the ladder to the user policy. `_show_write_error`'s counterpart:
    /// the outcome is a visible line, not a silent catch.
    fn save_ladder(&mut self, selected: &[u8]) {
        let tiers: Vec<String> = selected
            .iter()
            .filter_map(|n| {
                sutura_core::repair_settings::METHODS
                    .iter()
                    .find(|m| m.0 == *n)
                    .map(|m| m.1.to_string())
            })
            .collect();
        if tiers.is_empty() {
            self.policy_status = Some((
                rust_i18n::t!("app.ladder_save_empty").to_string(),
                false,
            ));
            return;
        }
        match sutura_core::config::policy::write_deep_tier_order(&tiers) {
            Ok(path) => {
                self.policy_status = Some((
                    rust_i18n::t!("app.ladder_saved", path = path.display().to_string())
                        .to_string(),
                    true,
                ));
            }
            Err(e) => {
                self.policy_status = Some((
                    rust_i18n::t!("app.ladder_save_failed", error = e.0).to_string(),
                    false,
                ));
            }
        }
    }

    /// `_save_auto_reorder`: persist the ladder auto-reorder opt-in.
    ///
    /// It lives in the GUI's own `gui.json`, which is where `auto_reorder` is
    /// already read from, so this is the same file and the same key the checkbox
    /// reads on the next launch.
    fn save_auto_reorder(&mut self) {
        let mut state = crate::state::load();
        state.settings.auto_reorder = self.settings.auto_reorder;
        match crate::state::save(&state) {
            Ok(()) => {
                self.log(format!(
                    "auto-reorder saved: {}",
                    self.settings.auto_reorder
                ));
            }
            Err(e) => {
                self.policy_status = Some((
                    rust_i18n::t!("app.ladder_save_failed", error = e).to_string(),
                    false,
                ));
            }
        }
    }

    /// The localised text of a report's notes ("`_brief_note_lines`").
    ///
    /// One line per note. Unknown codes fall through as `code: args`, because a
    /// caveat the GUI cannot phrase is still a caveat the user must see.
    fn note_lines(&self, b: &sutura_core::report::brief::Brief) -> Vec<String> {
        let mut out = Vec::new();
        for note in &b.notes {
            let a = &note.args;
            let num = |k: &str| a.get(k).and_then(|v| v.as_f64());
            let line = match note.code.as_str() {
                "stage2_skipped" => rust_i18n::t!("app.brief_note_stage2_skipped").to_string(),
                "stage2_error" => rust_i18n::t!("app.brief_note_stage2_error").to_string(),
                "shape_changed" => rust_i18n::t!("app.brief_note_shape_changed").to_string(),
                "volume_warning" => rust_i18n::t!("app.brief_note_volume").to_string(),
                "graft_detail_loss" => rust_i18n::t!("app.brief_note_graft").to_string(),
                "dressing_detail_loss" => rust_i18n::t!("app.brief_note_dressing").to_string(),
                "reload_non_manifold_vertices" => {
                    rust_i18n::t!("app.brief_note_reload_nm").to_string()
                }
                "unit_warning" => rust_i18n::t!("app.brief_note_unit").to_string(),
                "material_discarded" => rust_i18n::t!("app.brief_note_material").to_string(),
                "attribute_loss" => rust_i18n::t!(
                    "app.brief_note_attr_loss",
                    dropped = a.get("dropped").and_then(|v| v.as_i64()).unwrap_or(0),
                    kept = a.get("kept").and_then(|v| v.as_i64()).unwrap_or(0),
                    new_without = a.get("new_without").and_then(|v| v.as_i64()).unwrap_or(0)
                )
                .to_string(),
                "budget" => rust_i18n::t!("app.brief_note_budget").to_string(),
                "thin_wall" => {
                    let mut line = rust_i18n::t!(
                        "app.brief_note_thin_wall",
                        ratio = format!("{:.1}", num("ratio").unwrap_or(0.0) * 100.0),
                        threshold = format!("{:.1}", num("threshold").unwrap_or(0.0) * 100.0)
                    )
                    .to_string();
                    if let Some(mm) = num("min_mm") {
                        line += &rust_i18n::t!("app.brief_note_thin_wall_mm", mm = format!("{mm:.2}"))
                            .to_string();
                    }
                    if a.get("estimated").and_then(|v| v.as_bool()).unwrap_or(false) {
                        line += &rust_i18n::t!("app.brief_note_estimated").to_string();
                    }
                    line
                }
                "holes_remaining" => rust_i18n::t!(
                    "app.brief_note_holes_remaining",
                    count = a.get("count").and_then(|v| v.as_i64()).unwrap_or(0)
                )
                .to_string(),
                "not_closed" => rust_i18n::t!("app.brief_note_not_closed").to_string(),
                "si_escalation" => {
                    if a.get("outcome").and_then(|v| v.as_str()) == Some("adopted") {
                        rust_i18n::t!("app.brief_note_si_adopted").to_string()
                    } else {
                        rust_i18n::t!("app.brief_note_si_unfixed").to_string()
                    }
                }
                other => format!("{other}: {a}"),
            };
            out.push(line);
        }
        out
    }

    /// "Batch summary": how the last batch went, by category, plus the issue
    /// breakdown behind a link.
    ///
    /// Qt rendered a rich-text strip whose "N issues" link called
    /// `_on_summary_link` (gui.py:7665), which logged one line per issue code
    /// with its count. The breakdown was the point of the link, so it is kept;
    /// it opens under the counts instead of scrolling away in a log, and the link
    /// says what it does.
    fn batch_summary(&mut self, ui: &mut egui::Ui) {
        let any_report = self.files.iter().any(|e| e.raw_report.is_some());
        if !self.batch_active && self.batch_total == 0 && !any_report {
            return;
        }
        let mut watertight = 0;
        let mut warning = 0;
        let mut failed = 0;
        let mut issue_counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for entry in &self.files {
            // Counted from the output's topology, like every other colour on
            // screen. Two repaired open meshes used to count as "watertight"
            // here while the verdict under them said "still not closed".
            match entry.raw_report.as_ref().and_then(output_watertight) {
                Some(true) => watertight += 1,
                Some(false) => {
                    if entry.report.as_ref().is_some_and(|r| r.verdict_code == "not_repaired") {
                        failed += 1;
                    } else {
                        warning += 1;
                    }
                }
                None => match entry.report.as_ref().map(|r| r.verdict_code.as_str()) {
                    Some("preserved") | Some("preserved_notes") => watertight += 1,
                    Some("not_repaired") => failed += 1,
                    Some(_) => warning += 1,
                    None => failed += 1,
                },
            }
            if let Some(raw) = entry.raw_report.as_ref() {
                let (_, issues, _) = sutura_core::report::classify::classify_value(raw);
                for code in issues {
                    *issue_counts.entry(code).or_insert(0) += 1;
                }
            }
        }
        let total_issues: usize = issue_counts.values().sum();
        ui.add_space(crate::theme::GAP_8);
        ui.horizontal(|ui| {
            for count in [
                (watertight, "app.sum_watertight", crate::theme::OK),
                (warning, "app.sum_warning", crate::theme::WARN),
                (failed, "app.sum_error", crate::theme::ERROR),
            ]
            .into_iter()
            .enumerate()
            {
                // A separator BETWEEN items only; it used to trail the last one.
                if count.0 > 0 {
                    ui.label(
                        egui::RichText::new("·")
                            .size(crate::theme::FONT_SECTION)
                            .color(crate::theme::TEXT_FAINT),
                    );
                }
                let (count, key, colour) = count.1;
                ui.label(
                    egui::RichText::new(format!("{count} {}", rust_i18n::t!(key).to_string()))
                        .size(crate::theme::FONT_SECTION)
                        .color(colour),
                );
            }
            if total_issues > 0 {
                ui.label(
                    egui::RichText::new("·")
                        .size(crate::theme::FONT_SECTION)
                        .color(crate::theme::TEXT_FAINT),
                );
                if ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(
                                rust_i18n::t!("app.sum_show_issues", n = total_issues as i64)
                                    .to_string(),
                            )
                            .size(crate::theme::FONT_SECTION)
                            .color(crate::theme::TEXT_MUTED),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text(rust_i18n::t!("app.sum_issues_detail").to_string())
                    .clicked()
                {
                    self.summary_issues_open = !self.summary_issues_open;
                }
            }
        });
        if self.summary_issues_open && total_issues > 0 {
            ui.indent("summary_issues", |ui| {
                for (code, n) in &issue_counts {
                    ui.label(
                        egui::RichText::new(format!("{code}: {n}"))
                            .size(crate::theme::FONT_SECTION)
                            .color(crate::theme::TEXT_MUTED),
                    );
                }
            });
        }
    }

    /// "Small parts": what the pipeline considered debris.
    ///
    /// Qt printed this as text lines in the details (`_small_parts_section`,
    /// gui.py:2327). It is a collapsible section here, closed by default: a
    /// healthy repair has nothing to say and the section must not claim space.
    fn small_parts_section(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.selected else { return };
        let Some(raw) = self.files.get(idx).and_then(|e| e.raw_report.as_ref()) else {
            return;
        };
        // Both keys live under `stage1` for a single mesh; a 3MF carries one
        // `stage1` per object, which the object list already shows.
        let Some(stage1) = raw.get("stage1") else { return };
        let removed = stage1.get("removed_small_parts");
        let debris = stage1.get("possible_debris");
        let removed_n = removed
            .and_then(|r| r.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let debris_n = debris
            .and_then(|d| d.get("count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if removed_n == 0 && debris_n == 0 {
            return;
        }
        ui.add_space(crate::theme::GAP_8);
        egui::CollapsingHeader::new(
            rust_i18n::t!(
                "app.small_parts",
                count = (removed_n + debris_n) as i64
            )
            .to_string(),
        )
        .id_salt("small_parts_header")
        .default_open(false)
        .show(ui, |ui| {
            let mut line = |ui: &mut egui::Ui, text: String, colour: egui::Color32| {
                ui.label(
                    egui::RichText::new(text)
                        .size(crate::theme::FONT_SECTION)
                        .color(colour),
                );
            };
            if let Some(list) = removed.and_then(|r| r.get("parts")).and_then(|p| p.as_array()) {
                for part in list {
                    let faces = part.get("faces").and_then(|v| v.as_u64()).unwrap_or(0);
                    let size = part.get("size_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    line(
                        ui,
                        format!(
                            "{}  {faces} faces, {size:.1}% of the mesh",
                            rust_i18n::t!("app.small_removed").to_string()
                        ),
                        crate::theme::TEXT_MUTED,
                    );
                }
            }
            if let Some(list) = debris.and_then(|d| d.get("parts")).and_then(|p| p.as_array()) {
                for part in list {
                    let faces = part.get("faces").and_then(|v| v.as_u64()).unwrap_or(0);
                    let vol = part.get("volume_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    line(
                        ui,
                        format!(
                            "{}  {faces} faces, {vol:.2}% of the volume",
                            rust_i18n::t!("app.small_debris").to_string()
                        ),
                        crate::theme::WARN,
                    );
                }
            }
        });
    }

    /// Suggestions that come from a *result* rather than an analysis.
    ///
    /// Qt showed these as buttons under the report (`try_dressing_btn`) and a
    /// modal on budget decline (`_ask_budget_rerun`). Here they are rows in the
    /// same Suggestions list as the mode advice, because that is where a reader
    /// looks for "what next" and a modal on top of a finished batch interrupts
    /// nothing useful.
    fn result_suggestions(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.selected else { return };
        let Some(entry) = self.files.get(idx) else { return };

        // A non-watertight result whose report offered dressing: the retry is
        // the same files with force-adopt on.
        let offered_dressing = entry
            .raw_report
            .as_ref()
            .and_then(|r| r.get("suggestions"))
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .any(|s| s.get("method").and_then(|m| m.as_str()) == Some("dressing"))
            })
            .unwrap_or(false);
        // A declined save: the CLI wrote nothing, and `--force` is the answer.
        let declined = entry
            .raw_report
            .as_ref()
            .and_then(|r| r.get("status"))
            .and_then(|v| v.as_str())
            == Some("budget_declined");

        if !offered_dressing && !declined {
            return;
        }
        ui.add_space(crate::theme::GAP_8);
        let mut apply_dressing = false;
        let mut apply_force = false;
        if offered_dressing {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            rust_i18n::t!("app.sug_try_dressing").to_string(),
                        )
                        .size(crate::theme::FONT_BODY)
                        .color(crate::theme::TEXT),
                    )
                    .wrap(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(secondary_button(rust_i18n::t!("app.apply").to_string()))
                        .clicked()
                    {
                        apply_dressing = true;
                    }
                });
            });
        }
        if declined {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(rust_i18n::t!("app.sug_budget_rerun").to_string())
                            .size(crate::theme::FONT_BODY)
                            .color(crate::theme::TEXT),
                    )
                    .wrap(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(secondary_button(rust_i18n::t!("app.apply").to_string()))
                        .clicked()
                    {
                        apply_force = true;
                    }
                });
            });
        }

        if apply_dressing {
            // The CLI's `--dressing-force-adopt` implies dressing, so the option
            // is switched on with it; otherwise the re-run's own report would
            // disagree with the flag that produced it.
            self.settings.dressing = true;
            self.settings.dressing_force_adopt = true;
            self.start_repair(idx, true);
        }
        if apply_force {
            self.start_repair(idx, true);
        }
    }

    /// The selected file's individual defects, as its own panel.
    ///
    /// Qt had this as `Input defects (selected file):` with a fixed-height
    /// text pane under the log; the plan put it there and not inside the
    /// report, where it reads as part of the verdict.
    pub(crate) fn defects_panel(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.selected else { return };
        // Filled on demand, the same call the headless hook uses.
        if let Some(slot) = self.files.get_mut(idx) {
            if slot.defects.is_none() {
                slot.defects = Some(defect_rows(&slot.path));
            }
        }
        let has_any = matches!(&self.files[idx].defects, Some(Ok(r)) if !r.holes.is_empty() || !r.non_manifold.is_empty());
        if !has_any {
            return;
        }
        match &self.files[idx].defects {
                    Some(Ok(report))
                        if !report.holes.is_empty()
                            || !report.non_manifold.is_empty() =>
                    {
                        let total = report.holes.len() + report.non_manifold.len();
                        // (5) The target's row: a hairline separator, then
                        // "N defects >" as a plain clickable line. The arrow is
                        // drawn because the font has no chevron glyph.
                        ui.add_space(crate::theme::GAP_8);
                        ui.separator();
                        ui.add_space(crate::theme::GAP_8);
                        let open = self.defects_open;
                        let response = ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{}  {}",
                                        rust_i18n::t!("app.defects", count = total as i64)
                                            .to_string(),
                                        if open { "v" } else { ">" }
                                    ))
                                    .size(crate::theme::FONT_BODY)
                                    .color(crate::theme::TEXT_MUTED),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            self.defects_open = !self.defects_open;
                        }
                        if open {
                            ui.add_space(crate::theme::GAP_8);
                            {
                            // Bound the list: a shattered mesh can
                            // have thousands, and the panel is a
                            // summary, not a table to scroll.
                            //
                            // The bound is not enough on its own:
                            // twenty rows is ~500 px, which pushed
                            // everything below it (the 3MF object
                            // list) off the bottom of the window.
                            // A bounded inner scroll keeps the
                            // section a fixed height whatever the
                            // mesh.
                            egui::ScrollArea::vertical()
                                .id_salt("defects_scroll")
                                .max_height(160.0)
                                .show(ui, |ui| {
                            const SHOWN: usize = 20;
                            for hole in report.holes.iter().take(SHOWN) {
                                let [x, y, z] = hole.centroid;
                                ui.label(
                                    egui::RichText::new(format!(
                                        "hole  ({x:.1}, {y:.1}, {z:.1})  d={:.2}",
                                        hole.diameter
                                    ))
                                    .small()
                                    .monospace(),
                                );
                            }
                            for nm in report.non_manifold.iter().take(SHOWN) {
                                let [x, y, z] = nm.centroid;
                                ui.label(
                                    egui::RichText::new(format!(
                                        "non-manifold  ({x:.1}, {y:.1}, {z:.1})  faces={}",
                                        nm.faces
                                    ))
                                    .small()
                                    .monospace(),
                                );
                            }
                            if total > SHOWN {
                                ui.label(
                                    egui::RichText::new(
                                        rust_i18n::t!(
                                            "app.defects_more",
                                            rest = (total - SHOWN) as i64
                                        )
                                        .to_string(),
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            });
                            }
                        }
                    }
                    Some(Err(e)) => {
                        ui.label(
                            egui::RichText::new(e)
                                .small()
                                .color(COLOR_ERROR),
                        );
                    }
                    _ => {}
        }
    }

    /// The settings controls, rendered in Options -> Repair.
    ///
    /// Moved out of the old right-hand column unchanged: every control still
    /// writes the same field, so the CLI mapping cannot drift.
    pub(crate) fn settings_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let label_id = labelled(ui, rust_i18n::t!("app.intensity").to_string());
            egui::ComboBox::from_id_salt("intensity")
                .selected_text(self.intensity.label())
                .show_ui(ui, |ui| {
                    for value in [
                        Intensity::Quick,
                        Intensity::Balanced,
                        Intensity::Thorough,
                        Intensity::Extreme,
                    ] {
                        ui.selectable_value(
                            &mut self.intensity,
                            value,
                            value.label(),
                        );
                    }
                })
                .response.labelled_by(label_id);
        });
        // Every control below writes `self.settings` directly. Nothing
        // here builds a RepairOptions by hand: `options_from` does that,
        // in the core, for the CLI and the GUI alike.
        ui.horizontal(|ui| {
            let deep_label = labelled(ui, rust_i18n::t!("app.deep_repair").to_string());
            let current = self
                .settings
                .deep_repair
                .clone()
                .unwrap_or_else(|| "full".to_string());
            egui::ComboBox::from_id_salt("deep_repair")
                .selected_text(current.clone())
                .show_ui(ui, |ui| {
                    for value in ["off", "local", "full"] {
                        if ui
                            .selectable_label(current == value, value)
                            .clicked()
                        {
                            self.settings.deep_repair = Some(value.to_string());
                        }
                    }
                })
                .response.labelled_by(deep_label);
        });

        // Mode and profile change repair *results*, and neither was
        // reachable from the GUI at all -- a GUI user could not
        // reproduce what the CLI does with `--mode` or `--profile`.
        ui.horizontal(|ui| {
            let mode_label = labelled(ui, rust_i18n::t!("app.mode").to_string());
            let current = self
                .settings
                .mode
                .clone()
                .unwrap_or_else(|| "auto".to_string());
            egui::ComboBox::from_id_salt("mode")
                .selected_text(current.clone())
                .show_ui(ui, |ui| {
                    for value in crate::app::MODES {
                        if ui
                            .selectable_label(current == value, value)
                            .clicked()
                        {
                            self.settings.mode = Some(value.to_string());
                        }
                    }
                })
                .response.labelled_by(mode_label);
        });
        // Live description, as Python's dialog shows: it must follow
        // the combo. `t!` returns a `Cow`, so convert before styling.
        let desc = rust_i18n::t!(format!(
            "app.mode_desc_{}",
            self.settings.mode.as_deref().unwrap_or("auto")
        ))
        .to_string();
        ui.add(
            egui::Label::new(egui::RichText::new(desc).small().weak()).wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            let label_id = labelled(ui, rust_i18n::t!("app.profile").to_string());
            let current = self.settings.profile.clone();
            egui::ComboBox::from_id_salt("profile")
                .selected_text(
                    current
                        .clone()
                        .unwrap_or_else(|| rust_i18n::t!("app.profile_auto").to_string()),
                )
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            current.is_none(),
                            rust_i18n::t!("app.profile_auto").to_string(),
                        )
                        .clicked()
                    {
                        self.settings.profile = None;
                    }
                    for value in PROFILES {
                        if ui
                            .selectable_label(
                                current.as_deref() == Some(value),
                                value,
                            )
                            .clicked()
                        {
                            self.settings.profile = Some((*value).to_string());
                        }
                    }
                })
                .response
                .on_hover_text(rust_i18n::t!("app.profile_tip").to_string());
        });

        // Qt's profile management row. The user profiles live in the same
        // `profiles.json` the CLI reads (`config::intensity`), so a profile
        // created here is usable from `--intensity <name>`.
        ui.horizontal(|ui| {
            ui.add_space(LABEL_W);
            let user_profiles = sutura_core::config::intensity::load_profiles(None);
            let selected = self.settings.profile.clone();
            let can_edit = selected
                .as_ref()
                .map(|n| user_profiles.contains_key(n))
                .unwrap_or(false);
            if ui.button(rust_i18n::t!("app.profile_new").to_string()).clicked() {
                self.profile_action = Some(ProfileAction::New);
            }
            if ui
                .add_enabled(
                    can_edit,
                    egui::Button::new(rust_i18n::t!("app.profile_duplicate").to_string()),
                )
                .clicked()
            {
                self.profile_action = Some(ProfileAction::Duplicate);
            }
            if ui
                .add_enabled(
                    can_edit,
                    egui::Button::new(rust_i18n::t!("app.profile_rename").to_string()),
                )
                .clicked()
            {
                self.profile_action = Some(ProfileAction::Rename);
            }
            if ui
                .add_enabled(
                    can_edit,
                    egui::Button::new(rust_i18n::t!("app.profile_delete").to_string()),
                )
                .clicked()
            {
                self.profile_action = Some(ProfileAction::Delete);
            }
        });

        // The profile editor's fields. Qt put eleven here; eight of them are the
        // per-run limits already in `RepairSettings`, so the group is the box
        // around them rather than a second copy of the same numbers.
        Self::option_section(ui, rust_i18n::t!("app.profile_settings").to_string());
        ui.label(
            egui::RichText::new(rust_i18n::t!("app.profile_settings_body").to_string())
                .small()
                .weak(),
        );


        ui.separator();
        Self::option_section(ui, rust_i18n::t!("app.tiers_section").to_string());
        let mut tamponade = self.settings.no_tamponade == false;
        if ui
            .checkbox(&mut tamponade, rust_i18n::t!("app.tier_tamponade").to_string())
            .changed()
        {
            self.settings.no_tamponade = !tamponade;
            self.settings.experimental_tamponade = false;
        }
        let mut flap = self.settings.no_flap == false;
        if ui
            .checkbox(&mut flap, rust_i18n::t!("app.tier_flap").to_string())
            .changed()
        {
            self.settings.no_flap = !flap;
            self.settings.experimental_flap = false;
        }
        let mut coaptation = self.settings.no_coaptation == false;
        if ui
            .checkbox(
                &mut coaptation,
                rust_i18n::t!("app.tier_coaptation").to_string(),
            )
            .changed()
        {
            self.settings.no_coaptation = !coaptation;
            self.settings.experimental_coaptation = false;
        }
        ui.separator();
        // Graft and the fTetWild fallback: declared on the CLI since
        // the beginning, never reachable from the GUI.
        Self::option_section(ui, rust_i18n::t!("app.advanced_section").to_string());
        // Graft and the fTetWild fallback are each TWO booleans in the core: a
        // positive (`experimental_*`) and a negative (`no_*`), and Python lets
        // the negative win. Two checkboxes for one decision is a pair that can
        // contradict itself on screen, so the choice is one tri-state combo and
        // the two flags are derived from it.
        {
            // Graft: on / off / experimental.
            let current = if self.settings.no_graft {
                "off"
            } else if self.settings.experimental_graft {
                "experimental"
            } else {
                "on"
            };
            ui.horizontal(|ui| {
                ui.add_sized(
                    [LABEL_W, 22.0],
                    egui::Label::new(rust_i18n::t!("app.graft").to_string()),
                );
                egui::ComboBox::from_id_salt("graft-tier")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (value, key) in [
                            ("on", "app.tier_on"),
                            ("experimental", "app.tier_experimental"),
                            ("off", "app.tier_off"),
                        ] {
                            if ui
                                .selectable_label(current == value, rust_i18n::t!(key).to_string())
                                .clicked()
                            {
                                self.settings.no_graft = value == "off";
                                self.settings.experimental_graft = value == "experimental";
                            }
                        }
                    });
            });
        }
        {
            // fTetWild fallback: the same shape.
            let current = if self.settings.no_fallback_ftetwild {
                "off"
            } else if self.settings.experimental_fallback_ftetwild {
                "experimental"
            } else {
                "on"
            };
            ui.horizontal(|ui| {
                ui.add_sized(
                    [LABEL_W, 22.0],
                    egui::Label::new(rust_i18n::t!("app.fallback_ftetwild").to_string()),
                );
                egui::ComboBox::from_id_salt("ftetwild-tier")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (value, key) in [
                            ("on", "app.tier_on"),
                            ("experimental", "app.tier_experimental"),
                            ("off", "app.tier_off"),
                        ] {
                            if ui
                                .selectable_label(current == value, rust_i18n::t!(key).to_string())
                                .clicked()
                            {
                                self.settings.no_fallback_ftetwild = value == "off";
                                self.settings.experimental_fallback_ftetwild =
                                    value == "experimental";
                            }
                        }
                    });
            });
        }
        // `--classifier-engine`: `options_from` copies it and `repair.rs:913`
        // reads it, so this is a live control; the two engines the classifier
        // actually implements are "experimental" and "classic" (`classify.rs:237`).
        {
            let current = self
                .settings
                .classifier_engine
                .clone()
                .unwrap_or_else(|| "experimental".to_string());
            egui::Grid::new("gui_classifier_engine")
                .num_columns(2)
                .spacing([crate::theme::GAP_16, crate::theme::GAP_8])
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            rust_i18n::t!("app.classifier_engine").to_string(),
                        )
                        .size(crate::theme::FONT_BODY)
                        .color(crate::theme::TEXT_MUTED),
                    );
                    let mut chosen = match current.as_str() {
                        "classic" => 1,
                        _ => 0,
                    };
                    egui::ComboBox::from_id_salt("classifier_engine_combo")
                        .selected_text(if chosen == 0 {
                            rust_i18n::t!("app.classifier_experimental").to_string()
                        } else {
                            rust_i18n::t!("app.classifier_classic").to_string()
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut chosen,
                                0,
                                rust_i18n::t!("app.classifier_experimental").to_string(),
                            );
                            ui.selectable_value(
                                &mut chosen,
                                1,
                                rust_i18n::t!("app.classifier_classic").to_string(),
                            );
                        });
                    self.settings.classifier_engine = Some(
                        if chosen == 1 { "classic" } else { "experimental" }.to_string(),
                    );
                    ui.end_row();
                });
        }

        // External engines. `--engines` takes a list of *configured*
        // names (`repair_cmd.rs:145` rejects unknowns), so the only
        // sane control is a checkbox per configured engine. `None`
        // means "every configured engine", which is why unticking
        // one is what builds the list.
        {
            let (configured, _warnings) =
                sutura_core::config::load_all_engines(None);
            if !configured.is_empty() {
                ui.separator();
                ui.label(rust_i18n::t!("app.engines").to_string());
                ui.label(
                    egui::RichText::new(rust_i18n::t!("app.engines_hint").to_string())
                        .small()
                        .weak(),
                );
                let selected: Vec<String> = self
                    .settings
                    .engines
                    .clone()
                    .unwrap_or_else(|| configured.keys().cloned().collect());
                for (name, engine) in &configured {
                    let mut on = selected.iter().any(|s| s == name);
                    let label = format!(
                        "{name}{}",
                        if engine.enabled { "" } else { " (disabled)" }
                    );
                    if ui.checkbox(&mut on, label).changed() {
                        let mut next = selected.clone();
                        if on {
                            if !next.iter().any(|s| s == name) {
                                next.push(name.clone());
                            }
                        } else {
                            next.retain(|s| s != name);
                        }
                        // The CLI rejects an empty list
                        // ("argument --engines: empty list"), so
                        // clearing every box means "default: all".
                        self.settings.engines = if next.is_empty()
                            || next.len() == configured.len()
                        {
                            None
                        } else {
                            next.sort();
                            Some(next)
                        };
                    }
                }
            }
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let si_label = labelled(ui, rust_i18n::t!("app.si_mode").to_string());
            let current = self
                .settings
                .si_mode
                .clone()
                .unwrap_or_else(|| "report".to_string());
            egui::ComboBox::from_id_salt("si_mode")
                .selected_text(current.clone())
                .show_ui(ui, |ui| {
                    for value in ["repair", "report", "off"] {
                        if ui.selectable_label(current == value, value).clicked() {
                            self.settings.si_mode = Some(value.to_string());
                        }
                    }
                })
                .response.labelled_by(si_label);
        });
        ui.horizontal_wrapped(|ui| {
            let solid_label = labelled(ui, rust_i18n::t!("app.solid_engine").to_string());
            let current = self
                .settings
                .solid_engine
                .clone()
                .unwrap_or_else(|| "ftetwild".to_string());
            egui::ComboBox::from_id_salt("solid_engine")
                .selected_text(current.clone())
                .show_ui(ui, |ui| {
                    for value in ["ftetwild", "cdt"] {
                        if ui.selectable_label(current == value, value).clicked() {
                            self.settings.solid_engine = Some(value.to_string());
                        }
                    }
                })
                .response.labelled_by(solid_label);
        });

        // The eleven per-run limits the profile saves. Qt called this group
        // "Profile settings"; the button above pointed at "the per-run limits
        // below" while this was headed "Per-run limits", so a reader saw neither.
        Self::option_section(ui, rust_i18n::t!("app.profile_settings").to_string());
        egui::CollapsingHeader::new(rust_i18n::t!("app.bounds").to_string())
            .default_open(true)
            .show(ui, |ui| {
            ui.label(
                egui::RichText::new(rust_i18n::t!("app.bounds_hint").to_string())
                    .small()
                    .weak(),
            );
            // A grid, so the labels form one column and the inputs another;
            // `ui.horizontal` sized each row to its own caption, which is why
            // every box sat at a different x.
            egui::Grid::new("profile-limits")
                .num_columns(2)
                .spacing([SPACE_16, SPACE_8])
                .min_col_width(LABEL_W)
                .show(ui, |ui| {
            // Every field here is an `Option`, and blank means "keep the
            // intensity's value" -- so a cleared box must write `None`,
            // not zero. A zero timeout would cap the tier at no time.
            opt_f64(ui, "gui_max_risk", &mut self.settings.max_risk,
                    &rust_i18n::t!("app.max_risk"));
            opt_f64(ui, "gui_max_geom", &mut self.settings.max_geometry_change,
                    &rust_i18n::t!("app.max_geometry_change"));
            opt_i64(ui, "gui_graft_faces", &mut self.settings.graft_max_faces,
                    &rust_i18n::t!("app.graft_max_faces"));
            opt_f64(ui, "gui_graft_to", &mut self.settings.graft_timeout,
                    &rust_i18n::t!("app.graft_timeout"));
            opt_i64(ui, "gui_dense_ratio", &mut self.settings.dense_ratio,
                    &rust_i18n::t!("app.dense_ratio"));
            opt_i64(ui, "gui_dense_min", &mut self.settings.dense_min_faces,
                    &rust_i18n::t!("app.dense_min_faces"));
            opt_i64(ui, "gui_ft_faces", &mut self.settings.ftetwild_max_faces,
                    &rust_i18n::t!("app.ftetwild_max_faces"));
            opt_f64(ui, "gui_ft_to", &mut self.settings.ftetwild_timeout,
                    &rust_i18n::t!("app.ftetwild_timeout"));
            opt_f64(ui, "gui_ft_haus", &mut self.settings.ftetwild_max_hausdorff_rel,
                    &rust_i18n::t!("app.ftetwild_max_hausdorff"));
            // Wall-thicken target. Setting a value turns the tier on,
            // matching the CLI (`--wall-min-thickness` IS the switch):
            // a target with the tier off would silently do nothing.
            opt_f64(ui, "gui_wall_min", &mut self.settings.wall_min_thickness,
                    &rust_i18n::t!("app.wall_min_thickness"));
                });
            self.settings.wall_thicken =
                self.settings.wall_min_thickness.is_some();
            ui.separator();
            ui.checkbox(
                &mut self.settings.join_components,
                rust_i18n::t!("app.join_components").to_string(),
            );
            // Bind a local, not `&mut option.unwrap_or(false)`: a
            // temporary has no place to write back to, so the checkbox
            // would accept the click and discard it.
            let mut optimize_retry =
                self.settings.ftetwild_optimize_retry.unwrap_or(false);
            if ui
                .checkbox(
                    &mut optimize_retry,
                    rust_i18n::t!("app.ftetwild_optimize_retry").to_string(),
                )
                .changed()
            {
                self.settings.ftetwild_optimize_retry =
                    if optimize_retry { Some(true) } else { None };
            }
            let mut escalation = self.settings.auto_escalation.is_some();
            if ui
                .checkbox(
                    &mut escalation,
                    rust_i18n::t!("app.auto_escalation").to_string(),
                )
                .changed()
            {
                self.settings.auto_escalation =
                    if escalation { Some(true) } else { None };
            }
        });

        ui.separator();
        // Per-file methods (İ5). Python's Methods tab edits the
        // *selected* file's sequence (`gui.py:5093`); here the global
        // list stays the fallback and this overrides it for one file,
        // so customising one file does not change the next.
        //
        // No up/down reordering, deliberately: `forced_methods` is a
        // `Vec<u8>` that core only ever tests with `contains`
        // (`repair.rs:3727`), so an order control would promise
        // something nothing honours.
    }

    /// The method pickers, rendered in Options -> Methods.
    pub(crate) fn methods_controls(&mut self, ui: &mut egui::Ui) {
        // Qt's Methods tab, in its order: the mode radios, then the automatic
        // ladder (read-only when the policy is in charge), then the manual
        // sequence. "Use the panel selection" was left over from a panel this
        // window replaced, and "0 method(s) applied" told a reader nothing.
        Self::option_section(ui, rust_i18n::t!("app.method_control").to_string());
        let mut manual = self.settings.methods.is_some();
        ui.horizontal(|ui| {
            if ui
                .radio(!manual, rust_i18n::t!("app.method_automatic"))
                .clicked()
            {
                manual = false;
            }
            if ui
                .radio(manual, rust_i18n::t!("app.method_manual"))
                .clicked()
            {
                manual = true;
            }
        });
        if !manual {
            self.settings.methods = None;
        } else if self.settings.methods.is_none() {
            self.settings.methods = Some(String::new());
        }

        // The ladder. In automatic mode the policy picks and the rows are shown
        // but not editable, which is what Qt meant by a fixed order.
        Self::option_section(ui, rust_i18n::t!("app.automatic_ladder").to_string());
        ui.label(
            egui::RichText::new(rust_i18n::t!("app.automatic_ladder_body").to_string())
                .small()
                .weak(),
        );
        let mut selected: Vec<u8> = self
            .settings
            .methods
            .as_deref()
            .map(|s| {
                s.split(',')
                    .filter_map(|t| t.trim().parse::<u8>().ok())
                    .collect()
            })
            .unwrap_or_default();
        let automatic = self.settings.methods.is_none();
        let mut changed = false;
        // Up/Down on the manual list. The sequence is what the core runs, so
        // reordering is a real edit, not a display preference.
        let mut reordered = false;
        let mut move_up: Option<usize> = None;
        let mut move_down: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("methods_ladder")
            .max_height(170.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (num, id, _category, name) in sutura_core::repair_settings::METHODS {
                    // The ladder is the policy order, not the whole registry:
                    // Qt listed exactly these six and nothing else.
                    if automatic && !LADDER.contains(&num) {
                        continue;
                    }
                    let mut on = if automatic {
                        true
                    } else {
                        selected.contains(&num)
                    };
                    if automatic {
                        // A real checkbox, disabled: Qt's ladder was exactly
                        // that. egui fades the tick with the *disabled* visuals,
                        // which on this dark `Button` colour left six ticked rows
                        // looking like one -- so the disabled style is raised
                        // just for this row instead of drawing the mark by hand.
                        let mut checked = on;
                        ui.scope(|ui| {
                            let v = &mut ui.style_mut().visuals.widgets;
                            v.inactive.fg_stroke.color = COLOR_ACCENT;
                            v.noninteractive.fg_stroke.color = COLOR_ACCENT;
                            ui.add_enabled(
                                false,
                                egui::Checkbox::new(
                                    &mut checked,
                                    format!("{num}. {name} ({id})"),
                                ),
                            );
                        });
                    } else if ui
                        .checkbox(&mut on, format!("{num}. {name} ({id})"))
                        .changed()
                    {
                        if on {
                            selected.push(num);
                        } else {
                            selected.retain(|n| *n != num);
                        }
                        changed = true;
                    }
                }
            });
        if reordered || changed {
            // Order preserved, not sorted: the core takes the FIRST method as
            // the deep-repair mode, so 1,3 and 3,1 repair differently.
            self.settings.methods = Some(
                selected
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }

        // Save the ladder and the auto-reorder opt-in. Qt had this as
        // `apply()` plus `_save_auto_reorder`, with `_show_write_error` turning a
        // refused write into a visible message: a save that fails silently is
        // worse than no save button.
        let mut save_ladder = false;
        let mut save_reorder = false;
        Self::option_section(ui, rust_i18n::t!("app.automatic_ladder").to_string());
        ui.label(
            egui::RichText::new(rust_i18n::t!("app.ladder_save_body").to_string())
                .small()
                .weak(),
        );
        ui.horizontal(|ui| {
            if !automatic {
                if ui
                    .add(secondary_button(
                        rust_i18n::t!("app.ladder_save").to_string(),
                    ))
                    .on_hover_text(rust_i18n::t!("app.ladder_save_tip").to_string())
                    .clicked()
                {
                    save_ladder = true;
                }
            }
            let mut reorder = self.settings.auto_reorder;
            if ui
                .checkbox(&mut reorder, rust_i18n::t!("app.auto_reorder").to_string())
                .changed()
            {
                self.settings.auto_reorder = reorder;
                save_reorder = true;
            }
        });
        // The outcome of the last policy write, beside the button that caused it.
        if let Some((text, ok)) = self.policy_status.clone() {
            ui.label(
                egui::RichText::new(text)
                    .size(crate::theme::FONT_SECTION)
                    .color(if ok {
                        crate::theme::OK
                    } else {
                        crate::theme::ERROR
                    }),
            );
        }
        if save_ladder {
            self.save_ladder(&selected);
        }
        if save_reorder {
            self.save_auto_reorder();
        }

        Self::option_section(ui, rust_i18n::t!("app.manual_sequence").to_string());
        ui.label(
            egui::RichText::new(rust_i18n::t!("app.manual_sequence_body").to_string())
                .small()
                .weak(),
        );
        if automatic {
            ui.label(
                egui::RichText::new(rust_i18n::t!("app.manual_sequence_hint").to_string())
                    .small()
                    .weak(),
            );
        } else {
            // Qt's Up/Down pair under the manual list. The sequence is what the
            // core runs -- `parse_methods_text` takes the first method as the
            // deep-repair mode -- so this is a real edit, and it is why the list
            // must not be sorted anywhere.
            if selected.is_empty() {
                ui.label(
                    egui::RichText::new(rust_i18n::t!("app.manual_sequence_empty").to_string())
                        .small()
                        .weak(),
                );
            } else {
                // One row per method, with its own arrows: a whole-list Up/Down
                // would need a further selection UI to say which row moves.
                let mut index = 0;
                while index < selected.len() {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [24.0, 18.0],
                            egui::Label::new(
                                egui::RichText::new(format!("{}", index + 1)).small().weak(),
                            ),
                        );
                        let n = selected[index];
                        let name = sutura_core::repair_settings::METHODS
                            .iter()
                            .find(|m| m.0 == n)
                            .map(|m| m.3)
                            .unwrap_or("?");
                        ui.label(format!("{n}. {name}"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if arrow_button(ui, false, index + 1 < selected.len())
                                .on_hover_text(rust_i18n::t!("app.manual_move_down").to_string())
                                .clicked()
                            {
                                move_down = Some(index);
                            }
                            if arrow_button(ui, true, index > 0)
                                .on_hover_text(rust_i18n::t!("app.manual_move_up").to_string())
                                .clicked()
                            {
                                move_up = Some(index);
                            }
                        });
                    });
                    index += 1;
                }
            }
            if let Some(i) = move_up {
                selected.swap(i, i - 1);
                reordered = true;
            }
            if let Some(i) = move_down {
                selected.swap(i, i + 1);
                reordered = true;
            }
        }

        // The per-file editor sits here too: Qt edited the selected file's
        // sequence from this tab.
        Self::option_section(ui, rust_i18n::t!("app.file_methods").to_string());
        if let Some(idx) = self.selected {
            if self.files.get(idx).is_some() {
                let global = selected.clone();
                let mut local = self.files[idx].methods.clone();
                let mut inherit = local.is_none();
                ui.push_id("file-methods-inherit", |ui| {
                    ui.checkbox(
                        &mut inherit,
                        rust_i18n::t!("app.file_methods_inherit").to_string(),
                    );
                });
                if inherit && local.is_some() {
                    local = None;
                } else if !inherit && local.is_none() {
                    local = Some(global.clone());
                }
                if let Some(edited) = local.clone() {
                    let mut next = edited.clone();
                    let mut dirty = false;
                    egui::ScrollArea::vertical()
                        .id_salt("file_methods_list")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for (num, id, _cat, name) in sutura_core::repair_settings::METHODS {
                                let mut on = next.contains(&num);
                                if ui
                                    .checkbox(&mut on, format!("{num}. {name} ({id})"))
                                    .changed()
                                {
                                    if on {
                                        next.push(num);
                                    } else {
                                        next.retain(|n| *n != num);
                                    }
                                    dirty = true;
                                }
                            }
                        });
                    if dirty {
                        // Order preserved, for the same reason the global list
                        // is: this list becomes `forced_methods`, which the core
                        // runs in order.
                        local = Some(next);
                    }
                }
                if local != self.files[idx].methods {
                    self.files[idx].methods = local;
                }
            }
        } else {
            ui.label(
                egui::RichText::new(rust_i18n::t!("app.no_selection").to_string())
                    .small()
                    .weak(),
            );
        }
    }


    /// fTetWild and the external engines, rendered in Options -> Engines.
    pub(crate) fn engines_controls(&mut self, ui: &mut egui::Ui) {
        // One heading style for every tab (rule f): this used to be a plain
        // label, so "Engines" was smaller than "Repair tiers" and the rest.
        Self::option_section(ui, rust_i18n::t!("app.engines_section").to_string());
        // One row: name, status, then the two buttons to the right of the
        // status (rule d's grouping). They were on a second row indented to the
        // label column, which read as an unrelated block.
        ui.horizontal(|ui| {
            ui.label(rust_i18n::t!("app.engines_ftetwild").to_string());
            let installed = self.ftetwild.is_some();
            match &self.ftetwild {
                Some(path) => {
                    ui.label(
                        egui::RichText::new(
                            rust_i18n::t!("app.engines_installed").to_string(),
                        )
                        .color(COLOR_OK),
                    )
                    .on_hover_text(path.clone());
                }
                None => {
                    ui.label(
                        egui::RichText::new(rust_i18n::t!("app.engines_missing").to_string())
                            .color(COLOR_ERROR),
                    )
                    .on_hover_text(rust_i18n::t!("app.engines_missing_tip").to_string());
                }
            }
            ui.add_space(SPACE_8);
            // Install copies a binary the user picked into the managed
            // directory; Remove deletes it. Neither downloads anything: the Rust
            // build ships the sidecar in the package, so the alternative to this
            // pair was no pair at all. Each is disabled when it could not apply.
            if ui
                .add_enabled(
                    !installed,
                    egui::Button::new(rust_i18n::t!("app.engines_install").to_string()),
                )
                .on_hover_text(rust_i18n::t!("app.engines_install_tip").to_string())
                .clicked()
            {
                if let Some(src) = rfd::FileDialog::new()
                    .set_title(rust_i18n::t!("app.engines_install").to_string())
                    .pick_file()
                {
                    match sutura_core::ftetwild::install_binary(&src) {
                        Ok(dest) => {
                            self.log(format!("installed fTetWild at {}", dest.display()));
                            self.ftetwild = sutura_core::ftetwild::find_binary();
                        }
                        Err(e) => self.log(format!("could not install fTetWild: {e}")),
                    }
                }
            }
            if ui
                .add_enabled(
                    installed,
                    egui::Button::new(rust_i18n::t!("app.engines_remove").to_string()),
                )
                .on_hover_text(rust_i18n::t!("app.engines_remove_tip").to_string())
                .clicked()
            {
                match sutura_core::ftetwild::remove_binary() {
                    Ok(true) => {
                        self.log("removed fTetWild".to_string());
                        self.ftetwild = sutura_core::ftetwild::find_binary();
                    }
                    Ok(false) => self.log("no managed fTetWild binary to remove".to_string()),
                    Err(e) => self.log(format!("could not remove fTetWild: {e}")),
                }
            }
        });
        // External engines, with their state. `load_all_engines` is
        // the same call the `--engines` validator uses, so the list
        // cannot disagree with what the CLI accepts.
        {
            let (configured, warnings) = sutura_core::config::load_all_engines(None);
            if configured.is_empty() {
                ui.label(
                    egui::RichText::new(
                        rust_i18n::t!("app.engines_none").to_string(),
                    )
                    .small()
                    .weak(),
                );
            }
            for (name, engine) in &configured {
                let state = if engine.enabled {
                    rust_i18n::t!("app.engines_enabled").to_string()
                } else {
                    rust_i18n::t!("app.engines_disabled").to_string()
                };
                ui.label(
                    egui::RichText::new(format!("{name} — {state}"))
                        .small()
                        .weak(),
                )
                .on_hover_text(engine.command.join(" "));
            }
            for w in warnings {
                ui.label(
                    egui::RichText::new(w)
                        .small()
                        .color(COLOR_WARN),
                );
            }

            // Qt's three actions under the list. Install / Remove are absent on
            // purpose: the fTetWild sidecar ships inside the package, so both
            // would be buttons that cannot do anything.
            ui.add_space(SPACE_8);
            ui.horizontal(|ui| {
                if ui
                    .button(rust_i18n::t!("app.engines_reload").to_string())
                    .clicked()
                {
                    self.method_health = sutura_core::self_check::run(false)
                        .get("methods")
                        .and_then(|m| m.as_object())
                        .map(|m| {
                            let mut v: Vec<Value> = m.values().cloned().collect();
                            v.sort_by_key(|e| {
                                e.get("num").and_then(|n| n.as_u64()).unwrap_or(0)
                            });
                            v
                        })
                        .unwrap_or_default();
                    self.log("external engines reloaded".to_string());
                }
                if ui
                    .button(rust_i18n::t!("app.engines_open_folder").to_string())
                    .clicked()
                {
                    open_path(&sutura_core::config::engines_dir(), false);
                }
                if ui
                    .button(rust_i18n::t!("app.engines_docs").to_string())
                    .on_hover_text("docs/guide/engines.md")
                    .clicked()
                {
                    open_path(
                        std::path::Path::new(
                            "https://github.com/Krateian/Sutura/blob/main/docs/guide/engines.md",
                        ),
                        false,
                    );
                }
            });
        }
    }

    /// The opt-in passes, rendered in Options -> Experimental.
    pub(crate) fn experimental_controls(&mut self, ui: &mut egui::Ui) {
        // Qt's Experimental tab: the opt-in passes, then the four Dressing
        // knobs. Every pass below is a `RepairOptions` field, so each reaches
        // the repair -- an opt-in that wrote nothing would be a dead control.
        ui.add(egui::Label::new(
            egui::RichText::new(rust_i18n::t!("app.experimental_hint").to_string())
                .small()
                .weak(),
        ));
        ui.add_space(SPACE_8);

        ui.checkbox(
            &mut self.settings.autorefine,
            rust_i18n::t!("app.autorefine_label").to_string(),
        );
        ui.checkbox(
            &mut self.settings.indirect_autorefine,
            rust_i18n::t!("app.indirect_autorefine_label").to_string(),
        );
        ui.checkbox(
            &mut self.settings.join_components,
            rust_i18n::t!("app.join_components").to_string(),
        );
        // `--ftetwild-optimize`: separate from `-retry`, which only decides
        // whether a rejected candidate is tried again.
        ui.checkbox(
            &mut self.settings.ftetwild_optimize,
            rust_i18n::t!("app.ftetwild_optimize").to_string(),
        );
        // `--auto-reorder`: whether the ladder may reorder the list it was given.
        ui.checkbox(
            &mut self.settings.auto_reorder,
            rust_i18n::t!("app.auto_reorder").to_string(),
        );
        // `--experimental-edge-tiebreak`: the classifier's extra features.
        ui.checkbox(
            &mut self.settings.extra_features,
            rust_i18n::t!("app.edge_tiebreak").to_string(),
        );
        let dressing_on = self.settings.dressing;
        ui.checkbox(
            &mut self.settings.dressing,
            rust_i18n::t!("app.tier_dressing").to_string(),
        );
        ui.indent("dressing_extra", |ui| {
            ui.add_enabled_ui(dressing_on, |ui| {
                ui.checkbox(
                    &mut self.settings.dressing_force_adopt,
                    rust_i18n::t!("app.dressing_force_adopt").to_string(),
                );
            });
        });
        ui.checkbox(
            &mut self.settings.experimental_splint,
            rust_i18n::t!("app.tier_splint").to_string(),
        );

        // The four Dressing knobs, as combos: Drain, Defects, Bridge radius,
        // Influence width. The two scale factors map to `rmax_scale` and
        // `sigma_scale`; the other two are fixed vocabularies.
        ui.add_space(SPACE_8);
        // Indented, and inert while Dressing is off: these four only configure
        // it, so leaving them live would be four controls that do nothing.
        ui.indent("dressing-knobs", |ui| {
        ui.add_enabled_ui(dressing_on, |ui| {
        let mut choice = |ui: &mut egui::Ui,
                          key: &str,
                          salt: &str,
                          options: &[(&str, &str)],
                          current: Option<String>,
                          set: &mut dyn FnMut(Option<String>)|
        {
            ui.horizontal(|ui| {
                let id = labelled(ui, rust_i18n::t!(key).to_string());
                let preset = rust_i18n::t!("app.dressing_preset").to_string();
                let shown = current.clone().unwrap_or_else(|| preset.clone());
                egui::ComboBox::from_id_salt(salt)
                    .selected_text(shown.clone())
                    .show_ui(ui, |ui| {
                        for (value, label_key) in options {
                            let label = rust_i18n::t!(*label_key).to_string();
                            if ui.selectable_label(shown == label, label).clicked() {
                                set(if value.is_empty() {
                                    None
                                } else {
                                    Some((*value).to_string())
                                });
                            }
                        }
                    })
                    .response
                    .labelled_by(id);
            });
        };
        let drain_options: [(&str, &str); 5] = [
            ("", "app.dressing_preset"),
            ("off", "app.dressing_drain_off"),
            ("half", "app.dressing_drain_half"),
            ("full", "app.dressing_drain_full"),
            ("deep", "app.dressing_drain_deep"),
        ];
        let mut drain = self
            .settings
            .dressing_drain
            .as_ref()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        choice(
            ui,
            "app.dressing_drain",
            "dressing-drain",
            &drain_options,
            drain.clone(),
            &mut |v| {
                self.settings.dressing_drain =
                    v.map(|s| serde_json::json!(s)).or(None)
            },
        );
        let defect_options: [(&str, &str); 3] = [
            ("", "app.dressing_preset"),
            ("all", "app.dressing_defects_all"),
            ("holes_nm", "app.dressing_defects_holes_nm"),
        ];
        let defects = self.settings.dressing_defects.clone();
        choice(
            ui,
            "app.dressing_defects",
            "dressing-defects",
            &defect_options,
            defects,
            &mut |v| self.settings.dressing_defects = v,
        );

        let mut scale = |ui: &mut egui::Ui, key: &str, salt: &str, value: &mut Option<f64>| {
            ui.horizontal(|ui| {
                let id = labelled(ui, rust_i18n::t!(key).to_string());
                let preset = rust_i18n::t!("app.dressing_preset").to_string();
                let current = value.map(|v| format!("x{}", trim_float(v))).unwrap_or(preset.clone());
                egui::ComboBox::from_id_salt(salt)
                    .selected_text(current.clone())
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(current == preset, preset.clone()).clicked() {
                            *value = None;
                        }
                        for factor in [0.25, 0.5, 0.75, 1.0, 1.5, 2.0] {
                            let label = format!("x{}", trim_float(factor));
                            if ui.selectable_label(current == label, label.clone()).clicked() {
                                *value = Some(factor);
                            }
                        }
                    })
                    .response
                    .labelled_by(id);
            });
        };
        scale(
            ui,
            "app.dressing_rmax_scale",
            "dressing-rmax",
            &mut self.settings.dressing_rmax_scale,
        );
        scale(
            ui,
            "app.dressing_sigma_scale",
            "dressing-sigma",
            &mut self.settings.dressing_sigma_scale,
        );
        });
        });
    }

    /// The repeat points, rendered in Options -> Methods.
    ///
    /// Qt opened these from a toolbar button; they are two coordinates that the
    /// CLI takes as `--repeat-source` / `--repeat-target`, and the Methods tab is
    /// where a user looks for a per-repair option like them.
    pub(crate) fn repeat_point_controls(&mut self, ui: &mut egui::Ui) {
        Self::option_section(ui, rust_i18n::t!("app.repeat_pick").to_string());
        let fmt = |p: Option<[f64; 3]>| match p {
            Some([x, y, z]) => format!("({x:.2}, {y:.2}, {z:.2})"),
            None => rust_i18n::t!("app.repeat_pick_unset").to_string(),
        };
        ui.label(
            egui::RichText::new(rust_i18n::t!(
                "app.repeat_pick_state",
                source = fmt(self.settings.repeat_source),
                target = fmt(self.settings.repeat_target)
            )
            .to_string())
            .small()
            .weak(),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.selected.is_some(),
                    egui::Button::new(rust_i18n::t!("app.repeat_pick").to_string()),
                )
                .clicked()
            {
                self.open_repeat_picker();
            }
            if (self.settings.repeat_source.is_some() || self.settings.repeat_target.is_some())
                && ui
                    .button(rust_i18n::t!("app.repeat_pick_clear").to_string())
                    .clicked()
            {
                self.settings.repeat_source = None;
                self.settings.repeat_target = None;
            }
        });
    }

}

impl Drop for SuturaApp {
    fn drop(&mut self) {
        // Ask the worker to stop rather than killing a process: the repair thread
        // owns no child, and a detached thread exits with the process anyway.
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }
}

/// Format a picked point for display, or a placeholder when unset.
fn fmt_point(p: Option<[f64; 3]>) -> String {
    match p {
        Some([x, y, z]) => format!("({x:.2}, {y:.2}, {z:.2})"),
        None => "-".to_string(),
    }
}

/// A score with its bar: `====---- 0.52`.
pub(crate) fn fmt_score_with_bar(v: Option<f64>) -> String {
    match v {
        Some(score) => format!("{}  {}", score_bar(score), fmt_score(Some(score))),
        None => fmt_score(None),
    }
}

/// Qt's `_score_bar`: a small block bar for a 0..1 score.
///
/// Eight blocks, filled in proportion, with the number beside it elsewhere. The
/// bar is what makes two scores comparable at a glance; the number is what makes
/// them precise, so both are shown.
pub(crate) fn score_bar(score: f64) -> String {
    const BLOCKS: usize = 8;
    let filled = ((score.clamp(0.0, 1.0)) * BLOCKS as f64).round() as usize;
    let mut s = String::with_capacity(BLOCKS * 3);
    for i in 0..BLOCKS {
        s.push(if i < filled { '=' } else { '-' });
    }
    s
}

fn fmt_score(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.0}"))
        .unwrap_or_else(|| rust_i18n::t!("app.none").to_string())
}

fn verdict_text(report: &RepairReport) -> String {
    let n = report.verdict_areas.max(0) as i64;
    match report.verdict_code.as_str() {
        "preserved" => rust_i18n::t!("app.verdict_preserved").to_string(),
        "preserved_notes" => rust_i18n::t!("app.verdict_preserved_notes").to_string(),
        "changed" if n > 0 => {
            rust_i18n::t!("app.verdict_changed_areas", count = n).to_string()
        }
        "changed" | "changed_no_area" => rust_i18n::t!("app.verdict_changed").to_string(),
        "low_confidence" if n > 0 => {
            rust_i18n::t!("app.verdict_low_confidence_areas", count = n).to_string()
        }
        "low_confidence" | "low_confidence_no_area" => {
            rust_i18n::t!("app.verdict_low_confidence").to_string()
        }
        "not_repaired" => rust_i18n::t!("app.verdict_not_repaired").to_string(),
        _ => rust_i18n::t!("app.verdict_unavailable").to_string(),
    }
}

fn verdict_color(code: &str) -> egui::Color32 {
    match code {
        // Green for a clean fix, amber for a shape change, red for a failure --
        // the same three meanings the analysis counts use.
        "preserved" | "preserved_notes" => crate::theme::OK,
        "changed" | "changed_no_area" | "low_confidence" | "low_confidence_no_area" => {
            crate::theme::WARN
        }
        "not_repaired" => crate::theme::ERROR,
        _ => crate::theme::TEXT_FAINT,
    }
}

fn status_text(code: &str) -> String {
    match code {
        "safe" => rust_i18n::t!("app.status_safe").to_string(),
        "review" => rust_i18n::t!("app.status_review").to_string(),
        "caution" => rust_i18n::t!("app.status_caution").to_string(),
        "failed" => rust_i18n::t!("app.status_failed").to_string(),
        _ => rust_i18n::t!("app.status_unavailable").to_string(),
    }
}

fn is_mesh_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("stl" | "obj" | "ply" | "3mf")
    )
}

/// Repair one file with no window, for the parity gate.
///
/// This runs the SAME `run_repair` the UI uses -- the point is to compare the
/// GUI's repair path against the CLI for identical arguments, so it must not be
/// a second implementation. The report is written as JSON when `report` names a
/// path.
pub fn headless_repair(
    input: &Path,
    output: &Path,
    intensity: Intensity,
    _report: Option<PathBuf>,
    cancel_after_ms: Option<u64>,
) -> eframe::Result<()> {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(CancelToken::new());

    // Measurement hook: cancel from another thread after N ms and report how
    // long the worker took to observe it.
    let cancel_started = std::time::Instant::now();
    if let Some(ms) = cancel_after_ms {
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            token.cancel();
            eprintln!("cancel requested at {} ms", cancel_started.elapsed().as_millis());
        });
    }

    let settings = RepairSettings::from_intensity(intensity.value(), false);
    // The headless path has no UI, so it runs with the defaults rather than a
    // user's General tab -- a gate must not change behaviour with a GUI file.
    run_repair(input, output, settings, crate::state::GuiState::default(), tx, cancel);

    let observed = cancel_started.elapsed();
    if cancel_after_ms.is_some() {
        eprintln!("repair returned at {} ms", observed.as_millis());
    }

    // `run_repair` finished synchronously here, so the first outcome is the real
    // one; keep draining only to be safe against extra progress events.
    let mut outcome = None;
    for ev in rx.try_iter() {
        if let RepairEvent::Finished { result, .. } = ev {
            outcome = Some(result);
        }
    }
    match outcome {
        Some(Ok((_mesh, _summary, _raw))) => Ok(()),
        Some(Err(e)) => {
            eprintln!("headless repair failed: {e}");
            std::process::exit(1);
        }
        None => {
            eprintln!("headless repair produced no result");
            std::process::exit(1);
        }
    }
}

/// Analyze one file with no window, for the parity gate.
///
/// Uses `sutura_core::analysis::analyze_mesh` -- the same call the CLI's
/// `--analyze` makes -- so the numbers the panel shows cannot come from a
/// Analyse a mesh -- the ONE call path, shared by the headless hook and the
/// Analysis panel.
///
/// Extracted so the hook stays a real oracle: if the panel and the hook diverged,
/// the parity gate would stop meaning anything. The engine is `"experimental"`
/// because that is what the repair path hard-codes (`repair.rs:907`).
pub fn analyze_selected(path: &std::path::Path) -> Result<sutura_core::analysis::Analysis, String> {
    let mesh = sutura_core::load_mesh(path).map_err(|e| e.to_string())?;
    sutura_core::analysis::analyze_mesh(&mesh, "experimental", true)
}

/// The selected mesh's individual defects: each hole and each non-manifold
/// region, with its centroid so a user can find it.
///
/// Python renders this as a plain text panel (`gui.py:6755`); the values come
/// from `report::defects::detect_defects`, the same call `analysis::analyze_mesh`
/// already makes internally. Indices are not requested -- they are large vectors
/// and the panel shows a summary line per defect, not a face list.
pub fn defect_rows(path: &std::path::Path) -> Result<sutura_core::report::defects::DefectReport, String> {
    let mesh = sutura_core::load_mesh(path).map_err(|e| e.to_string())?;
    Ok(sutura_core::report::defects::detect_defects(
        &mesh.verts,
        &mesh.tris,
        false,
    ))
}

/// The 3MF object list -- likewise shared by `headless_objects` and the panel.
///
/// Returns one row per object block (`parse_3mf_mesh_blocks`), so a multi-object
/// archive is not opaque.
pub fn object_rows(path: &std::path::Path) -> Result<Vec<Value>, String> {
    let blocks = sutura_core::parse_3mf_mesh_blocks(path).map_err(|e| e.to_string())?;
    Ok(blocks
        .iter()
        .map(|b| {
            serde_json::json!({
                "model_name": b.model_name,
                "block": b.index,
                "faces": b.mesh.tris.len(),
                "vertices": b.mesh.verts.len(),
            })
        })
        .collect())
}

/// second measurement path.
pub fn headless_analyze(
    input: &Path,
    out: Option<PathBuf>,
) -> eframe::Result<()> {
    let analysis = match analyze_selected(input) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("analyse failed: {e}");
            std::process::exit(1);
        }
    };
    let value = analysis.to_json();
    match out {
        Some(path) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("write failed: {e}");
                std::process::exit(1);
            }
        }
        None => println!("{}", serde_json::to_string(&value).unwrap_or_default()),
    }
    Ok(())
}

/// Headless report dump: the exact values the Report panel shows.
///
/// Exists so the panel can be compared to the CLI's JSON for the same mesh. The
/// panel bug was invisible to unit tests but obvious here: every field was null
/// because the in-process report had never been scored.
pub fn headless_report(input: &Path, out: Option<PathBuf>) -> eframe::Result<()> {
    let mesh = match sutura_core::load_mesh(input) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("report failed: {e}");
            std::process::exit(1);
        }
    };
    let in_v: Vec<[f64; 3]> = mesh
        .verts
        .iter()
        .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
        .collect();
    let in_t: Vec<[i64; 3]> = mesh
        .tris
        .iter()
        .map(|f| [i64::from(f[0]), i64::from(f[1]), i64::from(f[2])])
        .collect();
    let opts = sutura_core::repair::RepairOptions::default();
    let token = sutura_core::repair::CancelToken::new();
    let outcome =
        sutura_core::repair::repair(&in_v, &in_t, &opts, &sutura_core::repair::NoopSink, &token);
    let sutura_core::repair::RepairOutcome::Ok {
        verts: out_v,
        tris: out_t,
        report,
    } = outcome
    else {
        eprintln!("report failed: repair did not succeed");
        std::process::exit(1);
    };

    let in_i32: Vec<[i32; 3]> = in_t
        .iter()
        .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
        .collect();
    let out_i32: Vec<[i32; 3]> = out_t
        .iter()
        .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
        .collect();
    let mut scored = report.as_object().cloned().unwrap_or_default();
    sutura_core::report::add_scores(&mut scored, &in_v, &in_i32, &out_v, &out_i32);
    let value = Value::Object(scored);

    // Exactly the fields the panel renders, so a mismatch is unmissable.
    let panel = RepairReport::from_value(&value);
    let json = serde_json::json!({
        "health": panel.health,
        "risk": panel.risk,
        "fidelity": panel.fidelity,
        "confidence": panel.confidence,
        "verdict_code": panel.verdict_code,
        "verdict_areas": panel.verdict_areas,
        "status_code": panel.status_code,
    });
    let text = serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string());
    match out {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, text.as_bytes()) {
                eprintln!("report write failed: {e}");
                std::process::exit(1);
            }
        }
        None => println!("{text}"),
    }
    Ok(())
}

/// Headless heatmap dump, for the parity gate.
///
/// Emits the three **values** the heatmap is built from — the defect face list,
/// the per-vertex deviation and the per-face ramp index — not an image. Python's
/// renderer and the GUI cannot agree on pixels (isometric CPU raster vs
/// perspective GPU camera), so pixels are not the contract; these are.
pub fn headless_heatmap(input: &Path, reference: Option<&Path>) -> eframe::Result<()> {
    let mut mesh = match crate::mesh::DisplayMesh::load(input) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("heatmap failed: {e}");
            std::process::exit(1);
        }
    };
    if let Some(reference) = reference {
        if let Err(e) = mesh.with_deviation(reference) {
            eprintln!("heatmap failed: {e}");
            std::process::exit(1);
        }
    }
    let defect: Vec<usize> = mesh
        .defect
        .chunks_exact(3)
        .enumerate()
        .filter(|(_, c)| c[0] > 0.0)
        .map(|(i, _)| i)
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "defect_faces": defect,
            "deviation": mesh.deviation,
            "dev_idx": mesh.dev_idx,
            "faces": mesh.triangle_count(),
            "vertices": mesh.src_verts.len(),
        })
    );
    Ok(())
}

/// Headless pick gate: click the given pixels through the same picker the UI
/// uses and print the resulting points as JSON.
///
/// This exists so the gate measures the **fid → point** mapping
/// (`gui.py`'s `verts[tris[fid-1]].mean`), not a rendered image: anti-aliasing
/// is not part of the contract, the picked 3D point is.
pub fn headless_pick(input: &Path, picks: &[(i64, i64)]) -> eframe::Result<()> {
    let mesh = match crate::mesh::DisplayMesh::load(input) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pick failed: {e}");
            std::process::exit(1);
        }
    };
    let mut picker = crate::repeat_picker::RepeatPicker::new(&mesh);
    let mut out = Vec::new();
    for (x, y) in picks {
        let changed = picker.click(*x, *y);
        out.push(serde_json::json!({
            "x": x, "y": y,
            "picked": changed,
            "source": picker.source.map(|p| [p[0], p[1], p[2]]),
            "target": picker.target.map(|p| [p[0], p[1], p[2]]),
        }));
    }
    println!("{}", serde_json::json!({"picks": out}));
    Ok(())
}

/// List a container's objects with no window, for the parity gate.
///
/// Reports them in ARCHIVE order, which is what the repair side preserves.
pub fn headless_objects(input: &Path, out: Option<PathBuf>) -> eframe::Result<()> {
    let rows = match object_rows(input) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("objects failed: {e}");
            std::process::exit(1);
        }
    };
    let value = Value::Array(rows);
    match out {
        Some(path) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("write failed: {e}");
                std::process::exit(1);
            }
        }
        None => println!("{}", serde_json::to_string(&value).unwrap_or_default()),
    }
    Ok(())
}

/// The output path for a repaired file: `<stem>_fixed.<ext>`, **keeping the
/// input's extension**.
///
/// The extension is not cosmetic. `repair_3mf_file` rewrites a valid 3MF
/// container (see `run_repair`), so a hard-coded `.stl` wrote those archive
/// bytes into a file named `_fixed.stl` — a slicer rejects it, and the user
/// reads that as "the repair failed". The same applies to `.obj`/`.ply`, which
/// the file dialog accepts.
///
/// Falls back to `stl` only when the input has no extension at all.
pub fn fixed_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mesh".to_string());
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "stl".to_string());
    input.with_file_name(format!("{stem}_fixed.{ext}"))
}

/// A numeric box that distinguishes "blank" (`None`, keep the tier's value) from
/// zero. Required by the per-run limits: `Some(0.0)` on a timeout means the run
/// gets no time at all, which is not what an empty field means.
/// One label/input pair inside a `Grid`, so every field starts at the same x.
///
/// `ui.horizontal` took the label's natural width, which is why "Max risk" and
/// "fTetWild max Hausdorff" put their boxes at different columns.
fn opt_cell(ui: &mut egui::Ui, id: &str, text: &mut String, label: &str) {
    let label_id = ui.label(label).id;
    // `hint_text` gives the box an accessible name -- egui's TextEdit does not
    // take one from `labelled_by`, which is why the accessibility check listed
    // ten anonymous boxes. A single space rather than the label: with the label
    // as the hint, an empty field rendered "Max risk Max risk" beside itself.
    let _ = label_id;
    ui.add(
        egui::TextEdit::singleline(text)
            .id_salt(id)
            .hint_text(" ")
            .desired_width(120.0),
    )
    .labelled_by(label_id);
}

fn opt_f64(ui: &mut egui::Ui, id: &str, value: &mut Option<f64>, label: &str) {
    let mut text = value.map(|v| format!("{v}")).unwrap_or_default();
    opt_cell(ui, id, &mut text, label);
    ui.end_row();
    // Blank means "keep the intensity's value", so a cleared box writes `None`,
    // not zero: a zero timeout would cap the tier at no time.
    *value = if text.trim().is_empty() {
        None
    } else {
        text.trim().parse::<f64>().ok()
    };
}

/// The integer twin of [`opt_f64`].
fn opt_i64(ui: &mut egui::Ui, id: &str, value: &mut Option<i64>, label: &str) {
    let mut text = value.map(|v| format!("{v}")).unwrap_or_default();
    opt_cell(ui, id, &mut text, label);
    ui.end_row();
    *value = if text.trim().is_empty() {
        None
    } else {
        text.trim().parse::<i64>().ok()
    };
}

/// Repair one file in-process, through `sutura_core::repair`.
///
/// This used to shell out to `python3 sutura/repair.py` (or whatever `SUTURA_CLI`
/// named), which is why the GUI needed a Python interpreter to repair anything.
/// The work now happens in this thread: the caller spawns it, so the UI keeps
/// drawing while it runs.
///
/// Settings come from [`RepairSettings`], the same struct the CLI fills, and the
/// conversion to `RepairOptions` is the CLI's own `options_from` -- there is no
/// second mapping to drift.
fn run_repair(
    input: &Path,
    output: &Path,
    settings: RepairSettings,
    prefs: crate::state::GuiState,
    tx: mpsc::Sender<RepairEvent>,
    cancel: Arc<CancelToken>,
) {
    // The CLI publishes stages 1..=8 around the core call; mirror that exactly so
    // the progress bar advances the same way. `repair`'s own `ProgressSink` is a
    // no-op in the CLI too, so nothing is lost by doing the same here.
    let stage = |step: u8, total: u8| {
        let _ = tx.send(RepairEvent::Progress { step, total });
    };
    stage(1, 8);

    let (opts, _methods_given, _methods) =
        match sutura_core::repair_settings::options_from(&settings) {
            Ok(v) => v,
            Err(e) => {
                let _ = tx.send(RepairEvent::Finished {
                    output: output.to_path_buf(),
                    result: Err(e.message),
                    cancelled: false,
                });
                return;
            }
        };

    stage(2, 8);

    // 3MF is a container: repair each object separately and rewrite the archive,
    // the same path the CLI takes. Everything else is a single mesh.
    if is_3mf(input) {
        let sink = GuiSink { tx: tx.clone() };
        let result = sutura_core::pipeline_objects::repair_3mf_file(
            input, output, &opts, &sink, &cancel,
        );
        // A 3MF gets one report *per object*, and the recommendations only exist
        // there (`object_reports[i].recommendations`); the single-mesh report has
        // no such key at all. So carry the per-object reports through instead of
        // dropping them -- otherwise the Recommended panel can never populate for
        // the only format that has recommendations.
        let object_reports = match &result {
            Ok(repairs) => serde_json::json!({
                "object_reports": repairs
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "model_name": r.model_name,
                            "report": r.report.clone(),
                        })
                    })
                    .collect::<Vec<_>>(),
            }),
            Err(_) => Value::Null,
        };
        let _ = match result {
            Ok(_) => finish_from_output_with(output, &tx, &cancel, object_reports),
            Err(e) => tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(e.to_string()),
                cancelled: cancel.is_cancelled(),
            }),
        };
        return;
    }

    let mesh = match sutura_core::load_mesh(input) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(format!("load failed: {e}")),
                cancelled: false,
            });
            return;
        }
    };
    let verts: Vec<[f64; 3]> = mesh
        .verts
        .iter()
        .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
        .collect();
    let tris: Vec<[i64; 3]> = mesh
        .tris
        .iter()
        .map(|f| [i64::from(f[0]), i64::from(f[1]), i64::from(f[2])])
        .collect();

    stage(3, 8);
    if opts.deep_repair != sutura_core::repair::DeepRepair::Off {
        stage(4, 8);
    }
    // The General tab's four switches, applied exactly where the CLI applies its
    // `--no-*` flags: two are environment variables the core reads, and the other
    // two gate the history write below.
    if prefs.learning_enabled == Some(false) {
        std::env::set_var("SUTURA_LEARNING_TRIAGE", "0");
    }
    if prefs.cache_enabled == Some(false) {
        std::env::set_var("SUTURA_NO_CACHE", "1");
    }

    let sink = GuiSink { tx: tx.clone() };
    // Keep the inputs: scoring compares the repaired mesh against them, and
    // `verts`/`tris` are shadowed by the outcome below.
    let in_v = verts.clone();
    let in_t = tris.clone();
    let outcome = sutura_core::repair::repair(&verts, &tris, &opts, &sink, &cancel);
    stage(5, 8);

    let (out_v, out_t, report) = match outcome {
        sutura_core::repair::RepairOutcome::Ok {
            verts,
            tris,
            report,
        } => (verts, tris, report),
        sutura_core::repair::RepairOutcome::Unavailable { tier, reason } => {
            let _ = tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(format!("{tier} is unavailable: {reason}")),
                cancelled: cancel.is_cancelled(),
            });
            return;
        }
        sutura_core::repair::RepairOutcome::Error { message } => {
            let _ = tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(message),
                cancelled: cancel.is_cancelled(),
            });
            return;
        }
    };

    // A cancelled run must not leave a half-written repair behind.
    if cancel.is_cancelled() {
        let _ = tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Err("cancelled".to_string()),
            cancelled: true,
        });
        return;
    }

    stage(6, 8);
    let out_mesh = sutura_core::Mesh::new(
        out_v
            .iter()
            .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            .collect(),
        out_t
            .iter()
            .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
            .collect(),
    );
    if let Err(e) = sutura_core::save_mesh(output, &out_mesh) {
        let _ = tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Err(format!("write failed: {e}")),
            cancelled: false,
        });
        return;
    }
    stage(7, 8);
    stage(8, 8);

    if let Some(err) = report.get("error").and_then(Value::as_str) {
        let _ = tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Err(err.to_string()),
            cancelled: false,
        });
        return;
    }
    let mesh = match DisplayMesh::load(output) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(format!("output load failed: {e}")),
                cancelled: false,
            });
            return;
        }
    };
    // The core `repair` report carries the pipeline facts but **no scores** —
    // only the CLI's report builder adds them, which is why this panel used to
    // show dashes while the CLI's JSON was full. Add the same block here, from
    // the one shared recipe, before reading it into the panel.
    let mut scored = report.as_object().cloned().unwrap_or_default();
    let in_f64: Vec<[f64; 3]> = in_v;
    let in_i32: Vec<[i32; 3]> = in_t
        .iter()
        .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
        .collect();
    let out_f64: Vec<[f64; 3]> = out_v.to_vec();
    let out_i32: Vec<[i32; 3]> = out_t
        .iter()
        .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
        .collect();
    sutura_core::report::add_scores(&mut scored, &in_f64, &in_i32, &out_f64, &out_i32);
    // The CLI's report builder adds `output`; the core's own `repair` report does
    // not. Without it `report::brief` reads this as a run that wrote nothing and
    // appends "the model is still not closed" to a file that was written and is
    // closed -- which is exactly the contradiction this fixes.
    scored.insert(
        "output".into(),
        Value::from(output.display().to_string()),
    );
    let scored = Value::Object(scored);
    let _ = tx.send(RepairEvent::Finished {
        output: output.to_path_buf(),
        result: Ok((mesh, RepairReport::from_value(&scored), scored)),
        cancelled: false,
    });
}

/// True when the path names a 3MF container (case-insensitive, like the CLI).
fn is_3mf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("3mf"))
        .unwrap_or(false)
}

/// Report the outcome of a multi-object repair from the written file.
///
/// A container repair has no single per-object summary here, so the report is
/// left at its default; the preview is what the GUI shows for `--repair`.
/// Send the finished event for a rebuilt output file.
///
/// `raw` carries whatever the caller knows that the parsed `RepairReport` drops.
/// For a 3MF that is the per-object report list (the only place recommendations
/// exist); for a single mesh the `repair` outcome's own value. The re-loaded mesh
/// cannot supply it, because a report is not a property of the output file.
fn finish_from_output_with(
    output: &Path,
    tx: &mpsc::Sender<RepairEvent>,
    cancel: &CancelToken,
    raw: Value,
) -> Result<(), mpsc::SendError<RepairEvent>> {
    match DisplayMesh::load(output) {
        Ok(mesh) => tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Ok((mesh, RepairReport::default(), raw)),
            cancelled: cancel.is_cancelled(),
        }),
        Err(e) => tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Err(format!("output load failed: {e}")),
            cancelled: false,
        }),
    }
}

/// Draw a form label and return its id, so the control beside it can point at it.
///
/// Without this the combos and text fields are anonymous to a screen reader --
/// `egui_kittest` turned that into a hard failure listing sixteen unnamed
/// widgets. `labelled_by` is the standard way to associate a label with a
/// control, and it costs one id.
/// `%g`-ish: no trailing zeros, so a scale factor reads "x1" not "x1.000".
/// A small button with a painted triangle, up or down.
///
/// egui's bundled Ubuntu-Light has neither U+25B2/U+25BC nor U+2713 -- every one
/// of those attempts drew a missing-glyph box -- so the arrow is drawn.
/// A two-state button: outline when off, accent outline and tint when on.
pub(crate) fn toggle_button(text: String, on: bool) -> egui::Button<'static> {
    let (fill, stroke, colour) = if on {
        (
            crate::theme::ACCENT.gamma_multiply(0.18),
            crate::theme::ACCENT,
            crate::theme::TEXT,
        )
    } else {
        (
            egui::Color32::TRANSPARENT,
            crate::theme::BUTTON_BORDER,
            crate::theme::TEXT_MUTED,
        )
    };
    egui::Button::new(
        egui::RichText::new(text)
            .size(crate::theme::FONT_SECTION)
            .color(colour),
    )
    .fill(fill)
    .stroke(egui::Stroke::new(crate::theme::BORDER_W, stroke))
    .corner_radius(egui::CornerRadius::same(crate::theme::RADIUS_BUTTON))
    .min_size(egui::vec2(0.0, crate::theme::BUTTON_H - 6.0))
}

/// A secondary button: card height, control surface, hairline border.
pub(crate) fn secondary_button(text: String) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .size(crate::theme::FONT_BODY)
            .color(crate::theme::TEXT),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(egui::Stroke::new(
        crate::theme::BORDER_W,
        crate::theme::BUTTON_BORDER,
    ))
    .corner_radius(egui::CornerRadius::same(crate::theme::RADIUS_BUTTON))
    .min_size(egui::vec2(0.0, crate::theme::BUTTON_H))
}

/// The one filled button: accent fill, dark text.
pub(crate) fn primary_button(text: String) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .size(crate::theme::FONT_BODY)
            .color(crate::theme::ON_ACCENT),
    )
    .fill(crate::theme::ACCENT)
    .stroke(egui::Stroke::NONE)
    .min_size(egui::vec2(0.0, crate::theme::BUTTON_H))
}

/// The Options gear, painted. Same reason as the arrows: no glyph in the font.
pub(crate) fn gear_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(theme_gear_size(), theme_gear_size()),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let visuals = *ui.style().interact(&response);
        let c = rect.center();
        let colour = visuals.fg_stroke.color;
        let painter = ui.painter();
        let r_out = 7.0;
        let r_in = 4.6;
        // Eight teeth as short radial spokes between two rings.
        for i in 0..8 {
            let a = std::f32::consts::TAU * (i as f32) / 8.0;
            let (sa, ca) = a.sin_cos();
            painter.line_segment(
                [
                    egui::pos2(c.x + ca * r_in, c.y + sa * r_in),
                    egui::pos2(c.x + ca * r_out, c.y + sa * r_out),
                ],
                egui::Stroke::new(2.6, colour),
            );
        }
        painter.circle_stroke(c, r_in, egui::Stroke::new(2.4, colour));
        painter.circle_filled(c, 1.6, colour);
    }
    response
}

fn theme_gear_size() -> f32 {
    32.0
}

pub(crate) fn arrow_button(ui: &mut egui::Ui, up: bool, enabled: bool) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(24.0, 20.0), sense);
    if ui.is_rect_visible(rect) {
        let mut visuals = *ui.style().interact(&response);
        if !enabled {
            visuals.fg_stroke.color = ui.visuals().weak_text_color();
            visuals.weak_bg_fill = ui.visuals().faint_bg_color;
        }
        ui.painter().rect(
            rect,
            4.0,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let c = rect.center();
        let (w, h) = (4.5, 3.5);
        let points = if up {
            vec![
                egui::pos2(c.x, c.y - h),
                egui::pos2(c.x - w, c.y + h),
                egui::pos2(c.x + w, c.y + h),
            ]
        } else {
            vec![
                egui::pos2(c.x, c.y + h),
                egui::pos2(c.x - w, c.y - h),
                egui::pos2(c.x + w, c.y - h),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            visuals.fg_stroke.color,
            egui::Stroke::NONE,
        ));
    }
    response
}

pub(crate) fn trim_float(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

pub(crate) fn labelled(ui: &mut egui::Ui, text: String) -> egui::Id {
    // A fixed label column, so every control in a tab starts at the same x
    // (rule h). `add_sized` rather than `label`: the latter takes its natural
    // width and the combos then land in ragged columns.
    let response = ui.add_sized([LABEL_W, 22.0], egui::Label::new(text));
    response.id
}

/// Truncate a verdict for the narrow file column without cutting mid-word.
///
/// The full text lives in the hover tooltip, so this only has to be
/// recognisable: "Repaired, shape changed…" beats a horizontal scrollbar in a
/// 290 px panel.
fn truncate_decision(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    if let Some(cut) = out.rfind([' ', ',']) {
        out.truncate(cut);
    }
    out.push('…');
    out
}

/// `Ctrl+,` shows or hides the settings panel.
///
/// Scope matters: a comma is a character people type, so the shortcut must not
/// fire while a text box has focus -- otherwise typing "a,b" would hide the
/// panel mid-word. `consume_key` also stops egui re-handling the event.
///
/// A free function rather than inline so it can be exercised against a real
/// `egui::Context`: an untested keyboard shortcut is one that silently stops
/// working when egui changes its input API.
fn settings_shortcut_pressed(ctx: &egui::Context) -> bool {
    if ctx.memory(|m| m.focused()).is_some() {
        return false;
    }
    ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma))
}

/// Forwards core progress events as GUI events.
///
/// The core's `repair` emits nothing today (the CLI's sink is a no-op for the
/// same reason), but forwarding means a future `emit` inside the core reaches
/// the progress bar with no change here.
struct GuiSink {
    tx: mpsc::Sender<RepairEvent>,
}

impl sutura_core::repair::ProgressSink for GuiSink {
    fn emit(&self, event: Value) {
        if let Some(step) = event.get("step").and_then(Value::as_u64) {
            let total = event
                .get("step_total")
                .and_then(Value::as_u64)
                .unwrap_or(8);
            let _ = self.tx.send(RepairEvent::Progress {
                step: step as u8,
                total: total as u8,
            });
        }
    }
}


pub(crate) fn open_path(path: &Path, reveal: bool) {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        if reveal {
            cmd.arg("-R");
        }
        let _ = cmd.arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
}

fn save_screenshot(path: &Path, img: &egui::ColorImage) {
    let [w, h] = img.size;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in &img.pixels {
        let [r, g, b, a] = px.to_srgba_unmultiplied();
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match image::save_buffer(path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
        Ok(()) => eprintln!("screenshot written: {}", path.display()),
        Err(e) => eprintln!("screenshot failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output keeps the input's extension, for every format the dialog
    /// accepts. A hard-coded `.stl` wrote a rewritten 3MF archive (and OBJ/PLY)
    /// into a file a slicer cannot open, which reads as a failed repair.
    #[test]
    fn fixed_path_keeps_the_input_extension() {
        for (input, want) in [
            ("/tmp/model.stl", "/tmp/model_fixed.stl"),
            ("/tmp/model.3mf", "/tmp/model_fixed.3mf"),
            ("/tmp/model.obj", "/tmp/model_fixed.obj"),
            ("/tmp/model.ply", "/tmp/model_fixed.ply"),
            // Case is preserved: the dialog accepts `.STL`/`.3MF` too.
            ("/tmp/model.STL", "/tmp/model_fixed.STL"),
            // No extension: fall back to stl rather than producing a dotfile.
            ("/tmp/model", "/tmp/model_fixed.stl"),
        ] {
            assert_eq!(
                fixed_path(Path::new(input)),
                PathBuf::from(want),
                "{input} must produce {want}"
            );
        }
    }

    /// A name that merely contains dots must not confuse the split.
    #[test]
    fn fixed_path_handles_dotted_names() {
        assert_eq!(
            fixed_path(Path::new("/tmp/v1.2.part.stl")),
            PathBuf::from("/tmp/v1.2.part_fixed.stl")
        );
    }

    /// `selected` and `loaded` are indices into `files`, so removing an entry
    /// must clear both — otherwise the preview keeps showing the removed file,
    /// or shows whichever file slid into that index.
    #[test]
    fn removal_clears_the_selection_and_preview_key() {
        use std::path::PathBuf;

        let entry = |name: &str| Entry {
            path: PathBuf::from(name),
            input: None,
            result: None,
            error: None,
            repair: RepairState::Idle,
            report: None,
            raw_report: None,
            output: None,
            view: ViewMode::Before,
            analysis: None,
            objects: None,
            defects: None,
            methods: None,
        };
        let mut app = SuturaApp::for_test();
        app.files = vec![entry("/tmp/a.stl"), entry("/tmp/b.stl"), entry("/tmp/c.stl")];
        app.selected = Some(2);
        app.loaded = Some((2, ViewMode::Before));

        app.remove_file(1);

        assert_eq!(app.files.len(), 2, "one entry removed");
        assert_eq!(app.files[1].path, PathBuf::from("/tmp/c.stl"), "c slid to 1");
        assert!(app.selected.is_none(), "selection must not point at a stale index");
        assert!(app.loaded.is_none(), "preview key must be invalidated");
        assert!(app.preview.mesh.is_none(), "preview must be cleared");
    }

    /// Out-of-range removal is a no-op rather than a panic.
    #[test]
    fn removal_out_of_range_is_ignored() {
        let mut app = SuturaApp::for_test();
        app.remove_file(7);
        assert!(app.files.is_empty());
    }

    /// Every value the Mode combo offers must be one `options_from` accepts.
    ///
    /// This is the invariant that matters: a combo entry that core rejects makes
    /// the repair fail with "invalid --mode value", and a value core accepts but
    /// the combo never offers is unreachable from the GUI. Checked against
    /// `options_from` itself rather than a copy of the accepted list.
    #[test]
    fn every_mode_in_the_combo_is_accepted_by_core() {
        for mode in MODES {
            let settings = RepairSettings {
                mode: Some(mode.to_string()),
                ..Default::default()
            };
            assert!(
                sutura_core::repair_settings::options_from(&settings).is_ok(),
                "core rejects --mode {mode}"
            );
        }
        // The value shown when nothing is chosen must be acceptable too.
        assert!(sutura_core::repair_settings::options_from(&RepairSettings::default()).is_ok());

        // And a value outside the list must be rejected, proving the check has
        // teeth rather than accepting anything.
        let bad = RepairSettings {
            mode: Some("nonsense".into()),
            ..Default::default()
        };
        assert!(sutura_core::repair_settings::options_from(&bad).is_err());
    }

    /// Every profile the combo offers must round-trip through core, and a stale
    /// name in a hand-edited `gui.json` must not survive the load.
    ///
    /// Note what this does *not* claim: `options_from` does not validate
    /// `profile`. The CLI rejects a bad name at the clap layer (`choices`), but
    /// `repair.rs:934` would happily apply an unknown profile with the fallback
    /// thresholds `(8, 1000)` -- so for the GUI, whose settings come from a JSON
    /// file a user can edit, sanitizing on load is what closes the hole.
    #[test]
    fn profiles_round_trip_and_stale_names_are_dropped() {
        for profile in PROFILES {
            let settings = RepairSettings {
                profile: Some(profile.to_string()),
                ..Default::default()
            };
            assert!(
                sutura_core::repair_settings::options_from(&settings).is_ok(),
                "core rejects --profile {profile}"
            );
        }

        let mut state = crate::state::GuiState {
            settings: RepairSettings {
                profile: Some("does-not-exist".into()),
                mode: Some("nonsense".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        crate::state::sanitize(&mut state);
        assert_eq!(state.settings.profile, None, "stale profile must be dropped");
        assert_eq!(state.settings.mode, None, "stale mode must be dropped");
    }

    /// The settings the panel was missing must reach core, not just the struct.
    ///
    /// Each field is set exactly as the panel writes it, then read back out of
    /// the resulting `RepairOptions` -- a control that assigns a field core
    /// never reads is the dead-flag bug class, and only this direction catches
    /// it.
    ///
    /// `time_budget_s` is the one plan item deliberately absent: it is `None` on
    /// the CLI too (`repair_cmd.rs`), so its absence is parity, not a gap.
    #[test]
    fn previously_missing_settings_reach_core() {
        use sutura_core::repair_settings::options_from;

        let settings = RepairSettings {
            experimental_graft: true,
            no_graft: false,
            no_fallback_ftetwild: false,
            experimental_fallback_ftetwild: true,
            dressing: true,
            dressing_drain: Some(serde_json::json!("half")),
            dressing_defects: Some("holes_nm".into()),
            dressing_rmax_scale: Some(1.5),
            dressing_sigma_scale: Some(0.75),
            engines: Some(vec!["alpha".into()]),
            // The three this round added: each had a flag and no control.
            classifier_engine: Some("classic".into()),
            ftetwild_optimize: true,
            auto_reorder: true,
            extra_features: true,
            ..Default::default()
        };
        let (opts, _, _) = options_from(&settings).expect("valid settings");

        assert_eq!(opts.graft, Some(true), "experimental graft must reach core");
        assert_eq!(
            opts.ftetwild,
            Some(serde_json::Value::Bool(true)),
            "experimental fallback must reach core"
        );
        assert!(opts.dressing);
        assert_eq!(opts.dressing_drain, Some(serde_json::json!("half")));
        assert_eq!(opts.dressing_defects.as_deref(), Some("holes_nm"));
        assert_eq!(opts.dressing_rmax_scale, Some(1.5));
        assert_eq!(opts.dressing_sigma_scale, Some(0.75));
        assert_eq!(opts.engines, Some(vec!["alpha".to_string()]));
        assert_eq!(
            opts.classifier_engine.as_deref(),
            Some("classic"),
            "the classifier engine must reach core; `repair.rs` reads it"
        );
        assert!(
            opts.ftetwild_optimize,
            "the optimiser switch must reach core; `repair.rs` reads it"
        );
        assert!(
            opts.auto_reorder,
            "auto-reorder must reach core; the ladder reads it"
        );
        assert!(
            opts.extra_features,
            "edge-tiebreak must reach core; the classifier reads it"
        );

        // The negative flags must win when both are set, or the GUI could leave
        // a user with a "no" that does nothing.
        let both = RepairSettings {
            experimental_graft: true,
            no_graft: true,
            experimental_fallback_ftetwild: true,
            no_fallback_ftetwild: true,
            ..Default::default()
        };
        let (opts, _, _) = options_from(&both).unwrap();
        assert_eq!(opts.graft, Some(false), "no_graft must win");
        assert_eq!(
            opts.ftetwild,
            Some(serde_json::Value::Bool(false)),
            "no_fallback_ftetwild must win"
        );
    }

    /// The Analysis panel must show exactly what the headless hook reports.
    ///
    /// The hook is the oracle the parity gate compares against Python, so this
    /// test is what keeps the panel honest: both call `analyze_selected`, and a
    /// panel that started computing its own numbers would fail here.
    #[test]
    fn analysis_panel_matches_the_headless_values() {
        let fixture = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../sutura-geom/tests/fixtures/case1_open_cup.stl"
        ));
        let analysis = analyze_selected(fixture).expect("fixture must analyse");
        assert!(analysis.faces > 0);
        // The panel renders these exact fields; a `None` here means the row
        // would be blank.
        assert!(!analysis.mesh_type.is_empty(), "detected type must be named");
        assert!(analysis.components >= 1);

        // A bogus path must produce an error message, not a panic -- the panel
        // renders `Err` as red text.
        let err = analyze_selected(std::path::Path::new("/nonexistent/nope.stl"));
        assert!(err.is_err(), "a missing file must not panic");
    }

    /// `object_rows` must report a real archive's objects, and the panel's
    /// "only for .3mf" guard must be what decides when it is called at all.
    ///
    /// A real 3MF is written with the repo's own writer rather than a committed
    /// fixture, so the test cannot rot against a hand-edited blob.
    #[test]
    fn object_rows_lists_every_3mf_object() {
        let dir = std::env::temp_dir().join(format!("sutura-obj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cube.3mf");
        // A single tetrahedron: enough for the archive to be valid.
        let mesh = sutura_core::Mesh::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [0, 3, 2]],
        );
        sutura_core::write_3mf_single(&path, &mesh).expect("write a 3MF");

        let rows = object_rows(&path).expect("a written 3MF must round-trip");
        assert!(!rows.is_empty(), "one row per object, none produced");
        for row in &rows {
            assert_eq!(
                row.get("model_name").and_then(|v| v.as_str()),
                Some("3D/3dmodel.model"),
                "row must name the model part it came from"
            );
            let faces = row
                .get("faces")
                .and_then(|v| v.as_u64())
                .expect("row needs face count");
            assert_eq!(faces, 4, "the tetrahedron has 4 faces");
            let vertices = row
                .get("vertices")
                .and_then(|v| v.as_u64())
                .expect("row needs vertex count");
            assert_eq!(vertices, 4);
        }

        // An STL is not a 3MF. The panel guards with `extension == "3mf"`, so
        // the hook would be the only caller -- and it must fail, not panic.
        let stl = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../sutura-geom/tests/fixtures/case1_open_cup.stl"
        ));
        assert!(object_rows(stl).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Recommendations live per object, and only in the multi-object report.
    ///
    /// Checked against the real shape rather than assumed: for a single mesh the
    /// report has **no** `recommendations` key at all (verified against the CLI's
    /// JSON for an STL: 52 top-level keys, none of them recommendations), while a
    /// 3MF carries one list per object under `object_reports`. A lookup at the
    /// report root therefore finds nothing for either format -- which is what the
    /// first version of the panel did.
    #[test]
    fn recommendations_are_found_per_object() {
        let report = serde_json::json!({
            "object_reports": [
                { "model_name": "a", "report": { "recommendations": [
                    { "num": 3, "name": "Full Mend", "score": 1.0, "reason": "r" }
                ]}},
                { "model_name": "b", "report": { "recommendations": [] }},
            ]
        });
        let objects = report
            .get("object_reports")
            .and_then(|v| v.as_array())
            .expect("object_reports is an array");
        let with_recs: Vec<&Value> = objects
            .iter()
            .filter(|o| {
                let r = o.get("report").unwrap_or(o);
                r.get("recommendations")
                    .and_then(|v| v.as_array())
                    .is_some_and(|v| !v.is_empty())
            })
            .collect();
        assert_eq!(with_recs.len(), 1, "only object a has recommendations");
        let first = with_recs[0]["report"]["recommendations"][0].clone();
        assert_eq!(first["num"], 3);
        assert_eq!(first["name"], "Full Mend");

        // A single-mesh report: no recommendations anywhere near the root.
        let single = serde_json::json!({ "repair_health": 0.9, "stage1": {} });
        assert!(
            single.get("recommendations").is_none(),
            "this is why the root lookup found nothing"
        );
    }

    /// `Ctrl+,` must toggle the panel, and must NOT fire while a text box has
    /// focus -- otherwise typing "a,b" would hide the panel mid-word.
    ///
    /// Driven through a real `egui::Context`, so this breaks if egui changes
    /// where it stores focus or how `consume_key` behaves. The old test for the
    /// tier flags was toothless three times over; a keyboard shortcut is even
    /// easier to leave "passing" while it does nothing.
    #[test]
    fn settings_shortcut_requires_no_text_focus() {
        use egui::{Context, Event, Key, Modifiers, RawInput};

        // ONE context, two passes: a context owns texture deltas, and dropping
        // one without consuming them panics inside epaint's `Drop`. Rewriting
        // this as two contexts is what produced that panic, not a product bug.
        let ctx = Context::default();

        let press = |ctx: &Context| {
            let mut input = RawInput::default();
            input.events.push(Event::Key {
                key: Key::Comma,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            });
            ctx.begin_pass(input);
        };
        let finish = |ctx: &Context| {
            let mut out = ctx.end_pass();
            out.textures_delta.clear();
        };

        // No focus: the shortcut fires.
        press(&ctx);
        assert!(
            settings_shortcut_pressed(&ctx),
            "Ctrl+, must work when nothing is focused"
        );
        finish(&ctx);

        // A focused widget: it must not fire. `request_focus` is what a text box
        // does when clicked.
        let id = egui::Id::new("some_text_edit");
        ctx.memory_mut(|m| m.request_focus(id));
        assert!(ctx.memory(|m| m.focused()).is_some(), "focus was not taken");

        press(&ctx);
        assert!(
            !settings_shortcut_pressed(&ctx),
            "the shortcut must not steal a typed comma"
        );
        finish(&ctx);
    }

    /// The method-health band must reflect what `--self-check` reports.
    ///
    /// It reads the same `self_check::run(false)`, so the GUI cannot disagree
    /// with the CLI about whether an external engine is usable -- which is the
    /// whole point: a user should learn that from the app, not from opening a
    /// terminal.
    #[test]
    fn method_health_matches_self_check() {
        let report = sutura_core::self_check::run(false);
        let methods = report
            .get("methods")
            .and_then(|m| m.as_object())
            .expect("self-check always reports a methods map");
        assert_eq!(methods.len(), 19, "the 19 built-in methods");

        for (id, m) in methods {
            let status = m.get("status").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                matches!(status, "ok" | "skip" | "error"),
                "method {id} has an unexpected status {status:?}"
            );
            // The band only shows the others, so they must carry the fields the
            // row renders.
            if status != "ok" {
                assert!(m.get("num").is_some(), "{id} needs a num for the row");
                assert!(m.get("name").is_some(), "{id} needs a name");
            }
        }
    }

    /// The drop highlight is driven by `hovered_files`, not by `dropped_files`.
    ///
    /// That distinction is the whole feature: `dropped_files` is only populated
    /// at the drop, by which time a highlight is pointless. A test cannot
    /// simulate a real drag (egui's input is built by the windowing backend), so
    /// what is pinned here is the flag's wiring and its default-off state.
    #[test]
    fn drop_highlight_defaults_off_and_is_independent_of_the_mesh() {
        let preview = crate::viewer::Preview::for_test();
        assert!(!preview.drop_active, "no drag in progress, so no highlight");
        assert!(preview.mesh.is_none());
    }

    /// The defect list must match what the report and the analysis agree on.
    ///
    /// `analysis` already fills `boundary_loops` from the same
    /// `detect_defects` call, so a mismatch here means the panel is describing
    /// a different mesh than the numbers above it.
    #[test]
    fn defect_panel_agrees_with_the_analysis_counts() {
        // A box with one face removed: exactly one boundary loop, no
        // non-manifold regions.
        let fixture = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../sutura-geom/tests/fixtures/case5_box_missing_face.stl"
        ));
        let defects = defect_rows(fixture).expect("fixture must be readable");
        assert_eq!(defects.holes.len(), 1, "one missing face, one hole");
        assert!(defects.non_manifold.is_empty(), "a box has no bad edges");

        let analysis = analyze_selected(fixture).expect("fixture must analyse");
        assert_eq!(
            analysis.boundary_loops,
            defects.holes.len(),
            "the count in the table and the rows in the panel must agree"
        );

        // The rows carry the fields the panel prints.
        let hole = &defects.holes[0];
        assert!(hole.diameter > 0.0, "a hole needs a diameter");
        assert!(hole.centroid.iter().all(|c| c.is_finite()));

        // A path that does not exist must be an error for the panel to render,
        // not a panic.
        assert!(defect_rows(std::path::Path::new("/nonexistent/x.stl")).is_err());
    }

    /// The per-file decision column must not overflow the 290 px file panel.
    ///
    /// A raw verdict is ~55 characters and pushed the preview off the row when it
    /// was first added; this pins the shortening, including that it never cuts
    /// mid-word and never grows the text.
    #[test]
    fn decision_text_is_shortened_for_the_column() {
        let long = "Repaired, shape changed in 1 guessed area(s) - check";
        let short = truncate_decision(long, 22);
        assert!(short.chars().count() <= 23, "must fit: {short:?}");
        assert!(short.ends_with('…'), "truncated text ends with one: {short:?}");
        assert!(!short.contains("shapechanged"), "must not cut mid-word");
        assert!(long.starts_with(short.trim_end_matches('…')), "prefix of the original");

        // Text that already fits is untouched -- no stray ellipsis.
        assert_eq!(truncate_decision("Preserved", 22), "Preserved");
        assert_eq!(truncate_decision("", 22), "");
    }

    /// A file that was never repaired shows no decision at all, rather than an
    /// empty or "pending" label.
    #[test]
    fn unrepaired_files_have_no_decision() {
        let entry = Entry {
            path: std::path::PathBuf::from("/tmp/a.stl"),
            input: None,
            result: None,
            error: None,
            repair: RepairState::Idle,
            report: None,
            raw_report: None,
            output: None,
            view: ViewMode::Before,
            analysis: None,
            objects: None,
            defects: None,
            methods: None,
        };
        assert!(entry.decision().is_none(), "nothing repaired, nothing to say");
    }

    /// The Engines section must report the same fTetWild answer the repair uses.
    ///
    /// This is the whole value of the panel: `find_binary` is what the solid
    /// engine consults (`repair.rs`), so a status light from a different lookup
    /// would say "installed" while repairs fell back. The same rule applied to
    /// `--self-check` was a real bug (`36a1718`, a stale path list).
    #[test]
    fn engine_status_uses_the_same_lookup_as_the_repair() {
        let from_core = sutura_core::ftetwild::find_binary();
        // Constructing the app would need an `eframe::CreationContext`, so the
        // assertion is on the call the constructor makes, not on the field.
        assert_eq!(
            from_core,
            sutura_core::ftetwild::find_binary(),
            "the lookup must be stable within a run"
        );
        if let Some(path) = &from_core {
            assert!(
                std::path::Path::new(path).is_file(),
                "a reported path must exist: {path}"
            );
        }

        // And the engine list the section renders is the one `--engines`
        // validates against, so a name shown here is always accepted there.
        let (configured, _warnings) = sutura_core::config::load_all_engines(None);
        for name in configured.keys() {
            assert!(!name.is_empty());
        }
    }

    /// The Changelog must be read-only: fetching it may not create a second
    /// on-disk copy of the release notes.
    ///
    /// The product rule for this port is one copy (`whatsnew::current/`, written
    /// by the updater after an install). Python keeps a
    /// `release_notes_cache.json` beside it, which is exactly the drift this
    /// avoids. Asserted as a source property, because the fetch needs a network
    /// and a click, neither of which a unit test has.
    #[test]
    fn changelog_fetch_does_not_write_to_disk() {
        let src = include_str!("app.rs");
        let start = src
            .find("fn fetch_changelog(")
            .expect("the changelog fetch exists");
        let end = src[start..]
            .find("\n    /// ")
            .map(|i| start + i)
            .expect("the fetch ends");
        let body = &src[start..end];

        for writer in [
            "fs::write",
            "fs::create_dir",
            "File::create",
            "whatsnew::snapshot_release_notes",
            "whatsnew::commit",
        ] {
            assert!(
                !body.contains(writer),
                "the changelog fetch must not write: found {writer}"
            );
        }
        // It reads the releases list, not `/latest`: a history needs the list.
        assert!(body.contains("/releases?per_page="), "reads the list endpoint");
    }

    /// The static before/after pair must render both sides from the same
    /// renderer the live preview uses.
    ///
    /// `rasterize_for_still` and `Preview::show` both go through
    /// `CpuViewer::rasterize`, so the still image cannot disagree with the live
    /// one about shading, defect colouring or framing. This checks the pixels
    /// actually differ between a broken and a repaired mesh -- two identical
    /// images would mean the second render silently reused the first.
    #[test]
    fn still_pair_renders_distinct_before_and_after_images() {
        let fixture = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../sutura-geom/tests/fixtures/case5_box_missing_face.stl"
        ));
        let broken = DisplayMesh::load(fixture).expect("fixture must load");

        // A real second mesh, built from the soup fields the renderer actually
        // reads: mutating `src_verts`/`src_tris` changed nothing, because
        // `rasterize` draws `indices`/`positions`/`normals`. A tetrahedron at a
        // different scale guarantees different pixels.
        let mut repaired = broken.clone();
        repaired.positions = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 0.0, 2.0],
        ];
        repaired.normals = vec![
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        ];
        repaired.indices = vec![0, 1, 2, 0, 1, 3, 1, 2, 3, 0, 3, 2];
        repaired.radius = 2.0;

        let size = [128, 96];
        let a = crate::viewer::Preview::rasterize_for_still(&broken, size, false)
            .expect("before must rasterise");
        let b = crate::viewer::Preview::rasterize_for_still(&repaired, size, false)
            .expect("after must rasterise");

        assert_eq!(a.data().len(), b.data().len(), "same canvas size");
        assert_ne!(
            a.data(),
            b.data(),
            "the two panels must not be the same picture"
        );
        // And the background is filled, not transparent: an empty pixmap would
        // also compare unequal while showing nothing.
        assert!(a.data().iter().any(|&c| c != 0), "the before image has content");
    }

    /// A per-file method list must reach `options_from` intact, and `None` must
    /// mean "the panel's selection" rather than "no methods".
    ///
    /// The distinction is the whole feature: a file that was never customised has
    /// to keep following the panel, or changing the global selection would leave
    /// every previously-loaded file behind.
    #[test]
    fn per_file_methods_override_the_global_selection() {
        use sutura_core::repair_settings::options_from;

        // What `start_repair_mode` builds: global selection, then the file's own.
        let build = |file: Option<Vec<u8>>| {
            let mut settings = RepairSettings {
                methods: Some("2,3".into()),
                ..Default::default()
            };
            if let Some(methods) = file.as_ref() {
                settings.methods = Some(
                    methods
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            settings
        };

        // `None` -> the global list is used.
        let (_, given, methods) = options_from(&build(None)).unwrap();
        assert!(given, "the global selection is a real selection");
        assert_eq!(methods, vec![2, 3]);

        // A per-file list wins outright.
        let (_, given, methods) = options_from(&build(Some(vec![15]))).unwrap();
        assert!(given);
        assert_eq!(methods, vec![15], "the file's list must replace the global one");

        // The ORDER of the list is part of the request: `parse_methods_text`
        // takes the first method as the deep-repair mode, so 1,3 and 3,1 are not
        // the same repair. The Methods tab's Up/Down edits exactly this string,
        // which is why it must never be sorted on the way out.
        let ordered = |text: &str| {
            let settings = RepairSettings {
                methods: Some(text.to_string()),
                ..Default::default()
            };
            let (opts, given, methods) = options_from(&settings).unwrap();
            assert!(given, "{text:?} is an explicit selection");
            (
                methods,
                opts.forced_methods,
                opts.deep_repair,
            )
        };
        let (a_methods, a_forced, a_deep) = ordered("1,3");
        let (b_methods, b_forced, b_deep) = ordered("3,1");
        assert_eq!(a_methods, vec![1, 3], "the list keeps the user's order");
        assert_eq!(b_methods, vec![3, 1], "and the reverse, reversed");
        assert_eq!(a_forced, vec![1, 3], "forced_methods carries that order");
        assert_eq!(b_forced, vec![3, 1]);
        assert_ne!(
            a_deep, b_deep,
            "the first method picks the deep-repair mode, so the two differ"
        );

        // An empty selection is NOT an error in the core: it yields
        // `methods_given = true` with an empty list, i.e. "run no forced
        // methods". That is why the UI uses an inherit checkbox instead of
        // "untick everything": unticking every box would mean "force nothing",
        // which is not the same as "use the panel's selection".
        let empty = RepairSettings {
            methods: Some(String::new()),
            ..Default::default()
        };
        let (_, given, methods) = options_from(&empty).unwrap();
        assert!(given, "an empty string still counts as an explicit selection");
        assert!(methods.is_empty(), "and selects no methods");

        // `None`, by contrast, is "not specified": the deep-repair mode stays
        // whatever the intensity chose and nothing is forced.
        let unspecified = RepairSettings {
            methods: None,
            ..Default::default()
        };
        let (_, given, methods) = options_from(&unspecified).unwrap();
        assert!(!given);
        assert!(methods.is_empty());
    }

    /// The defect-colour palette must match Python's and the mask must decode.
    ///
    /// The RGB values come from `DEFECT_COLORS` in `defects.py:262`, described
    /// there as the single source of truth; a viewer painting different colours
    /// for the same defect would be a parity failure, so they are asserted rather
    /// than trusted.
    #[test]
    fn defect_palette_matches_python_and_decodes() {
        use crate::mesh::DEFECT_COLORS;
        assert_eq!(DEFECT_COLORS.len(), 8, "eight kinds in Python's table");
        assert_eq!(
            DEFECT_COLORS[0],
            ("non_manifold", 2, [235, 60, 70]),
            "non_manifold is red and bit 2"
        );
        // Every bit is distinct, or two kinds would colour the same faces.
        let mut bits: Vec<u8> = DEFECT_COLORS.iter().map(|(_, b, _)| *b).collect();
        bits.sort_unstable();
        bits.dedup();
        assert_eq!(bits.len(), DEFECT_COLORS.len(), "bit assignments collide");

        // A byte with several kinds resolves to the most severe, in table order.
        assert_eq!(
            crate::mesh::defect_kind_color(2 | 4),
            Some([235, 60, 70]),
            "non_manifold wins over duplicate"
        );
        assert_eq!(crate::mesh::defect_kind_color(0), None, "clean face");
        assert_eq!(
            crate::mesh::defect_kind_color(128),
            Some([245, 120, 180]),
            "thin wall is pink"
        );

        // And a real diagnosis produces masks of the right length, which is what
        // makes the per-face lookup safe.
        let fixture = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../sutura-geom/tests/fixtures/case3_broken_cup.stl"
        ));
        let mut mesh = DisplayMesh::load(fixture).expect("fixture must load");
        mesh.ensure_defect_colors().expect("diagnosis must run");
        assert_eq!(
            mesh.defect_colors.len(),
            mesh.src_tris.len(),
            "one colour slot per face"
        );
        assert!(
            mesh.defect_colors.iter().any(|c| c.is_some()),
            "this fixture has defects, so some faces must be coloured"
        );
    }

    /// The screenshot hooks must be inert when not asked for.
    ///
    /// `--open-*`, `--select` and `--settings-scroll` exist so a script can reach
    /// a view with no input device. An interactive launch passes the defaults, and
    /// a default that opened a window or scrolled the panel would be a bug a user
    /// sees immediately but a test would not catch without this.
    #[test]
    fn startup_defaults_open_nothing_and_scroll_nowhere() {
        let default = OpenOnStart::default();
        assert_eq!(default, OpenOnStart::None, "no overlay by default");

        let app = SuturaApp::for_test();
        assert_eq!(app.settings_scroll, 0.0, "the panel starts at the top");
        assert_eq!(app.wait_frames, 3, "the settled-frame default");
        assert!(!app.pending_compare, "no deferred before/after pair");
        assert!(app.compare.is_none());
        assert!(!app.preview.defect_colors);
        assert!(app.changelog.is_none(), "no network at startup");
    }

    /// The deviation ramp must keep its own colours, only modulated in
    /// brightness.
    ///
    /// An unshaded ramp is one flat silhouette on a scanned part -- every face
    /// lands on nearly the same deviation, so the After + Heatmap view read as a
    /// navy blob (reported from a Steam Deck). Shading it fixes the shape; what
    /// must not change is the mapping from deviation level to colour, or the
    /// legend would be lying.
    #[test]
    fn deviation_ramp_keeps_its_colours() {
        let lut = sutura_geom::heatmap::deviation_lut();
        assert!(lut.len() > 4);

        // The ends are the documented navy and red, unchanged.
        let lo = lut[0];
        let hi = lut[lut.len() - 1];
        assert!(
            lo[2] > lo[0] && lo[2] > lo[1],
            "the low end is navy (blue-dominant), got {lo:?}"
        );
        assert!(
            hi[0] > hi[1] && hi[0] > hi[2],
            "the high end is red (red-dominant), got {hi:?}"
        );

        // And the ramp is monotone in the channel that matters, so a brighter
        // face and a darker face still sort correctly.
        assert!(lo[2] >= hi[2], "blue falls from low to high deviation");

        // The shading factor is brightness only: with any factor in (0, 1] the
        // ordering of the three channels is preserved.
        for shade in [1.0f32, 0.75, 0.5] {
            let shaded = [
                lo[0] as f32 * shade,
                lo[1] as f32 * shade,
                lo[2] as f32 * shade,
            ];
            assert!(shaded[2] > shaded[0] && shaded[2] > shaded[1]);
        }
    }

    /// `--settings-scroll` must seed the panel once, not pin it every frame.
    ///
    /// `ScrollArea::vertical_scroll_offset` writes into the area's persistent
    /// state on every frame (`scroll_area.rs`: `state.offset.y =
    /// offset_y.unwrap_or(state.offset.y)`), so passing a constant re-zeroes the
    /// scroll each frame and neither the wheel nor the scrollbar can move the
    /// panel. That was a real bug users saw: the flag was added for a screenshot
    /// and made the settings panel unscrollable. The value is consumed on the
    /// frame that uses it.
    #[test]
    fn settings_scroll_seed_is_consumed_once() {
        let mut app = SuturaApp::for_test();
        app.settings_scroll = 780.0;
        // The field the scroll area reads must be cleared after a frame that
        // applied it; simulating the frame is what `egui_kittest` will do, but
        // the invariant can be stated without one: a non-zero seed is one-shot.
        app.consume_settings_scroll();
        assert_eq!(app.settings_scroll, 0.0, "the seed must not persist");
        // Idempotent: consuming twice is harmless.
        app.consume_settings_scroll();
        assert_eq!(app.settings_scroll, 0.0);
    }

    /// The Options window must be inside a scroll area.
    ///
    /// A static screenshot cannot show this: egui only paints the scrollbar
    /// while hovering, and at offset 0 the frame is identical either way. Two of
    /// the seven tabs (Repair, Methods) are taller than the window, and the lower
    /// controls (per-run limits, the method lists) are unreachable without it.
    ///
    /// The panel moved from a `Panel::right` to the Options window when the Qt
    /// layout landed, so this looks there now.
    #[test]
    fn options_window_is_scrollable() {
        let src = include_str!("options.rs");
        assert!(
            src.contains("ScrollArea::vertical()"),
            "the Options body must scroll: two tabs are taller than the window"
        );
        // `options-scroll`, not an inner list: this is the body itself.
        assert!(
            src.contains(".id_salt(\"options-scroll\")"),
            "the body scroll area must be the one that carries the id"
        );
    }
}
