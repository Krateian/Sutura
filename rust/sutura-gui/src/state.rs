// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Persisted GUI state: the settings panel, intensity and language.
//!
//! A **separate file** from the CLI's `config.json`, in the same directory:
//! both runtimes write their own file, so neither can clobber the other's keys
//! (`config.json` is owned by the updater and the intensity resolver). Written
//! atomically -- temp file plus rename -- so a crash mid-write cannot leave a
//! truncated file that fails to parse on the next launch.
//!
//! Deliberately stricter than the Python GUI, which persists twelve keys and
//! keeps the tier switches in memory only: here the whole `RepairSettings`
//! round-trips, because a checkbox that silently resets on restart is the same
//! class of bug as a setting that does nothing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use sutura_core::repair_settings::RepairSettings;

/// The whole persisted GUI state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiState {
    pub settings: RepairSettings,
    /// One of "quick"/"balanced"/"thorough"/"extreme".
    pub intensity: Option<String>,
    /// One of "system"/"en"/"tr".
    pub language: Option<String>,
    /// The window's inner size in logical points, saved on close.
    ///
    /// Needed because egui does not know the monitor size on Wayland
    /// (`viewport().monitor_size` is `None` there), so "90% of the screen" cannot
    /// be computed on first launch. Remembering the size the user actually had
    /// is the portable answer: the first run asks for maximized, and every run
    /// after that restores what that produced.
    pub window_size: Option<[f32; 2]>,
    /// Anonymous usage history. Qt's General tab, and the same three preferences
    /// it persisted in `config.json`; they live here because `gui.json` is the
    /// GUI's file and `RepairOptions` has no field for any of them.
    ///
    /// `None` means "the default", which is on -- so an old file without the key
    /// keeps the CLI's behaviour rather than silently disabling it.
    pub history_enabled: Option<bool>,
    pub cache_enabled: Option<bool>,
    pub fidelity_enabled: Option<bool>,
    pub learning_enabled: Option<bool>,
    /// Updates tab: Qt persisted these in `config.json`, and the Rust updater
    /// reads `check_on_startup` when it decides whether to check on launch.
    pub check_for_updates: Option<bool>,
    pub check_on_startup: Option<bool>,
    /// Unix seconds of the last update check, for the "Last check" line.
    pub last_check: Option<i64>,
}

/// `<config dir>/gui.json`, beside the CLI's `config.json`.
pub fn state_path() -> PathBuf {
    sutura_core::config::config_intensity_path().with_file_name("gui.json")
}

/// Read the saved state; any problem (missing, unreadable, malformed) yields the
/// defaults rather than an error, because a bad settings file must not stop the
/// app from starting.
pub fn load() -> GuiState {
    let Ok(text) = std::fs::read_to_string(state_path()) else {
        return GuiState::default();
    };
    let mut state: GuiState = serde_json::from_str(&text).unwrap_or_default();
    sanitize(&mut state);
    state
}

/// Drop values a hand-edited or stale file can carry but the program cannot
/// honour.
///
/// `options_from` validates `mode` (it returns an error), but it does **not**
/// validate `profile`: `repair.rs:934` applies an unknown profile with the
/// fallback thresholds `(8, 1000)`. The CLI cannot hit that because clap
/// rejects a bad `--profile` via `choices`; the GUI's settings come from a JSON
/// file, so this is the only place the hole can be closed without changing core
/// semantics that go's code depends on.
pub fn sanitize(state: &mut GuiState) {
    let modes = crate::app::MODES;
    if state
        .settings
        .mode
        .as_deref()
        .is_some_and(|m| !modes.contains(&m))
    {
        state.settings.mode = None;
    }
    let profiles = crate::app::PROFILES;
    if state
        .settings
        .profile
        .as_deref()
        .is_some_and(|p| !profiles.contains(&p))
    {
        state.settings.profile = None;
    }
}

