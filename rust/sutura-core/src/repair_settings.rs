// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The repair settings both front ends share.
//!
//! The CLI (clap) and the GUI (egui) must map their controls onto
//! [`RepairOptions`] in exactly one place, or the two drift apart and a GUI
//! repair stops matching the CLI's output for the same arguments. So the method
//! registry and the settings -> options conversion live here, with no
//! dependency on clap or on the `sutura-cli` binary:
//!
//! ```text
//!   Cli (clap)  ─┐
//!                 ├─→ RepairSettings ─→ options_from() ─→ RepairOptions
//!   GUI (egui)  ─┘
//! ```
//!
//! `RepairFailure` is defined here for the same reason: a settings error must be
//! reportable by both callers without either owning the type.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::repair::{ClosingMode, DeepRepair, RepairMode, RepairOptions, RepeatMode, SiMode, SolidEngine};

/// The 19 built-in repair methods, in registry order.
///
/// This is THE table: `method_by_num`, `parse_methods` and the CLI's
/// `--list-methods` all read it, so a method's id/category/name can never
/// disagree between the places that show it and the places that run it.
pub const METHODS: [(u8, &str, &str, &str); 19] = [
    (1, "quick_clean", "clean", "Quick Clean"),
    (2, "local_mend", "topology", "Local Mend"),
    (3, "full_mend", "topology", "Full Mend"),
    (4, "join", "topology", "Join"),
    (5, "autorefine", "si", "Autorefine"),
    (6, "exact_refine", "si", "Exact Refine"),
    (7, "ftetwild", "envelope", "fTetWild"),
    (8, "balloon", "closing", "Balloon"),
    (9, "backplate", "closing", "Backplate"),
    (10, "scaffold", "template", "Scaffold"),
    (11, "transplant", "pattern", "Transplant"),
    (12, "transplant_plus", "pattern", "Transplant+"),
    (13, "graft", "envelope", "Graft"),
    (14, "mirror_complete", "closing", "Mirror Complete"),
    (15, "wall_thicken", "envelope", "Wall Thicken"),
    (16, "dressing", "envelope", "Dressing"),
    (17, "tamponade", "interior", "Tamponade"),
    (18, "splint", "solid", "Splint"),
    (19, "coaptation", "solid", "Coaptation"),
];

/// Look a method up by its registry number.
pub fn method_by_num(num: u8) -> Option<&'static (u8, &'static str, &'static str, &'static str)> {
    METHODS.iter().find(|(n, _, _, _)| *n == num)
}

/// Every method as `(num, id, category, display_name)` rows.
pub fn method_rows() -> &'static [(u8, &'static str, &'static str, &'static str)] {
    &METHODS
}

/// A settings error with the process exit code the CLI should use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairFailure {
    pub code: u8,
    pub message: String,
}

impl RepairFailure {
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RepairFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Every repair control both front ends expose, independent of how it is
/// presented (clap argument or egui widget).
///
/// Field names follow the CLI flags so the mapping stays obvious. `None` means
/// "not specified", which each `options_from` branch translates with the same
/// default the CLI has always used.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepairSettings {
    pub mode: Option<String>,
    pub intensity: Option<String>,
    pub profile: Option<String>,
    pub deep_repair: Option<String>,
    pub si_mode: Option<String>,
    pub solid_engine: Option<String>,
    pub classifier_engine: Option<String>,
    pub methods: Option<String>,
    pub max_geometry_change: Option<f64>,
    pub max_risk: Option<f64>,
    pub force: bool,
    pub no_graft: bool,
    pub experimental_graft: bool,
    pub no_tamponade: bool,
    pub experimental_tamponade: bool,
    pub no_flap: bool,
    pub experimental_flap: bool,
    pub no_coaptation: bool,
    pub experimental_coaptation: bool,
    pub experimental_splint: bool,
    pub no_splint: bool,
    pub no_fallback_ftetwild: bool,
    pub experimental_fallback_ftetwild: bool,
    pub repeat_source: Option<[f64; 3]>,
    pub repeat_target: Option<[f64; 3]>,
    pub time_budget_s: Option<f64>,
    /// Dressing tier and its four knobs. Present on `RepairOptions` already; the
    /// switches live here so the GUI can expose them through the same mapping.
    pub dressing: bool,
    pub dressing_drain: Option<serde_json::Value>,
    pub dressing_defects: Option<String>,
    pub dressing_rmax_scale: Option<f64>,
    pub dressing_sigma_scale: Option<f64>,
    pub dressing_force_adopt: bool,
    pub wall_thicken: bool,
}

