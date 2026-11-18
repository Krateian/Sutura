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
enum ViewMode {
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
}

enum RepairEvent {
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
}

pub struct SuturaApp {
    files: Vec<Entry>,
    selected: Option<usize>,
    loaded: Option<(usize, ViewMode)>,
    language: Language,
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
    settings: RepairSettings,
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
    updater: crate::updater_ui::Updater,
    /// The "what's new" window, reading the snapshot off disk.
    notes_open: bool,
    screenshot_path: Option<PathBuf>,
    screenshot_requested: bool,
    exit_after_screenshot: bool,
    auto_repair: bool,
    auto_repair_started: bool,
    ready_frames: u8,
    /// Serialized form of the last persisted state. Compared at the end of every
    /// frame so a write happens only when something actually changed -- the
    /// settings panel mutates `self.settings` in a dozen places and hooking each
    /// one would drift.
    persisted: String,
    /// `Ctrl+,` toggles the settings panel. Python's only shortcut opens its
    /// options dialog; settings live in a permanent panel here, so the honest
    /// equivalent is to show or hide that panel.
    settings_open: bool,
    /// Method availability, from `self_check::run`. Process-wide, so it is
    /// computed once at startup rather than per file -- and with
    /// `run_fixtures:false`, the same cheap availability probe `--self-check`
    /// does without `--self-check-fixtures`.
    method_health: Vec<Value>,
    /// Set when a panic was recorded in this process. The dialog is the whole
    /// point of the crash log: a trace nobody is told about is not reporting.
    crash_open: bool,
    /// Result message of the last diagnostics export, shown in the dialog.
    diagnostics_note: Option<String>,
}

