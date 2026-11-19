//! The Options window: Qt's 7-tab dialog, which replaces the settings column.
//!
//! Qt kept these controls in a non-modal `QDialog` with a `QTabWidget`, and a
//! user coming from v0.9.9 looks for exactly that: the main window carries no
//! settings, and the tabs are in a fixed order. This reproduces the shape in
//! egui.
//!
//! The control bodies are the ones that used to live in the right column, moved
//! rather than rewritten: every widget still writes the same `settings` field, so
//! the `options_from` -> CLI mapping cannot drift.
//!
//! Two of the seven tabs (Repair, Methods) are taller than any sane window,
//! exactly as they were in Qt, where they scrolled. Here the scroll is shared by
//! the whole tab body.

use crate::app::{
    labelled, OptionsTab, SuturaApp, COLOR_ERROR, SPACE_16, SPACE_24, SPACE_8,
};

/// Remove the cached chart files and report how many went.
///
/// Qt had a `Clear Cache` button for these; the files live beside the learning
/// history, so this is the same sweep for the same reason.
fn clear_chart_cache() -> usize {
    let dir = sutura_core::config::learning::data_dir();
    std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with("-charts.json"))
                .filter(|e| std::fs::remove_file(e.path()).is_ok())
                .count()
        })
        .unwrap_or(0)
}

impl SuturaApp {
    /// Draw the Options window, if it is open.
    pub(crate) fn options_window(&mut self, ctx: &egui::Context) {
        if !self.options_open {
            return;
        }
        let mut open = self.options_open;
        let mut tab = self.options_tab;

        egui::Window::new(rust_i18n::t!("app.options_btn").to_string())
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([620.0, 520.0])
            .min_size([520.0, 360.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                // Wrapped, so a narrow window moves tabs to a second line rather
                // than clipping the last one. Qt clipped "Engi" at its own
                // default width; copying a clipping bug is not parity.
                ui.horizontal_wrapped(|ui| {
                    for candidate in OptionsTab::ALL {
                        if ui
                            .add(egui::Button::selectable(tab == candidate, candidate.label()))
                            .clicked()
                        {
                            tab = candidate;
                        }
                    }
                });
                ui.add_space(SPACE_8);
                ui.separator();
                ui.add_space(SPACE_8);

                egui::ScrollArea::vertical()
                    .id_salt("options-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width());
                        match tab {
                            OptionsTab::General => self.tab_general(ui),
                            OptionsTab::Repair => self.tab_repair(ui),
                            OptionsTab::Methods => self.tab_methods(ui),
                            OptionsTab::Experimental => self.tab_experimental(ui),
                            OptionsTab::Updates => self.tab_updates(ui),
                            OptionsTab::Changelog => self.tab_changelog(ui),
                            OptionsTab::Engines => self.tab_engines(ui),
                        }
                    });
            });

