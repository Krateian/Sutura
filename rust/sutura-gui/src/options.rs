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
    labelled, OptionsTab, SuturaApp, COLOR_ERROR, LABEL_W, SPACE_16, SPACE_24, SPACE_8,
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

        // A real modal, not a `Window`: the main window's Methods/Options/
        // Profile buttons looked like empty boxes behind an ordinary window, and
        // a click could land on them. `Modal` dims the backdrop and blocks input
        // to the rest of the UI, which is what a tabbed options dialog is.
        // Dim the whole viewport, not just the dialog's rect: `Modal` paints its
        // own area, which made the dialog brighter than the window behind it and
        // read as "no backdrop at all".
        let screen = ctx.content_rect();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("options-backdrop"),
        ))
        .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));

        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("options-modal"))
            .backdrop_color(egui::Color32::TRANSPARENT)
            .show(ctx, |ui| {
            ui.set_width(580.0);
            // No `set_min_height`: reserving 640 px before the heading pushed
            // the title and the tab strip on top of each other. The scroll area
            // below gets its own fixed height instead.
            ui.horizontal(|ui| {
                ui.heading(rust_i18n::t!("app.options_btn").to_string());
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .button(egui::RichText::new("×").size(18.0))
                            .on_hover_text(rust_i18n::t!("app.close").to_string())
                            .clicked()
                        {
                            close = true;
                        }
                    },
                );
            });
            ui.add_space(SPACE_8);

            // Wrapped, so a narrow window moves tabs to a second line rather
            // than clipping the last one. Qt clipped "Engi" at its own default
            // width; copying a clipping bug is not parity.
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

            // The handle's resting opacity is what made the bar invisible
            // before (see `theme.rs`); the visibility policy only decides
            // whether a bar is allocated at all, and a full-height bar in a tab
            // that fits is noise -- Updates and Engines showed one.
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("options-scroll")
                .scroll_bar_visibility(
                    egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                )
                .max_height(500.0)
                .min_scrolled_height(500.0)
                .auto_shrink([false, false]);
            // `--options-scroll`: the same one-shot seed as `--settings-scroll`.
            // `vertical_scroll_offset` writes the offset on every frame it is
            // set, so applying it repeatedly would snap the wheel back.
            if let Some(offset) = self.options_scroll.take() {
                scroll = scroll.vertical_scroll_offset(offset);
            }
            let scroll = scroll.show(ui, |ui| {
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
            #[cfg(test)]
            self.options_scroll_probe.store(
                scroll.state.clamped_offset().y.to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
        });

        // Clicking the backdrop closes it, as a modal should.
        if modal.backdrop_response.clicked() || close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }

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
    /// Qt's `_open_policy` (gui.py:4468): reveal the policy file.
    ///
    /// Behind a button rather than automatic: the file is a JSON document a user
    /// edits by hand, and opening a text editor unbidden is hostile.
    fn policy_row(ui: &mut egui::Ui) {
        ui.add_space(SPACE_16);
        ui.horizontal(|ui| {
            if ui
                .button(rust_i18n::t!("app.open_policy").to_string())
                .on_hover_text(rust_i18n::t!("app.open_policy_tip").to_string())
                .clicked()
            {
                let path = sutura_core::config::policy::user_policy_path();
                crate::app::open_path(&path, false);
            }
        });
    }

    fn tab_general(&mut self, ui: &mut egui::Ui) {
        // Qt's four General switches, each a real checkbox: they are persisted
        // in `gui.json` and applied in `run_repair`, so they do something.
        let mut history = self.prefs.history_enabled.unwrap_or(true);
        if ui
            .checkbox(
                &mut history,
                rust_i18n::t!("app.opt_usage_history").to_string(),
            )
            .changed()
        {
            self.prefs.history_enabled = Some(history);
        }

        ui.horizontal(|ui| {
            let mut cache = self.prefs.cache_enabled.unwrap_or(true);
            if ui
                .checkbox(&mut cache, rust_i18n::t!("app.opt_chart_cache").to_string())
                .changed()
            {
                self.prefs.cache_enabled = Some(cache);
            }
            if ui
                .button(rust_i18n::t!("app.opt_clear_cache").to_string())
                .clicked()
            {
                let n = clear_chart_cache();
                self.log(format!("cleared {n} cached chart file(s)"));
            }
        });

        let mut fidelity = self.prefs.fidelity_enabled.unwrap_or(true);
        if ui
            .checkbox(
                &mut fidelity,
                rust_i18n::t!("app.opt_fidelity").to_string(),
            )
            .changed()
        {
            self.prefs.fidelity_enabled = Some(fidelity);
        }

        ui.horizontal(|ui| {
            let mut learning = self.prefs.learning_enabled.unwrap_or(true);
            if ui
                .checkbox(&mut learning, rust_i18n::t!("app.opt_learning").to_string())
                .changed()
            {
                self.prefs.learning_enabled = Some(learning);
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
        // The explanation belongs to the checkboxes above it, so it sits directly
        // under them and in the muted colour.
        ui.add(egui::Label::new(
            egui::RichText::new(rust_i18n::t!("app.opt_learning_body").to_string())
                .small()
                .weak(),
        ));

        // --- Rust extra: language ------------------------------------------
        //
        // No section heading: the control's own label already reads "Language",
        // and a heading with the same word directly above it was the duplication.
        ui.add_space(SPACE_24);
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
        Self::option_section(ui, rust_i18n::t!("app.diagnostics_title").to_string());
        ui.add(egui::Label::new(
            egui::RichText::new(rust_i18n::t!("app.diagnostics_tip").to_string())
                .small()
                .weak(),
        ));
        if ui.button(rust_i18n::t!("app.diagnostics").to_string()).clicked() {
            self.export_diagnostics();
        }
        Self::policy_row(ui);
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

        // Qt's order: installed version, last check, [Check now] [Automatic
        // updates], [Check on every start], then the note.
        ui.label(
            egui::RichText::new(
                rust_i18n::t!("app.opt_version", version = crate::app::SUTURA_VERSION).to_string(),
            )
            .strong(),
        );
        let last = self
            .prefs
            .last_check
            .map(|ts| {
                // `%Y-%m-%d %H:%M` without pulling in a date crate: the value is
                // only ever shown, never parsed back.
                let secs = ts.max(0) as u64;
                let days = secs / 86_400;
                let rem = secs % 86_400;
                format!("{}d {:02}:{:02}", days, rem / 3600, (rem % 3600) / 60)
            })
            .unwrap_or_else(|| rust_i18n::t!("app.opt_never").to_string());
        ui.label(
            egui::RichText::new(
                rust_i18n::t!("app.opt_last_check", when = last).to_string(),
            )
            .small()
            .weak(),
        );

        let mut check_now = false;
        let mut auto = self.prefs.check_for_updates.unwrap_or(false);
        let mut on_start = self.prefs.check_on_startup.unwrap_or(false);
        ui.horizontal(|ui| {
            if ui
                .button(rust_i18n::t!("app.updater_check").to_string())
                .clicked()
            {
                check_now = true;
            }
            if ui
                .checkbox(&mut auto, rust_i18n::t!("app.opt_auto_updates").to_string())
                .changed()
            {
                self.prefs.check_for_updates = Some(auto);
            }
        });
        // A sub-option of Automatic updates, which is what Qt's indented row
        // meant: indented under it, and inert while Automatic is off.
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.add_enabled_ui(auto, |ui| {
                if ui
                    .checkbox(
                        &mut on_start,
                        rust_i18n::t!("app.opt_check_on_start").to_string(),
                    )
                    .changed()
                {
                    self.prefs.check_on_startup = Some(on_start);
                }
            });
        });
        ui.add(egui::Label::new(
            egui::RichText::new(rust_i18n::t!("app.opt_auto_note").to_string())
                .small()
                .weak(),
        ));

        ui.add_space(SPACE_16);
        ui.separator();
        ui.add_space(SPACE_8);
        // The live phase, so the tab says what a check is doing.
        let u = &mut self.updater;
        match &u.phase {
            Phase::Idle => {}
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

        if check_now {
            self.prefs.last_check = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            );
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