impl SuturaApp {
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
            persisted: String::new(),
            settings_open: true,
            method_health: Vec::new(),
            crash_open: false,
            diagnostics_note: None,
        }
    }

    pub fn new(
        cc: &eframe::CreationContext<'_>,
        files: Vec<PathBuf>,
        language: Language,
        options: StartupOptions,
    ) -> Self {
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
            // Left empty on purpose: the first frame writes the current state
            // out, which creates the file and makes the load/save loop
            // observable. Seeding it with `state_json()` instead would mean a
            // first launch never wrote anything, and the whole path stayed
            // unexercised until some control happened to change.
            persisted: String::new(),
            settings_open: true,
            method_health,
            crash_open: crate::crash::panic_recorded(),
            diagnostics_note: None,
        };
        for path in files {
            app.add_path(path);
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
                }
            }
        };
        self.files.push(entry);
        self.selected = Some(self.files.len() - 1);
    }

    /// Open the repeat picker on the selected file's input mesh.
    fn open_repeat_picker(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(entry) = self.files.get(idx) else { return };
        let Some(mesh) = entry.input.clone() else { return };
        self.repeat_picker = Some(crate::repeat_picker::RepeatPicker::new(&mesh));
    }

    /// Write a diagnostics archive wherever the user says.
    ///
    /// Same builder the CLI's `--diagnostics` uses (`diagnostics::create_archive`),
    /// so a report from the GUI and one from the terminal contain the same
    /// files. Defaulted to the data directory so the button always produces
    /// something even if the picker is dismissed.
    fn export_diagnostics(&mut self) {
        let suggested = sutura_core::config::data_dir().join("sutura-diagnostics.zip");
        let target = rfd::FileDialog::new()
            .set_file_name(suggested.file_name().unwrap_or_default().to_string_lossy())
            .save_file()
            .unwrap_or(suggested);
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
        };
        serde_json::to_string(&state).unwrap_or_default()
    }

    /// Write the settings out when they differ from what was last written.
    ///
    /// Called once per frame, so it must be cheap: `to_string` on a small
    /// struct, then a file write only on an actual change. Saving
    /// unconditionally would touch the disk 60 times a second.
    fn persist_if_changed(&mut self) {
        let now = self.state_json();
        if now == self.persisted {
            return;
        }
        let state = crate::state::GuiState {
            settings: self.settings.clone(),
            intensity: Some(self.intensity.value().to_string()),
            language: Some(self.language.code().to_string()),
        };
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
        }
        self.loaded = Some(key);
    }

    fn log(&mut self, line: String) {
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
        let settings = self.settings.clone();
        std::thread::spawn(move || {
            run_repair(&input, &output, settings, tx, cancel);
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
        if self.ready_frames < 3 {
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
        let ctx = ui.ctx().clone();
        // Deferred so the panel closure can set it while `self.files` is borrowed.
        let mut clear_list = false;
        self.poll_repair();
        // Updater events drive its panel; a change means a repaint.
        if self.updater.poll() {
            ctx.request_repaint();
        }

        if settings_shortcut_pressed(&ctx) {
            self.settings_open = !self.settings_open;
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
        if self.auto_repair && !self.auto_repair_started {
            if let Some(idx) = self.selected {
                self.auto_repair_started = true;
                self.start_repair(idx, false);
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

        egui::containers::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(rust_i18n::t!("app.title").to_string());
                ui.separator();
                if ui.button(rust_i18n::t!("app.add_files").to_string()).clicked() {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter("Meshes", &["stl", "obj", "ply", "3mf"])
                        .pick_files()
                    {
                        for path in paths {
                            self.add_path(path);
                        }
                    }
                }
                let can_repair = self
                    .selected
                    .and_then(|i| self.files.get(i))
                    .map(|e| e.input.is_some() && !matches!(e.repair, RepairState::Running { .. }))
                    .unwrap_or(false);
                if ui
                    .add_enabled(
                        can_repair,
                        egui::Button::new(rust_i18n::t!("app.repair").to_string()),
                    )
                    .clicked()
                {
                    if let Some(idx) = self.selected {
                        self.start_repair(idx, false);
                    }
                }
                let can_batch = self.files.iter().any(|e| e.input.is_some())
                    && !self.batch_active
                    && self.repair_rx.is_none();
                if ui
                    .add_enabled(
                        can_batch,
                        egui::Button::new(rust_i18n::t!("app.repair_all").to_string()),
                    )
                    .clicked()
                {
                    self.enqueue_all();
                }
                if running
                    && ui
                        .button(rust_i18n::t!("app.cancel").to_string())
                        .clicked()
                {
                    cancel_clicked = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("language")
                        .selected_text(self.language.label())
                        .show_ui(ui, |ui| {
                            for lang in [Language::System, Language::En, Language::Tr] {
                                if ui
                                    .selectable_value(&mut self.language, lang, lang.label())
                                    .clicked()
                                {
                                    i18n::apply(lang);
                                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                                        rust_i18n::t!("app.title").to_string(),
                                    ));
                                }
                            }
                        });
                    ui.label(rust_i18n::t!("app.language").to_string());
                    if ui.button(rust_i18n::t!("app.updates").to_string()).clicked() {
                        self.updater_open = true;
                    }
                    if ui.button(rust_i18n::t!("app.repeat_pick").to_string()).clicked() {
                        self.open_repeat_picker();
                    }
                    if ui.button(rust_i18n::t!("app.about").to_string()).clicked() {
                        self.about_open = true;
                    }
                    if ui
                        .button(rust_i18n::t!("app.diagnostics").to_string())
                        .on_hover_text(rust_i18n::t!("app.diagnostics_tip").to_string())
                        .clicked()
                    {
                        self.export_diagnostics();
                    }
                });

            });
                // Method-health band: one dot per method, from the same
                // availability probe `--self-check` runs. Only shown when
                // something is NOT fine -- a green strip every launch is noise,
                // and the point is to notice a broken external engine without
                // opening a terminal.
                let unhealthy: Vec<&Value> = self
                    .method_health
                    .iter()
                    .filter(|m| {
                        m.get("status").and_then(|v| v.as_str()) != Some("ok")
                    })
                    .collect();
                if !unhealthy.is_empty() {
                    // Plain `horizontal`, not `horizontal_wrapped`: wrapping
                    // inside the top panel made it claim the full window height
                    // (verified in a screenshot -- the whole preview was pushed
                    // off-screen). The list is short enough not to need it.
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(
                                rust_i18n::t!("app.method_health", count = unhealthy.len() as i64)
                                    .to_string(),
                            )
                            .small()
                            .weak(),
                        );
                        for m in unhealthy {
                            let num = m.get("num").and_then(|v| v.as_u64()).unwrap_or(0);
                            let name = m
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let status = m
                                .get("status")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let reason = m
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let colour = if status == "skip" {
                                egui::Color32::from_rgb(0xc0, 0xa0, 0x40)
                            } else {
                                egui::Color32::LIGHT_RED
                            };
                            ui.label(
                                egui::RichText::new(format!("#{num} {status}"))
                                    .small()
                                    .color(colour),
                            )
                            .on_hover_text(format!("{name}: {reason}"));
                        }
                    });
                }
        });

        egui::containers::Panel::left("files")
            .resizable(true)
            .default_size(290.0)
            .show(ui, |ui| {
                ui.heading(rust_i18n::t!("app.files").to_string());
                ui.horizontal(|ui| {
                    let can_clear = !self.files.is_empty() && !self.busy();
                    if ui
                        .add_enabled(
                            can_clear,
                            egui::Button::new(rust_i18n::t!("app.clear_list").to_string()),
                        )
                        .clicked()
                    {
                        clear_list = true;
                    }
                });
                ui.separator();
                let mut clicked = None;
                let mut remove = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (idx, entry) in self.files.iter().enumerate() {
                        let name = entry
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| entry.path.display().to_string());
                        let selected = self.selected == Some(idx);
                        let mut text = egui::RichText::new(name);
                        if entry.error.is_some() {
                            text = text.color(egui::Color32::LIGHT_RED);
                        }
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selected, text).clicked() {
                                clicked = Some(idx);
                            }
                            // Removing the file being repaired would orphan the
                            // worker's index, so it is disabled while busy.
                            // Plain "x": egui's bundled font has no U+2715, so
                            // "✕" rendered as a missing-glyph box.
                            if ui
                                .add_enabled(!self.busy(), egui::Button::new(" x "))
                                .on_hover_text(
                                    rust_i18n::t!("app.remove_file_tip").to_string(),
                                )
                                .clicked()
                            {
                                remove = Some(idx);
                            }
                        });
                        match &entry.repair {
                            RepairState::Running { step, total } => {
                                ui.add(
                                    egui::ProgressBar::new(*step as f32 / (*total).max(1) as f32)
                                        .text(
                                            rust_i18n::t!(
                                                "app.progress",
                                                step = *step as i64,
                                                total = *total as i64
                                            )
                                            .to_string(),
                                        ),
                                );
                            }
                            RepairState::Failed(e) => {
                                ui.label(
                                    egui::RichText::new(e).small().color(egui::Color32::LIGHT_RED),
                                );
                            }
                            _ => {}
                        }
                        if let Some(e) = &entry.error {
                            ui.label(egui::RichText::new(e).small().color(egui::Color32::LIGHT_RED));
                        }
                    }
                });
                if let Some(idx) = clicked {
                    self.selected = Some(idx);
                }
                if let Some(idx) = remove {
                    self.remove_file(idx);
                }
            });

        if clear_list {
            self.clear_files();
        }

        let mut open_action: Option<(PathBuf, bool)> = None;
        let mut cancel_in_panel = false;
        // Not creating the panel is what hides it: the central preview then
        // takes the space back on its own.
        if self.settings_open {
        egui::containers::Panel::right("settings")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                // Scrollable: on a 720-px window (the stated minimum)
                // the per-run limits and the method list ran off the bottom
                // and were unreachable. Same pattern as the other panels.
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    // Test hook, same spirit as `--screenshot`: the panel is
                    // taller than any window, so proving a control
                    // near its bottom (the analysis table, the method list)
                    // requires starting the scroll somewhere other than 0.
                    // Unset means normal behaviour; costs one env lookup a frame.
                    .vertical_scroll_offset(
                        std::env::var("SUTURA_GUI_SCROLL")
                            .ok()
                            .and_then(|v| v.parse::<f32>().ok())
                            .unwrap_or(0.0),
                    )
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    ui.heading(rust_i18n::t!("app.settings").to_string());
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(rust_i18n::t!("app.intensity").to_string());
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
                            });
                    });
                    // Every control below writes `self.settings` directly. Nothing
                    // here builds a RepairOptions by hand: `options_from` does that,
                    // in the core, for the CLI and the GUI alike.
                    ui.horizontal(|ui| {
                        ui.label(rust_i18n::t!("app.deep_repair").to_string());
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
                            });
                    });

                    // Mode and profile change repair *results*, and neither was
                    // reachable from the GUI at all -- a GUI user could not
                    // reproduce what the CLI does with `--mode` or `--profile`.
                    ui.horizontal(|ui| {
                        ui.label(rust_i18n::t!("app.mode").to_string());
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
                            });
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
                    ui.horizontal(|ui| {
                        ui.label(rust_i18n::t!("app.profile").to_string());
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

                    ui.separator();
                    ui.label(rust_i18n::t!("app.tiers").to_string());
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
                    // Splint is OFF by default, so it is expressed as an opt-in.
                    ui.checkbox(
                        &mut self.settings.experimental_splint,
                        rust_i18n::t!("app.tier_splint").to_string(),
                    );
                    let mut dressing = self.settings.dressing;
                    if ui
                        .checkbox(&mut dressing, rust_i18n::t!("app.tier_dressing").to_string())
                        .changed()
                    {
                        self.settings.dressing = dressing;
                    }
                    if self.settings.dressing {
                        ui.checkbox(
                            &mut self.settings.dressing_force_adopt,
                            rust_i18n::t!("app.dressing_force_adopt").to_string(),
                        );
                        // The four dressing knobs, previously CLI-only. Each is
                        // tri-state (`None` = CLI default), so a blank field
                        // means "leave it to the intensity", exactly like the
                        // per-run limits above.
                        ui.indent("dressing_knobs", |ui| {
                            ui.horizontal(|ui| {
                                ui.label(rust_i18n::t!("app.dressing_drain").to_string());
                                let mut text = self
                                    .settings
                                    .dressing_drain
                                    .as_ref()
                                    .map(|v| match v {
                                        serde_json::Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default();
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut text)
                                            .hint_text(rust_i18n::t!("app.dressing_drain_hint")),
                                    )
                                    .changed()
                                {
                                    let t = text.trim();
                                    self.settings.dressing_drain = if t.is_empty() {
                                        None
                                    } else {
                                        Some(match t.parse::<f64>() {
                                            // Same shape the CLI produces: a bare
                                            // number stays a number.
                                            Ok(n) => serde_json::json!(n),
                                            Err(_) => {
                                                serde_json::Value::String(t.to_lowercase())
                                            }
                                        })
                                    };
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label(rust_i18n::t!("app.dressing_defects").to_string());
                                let mut text = self
                                    .settings
                                    .dressing_defects
                                    .clone()
                                    .unwrap_or_default();
                                if ui
                                    .add(
                                        egui::TextEdit::singleline(&mut text)
                                            .hint_text(rust_i18n::t!("app.dressing_defects_hint")),
                                    )
                                    .changed()
                                {
                                    let t = text.trim();
                                    self.settings.dressing_defects = if t.is_empty() {
                                        None
                                    } else {
                                        Some(t.to_string())
                                    };
                                }
                            });
                            let mut knob = |ui: &mut egui::Ui, label: &str, value: &mut Option<f64>| {
                                ui.horizontal(|ui| {
                                    ui.label(label);
                                    let mut text =
                                        value.map(|v| v.to_string()).unwrap_or_default();
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut text)
                                                .desired_width(70.0),
                                        )
                                        .changed()
                                    {
                                        let t = text.trim();
                                        *value = if t.is_empty() {
                                            None
                                        } else {
                                            t.parse::<f64>().ok()
                                        };
                                    }
                                });
                            };
                            knob(
                                ui,
                                &rust_i18n::t!("app.dressing_rmax_scale").to_string(),
                                &mut self.settings.dressing_rmax_scale,
                            );
                            knob(
                                ui,
                                &rust_i18n::t!("app.dressing_sigma_scale").to_string(),
                                &mut self.settings.dressing_sigma_scale,
                            );
                        });
                    }

                    ui.separator();
                    // Graft and the fTetWild fallback: declared on the CLI since
                    // the beginning, never reachable from the GUI.
                    ui.label(rust_i18n::t!("app.advanced").to_string());
                    ui.checkbox(
                        &mut self.settings.experimental_graft,
                        rust_i18n::t!("app.experimental_graft").to_string(),
                    );
                    // Python: the negative flag wins over the positive one, and
                    // `options_from` keeps them as two independent booleans, so
                    // the GUI must not let both be on.
                    let mut no_graft = self.settings.no_graft;
                    if ui
                        .checkbox(&mut no_graft, rust_i18n::t!("app.no_graft").to_string())
                        .changed()
                    {
                        self.settings.no_graft = no_graft;
                    }
                    ui.checkbox(
                        &mut self.settings.experimental_fallback_ftetwild,
                        rust_i18n::t!("app.experimental_fallback_ftetwild").to_string(),
                    );
                    let mut no_fallback = self.settings.no_fallback_ftetwild;
                    if ui
                        .checkbox(
                            &mut no_fallback,
                            rust_i18n::t!("app.no_fallback_ftetwild").to_string(),
                        )
                        .changed()
                    {
                        self.settings.no_fallback_ftetwild = no_fallback;
                    }
                    // `--classifier-engine` has NO control here on purpose:
                    // `repair.rs:907` hard-codes "experimental", so writing
                    // `settings.classifier_engine` would be a control that does
                    // nothing -- the same bug class as a dead flag. It gets one
                    // in İ6, where the analysis panel reads it.

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
                    ui.horizontal(|ui| {
                        ui.label(rust_i18n::t!("app.si_mode").to_string());
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
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label(rust_i18n::t!("app.solid_engine").to_string());
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
                            });
                    });

                    ui.separator();
                    // Open by default: these replace the old "More settings will
                    // arrive with the core profiles API" placeholder, and a
                    // collapsed group nobody opens is the same as no group.
                    egui::CollapsingHeader::new(rust_i18n::t!("app.bounds").to_string())
                        .default_open(true)
                        .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(rust_i18n::t!("app.bounds_hint").to_string())
                                .small()
                                .weak(),
                        );
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
                    ui.label(rust_i18n::t!("app.methods").to_string());
                    // Multiple selection, not a sequence: `forced_methods` is a set
                    // in the core (membership only), so offering reordering would
                    // promise an order nothing honours.
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
                    let mut changed = false;
                    egui::ScrollArea::vertical()
                        .id_salt("methods_scroll")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for (num, id, _category, name) in
                                sutura_core::repair_settings::METHODS
                            {
                                let mut on = selected.contains(&num);
                                if ui
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
                    if changed {
                        self.settings.methods = if selected.is_empty() {
                            None
                        } else {
                            selected.sort_unstable();
                            Some(
                                selected
                                    .iter()
                                    .map(|n| n.to_string())
                                    .collect::<Vec<_>>()
                                    .join(","),
                            )
                        };
                    }
                    ui.separator();
                    ui.heading(rust_i18n::t!("app.report").to_string());
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
                        ui.label(rust_i18n::t!("app.report_empty").to_string());
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
                        egui::Grid::new("report-grid")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.label(rust_i18n::t!("app.health").to_string());
                                ui.label(fmt_score(report.health));
                                ui.end_row();
                                ui.label(rust_i18n::t!("app.risk").to_string());
                                ui.label(fmt_score(report.risk));
                                ui.end_row();
                                ui.label(rust_i18n::t!("app.fidelity").to_string());
                                ui.label(fmt_score(report.fidelity));
                                ui.end_row();
                                ui.label(rust_i18n::t!("app.confidence").to_string());
                                ui.label(fmt_score(report.confidence));
                                ui.end_row();
                            });
                        let verdict = verdict_text(report);
                        let color = verdict_color(&report.verdict_code);
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
                    } else if !matches!(entry.repair, RepairState::Running { .. })
                        && !matches!(entry.repair, RepairState::Failed(_))
                    {
                        ui.label(rust_i18n::t!("app.report_empty").to_string());
                    }
                    // Panels come last: they need `entry` only for an index
                    // sanity check, and the data is already cached above.
                    if let Some(idx) = self.selected {
                        if self.files.get(idx).is_some() {
                            egui::CollapsingHeader::new(
                                rust_i18n::t!("app.analysis").to_string(),
                            )
                            .id_salt("analysis_header")
                            .default_open(true)
                            .show(ui, |ui| {
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
                                        egui::Grid::new("analysis-grid")
                                            .num_columns(2)
                                            .spacing([12.0, 4.0])
                                            .show(ui, |ui| {
                                                for (label, value) in rows {
                                                    ui.label(label);
                                                    ui.label(value);
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
                            });
                            // Individual defects, as Python's plain-text panel
                            // lists them (`gui.py:6755`): one line per hole and
                            // per non-manifold region, with its centroid so the
                            // user can locate it.
                            match &self.files[idx].defects {
                                Some(Ok(report))
                                    if !report.holes.is_empty()
                                        || !report.non_manifold.is_empty() =>
                                {
                                    let total = report.holes.len() + report.non_manifold.len();
                                    egui::CollapsingHeader::new(
                                        rust_i18n::t!("app.defects", count = total as i64)
                                            .to_string(),
                                    )
                                    .id_salt("defects_header")
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        // Bound the list: a shattered mesh can
                                        // have thousands, and the panel is a
                                        // summary, not a table to scroll.
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
                                Some(Err(e)) => {
                                    ui.label(
                                        egui::RichText::new(e)
                                            .small()
                                            .color(egui::Color32::LIGHT_RED),
                                    );
                                }
                                _ => {}
                            }
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
                                    .default_open(false)
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
                        ui.label(egui::RichText::new(output.display().to_string()).small().weak());
                        if ui.button(rust_i18n::t!("app.open_result").to_string()).clicked() {
                            open_action = Some((output.clone(), false));
                        }
                        if ui
                            .button(rust_i18n::t!("app.show_in_folder").to_string())
                            .clicked()
                        {
                            open_action = Some((output.clone(), true));
                        }
                    }
                    });
            });
        }
        if cancel_clicked || cancel_in_panel {
            self.cancel_repair();
        }
        if let Some((path, reveal)) = open_action {
            open_path(&path, reveal);
        }

        let mut view_changed = false;
        egui::containers::CentralPanel::default().show(ui, |ui| {
            if let Some(idx) = self.selected {
                let (before, after, has_result) = self
                    .files
                    .get(idx)
                    .map(|e| {
                        (
                            e.view == ViewMode::Before,
                            e.view == ViewMode::After,
                            e.result.is_some(),
                        )
                    })
                    .unwrap_or((true, false, false));
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(before, rust_i18n::t!("app.before").to_string())
                        .clicked()
                    {
                        self.files[idx].view = ViewMode::Before;
                        view_changed = true;
                    }
                    if ui
                        .add_enabled(
                            has_result,
                            egui::Button::selectable(
                                after,
                                rust_i18n::t!("app.after").to_string(),
                            ),
                        )
                        .clicked()
                    {
                        self.files[idx].view = ViewMode::After;
                        view_changed = true;
                    }
                    let mut heat = self.preview.heatmap;
                    if ui
                        .checkbox(&mut heat, rust_i18n::t!("app.heatmap").to_string())
                        .changed()
                    {
                        self.preview.heatmap = heat;
                        self.preview.dirty = true;
                    }
                });
            }
            self.preview.show(ui);
        });
        if view_changed {
            self.apply_selection();
        }

        egui::containers::Panel::bottom("log")
            .resizable(true)
            .default_size(130.0)
            .show(ui, |ui| {
                ui.heading(rust_i18n::t!("app.log").to_string());
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.label(egui::RichText::new(line).monospace().small());
                        }
                    });
            });

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
                    ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
                    ui.separator();
                    ui.label(rust_i18n::t!("app.about_body").to_string());
                });
            self.about_open = open;
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

        self.apply_selection();
        self.maybe_screenshot(&ctx);
        self.persist_if_changed();
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
        "preserved" | "preserved_notes" => egui::Color32::from_rgb(120, 200, 140),
        "changed" | "changed_no_area" | "low_confidence" | "low_confidence_no_area" => {
            egui::Color32::from_rgb(230, 190, 90)
        }
        "not_repaired" => egui::Color32::LIGHT_RED,
        _ => egui::Color32::GRAY,
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
    run_repair(input, output, settings, tx, cancel);

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
fn opt_f64(ui: &mut egui::Ui, id: &str, value: &mut Option<f64>, label: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut text = value.map(|v| format!("{v}")).unwrap_or_default();
        if ui
            .add(egui::TextEdit::singleline(&mut text).id_salt(id).desired_width(90.0))
            .changed()
        {
            *value = if text.trim().is_empty() {
                None
            } else {
                text.trim().parse::<f64>().ok()
            };
        }
    });
}