/// Write the state atomically.
pub fn save(state: &GuiState) -> Result<(), String> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        // Leave nothing half-written behind on a failed rename.
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("sutura-gui-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    /// A full round-trip: every field written comes back identical. This is the
    /// invariant the plan asks for, and it covers the tier switches Python
    /// keeps in memory only.
    #[test]
    fn round_trip_preserves_every_setting() {
        let state = GuiState {
            settings: RepairSettings {
                mode: Some("aggressive".into()),
                profile: Some("scan".into()),
                no_flap: true,
                experimental_splint: true,
                dressing: true,
                dressing_force_adopt: true,
                max_risk: Some(42.0),
                graft_timeout: Some(11.5),
                ftetwild_max_faces: Some(250_000),
                wall_min_thickness: Some(1.5),
                wall_thicken: true,
                join_components: true,
                repeat_source: Some([1.0, 2.0, 3.0]),
                repeat_target: Some([4.0, 5.0, 6.0]),
                ..Default::default()
            },
            intensity: Some("thorough".into()),
            language: Some("tr".into()),
            window_size: Some([1280.0, 800.0]),
            history_enabled: Some(false),
            cache_enabled: Some(false),
            fidelity_enabled: Some(false),
            learning_enabled: Some(false),
            check_for_updates: Some(true),
            check_on_startup: Some(false),
            last_check: Some(1_700_000_000),
        };
        let text = serde_json::to_string(&state).unwrap();
        let back: GuiState = serde_json::from_str(&text).unwrap();
        assert_eq!(back.settings.mode.as_deref(), Some("aggressive"));
        assert_eq!(back.settings.profile.as_deref(), Some("scan"));
        assert!(back.settings.no_flap);
        assert!(back.settings.experimental_splint);
        assert!(back.settings.dressing && back.settings.dressing_force_adopt);
        assert_eq!(back.settings.max_risk, Some(42.0));
        assert_eq!(back.settings.graft_timeout, Some(11.5));
        assert_eq!(back.settings.ftetwild_max_faces, Some(250_000));
        assert_eq!(back.settings.wall_min_thickness, Some(1.5));
        assert!(back.settings.wall_thicken);
        assert!(back.settings.join_components);
        assert_eq!(back.settings.repeat_source, Some([1.0, 2.0, 3.0]));
        assert_eq!(back.settings.repeat_target, Some([4.0, 5.0, 6.0]));
        assert_eq!(back.intensity.as_deref(), Some("thorough"));
        assert_eq!(back.language.as_deref(), Some("tr"));
        assert_eq!(
            back.window_size,
            Some([1280.0, 800.0]),
            "the window size must round-trip with the settings"
        );
        assert_eq!(
            (
                back.history_enabled,
                back.cache_enabled,
                back.fidelity_enabled,
                back.learning_enabled,
            ),
            (Some(false), Some(false), Some(false), Some(false)),
            "the General tab's switches must round-trip too"
        );
        assert_eq!(
            (back.check_for_updates, back.check_on_startup, back.last_check),
            (Some(true), Some(false), Some(1_700_000_000)),
            "the Updates tab's settings must round-trip"
        );
    }

    /// A malformed or older file must not stop the app: unknown keys are
    /// ignored and missing ones default.
    #[test]
    fn malformed_state_falls_back_to_defaults() {
        let state: GuiState = serde_json::from_str("{not json").unwrap_or_default();
        assert_eq!(state.intensity, None);
        assert!(!state.settings.no_flap);

        // `#[serde(default)]` on the struct: a partial file still parses.
        let partial: GuiState = serde_json::from_str(r#"{"intensity":"quick"}"#).unwrap();
        assert_eq!(partial.intensity.as_deref(), Some("quick"));
        assert!(!partial.settings.no_flap, "absent fields take their default");
    }

    /// The write is atomic: the temp file is renamed, and nothing is left over.
    #[test]
    fn save_leaves_no_temp_file() {
        let base = scratch("atomic");
        let path = base.join("gui.json");
        // Save via the same logic `save` uses, but to a controlled path.
        let text = serde_json::to_string_pretty(&GuiState::default()).unwrap();
        let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        assert!(path.is_file());
        assert!(!tmp.exists(), "temp file must be gone after the rename");
        let base = base.canonicalize().unwrap();
        assert!(!std::fs::read_dir(&base)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-")));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// End-to-end through the real functions and the real path, **including the
    /// missing-file case** -- in ONE test on purpose.
    ///
    /// `XDG_CONFIG_HOME` is process-global, so two tests setting it would race
    /// when the harness runs them in parallel (which is exactly how a test
    /// isolation bug bit this branch before). Keeping every env mutation in a
    /// single test is the cheap fix; a mutex or a path parameter would be more
    /// machinery than three assertions warrant.
    #[test]
    fn save_then_load_round_trips_on_disk() {
        let dir = scratch("ondisk");
        // Safe: this is the only test that touches the process env.
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let path = state_path();
        assert!(path.ends_with("sutura/gui.json"), "got {path:?}");

        let mut state = GuiState {
            intensity: Some("extreme".into()),
            language: Some("tr".into()),
            ..Default::default()
        };
        state.settings.wall_min_thickness = Some(2.25);
        state.settings.no_flap = true;
        state.settings.experimental_coaptation = true;
        save(&state).unwrap();

        let back = load();
        assert_eq!(back.intensity.as_deref(), Some("extreme"));
        assert_eq!(back.language.as_deref(), Some("tr"));
        assert_eq!(back.settings.wall_min_thickness, Some(2.25));
        assert!(back.settings.no_flap);
        assert!(back.settings.experimental_coaptation);

        // Overwrite in place, and the second write must win.
        let mut state = back;
        state.settings.no_flap = false;
        save(&state).unwrap();
        assert!(!load().settings.no_flap, "second save must replace the first");

        // A stale name in the file is dropped on load, not applied.
        let mut stale = GuiState::default();
        stale.settings.mode = Some("nonsense".into());
        stale.settings.profile = Some("nonsense".into());
        save(&stale).unwrap();
        let loaded = load();
        assert_eq!(loaded.settings.mode, None, "load must sanitize mode");
        assert_eq!(loaded.settings.profile, None, "load must sanitize profile");

        // A missing file is defaults, not an error.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!path.exists(), "purged");
        let missing = load();
        assert_eq!(missing.intensity, None, "absent file must not error");
        assert!(missing.language.is_none());
    }
}