impl RepairSettings {
    /// The settings a GUI with only an intensity preset and a force toggle
    /// produces -- exactly what `sutura-gui` had before this module existed.
    pub fn from_intensity(intensity: &str, force: bool) -> Self {
        Self {
            intensity: Some(intensity.to_string()),
            force,
            ..Self::default()
        }
    }
}

fn parse_mode(text: Option<&str>) -> Result<RepairMode, RepairFailure> {
    match text.unwrap_or("auto") {
        "auto" => Ok(RepairMode::Auto),
        "low" => Ok(RepairMode::Low),
        "medium" => Ok(RepairMode::Medium),
        "aggressive" => Ok(RepairMode::Aggressive),
        "extreme" => Ok(RepairMode::Extreme),
        other => Err(RepairFailure::new(
            2,
            format!("invalid --mode value: {other}"),
        )),
    }
}

/// Deep-repair defaults follow the intensity, exactly as the CLI has always
/// done (`--intensity quick` alone means no deep ladder).
fn parse_deep(text: Option<&str>, intensity: &str) -> Result<DeepRepair, RepairFailure> {
    match text {
        Some("off") => Ok(DeepRepair::Off),
        Some("local") => Ok(DeepRepair::Local),
        Some("full") => Ok(DeepRepair::Full),
        Some(other) => Err(RepairFailure::new(
            2,
            format!("invalid --deep-repair value: {other}"),
        )),
        None => Ok(match intensity {
            "quick" => DeepRepair::Off,
            _ => DeepRepair::Full,
        }),
    }
}

fn parse_si(text: Option<&str>) -> Result<SiMode, RepairFailure> {
    match text.unwrap_or("report") {
        "repair" | "on" | "auto" => Ok(SiMode::Repair),
        "report" => Ok(SiMode::Report),
        "off" => Ok(SiMode::Off),
        other => Err(RepairFailure::new(
            2,
            format!("invalid --si-mode value: {other}"),
        )),
    }
}

fn parse_solid(text: Option<&str>) -> Result<SolidEngine, RepairFailure> {
    match text.unwrap_or("ftetwild") {
        "ftetwild" => Ok(SolidEngine::Ftetwild),
        "cdt" => Ok(SolidEngine::Cdt),
        other => Err(RepairFailure::new(
            2,
            format!("invalid --solid-engine value: {other}"),
        )),
    }
}

/// Parse `--methods`-style text into `(deep_repair, methods_given, numbers)`.
///
/// Every number in [`METHODS`] is accepted: `forced_methods` carries them into
/// `repair()`, so a tag actually runs. The three baseline methods also select
/// the deep-repair mode, matching the CLI's historical behaviour; method 12
/// (manual repeat) deliberately does not, because it needs its two points.
pub fn parse_methods_text(
    value: Option<&str>,
    deep: DeepRepair,
) -> Result<(DeepRepair, bool, Vec<u64>), RepairFailure> {
    let Some(raw) = value else {
        return Ok((deep, false, Vec::new()));
    };
    let mut nums = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let num = token
            .parse::<u64>()
            .map_err(|_| RepairFailure::new(2, format!("unknown method: {token}")))?;
        if num == 0 || num > METHODS.len() as u64 {
            return Err(RepairFailure::new(2, format!("unknown method: {num}")));
        }
        nums.push(num);
    }
    let mapped = match nums.last().copied() {
        Some(1) => DeepRepair::Off,
        Some(2) => DeepRepair::Local,
        Some(3) => DeepRepair::Full,
        _ => deep,
    };
    Ok((mapped, true, nums))
}

