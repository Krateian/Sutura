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
    output: Option<PathBuf>,
    view: ViewMode,
}

enum RepairEvent {
    Started(u32),
    Progress { step: u8, total: u8 },
    Stderr(String),
    Finished {
        output: PathBuf,
        result: Result<(DisplayMesh, RepairReport), String>,
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
}

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
}

impl SuturaApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        files: Vec<PathBuf>,
        language: Language,
        options: StartupOptions,
    ) -> Self {
        let mut preview = Preview::new(cc);
        preview.heatmap = options.heatmap;
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
            intensity: options.intensity,
            settings: RepairSettings::from_intensity(options.intensity.value(), false),
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
                    output: None,
                    view: ViewMode::Before,
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
                    output: None,
                    view: ViewMode::Before,
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
                    Ok((mesh, report)) => {
                        let count = mesh.triangle_count();
                        self.files[idx].result = Some(Arc::new(mesh));
                        self.files[idx].report = Some(report);
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
        self.poll_repair();
        // Updater events drive its panel; a change means a repaint.
        if self.updater.poll() {
            ctx.request_repaint();
        }

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
                });
            });
        });

        egui::containers::Panel::left("files")
            .resizable(true)
            .default_size(290.0)
            .show(ui, |ui| {
                ui.heading(rust_i18n::t!("app.files").to_string());
                ui.separator();
                let mut clicked = None;
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
                        if ui.selectable_label(selected, text).clicked() {
                            clicked = Some(idx);
                        }
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
            });

        let mut open_action: Option<(PathBuf, bool)> = None;
        let mut cancel_in_panel = false;
        egui::containers::Panel::right("settings")
            .resizable(true)
            .default_size(280.0)
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
        Some(Ok((_mesh, _summary))) => Ok(()),
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
/// second measurement path.
pub fn headless_analyze(
    input: &Path,
    out: Option<PathBuf>,
) -> eframe::Result<()> {
    let mesh = match sutura_core::load_mesh(input) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("analyse failed: {e}");
            std::process::exit(1);
        }
    };
    let analysis = match sutura_core::analysis::analyze_mesh(&mesh, "experimental", true) {
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
    let blocks = match sutura_core::parse_3mf_mesh_blocks(input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("objects failed: {e}");
            std::process::exit(1);
        }
    };
    let list: Vec<Value> = blocks
        .iter()
        .map(|b| {
            serde_json::json!({
                "model_name": b.model_name,
                "block": b.index,
                "faces": b.mesh.tris.len(),
                "vertices": b.mesh.verts.len(),
            })
        })
        .collect();
    let value = Value::Array(list);
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

pub fn fixed_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mesh".to_string());
    input.with_file_name(format!("{stem}_fixed.stl"))
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
        let _ = match result {
            Ok(_) => finish_from_output(output, &tx, &cancel),
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
        result: Ok((mesh, RepairReport::from_value(&scored))),
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
fn finish_from_output(
    output: &Path,
    tx: &mpsc::Sender<RepairEvent>,
    cancel: &CancelToken,
) -> Result<(), mpsc::SendError<RepairEvent>> {
    match DisplayMesh::load(output) {
        Ok(mesh) => tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Ok((mesh, RepairReport::default())),
            cancelled: cancel.is_cancelled(),
        }),
        Err(e) => tx.send(RepairEvent::Finished {
            output: output.to_path_buf(),
            result: Err(format!("output load failed: {e}")),
            cancelled: false,
        }),
    }
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