/// The integer twin of [`opt_f64`].
fn opt_i64(ui: &mut egui::Ui, id: &str, value: &mut Option<i64>, label: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut text = value.map(|v| format!("{v}")).unwrap_or_default();
        if ui
            .add(egui::TextEdit::singleline(&mut text).id_salt(id).desired_width(90.0))
            .changed()
        {
            *value = if text.trim().is_empty() {
                None
            } else {
                text.trim().parse::<i64>().ok()
            };
        }
    });
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



fn open_path(path: &Path, reveal: bool) {
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
    /// Two of the plan's thirteen are deliberately absent here:
    /// * `time_budget_s` is `None` on the CLI too (`repair_cmd.rs:272`), so its
    ///   absence is parity, not a gap.
    /// * `classifier_engine` is a live field in `RepairSettings` that
    ///   `options_from` never reads, and `repair.rs:907` hard-codes
    ///   "experimental". The GUI has **no** control for it rather than a dead
    ///   one; it returns in İ6 with the analysis panel as its consumer.
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

    /// The settings panel must be inside a scroll area.
    ///
    /// A static screenshot cannot show this: egui only paints the scrollbar
    /// while hovering, and at scroll offset 0 the frame is identical either
    /// way. The window is a fixed 1024x720 with a 720x480 minimum
    /// (`main.rs:174-175`), so at the minimum size the panel is taller than the
    /// window and the lower controls (wall thickness, join components, retries,
    /// the whole method list) are unreachable without this.
    #[test]
    fn settings_panel_is_scrollable() {
        let src = include_str!("app.rs");
        let start = src
            .find("Panel::right(\"settings\")")
            .expect("settings panel exists");
        let end = src[start..]
            .find("\n            });")
            .map(|i| start + i)
            .expect("settings panel ends");
        let panel = &src[start..end];
        // `vertical()`, not `horizontal()`: only vertical scrolling makes the
        // lower controls reachable, and a substring check for "ScrollArea"
        // alone would pass either way.
        assert!(
            panel.contains("ScrollArea::vertical()"),
            "the settings panel must scroll vertically, or its lower half is \
             unreachable at 720x480"
        );
        assert!(
            !panel.contains("ScrollArea::horizontal()"),
            "horizontal scrolling does not make the lower controls reachable"
        );
    }
}