/// The single settings -> options conversion, shared by the CLI and the GUI.
///
/// Returns `(options, methods_given, method_numbers)` to mirror what
/// `build_options` has always returned.
pub fn options_from(
    settings: &RepairSettings,
) -> Result<(RepairOptions, bool, Vec<u64>), RepairFailure> {
    let mode = parse_mode(settings.mode.as_deref())?;
    let si_mode = parse_si(settings.si_mode.as_deref())?;
    let solid_engine = parse_solid(settings.solid_engine.as_deref())?;
    let intensity = settings
        .intensity
        .clone()
        .unwrap_or_else(|| "balanced".to_string());
    let mut base_deep = parse_deep(settings.deep_repair.as_deref(), &intensity)?;
    // `--no-fallback-ftetwild` / `--experimental-fallback-ftetwild` also move the
    // deep-repair mode when `--deep-repair` is absent, and the explicit flag
    // always wins. This ordering is the CLI's, preserved exactly.
    if settings.deep_repair.is_none() {
        if settings.no_fallback_ftetwild {
            base_deep = DeepRepair::Off;
        } else if settings.experimental_fallback_ftetwild {
            base_deep = DeepRepair::Full;
        }
    }
    let (deep_repair, methods_given, methods) =
        parse_methods_text(settings.methods.as_deref(), base_deep)?;

    // `--repeat-source` / `--repeat-target` are required together when method
    // 12 is tagged, exactly as the Python CLI requires.
    if methods.contains(&12)
        && (settings.repeat_source.is_none() || settings.repeat_target.is_none())
    {
        return Err(RepairFailure::new(
            2,
            "--methods 12 requires --repeat-source X,Y,Z and --repeat-target Y,Z",
        ));
    }
    let repeat = if settings.repeat_source.is_some() || settings.repeat_target.is_some() {
        Some(RepeatMode::Manual)
    } else {
        None
    };

    // A tri-state toggle: `--no-X` wins, then `--experimental-X`, else default
    // on. Written once here so both front ends inherit the same precedence.
    let tri = |no: bool, yes: bool, default_on: bool| -> Option<bool> {
        if no {
            Some(false)
        } else if yes {
            Some(true)
        } else if default_on {
            Some(true)
        } else {
            None
        }
    };

    let mut opts = RepairOptions {
        mode,
        intensity: intensity.clone(),
        profile: settings.profile.clone(),
        deep_repair,
        closing: None::<ClosingMode>,
        proxy_template: false,
        graft: tri(settings.no_graft, settings.experimental_graft, true),
        tamponade: tri(
            settings.no_tamponade,
            settings.experimental_tamponade,
            true,
        ),
        flap: tri(settings.no_flap, settings.experimental_flap, true),
        coaptation: tri(
            settings.no_coaptation,
            settings.experimental_coaptation,
            true,
        ),
        splint: tri(settings.no_splint, settings.experimental_splint, false)
            .unwrap_or(false),
        dressing: settings.dressing,
        dressing_drain: settings.dressing_drain.clone(),
        dressing_defects: settings.dressing_defects.clone(),
        dressing_rmax_scale: settings.dressing_rmax_scale,
        dressing_sigma_scale: settings.dressing_sigma_scale,
        dressing_force_adopt: settings.dressing_force_adopt,
        wall_thicken: settings.wall_thicken,
        repeat,
        repeat_source: settings.repeat_source,
        repeat_target: settings.repeat_target,
        solid_engine,
        si_mode,
        ftetwild: if settings.no_fallback_ftetwild {
            Some(Value::Bool(false))
        } else if settings.experimental_fallback_ftetwild {
            Some(Value::Bool(true))
        } else {
            None
        },
        time_budget_s: settings.time_budget_s,
        seed: 0,
        forced_methods: methods
            .iter()
            .filter_map(|n| u8::try_from(*n).ok())
            .collect(),
        ..RepairOptions::default()
    };
    // The CLI has always forced the Graft path when method 13 is tagged.
    if methods.contains(&13) {
        opts.graft = Some(true);
        opts.deep_repair = DeepRepair::Off;
    }
    Ok((opts, methods_given, methods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dressing_and_wall_switches_reach_the_options() {
        // These were hardcoded to `false` before they were part of the settings;
        // a GUI checkbox must actually change the options.
        let off = RepairSettings::default();
        let (o, _, _) = options_from(&off).unwrap();
        assert!(!o.dressing, "dressing defaults off");
        assert!(!o.wall_thicken, "wall thicken defaults off");

        let on = RepairSettings {
            dressing: true,
            dressing_drain: Some(serde_json::json!({"kind": "manual"})),
            dressing_defects: Some("auto".to_string()),
            dressing_rmax_scale: Some(1.5),
            dressing_sigma_scale: Some(0.75),
            dressing_force_adopt: true,
            wall_thicken: true,
            ..Default::default()
        };
        let (o, _, _) = options_from(&on).unwrap();
        assert!(o.dressing);
        assert!(o.wall_thicken);
        assert!(o.dressing_force_adopt);
        assert_eq!(o.dressing_rmax_scale, Some(1.5));
        assert_eq!(o.dressing_sigma_scale, Some(0.75));
        assert_eq!(o.dressing_defects.as_deref(), Some("auto"));
        assert!(o.dressing_drain.is_some());
    }

    #[test]
    fn every_registry_number_parses_and_maps() {
        for num in 1..=19u64 {
            // Method 12 requires its two points, so supply them for the sweep;
            // every other method ignores them.
            let s = RepairSettings {
                methods: Some(num.to_string()),
                repeat_source: Some([0.0, 0.0, 0.0]),
                repeat_target: Some([1.0, 0.0, 0.0]),
                ..Default::default()
            };
            let (opts, given, nums) = options_from(&s).unwrap_or_else(|e| {
                panic!("method {num} rejected: {}", e.message)
            });
            assert!(given);
            assert_eq!(nums, vec![num]);
            assert_eq!(opts.forced_methods, vec![num as u8], "method {num}");
        }
    }

    #[test]
    fn unknown_method_is_rejected() {
        for bad in ["0", "20", "999"] {
            let s = RepairSettings {
                methods: Some(bad.to_string()),
                ..Default::default()
            };
            let err = options_from(&s).unwrap_err();
            assert_eq!(err.code, 2, "{bad}");
            assert!(err.message.contains("unknown method"), "{bad}");
        }
    }

    #[test]
    fn baseline_methods_select_the_deep_repair_mode() {
        for (num, want) in [(1u64, DeepRepair::Off), (2, DeepRepair::Local), (3, DeepRepair::Full)] {
            let s = RepairSettings {
                methods: Some(num.to_string()),
                ..Default::default()
            };
            let (opts, _, _) = options_from(&s).unwrap();
            assert_eq!(opts.deep_repair, want, "method {num}");
        }
        // Method 12 must not move the baseline mode.
        let s = RepairSettings {
            methods: Some("12".to_string()),
            repeat_source: Some([0.0, 0.0, 0.0]),
            repeat_target: Some([1.0, 0.0, 0.0]),
            ..Default::default()
        };
        let (opts, _, _) = options_from(&s).unwrap();
        assert_eq!(opts.deep_repair, DeepRepair::Full);
        assert_eq!(opts.repeat, Some(RepeatMode::Manual));
    }

    #[test]
    fn method_12_requires_both_points() {
        let s = RepairSettings {
            methods: Some("12".to_string()),
            ..Default::default()
        };
        let err = options_from(&s).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.message.contains("--repeat-source"));
    }

    #[test]
    fn gui_defaults_match_the_cli_defaults() {
        // A GUI with only intensity + force must produce the same options the
        // CLI produces with no repair flags at all.
        let gui = RepairSettings::from_intensity("balanced", false);
        let (from_gui, _, _) = options_from(&gui).unwrap();
        let (from_cli_defaults, _, _) = options_from(&RepairSettings::default()).unwrap();

        assert_eq!(from_gui.intensity, "balanced");
        assert_eq!(from_gui.deep_repair, DeepRepair::Full);
        assert_eq!(from_gui.graft, Some(true));
        assert_eq!(from_gui.tamponade, Some(true));
        assert_eq!(from_gui.flap, Some(true));
        assert_eq!(from_gui.coaptation, Some(true));
        assert!(!from_gui.splint);
        assert_eq!(from_gui.repeat, None);
        assert!(from_gui.forced_methods.is_empty());
        // The GUI's intensity must not smuggle in any other difference.
        assert_eq!(from_gui.deep_repair, from_cli_defaults.deep_repair);
        assert_eq!(from_gui.graft, from_cli_defaults.graft);
        assert_eq!(from_gui.solid_engine, from_cli_defaults.solid_engine);
        assert_eq!(from_gui.si_mode, from_cli_defaults.si_mode);
    }

    #[test]
    fn method_13_forces_graft_and_skips_the_deep_ladder() {
        let s = RepairSettings {
            methods: Some("13".to_string()),
            ..Default::default()
        };
        let (opts, _, _) = options_from(&s).unwrap();
        assert_eq!(opts.graft, Some(true));
        assert_eq!(opts.deep_repair, DeepRepair::Off);
    }

    #[test]
    fn tri_state_flags_prefer_off_then_on_then_default() {
        let off = RepairSettings {
            no_graft: true,
            experimental_graft: true,
            ..Default::default()
        };
        assert_eq!(options_from(&off).unwrap().0.graft, Some(false));
        let on = RepairSettings {
            experimental_graft: true,
            ..Default::default()
        };
        assert_eq!(options_from(&on).unwrap().0.graft, Some(true));
        let dflt = RepairSettings::default();
        assert_eq!(options_from(&dflt).unwrap().0.graft, Some(true));
    }
}