        self.options_open = open;
        self.options_tab = tab;
    }

    /// A section heading inside a tab: one step larger and bold (rule f).
    pub(crate) fn option_section(ui: &mut egui::Ui, text: String) {
        ui.add_space(SPACE_16);
        ui.add(egui::Label::new(egui::RichText::new(text).heading()).wrap());
        ui.add_space(SPACE_8);
    }

    /// General: Qt's four switches plus the Rust settings that needed a home.
    ///
    /// Qt's General tab held the usage history, the chart cache, the fidelity
    /// score and the learning triage. The language selector and the diagnostics
    /// export are Rust extras re-homed here rather than deleted, as the mapping
    /// table said.
    fn tab_general(&mut self, ui: &mut egui::Ui) {
        // --- Qt's General switches ----------------------------------------
        //
        // Qt had four here: usage history, chart cache, the fidelity score and
        // the learning triage. The first two are files, not flags -- Qt exposed
        // them as a checkbox plus an action button -- so they get the action and
        // no control that would write nothing.
        let mut grafts = !self.settings.no_graft;
        if ui
            .checkbox(&mut grafts, rust_i18n::t!("app.opt_graft").to_string())
            .on_hover_text(rust_i18n::t!("app.opt_graft_tip").to_string())
            .changed()
        {
            self.settings.no_graft = !grafts;
        }

        ui.add_space(SPACE_8);
        ui.horizontal(|ui| {
            if ui
                .button(rust_i18n::t!("app.opt_clear_cache").to_string())
                .clicked()
            {
                let n = clear_chart_cache();
                self.log(format!("cleared {n} cached chart file(s)"));
            }
            if ui
                .button(rust_i18n::t!("app.opt_reset_learning").to_string())
                .clicked()
            {
                if sutura_core::config::learning::clear_learning(None) {
                    self.log("learning history reset".to_string());
                } else {
                    self.log("learning history was already empty".to_string());
                }
            }
        });
        ui.add(egui::Label::new(
            egui::RichText::new(rust_i18n::t!("app.opt_learning_body").to_string())
                .small()
                .weak(),
        ));

        // --- Rust extra: language, as the mapping table places it ----------
        Self::option_section(ui, rust_i18n::t!("app.section_language").to_string());
        ui.horizontal(|ui| {
            let id = labelled(ui, rust_i18n::t!("app.language").to_string());
            egui::ComboBox::from_id_salt("options-language")
                .selected_text(self.language.label())
                .show_ui(ui, |ui| {
                    for lang in [
                        crate::i18n::Language::System,
                        crate::i18n::Language::En,
                        crate::i18n::Language::Tr,
                    ] {
                        if ui
                            .selectable_value(&mut self.language, lang, lang.label())
                            .clicked()
                        {
                            crate::i18n::apply(lang);
                        }
                    }
                })
                .response
                .labelled_by(id);
        });

        // --- Rust extra: diagnostics export -------------------------------
        Self::option_section(ui, rust_i18n::t!("app.diagnostics").to_string());
        ui.label(
            egui::RichText::new(rust_i18n::t!("app.diagnostics_tip").to_string())
                .small()
                .weak(),
        );
        if ui.button(rust_i18n::t!("app.diagnostics").to_string()).clicked() {
            self.export_diagnostics();
        }
        if let Some(note) = &self.diagnostics_note {
            ui.label(egui::RichText::new(note).small().weak());
        }
    }

    /// Repair: Qt's main tab, which is the settings body that used to be the
    /// right-hand column.
    fn tab_repair(&mut self, ui: &mut egui::Ui) {
        self.settings_controls(ui);
    }

    /// Methods: the per-file editor, the global list, and the repeat points.
    fn tab_methods(&mut self, ui: &mut egui::Ui) {
        self.methods_controls(ui);
        ui.add_space(SPACE_24);
        self.repeat_point_controls(ui);
    }

    fn tab_experimental(&mut self, ui: &mut egui::Ui) {
        self.experimental_controls(ui);
    }

    fn tab_updates(&mut self, ui: &mut egui::Ui) {
        use crate::updater_ui::Phase;

        Self::option_section(ui, rust_i18n::t!("app.tab_updates").to_string());
        let u = &mut self.updater;
        ui.label(
            egui::RichText::new(
                rust_i18n::t!("app.updater_current", version = env!("CARGO_PKG_VERSION"))
                    .to_string(),
            )
            .small()
            .weak(),
        );
        ui.add_space(SPACE_8);
        match &u.phase {
            Phase::Idle => {
                ui.label(rust_i18n::t!("app.updater_body").to_string());
            }
            Phase::Checking => {
                ui.label(rust_i18n::t!("app.updater_checking").to_string());
            }
            Phase::Available { tag } => {
                ui.label(rust_i18n::t!("app.updater_available", tag = tag.as_str()).to_string());
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
            Phase::Done { tag, .. } => {
                ui.label(rust_i18n::t!("app.updater_done", tag = tag.as_str()).to_string());
            }
            Phase::Failed(msg) => {
                ui.colored_label(COLOR_ERROR, msg);
            }
        }
        ui.add_space(SPACE_8);
        if ui.button(rust_i18n::t!("app.updater_check").to_string()).clicked() {
            self.updater.start_check(true);
        }
    }

    fn tab_changelog(&mut self, ui: &mut egui::Ui) {
        if self.changelog.is_none() && self.changelog_error.is_none() {
            self.fetch_changelog();
        }
        if let Some(err) = &self.changelog_error {
            ui.label(egui::RichText::new(err).color(COLOR_ERROR));
            if ui.button(rust_i18n::t!("app.retry").to_string()).clicked() {
                self.changelog_error = None;
                self.fetch_changelog();
            }
            return;
        }
        match &self.changelog {
            None => {
                ui.label(rust_i18n::t!("app.changelog_loading").to_string());
            }
            Some(entries) => {
                for (tag, body) in entries {
                    egui::CollapsingHeader::new(tag.clone())
                        .id_salt(format!("options-changelog-{tag}"))
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(body).monospace().small());
                        });
                }
            }
        }
    }

    /// Engines: fTetWild availability and the configured external engines.
    fn tab_engines(&mut self, ui: &mut egui::Ui) {
        self.engines_controls(ui);
    }
}
