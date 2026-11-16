use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

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
    repair_pid: Option<u32>,
    cancel_requested: Arc<AtomicBool>,
    cancel_kill_at: Option<Instant>,
    pending_overwrite: Option<(usize, PathBuf)>,
    pending_batch: Option<Vec<usize>>,
    intensity: Intensity,
    batch: Vec<usize>,
    batch_total: usize,
    batch_done: usize,
    batch_force: bool,
    batch_active: bool,
    auto_batch: bool,
    auto_batch_started: bool,
    about_open: bool,
    updater_open: bool,
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
            repair_pid: None,
            cancel_requested: Arc::new(AtomicBool::new(false)),
            cancel_kill_at: None,
            pending_overwrite: None,
            pending_batch: None,
            intensity: options.intensity,
            batch: Vec::new(),
            batch_total: 0,
            batch_done: 0,
            batch_force: false,
            batch_active: false,
            auto_batch: options.auto_batch,
            auto_batch_started: false,
            about_open: options.about,
            updater_open: options.updater,
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
            self.preview.set_mesh(mesh);
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
        self.repair_pid = None;
        self.cancel_requested.store(false, Ordering::SeqCst);
        self.cancel_kill_at = None;
        self.files[idx].repair = RepairState::Running { step: 0, total: 8 };
        self.files[idx].error = None;
        self.log(
            rust_i18n::t!("app.log_repair_started", path = input.display().to_string())
                .to_string(),
        );
        let cancel = self.cancel_requested.clone();
        let intensity = self.intensity.value().to_string();
        std::thread::spawn(move || {
            run_repair(&input, &output, force, &intensity, tx, cancel);
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
            RepairEvent::Started(pid) => {
                self.repair_pid = Some(pid);
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
                self.repair_pid = None;
                self.cancel_kill_at = None;
                self.cancel_requested.store(false, Ordering::SeqCst);
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
        let Some(pid) = self.repair_pid else { return };
        self.cancel_requested.store(true, Ordering::SeqCst);
        self.log(
            rust_i18n::t!("app.log_kill", pid = pid.to_string()).to_string(),
        );
        kill_group(pid, 15);
        self.cancel_kill_at = Some(Instant::now());
    }

    fn tick_cancel(&mut self) {
        if let Some(t) = self.cancel_kill_at {
            if t.elapsed() >= Duration::from_secs(3) {
                if let Some(pid) = self.repair_pid {
                    kill_group(pid, 9);
                }
                self.cancel_kill_at = None;
            }
        }
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
        self.tick_cancel();

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
                ui.label(rust_i18n::t!("app.settings_more").to_string());
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
            egui::Window::new(rust_i18n::t!("app.updater_title").to_string())
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label(rust_i18n::t!("app.updater_body").to_string());
                });
            self.updater_open = open;
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
        if let Some(pid) = self.repair_pid.take() {
            kill_group(pid, 9);
        }
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

fn fixed_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mesh".to_string());
    input.with_file_name(format!("{stem}_fixed.stl"))
}

fn repair_command() -> Result<Command, String> {
    if let Ok(cli) = std::env::var("SUTURA_CLI") {
        let mut parts = cli.split_whitespace();
        let program = parts.next().ok_or_else(|| "SUTURA_CLI is empty".to_string())?;
        let mut cmd = Command::new(program);
        cmd.args(parts);
        return Ok(cmd);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .ok_or_else(|| "cannot locate repository root".to_string())?
        .to_path_buf();
    let script = root.join("sutura").join("repair.py");
    if !script.exists() {
        return Err(format!(
            "Python repair CLI not found at {} (set SUTURA_CLI)",
            script.display()
        ));
    }
    let python = std::env::var("SUTURA_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let mut cmd = Command::new(python);
    cmd.arg(script);
    Ok(cmd)
}

fn run_repair(
    input: &Path,
    output: &Path,
    force: bool,
    intensity: &str,
    tx: mpsc::Sender<RepairEvent>,
    cancel: Arc<AtomicBool>,
) {
    let mut cmd = match repair_command() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(e),
                cancelled: false,
            });
            return;
        }
    };
    cmd.arg(input)
        .arg("-o")
        .arg(output)
        .arg("--no-history")
        .arg("--json")
        .arg("--progress-json")
        .arg("--intensity")
        .arg(intensity);
    if force {
        cmd.arg("--force");
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(RepairEvent::Finished {
                output: output.to_path_buf(),
                result: Err(format!("could not start repair CLI: {e}")),
                cancelled: false,
            });
            return;
        }
    };
    let pid = child.id();
    let _ = tx.send(RepairEvent::Started(pid));

    let stderr_reader = child.stderr.take().map(|stderr| {
        let tx = tx.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().flatten() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                    match v.get("event").and_then(Value::as_str) {
                        Some("stage") => {
                            let step = v.get("step").and_then(Value::as_u64).unwrap_or(0) as u8;
                            let total =
                                v.get("step_total").and_then(Value::as_u64).unwrap_or(8) as u8;
                            let _ = tx.send(RepairEvent::Progress { step, total });
                        }
                        Some("cancel") => cancel.store(true, Ordering::SeqCst),
                        _ => {}
                    }
                } else {
                    let _ = tx.send(RepairEvent::Stderr(line));
                }
            }
        })
    });

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    let status = child.wait();
    if let Some(reader) = stderr_reader {
        let _ = reader.join();
    }
    let cancelled = cancel.load(Ordering::SeqCst);
    let success = status.as_ref().map(|s| s.success()).unwrap_or(false);
    let code = status.as_ref().ok().and_then(|s| s.code());
    let result = if success {
        parse_repair(&stdout, output)
    } else if cancelled {
        Err("cancelled".to_string())
    } else {
        Err(format!("repair CLI exited with {code:?}"))
    };
    let _ = tx.send(RepairEvent::Finished {
        output: output.to_path_buf(),
        result,
        cancelled,
    });
}

fn parse_repair(stdout: &str, output: &Path) -> Result<(DisplayMesh, RepairReport), String> {
    let last = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| "repair produced no JSON report".to_string())?;
    let value: Value =
        serde_json::from_str(last).map_err(|e| format!("invalid report JSON: {e}"))?;
    if let Some(err) = value.get("error").and_then(Value::as_str) {
        return Err(err.to_string());
    }
    let mesh = DisplayMesh::load(output).map_err(|e| format!("output load failed: {e}"))?;
    Ok((mesh, RepairReport::from_value(&value)))
}

#[cfg(unix)]
fn kill_group(pid: u32, signal: i32) {
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32, _signal: i32) {}

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
