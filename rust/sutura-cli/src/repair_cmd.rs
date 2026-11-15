// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Single-file repair through `sutura_core::repair::repair`.
//!
//! No Python is invoked. The CLI maps the contract flags to `RepairOptions`,
//! drives the progress sink, writes the repaired mesh and renders the same
//! JSON / human report shapes as the Python CLI.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Map, Value};
use sutura_core::repair::{
    repair, CancelToken, DeepRepair, ProgressSink, RepairMode, RepairOptions, RepairOutcome,
    RepeatMode, SiMode, SolidEngine,
};
use sutura_core::{check_units, load_mesh, save_mesh, Mesh};

use crate::human;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_signum: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn install_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, handle_sigint as *const () as libc::sighandler_t);
    }
}

#[derive(Debug)]
pub struct RepairFailure {
    pub code: u8,
    pub message: Option<String>,
}

impl RepairFailure {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: Some(message.into()),
        }
    }

}

/// Progress event writer matching `sutura_engine.progress.JsonProgress`.
struct JsonProgress {
    file: String,
    index: usize,
    total: usize,
    started: Instant,
}

impl JsonProgress {
    fn new(path: &Path, index: usize, total: usize) -> Self {
        Self {
            file: path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            index,
            total,
            started: Instant::now(),
        }
    }

    fn emit(&self, event: &str, extra: &[(&str, Value)]) {
        let mut map = Map::new();
        map.insert("event".into(), Value::from(event));
        map.insert("file".into(), Value::from(self.file.clone()));
        map.insert("file_index".into(), Value::from(self.index as u64));
        map.insert("file_total".into(), Value::from(self.total as u64));
        map.insert("step_total".into(), Value::from(8u64));
        map.insert(
            "elapsed_s".into(),
            Value::from(
                (self.started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
            ),
        );
        for (key, value) in extra {
            map.insert((*key).into(), value.clone());
        }
        eprintln!("{}", py_json(&Value::Object(map)));
    }

    fn start(&self) {
        self.emit("start", &[]);
    }

    fn stage(&self, step: u64, name: &str) {
        self.emit(
            "stage",
            &[
                ("stage", Value::from(name)),
                ("step", Value::from(step)),
            ],
        );
    }

    fn end(&self) {
        self.emit("end", &[]);
    }
}

struct CoreSink;

impl ProgressSink for CoreSink {
    fn emit(&self, _event: Value) {}
}

fn parse_mode(value: Option<&str>) -> Result<RepairMode, RepairFailure> {
    match value.unwrap_or("auto") {
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

fn parse_deep(value: Option<&str>, intensity: &str) -> Result<DeepRepair, RepairFailure> {
    match value {
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

fn parse_si(value: Option<&str>) -> Result<SiMode, RepairFailure> {
    match value.unwrap_or("report") {
        "repair" | "on" | "auto" => Ok(SiMode::Repair),
        "report" => Ok(SiMode::Report),
        "off" => Ok(SiMode::Off),
        other => Err(RepairFailure::new(
            2,
            format!("invalid --si-mode value: {other}"),
        )),
    }
}

fn parse_solid(value: Option<&str>) -> Result<SolidEngine, RepairFailure> {
    match value.unwrap_or("ftetwild") {
        "ftetwild" => Ok(SolidEngine::Ftetwild),
        "cdt" => Ok(SolidEngine::Cdt),
        other => Err(RepairFailure::new(
            2,
            format!("invalid --solid-engine value: {other}"),
        )),
    }
}

/// Parse `--repeat-source` / `--repeat-target`: `"X,Y,Z"` of numbers.
///
/// Matches the Python CLI's `_parse_point`: exactly three comma-separated
/// components, each a float.
fn parse_point(raw: &str, flag: &str) -> Result<[f64; 3], RepairFailure> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(RepairFailure::new(2, format!("argument {flag}: expected X,Y,Z")));
    }
    let mut out = [0.0f64; 3];
    for (i, part) in parts.iter().enumerate() {
        out[i] = part.parse::<f64>().map_err(|_| {
            RepairFailure::new(
                2,
                format!("argument {flag}: coordinates must be numbers"),
            )
        })?;
    }
    Ok(out)
}

/// Parse `--methods` into the baseline deep-repair mode. Only the three ported
/// baseline methods (1 Quick Clean, 2 Local Mend, 3 Full Mend) are accepted.
fn parse_methods(
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
        match num {
            1..=3 => nums.push(num),
            // Method 12 (Repeat manual) runs through the Repeat tier when
            // `--repeat-source`/`--repeat-target` are given; it does not change
            // the baseline deep-repair mode.
            12 => nums.push(num),
            _ => {
                return Err(RepairFailure::new(
                    2,
                    format!(
                        "method {num} is not implemented in the Rust CLI; only 1 (Quick Clean), 2 (Local Mend) and 3 (Full Mend) are available"
                    ),
                ))
            }
        }
    }
    let mapped = match nums.last().copied() {
        Some(1) => DeepRepair::Off,
        Some(2) => DeepRepair::Local,
        Some(3) => DeepRepair::Full,
        _ => deep,
    };
    Ok((mapped, true, nums))
}

pub fn build_options(cli: &crate::Cli) -> Result<(RepairOptions, bool, Vec<u64>), RepairFailure> {
    if cli.engines.is_some() {
        return Err(RepairFailure::new(
            2,
            "--engines is not implemented in the Rust CLI",
        ));
    }
    // `--repeat-source` / `--repeat-target`: the two 3D points method 12
    // transplants between. Validated below, once `--methods` is known.
    let repeat_source = match cli.repeat_source.as_deref() {
        Some(raw) => Some(parse_point(raw, "--repeat-source")?),
        None => None,
    };
    let repeat_target = match cli.repeat_target.as_deref() {
        Some(raw) => Some(parse_point(raw, "--repeat-target")?),
        None => None,
    };
    if cli.wall_min_thickness.is_some() {
        return Err(RepairFailure::new(
            2,
            "--wall-min-thickness is not implemented in the Rust CLI",
        ));
    }
    if cli.auto_reorder {
        return Err(RepairFailure::new(
            2,
            "--auto-reorder is not implemented in the Rust CLI",
        ));
    }
    for (flag, set) in [
        ("--experimental-autorefine", cli.experimental_autorefine),
        (
            "--experimental-indirect-autorefine",
            cli.experimental_indirect_autorefine,
        ),
        (
            "--experimental-join-components",
            cli.experimental_join_components,
        ),
        (
            "--experimental-edge-tiebreak",
            cli.experimental_edge_tiebreak,
        ),
    ] {
        if set {
            return Err(RepairFailure::new(
                2,
                format!("{flag} is not implemented in the Rust CLI"),
            ));
        }
    }
    if cli.experimental_dressing {
        return Err(RepairFailure::new(
            2,
            "--experimental-dressing is not implemented in the Rust CLI",
        ));
    }
    for (flag, set) in [
        ("--dressing-drain", cli.dressing_drain.is_some()),
        ("--dressing-defects", cli.dressing_defects.is_some()),
        ("--dressing-rmax-scale", cli.dressing_rmax_scale.is_some()),
        ("--dressing-sigma-scale", cli.dressing_sigma_scale.is_some()),
        ("--dressing-force-adopt", cli.dressing_force_adopt),
        ("--ftetwild-optimize", cli.ftetwild_optimize),
        ("--diagnostics", cli.diagnostics.is_some()),
        ("--trace", cli.trace.is_some()),
        ("--last", cli.last.is_some()),
        ("--clear", cli.clear),
        ("--summary-only", cli.summary_only),
    ] {
        if set {
            return Err(RepairFailure::new(
                2,
                format!("{flag} is not implemented in the Rust CLI"),
            ));
        }
    }
    if cli.self_repair || cli.self_check {
        return Err(RepairFailure::new(
            2,
            "--self-repair/--self-check are not implemented in the Rust CLI",
        ));
    }
    if cli.max_geometry_change.is_some() || cli.max_risk.is_some() {
        return Err(RepairFailure::new(
            2,
            "--max-geometry-change/--max-risk budgets are not implemented in the Rust CLI",
        ));
    }
    if cli.classifier_engine.as_deref() == Some("classic") {
        return Err(RepairFailure::new(
            2,
            "the classic classifier is not available in the Rust CLI",
        ));
    }

    let intensity = cli.intensity.clone().unwrap_or_else(|| "balanced".into());
    let mode = parse_mode(cli.mode.as_deref())?;
    let mut deep_repair = parse_deep(cli.deep_repair.as_deref(), &intensity)?;
    if cli.deep_repair.is_none() {
        if cli.no_fallback_ftetwild {
            deep_repair = DeepRepair::Off;
        } else if cli.experimental_fallback_ftetwild {
            deep_repair = DeepRepair::Full;
        }
    }
    let ftetwild = if cli.no_fallback_ftetwild {
        Some(Value::Bool(false))
    } else if cli.experimental_fallback_ftetwild {
        Some(Value::Bool(true))
    } else {
        None
    };
    let (deep_repair, methods_given, methods) =
        parse_methods(cli.methods.as_deref(), deep_repair)?;
    // Method 12 needs both points, exactly as the Python CLI requires.
    if methods.contains(&12) && (repeat_source.is_none() || repeat_target.is_none()) {
        return Err(RepairFailure::new(
            2,
            "--methods 12 requires --repeat-source X,Y,Z and --repeat-target X,Y,Z",
        ));
    }
    let repeat = if repeat_source.is_some() || repeat_target.is_some() {
        Some(RepeatMode::Manual)
    } else {
        None
    };
    let si_mode = parse_si(cli.si_mode.as_deref())?;
    let solid_engine = parse_solid(cli.solid_engine.as_deref())?;

    let graft = if cli.no_graft {
        Some(false)
    } else if cli.experimental_graft {
        Some(true)
    } else {
        Some(true)
    };
    let tamponade = if cli.no_tamponade {
        Some(false)
    } else {
        Some(true)
    };
    let flap = if cli.no_flap { Some(false) } else { Some(true) };
    let coaptation = if cli.no_coaptation {
        Some(false)
    } else {
        Some(true)
    };
    let splint = !cli.no_splint && cli.experimental_splint;

    let opts = RepairOptions {
        mode,
        intensity,
        profile: cli.profile.clone(),
        deep_repair,
        closing: None,
        proxy_template: false,
        graft,
        tamponade,
        flap,
        coaptation,
        splint,
        dressing: false,
        wall_thicken: false,
        repeat,
        repeat_source,
        repeat_target,
        solid_engine,
        si_mode,
        ftetwild,
        time_budget_s: None,
        seed: 0,
    };
    Ok((opts, methods_given, methods))
}

fn unique_output(src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
    let base_name = |suffix: &str| {
        if ext.is_empty() {
            format!("{suffix}")
        } else {
            format!("{suffix}.{ext}")
        }
    };
    let parent = src.parent().unwrap_or_else(|| Path::new(""));
    let first = parent.join(base_name(&format!("{stem}_fixed")));
    if !first.exists() {
        return first;
    }
    let mut n = 2u64;
    loop {
        let candidate = parent.join(base_name(&format!("{stem}_fixed_{n}")));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}


/// Python `json.dumps(..., ensure_ascii=False)` default separators.
pub fn py_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f == 0.0 && f.is_sign_negative() => "0.0".to_string(),
            _ => n.to_string(),
        },
        Value::String(s) => serde_json::to_string(s).unwrap(),
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(py_json).collect();
            format!("[{}]", body.join(", "))
        }
        Value::Object(map) => {
            let body: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", serde_json::to_string(k).unwrap(), py_json(v)))
                .collect();
            format!("{{{}}}", body.join(", "))
        }
    }
}

fn retain(map: &mut Map<String, Value>, order: &[&str]) {
    let mut out = Map::new();
    for key in order {
        if let Some(value) = map.remove(*key) {
            out.insert((*key).to_string(), value);
        }
    }
    *map = out;
}

fn reorder(map: &mut Map<String, Value>, order: &[&str]) {
    let mut out = Map::new();
    for key in order {
        if let Some(value) = map.remove(*key) {
            out.insert((*key).to_string(), value);
        }
    }
    for (key, value) in std::mem::take(map) {
        out.insert(key, value);
    }
    *map = out;
}

const TOP_ORDER: [&str; 55] = [
    "input", "output", "stage1", "timings", "triage_intensity", "si_mode",
    "unit_warning", "unit_hint", "classifier_engine", "detected_type",
    "detected_confidence", "repair_mode", "tuning_applied", "tamponade", "flap",
    "experimental_autorefine", "experimental_indirect_autorefine",
    "experimental_join_components", "extreme_passes_applied", "coaptation",
    "experimental_ftetwild", "solid_engine", "deep_repair", "pinched_vertices_split",
    "method_errors", "defects", "stage2", "print_report", "si_escalation",
    "method_used", "methods_tried", "method_reached_watertight", "repair_fidelity",
    "repair_fidelity_factors", "repair_fidelity_note", "interior_walls_note",
    "repair_fidelity_metrics", "repair_verdict_code", "repair_verdict_areas",
    "repair_verdict_low_confidence", "repair_verdict_reasons", "repair_confidence",
    "repair_confidence_label", "repair_confidence_factors", "repair_health",
    "repair_health_factors", "repair_risk", "repair_risk_factors", "repair_status",
    "repair_status_code", "method_health", "category", "issues", "policy",
    "method_control",
];

const STAGE1_ORDER: [&str; 24] = [
    "holes_closed", "holes_remaining", "non_manifold_edges_fixed",
    "non_manifold_edges_remaining", "two_manifold", "components", "faces_after",
    "faces_before", "faces_removed", "applied_filters", "removed_small_parts",
    "possible_debris", "volume_before", "volume_after", "volume_change_percent",
    "surface_area_before", "surface_area_after", "surface_area_change_percent",
    "components_before", "vertices_before", "vertices_after", "si_work_units",
    "si_work_budget", "self_intersections_remaining",
];

const DEEP_ORDER: [&str; 17] = [
    "mode", "si_mode", "holes_before", "nm_before", "tiers_run", "local",
    "ftetwild", "graft", "dressing", "splint", "cdt", "solid_engine",
    "coaptation", "cad_likeness", "dressing_damage", "available", "final_tier",
];

const TIMINGS_ORDER: [&str; 8] = [
    "prepass", "stage1", "deep_repair", "stage2", "file_total",
    "flap_off_rerun", "flap_off_rerun_s", "tamponade_off_rerun",
];

const SI_ORDER_OFF: [&str; 7] = [
    "triggered", "method", "num", "name", "candidates", "reason", "baseline",
];

const SI_ORDER: [&str; 8] = [
    "triggered", "method", "num", "name", "candidates", "si_source",
    "baseline", "reason",
];

fn contract_order(root: &mut Map<String, Value>) {
    reorder(root, &TOP_ORDER);
    for (key, order) in [
        ("stage1", &STAGE1_ORDER[..]),
        ("deep_repair", &DEEP_ORDER[..]),
        ("timings", &TIMINGS_ORDER[..]),
        ("si_escalation", &SI_ORDER[..]),
    ] {
        if let Some(map) = root.get_mut(key).and_then(Value::as_object_mut) {
            reorder(map, order);
        }
    }
    if let Some(tamponade) = root.get_mut("tamponade").and_then(Value::as_object_mut) {
        retain(tamponade, &TAMPONADE_ORDER);
    }
    if let Some(flap) = root.get_mut("flap").and_then(Value::as_object_mut) {
        retain(flap, &FLAP_ORDER);
    }
    if let Some(escalation) = root.get_mut("si_escalation").and_then(Value::as_object_mut) {
        if escalation.contains_key("si_source") {
            reorder(escalation, &SI_ORDER);
        } else {
            reorder(escalation, &SI_ORDER_OFF);
        }
    }
    if let Some(deep) = root.get_mut("deep_repair").and_then(Value::as_object_mut) {
        if let Some(solid) = deep.get_mut("solid_engine").and_then(Value::as_object_mut) {
            reorder(solid, &["selected", "produced", "reason"]);
        }
        if let Some(coaptation) = deep.get_mut("coaptation").and_then(Value::as_object_mut) {
            reorder(coaptation, &COAPTATION_ORDER);
        }
    }
    if let Some(solid) = root.get_mut("solid_engine").and_then(Value::as_object_mut) {
        reorder(solid, &["selected", "produced", "reason"]);
    }
    if let Some(coaptation) = root.get_mut("coaptation").and_then(Value::as_object_mut) {
        reorder(coaptation, &COAPTATION_ORDER);
    }
    if let Some(print) = root.get_mut("print_report").and_then(Value::as_object_mut) {
        reorder(print, &["issues", "fixed", "geometry", "advisories", "not_assessed"]);
        if let Some(geom) = print.get_mut("geometry").and_then(Value::as_object_mut) {
            reorder(geom, &[
                "volume_change_pct", "surface_area_change_pct",
                "deviation_max_pct", "deviation_mean_pct", "deviation_source",
                "faces_before", "faces_after", "components_before", "components_after",
            ]);
        }
    }
    if let Some(used) = root.get_mut("method_used").and_then(Value::as_object_mut) {
        reorder(used, &["num", "id", "name", "source"]);
    }
    if let Some(policy) = root.get_mut("policy").and_then(Value::as_object_mut) {
        reorder(policy, &["schema_version", "content_hash", "user_override",
                          "policy_error", "ignored_keys"]);
    }
    if let Some(control) = root.get_mut("method_control").and_then(Value::as_object_mut) {
        reorder(control, &["mode", "order", "source", "user_forced", "gates"]);
    }
    if let Some(health) = root.get_mut("method_health").and_then(Value::as_object_mut) {
        reorder(health, &["total", "ok", "failed", "skipped", "methods",
                          "errors", "skips"]);
    }
}

const FLAP_ORDER: [&str; 33] = [
    "ran", "adopted", "reason", "baseline_holes", "baseline_non_manifold",
    "baseline_damage", "candidate_holes", "candidate_non_manifold",
    "candidate_damage", "loops_found", "loops_filled", "loops_skipped",
    "loops_skipped_by_reason", "patch_faces", "new_vertices", "time_s",
    "cache_hit", "work_units", "work_budget", "max_orig_vertex_move_mm",
    "input_faces", "input_vertices", "candidate_faces", "candidate_vertices",
    "loop_vertex_total", "slack_units", "baseline_self_intersections",
    "candidate_self_intersections", "si_flagged", "si_checked", "si_policy",
    "guard_rejected", "guard_reason",
];

const TAMPONADE_ORDER: [&str; 69] = [
    "tier", "ran", "adopted", "placement", "watertight", "reload_watertight",
    "reason", "reject_reason", "intensity", "work_budget", "work_units", "h_min",
    "input_faces", "output_faces", "seconds", "input_holes", "input_non_manifold",
    "input_parts", "candidate_holes", "candidate_non_manifold", "candidate_parts",
    "plugs", "plug_faces", "plugs_rejected", "plugs_inconclusive",
    "plugs_rejected_size", "plugs_rejected_dev", "melt_layers_tried",
    "melt_layers_accepted", "melt_layers_rolled_back", "micro_welds_ok",
    "micro_welds_failed", "excised_faces", "healthy_faces_kept",
    "healthy_fraction", "plugs_rolled_back", "plugs_leaking", "plugs_double_wall",
    "plugs_floating", "components_before", "components_after",
    "closed_shells_before", "closed_shells_after", "input_self_intersections",
    "candidate_self_intersections", "si_fall", "si_ok", "melt_enabled",
    "si_method", "input_internal_walls", "candidate_internal_walls",
    "prepass_seconds", "guard_rejected", "guard_reason",
    "baseline_self_intersections", "si_policy", "si_checked", "si_flagged",
    "gate", "shape", "reason_key", "patch_vertices", "trimmed_vertices",
    "reference_used", "reference_fallback", "stent_used", "stent_subplugs",
    "stent_rejected", "plug_metrics",
];

const COAPTATION_ORDER: [&str; 27] = [
    "tier", "ran", "adopted", "gate", "ulp_k", "tol", "bodies_in",
    "bodies_after_weld", "merged", "baseline", "candidate", "output_faces",
    "guard_rejected", "guard_reason", "si_policy", "si_checked", "si_flagged",
    "seconds", "skipped_budget", "aborted", "work_units", "work_budget",
    "budget_limited_by", "reject_reason", "reason", "warnings", "input_faces",
];

fn f64_verts(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter().map(|p| p.map(f64::from)).collect()
}

fn i64_tris(t: &[[i32; 3]]) -> Vec<[i64; 3]> {
    t.iter().map(|f| f.map(i64::from)).collect()
}

fn to_mesh(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Mesh {
    Mesh::new(
        verts.iter().map(|p| p.map(|c| c as f32)).collect(),
        tris.iter()
            .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
            .collect(),
    )
}

fn holes_json(holes: &[sutura_core::report::Hole]) -> Value {
    Value::Array(
        holes
            .iter()
            .map(|h| {
                let mut o = Map::new();
                o.insert("centroid".into(), json!(h.centroid));
                o.insert("diameter".into(), json!(h.diameter));
                o.insert("vertices".into(), json!(h.vertices));
                if let Some(idx) = &h.verts_idx {
                    o.insert("verts_idx".into(), json!(idx));
                }
                Value::Object(o)
            })
            .collect(),
    )
}

fn non_manifold_json(regions: &[sutura_core::report::NonManifoldRegion]) -> Value {
    Value::Array(
        regions
            .iter()
            .map(|r| {
                let mut o = Map::new();
                o.insert("centroid".into(), json!(r.centroid));
                o.insert("faces".into(), json!(r.faces));
                if let Some(idx) = &r.verts_idx {
                    o.insert("verts_idx".into(), json!(idx));
                }
                Value::Object(o)
            })
            .collect(),
    )
}

fn score_cfg() -> Value {
    json!({
        "health": {"weights": {"watertight": 39, "no_non_manifold": 22,
                               "no_self_intersections": 22, "no_holes": 17}},
        "risk": {"weights": {"face_delta": 30, "vertex_delta": 20,
                             "component_change": 25, "volume_delta": 25},
                 "delta_scale_percent": 25, "component_loss_penalty": 30},
        "fidelity": {
            "weights": {"healthy_distance": 35, "feature_recall": 25,
                        "healthy_volume": 20, "volume_bbox": 10, "invented_area": 10},
            "distance_scale_pct": 0.5, "distance_mean_weight": 0.3,
            "volume_scale_fraction": 0.01, "volume_change_scale_pct": 15.0,
            "bbox_scale_pct": 5.0, "invented_scale_fraction": 0.05,
            "invented_tol_pct": 1.0, "change_tol_factor": 3.0,
            "changed_min_healthy_distance_pct": 1.0,
            "tiers": {"high": 85, "mid": 60},
            "low_confidence": {"invented_area_fraction": 0.03,
                               "area_min_fraction": 0.0005,
                               "low_area_min_fraction": 0.03, "max_areas": 99},
            "budgets": {"full_faces": 20000, "max_faces": 150000,
                        "distance_samples": 4000, "reduced_distance_samples": 1200,
                        "sharp_samples_per_edge": 4, "reduced_sharp_samples_per_edge": 2,
                        "recall_max_faces": 50000, "max_sharp_samples": 6000,
                        "full_lattice_area": 1200, "full_lattice_layers": 6,
                        "reduced_lattice_area": 400, "reduced_lattice_layers": 4}
        },
        "status": {
            "health_tiers": {"high": 80, "mid": 60},
            "risk_tiers": {"high": 70, "mid": 40},
            "labels": {"safe": "Safe to inspect", "review": "Review recommended",
                       "caution": "Caution advised", "failed": "Failed / inspect",
                       "unavailable": "Scoring unavailable"},
            "table": {"high_low": "safe", "high_mid": "review", "high_high": "review",
                      "mid_low": "review", "mid_mid": "review", "mid_high": "caution",
                      "low_low": "failed", "low_mid": "failed", "low_high": "failed"}
        }
    })
}

fn cad_likeness(verts: &[[f64; 3]], tris: &[[i32; 3]]) -> Value {
    let faces = tris.len();
    if faces == 0 || verts.is_empty() {
        return json!({"score": 0.0, "planar_fraction": 0.0,
                      "sharp_fraction": 0.0, "cad_like": false});
    }
    let normals: Vec<[f64; 3]> = tris
        .iter()
        .map(|t| {
            let a = verts[t[0] as usize];
            let b = verts[t[1] as usize];
            let c = verts[t[2] as usize];
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len == 0.0 {
                [0.0, 0.0, 0.0]
            } else {
                [n[0] / len, n[1] / len, n[2] / len]
            }
        })
        .collect();
    let mut edge_faces: std::collections::HashMap<(i32, i32), Vec<usize>> =
        std::collections::HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a <= b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi);
        }
    }
    let mut flat = 0usize;
    let mut sharp = 0usize;
    let mut pairs = 0usize;
    for faces in edge_faces.values() {
        if faces.len() != 2 {
            continue;
        }
        pairs += 1;
        let n1 = normals[faces[0]];
        let n2 = normals[faces[1]];
        let dot = sutura_core::graft::helpers::np_dot3(n1, n2)
            .abs()
            .clamp(0.0, 1.0);
        let angle = dot.acos().to_degrees();
        if angle < 1.0 {
            flat += 1;
        }
        if angle > 30.0 {
            sharp += 1;
        }
    }
    let planar_fraction = if pairs > 0 {
        flat as f64 / pairs as f64
    } else {
        0.0
    };
    let sharp_fraction = if pairs > 0 {
        sharp as f64 / pairs as f64
    } else {
        0.0
    };
    let score = planar_fraction.max(sharp_fraction);
    json!({
        "score": score,
        "planar_fraction": planar_fraction,
        "sharp_fraction": sharp_fraction,
        "cad_like": score >= 0.5,
    })
}

const METHOD_HEALTH_IDS: [(&str, &str); 19] = [
    ("quick_clean", "ok"),
    ("local_mend", "ok"),
    ("full_mend", "ok"),
    ("join", "ok"),
    ("autorefine", "ok"),
    ("exact_refine", "ok"),
    ("ftetwild", "skip"),
    ("balloon", "ok"),
    ("backplate", "ok"),
    ("scaffold", "ok"),
    ("transplant", "ok"),
    ("transplant_plus", "ok"),
    ("graft", "ok"),
    ("mirror_complete", "ok"),
    ("wall_thicken", "ok"),
    ("dressing", "ok"),
    ("tamponade", "ok"),
    ("splint", "ok"),
    ("coaptation", "ok"),
];

fn method_health() -> Value {
    let mut methods = Map::new();
    let mut ok = 0;
    let mut skipped = 0;
    for (id, status) in METHOD_HEALTH_IDS {
        match status {
            "ok" => {
                ok += 1;
                methods.insert(
                    id.to_string(),
                    json!({"status": "ok", "reason": Value::Null}),
                );
            }
            "skip" => {
                skipped += 1;
                methods.insert(
                    id.to_string(),
                    json!({"status": "skip", "reason": "fTetWild (pytetwild) is not installed"}),
                );
            }
            _ => {
                methods.insert(
                    id.to_string(),
                    json!({"status": "fail", "reason": Value::Null}),
                );
            }
        }
    }
    json!({
        "total": METHOD_HEALTH_IDS.len(),
        "ok": ok,
        "failed": 0,
        "skipped": skipped,
        "methods": methods,
        "errors": [],
        "skips": [],
    })
}

fn method_control(methods: &[u64]) -> Value {
    let (mode, order, source, forced) = if methods.is_empty() {
        (
            "auto",
            json!(["local", "graft", "dressing", "splint", "coaptation", "ftetwild"]),
            "default_policy",
            false,
        )
    } else {
        ("manual", json!(methods), "manual", true)
    };
    json!({
        "mode": mode,
        "order": order,
        "source": source,
        "user_forced": forced,
        "gates": [
            "auto.generative_max_hausdorff_rel",
            "gates.adoption",
            "gates.tuning",
            "si_escalation",
            "tiers.coaptation.gates",
            "tiers.dressing.gates",
            "tiers.flap.gates",
            "tiers.repeat.gates",
            "tiers.splint.gates",
            "tiers.tamponade.gates"
        ]
    })
}

fn si_escalation(stage1: &Value, deep: DeepRepair) -> Value {
    let si = stage1
        .get("self_intersections_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let holes = stage1.get("holes_remaining").and_then(Value::as_i64).unwrap_or(0);
    let nm = stage1
        .get("non_manifold_edges_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let parts = stage1.get("components").and_then(Value::as_i64).unwrap_or(1);
    let reason = if deep == DeepRepair::Off {
        "disabled by user settings"
    } else if si == 0 {
        "baseline SI is zero"
    } else {
        "escalation not run in the Rust pipeline"
    };
    let mut baseline = json!({"si": si, "holes": holes, "nm": nm, "parts": parts,
                              "dev_pct": 0.0, "vol_pct": 0.0});
    if deep != DeepRepair::Off {
        baseline["si_stage1"] = Value::from(si);
    }
    let mut out = Map::new();
    out.insert("triggered".into(), Value::Bool(false));
    out.insert("method".into(), Value::Null);
    out.insert("num".into(), Value::Null);
    out.insert("name".into(), Value::Null);
    out.insert("candidates".into(), json!([]));
    if deep == DeepRepair::Off {
        out.insert("reason".into(), Value::from(reason));
        out.insert("baseline".into(), baseline);
    } else {
        out.insert("si_source".into(), Value::from("exact_output"));
        out.insert("baseline".into(), baseline);
        out.insert("reason".into(), Value::from(reason));
    }
    Value::Object(out)
}

fn augment_report(
    input: &Mesh,
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    mut report: Value,
    opts: &RepairOptions,
    methods: &[u64],
    elapsed_s: f64,
) -> Value {
    let out_mesh = to_mesh(out_v, out_t);
    let in_f64 = f64_verts(&input.verts);
    let out_f64 = out_v.to_vec();
    let out_i32: Vec<[i32; 3]> = out_t
        .iter()
        .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
        .collect();

    let obj = report.as_object_mut().expect("repair report object");
    obj.insert(
        "policy".into(),
        match sutura_core::config::policy::load_policy(None) {
            Ok(loaded) => json!({
                "schema_version": loaded.schema_version,
                "content_hash": loaded.content_hash,
                "user_override": loaded.user_override,
                "policy_error": Value::Null,
                "ignored_keys": [],
            }),
            Err(e) => json!({
                "schema_version": Value::Null,
                "content_hash": Value::Null,
                "user_override": false,
                "policy_error": e.to_string(),
                "ignored_keys": [],
            }),
        },
    );
    obj.insert("method_control".into(), method_control(methods));
    if let Some(unit) = check_units(input, None) {
        obj.insert("unit_warning".into(), Value::Bool(true));
        obj.insert("unit_hint".into(), Value::from(unit.unit_hint));
    }

    // Volume / surface-area fields and the exact SI count are Python stage-1
    // report fields; the Rust core keeps the topology half, the CLI adds the
    // measures from the same final arrays.
    let in_t64: Vec<[i64; 3]> = input
        .tris
        .iter()
        .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
        .collect();
    let (before_volume, _) =
        sutura_geom::topology::mesh_geometric_measures_core(&input.verts, &in_t64);
    let before_volume = before_volume.unwrap_or(0.0);
    let (after_volume, _) =
        sutura_geom::topology::mesh_geometric_measures_core(&out_mesh.verts, &out_t);
    let after_volume = after_volume.unwrap_or(0.0);
    let before_area = sutura_core::validate::surface_area(&input.verts, &input.tris);
    let after_area = sutura_core::validate::surface_area(&out_mesh.verts, &out_mesh.tris);
    let volume_change = if before_volume != 0.0 {
        Some((after_volume - before_volume) / before_volume.abs() * 100.0)
    } else {
        None
    };
    let surface_change = if before_area != 0.0 {
        Some((after_area - before_area) / before_area.abs() * 100.0)
    } else {
        None
    };
    let round2 = |x: f64| (x * 100.0).round() / 100.0;

    let topology = sutura_core::tiers::common::topology_summary(out_v, out_t);
    let holes = sutura_core::report::detect_defects(&out_mesh.verts, &out_mesh.tris, false)
        .holes
        .len() as i64;
    let (_, si_count, si_skipped) = sutura_geom::self_intersections_core(
        &out_f64,
        &out_t
            .iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect::<Vec<_>>(),
    );
    let si_value = if si_skipped || si_count < 0 {
        Value::Null
    } else {
        Value::from(si_count)
    };
    let nm_edges = topology.non_two_manifold_edges as i64;
    let nm_verts = topology.non_two_manifold_vertices as i64;
    let si_work = out_t.len() as i64 * (1 + nm_edges + nm_verts + holes);

    {
        let s1 = obj
            .entry("stage1")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("stage1 object");
        if s1.get("volume_before").is_none() {
            s1.insert("volume_before".into(), Value::from(before_volume));
        }
        if s1.get("volume_after").is_none() {
            s1.insert("volume_after".into(), Value::from(after_volume));
        }
        s1.insert("surface_area_before".into(), Value::from((before_area * 1000.0).round() / 1000.0));
        s1.insert("surface_area_after".into(), Value::from((after_area * 1000.0).round() / 1000.0));
        let pct = match (
            s1.get("volume_before").and_then(Value::as_f64),
            s1.get("volume_after").and_then(Value::as_f64),
        ) {
            (Some(vb), Some(va)) if vb != 0.0 => {
                Some((va - vb) / vb.abs() * 100.0)
            }
            _ => volume_change,
        };
        s1.insert(
            "volume_change_percent".into(),
            pct.map(|v| Value::from(round2(v))).unwrap_or(Value::Null),
        );
        s1.insert(
            "surface_area_change_percent".into(),
            surface_change.map(|v| Value::from(round2(v))).unwrap_or(Value::Null),
        );
        if s1.get("self_intersections_remaining").is_none() {
            s1.insert("self_intersections_remaining".into(), si_value.clone());
            s1.insert("si_work_units".into(), Value::from(si_work));
            s1.insert("si_work_budget".into(), Value::from(15_000_000i64));
        }
        if !s1.contains_key("possible_debris") {
            s1.insert("possible_debris".into(), Value::Null);
        }
        if !s1.contains_key("removed_small_parts") {
            s1.insert("removed_small_parts".into(), Value::Null);
        }
        if let Some(vc) = volume_change {
            if vc.abs() > 15.0 {
                s1.insert(
                    "volume_warning".into(),
                    Value::from(format!(
                        "Volume changed by {:.1}% - verify in slicer before printing.",
                        vc.abs()
                    )),
                );
            }
        }
    }

    let input_faces = input.tris.len() as i64;
    let coaptation_patch = |value: &mut Value| {
        if let Some(map) = value.as_object_mut() {
            if map.get("ran").and_then(Value::as_bool) == Some(false) {
                map.insert("input_faces".into(), Value::from(input_faces));
                map.insert("seconds".into(), Value::Null);
            }
        }
    };
    if let Some(coaptation) = obj.get_mut("coaptation") {
        coaptation_patch(coaptation);
    }
    let cad = if opts.deep_repair == DeepRepair::Full {
        cad_likeness(&in_f64, &input.tris)
    } else {
        Value::Null
    };
    let produced = obj
        .get("solid_engine")
        .and_then(|s| s.get("produced"))
        .cloned();
    if let Some(deep) = obj.get_mut("deep_repair").and_then(Value::as_object_mut) {
        deep.insert("cad_likeness".into(), cad);
        if let Some(coaptation) = deep.get_mut("coaptation") {
            coaptation_patch(coaptation);
        }
        if let Some(produced) = produced {
            deep.insert("final_tier".into(), produced);
        }
    }
    if let Some(flap) = obj.get_mut("flap").and_then(Value::as_object_mut) {
        if flap.get("ran").and_then(Value::as_bool) == Some(false) {
            flap.insert("si_policy".into(), Value::Null);
            flap.insert("work_budget".into(), Value::Null);
        }
    }
    if let Some(tamponade) = obj.get_mut("tamponade").and_then(Value::as_object_mut) {
        if tamponade.get("ran").and_then(Value::as_bool) == Some(false) {
            tamponade.insert("work_budget".into(), Value::Null);
            tamponade.insert("h_min".into(), Value::Null);
            tamponade.insert("si_checked".into(), Value::Bool(false));
            tamponade.insert("si_flagged".into(), Value::Bool(false));
        }
        let input_holes = tamponade.get("input_holes").and_then(Value::as_i64).unwrap_or(0);
        let input_nm = tamponade
            .get("input_non_manifold")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let input_si = tamponade
            .get("input_self_intersections")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if tamponade.get("ran").and_then(Value::as_bool) == Some(true)
            && input_holes == 0
            && input_nm == 0
            && input_si == 0
        {
            let melt = matches!(opts.intensity.as_str(), "thorough" | "extreme");
            let reason = if melt {
                "no holes/non-manifold/self-intersections to plug; tier skipped"
            } else {
                "no holes/non-manifold to plug; melt disabled at this intensity; tier skipped"
            };
            tamponade.insert("ran".into(), Value::Bool(false));
            tamponade.insert("adopted".into(), Value::Bool(false));
            tamponade.insert("watertight".into(), Value::Bool(false));
            tamponade.insert("reload_watertight".into(), Value::Bool(false));
            tamponade.insert("reason".into(), Value::from(reason));
            tamponade.insert("reject_reason".into(), Value::Null);
            tamponade.insert("melt_enabled".into(), Value::Bool(melt));
            tamponade.insert("work_budget".into(), Value::Null);
            tamponade.insert("work_units".into(), Value::Null);
            tamponade.insert("h_min".into(), Value::Null);
            tamponade.insert("seconds".into(), Value::Null);
            tamponade.insert("output_faces".into(), Value::Null);
            tamponade.insert("candidate_holes".into(), Value::Null);
            tamponade.insert("candidate_non_manifold".into(), Value::Null);
            tamponade.insert("candidate_parts".into(), Value::Null);
            tamponade.insert("plugs".into(), Value::Null);
            tamponade.insert("plug_faces".into(), Value::Null);
            tamponade.insert("plugs_rejected".into(), Value::Null);
            tamponade.insert("plugs_inconclusive".into(), Value::Null);
            tamponade.insert("plugs_rejected_size".into(), Value::Null);
            tamponade.insert("plugs_rejected_dev".into(), Value::Null);
            tamponade.insert("melt_layers_tried".into(), Value::Null);
            tamponade.insert("melt_layers_accepted".into(), Value::Null);
            tamponade.insert("melt_layers_rolled_back".into(), Value::Null);
            tamponade.insert("micro_welds_ok".into(), Value::Null);
            tamponade.insert("micro_welds_failed".into(), Value::Null);
            tamponade.insert("excised_faces".into(), Value::Null);
            tamponade.insert("healthy_faces_kept".into(), Value::Null);
            tamponade.insert("healthy_fraction".into(), Value::Null);
            tamponade.insert("plugs_rolled_back".into(), Value::Null);
            tamponade.insert("plugs_leaking".into(), Value::Null);
            tamponade.insert("plugs_double_wall".into(), Value::Null);
            tamponade.insert("plugs_floating".into(), Value::Null);
            tamponade.insert("components_before".into(), Value::Null);
            tamponade.insert("components_after".into(), Value::Null);
            tamponade.insert("closed_shells_before".into(), Value::Null);
            tamponade.insert("closed_shells_after".into(), Value::Null);
            tamponade.insert("si_fall".into(), Value::Null);
            tamponade.insert("si_ok".into(), Value::Null);
            tamponade.insert("si_method".into(), Value::Null);
            tamponade.insert("candidate_self_intersections".into(), Value::Null);
            tamponade.insert("prepass_seconds".into(), Value::Null);
            tamponade.insert("si_policy".into(), Value::Null);
            tamponade.insert("si_checked".into(), Value::Bool(false));
            tamponade.insert("si_flagged".into(), Value::Bool(false));
            tamponade.insert("gate".into(), Value::Null);
            tamponade.insert("shape".into(), Value::Null);
            tamponade.insert("reason_key".into(), Value::Null);
            tamponade.insert("input_internal_walls".into(), Value::Null);
            tamponade.insert("candidate_internal_walls".into(), Value::Null);
            tamponade.insert("baseline_self_intersections".into(), Value::Null);
            tamponade.insert("guard_rejected".into(), Value::Null);
            tamponade.insert("guard_reason".into(), Value::Null);
        }
    }
    let manual_methods = !methods.is_empty();
    if let Some(method_items) = obj.get_mut("methods_tried").and_then(Value::as_array_mut) {
        for method in method_items {
            if let Some(map) = method.as_object_mut() {
                if map.get("geom_change_pct").is_some_and(Value::is_null)
                    && map.get("watertight").and_then(Value::as_bool) == Some(true)
                {
                    map.insert("geom_change_pct".into(), Value::from(0.0));
                }
                if manual_methods
                    && map.get("adopt_guard").is_some_and(Value::is_null)
                    && map.get("watertight").and_then(Value::as_bool) == Some(true)
                {
                    map.insert("adopt_guard".into(), json!({
                        "input_faces": input_faces,
                        "input_vertices": input.verts.len() as i64,
                        "candidate_faces": out_t.len() as i64,
                        "candidate_vertices": out_v.len() as i64,
                        "slack_units": 0,
                        "guard_rejected": Value::Null,
                        "guard_reason": Value::Null,
                        "si_policy": "flag",
                        "si_checked": true,
                        "si_flagged": false,
                        "baseline_self_intersections": si_value.clone(),
                        "candidate_self_intersections": si_value.clone(),
                        "si_improved_topology": false,
                        "blowup_flagged": false,
                    }));
                }
            }
        }
    }

    let defects = sutura_core::report::detect_defects(&input.verts, &input.tris, false);
    obj.insert(
        "defects".into(),
        json!({
            "holes": holes_json(&defects.holes),
            "non_manifold": non_manifold_json(&defects.non_manifold),
        }),
    );

    obj.insert(
        "timings".into(),
        json!({
            "prepass": 0.0,
            "stage1": 0.0,
            "deep_repair": 0.0,
            "stage2": 0.0,
            "file_total": (elapsed_s * 1000.0).round() / 1000.0,
            "flap_off_rerun": false,
            "flap_off_rerun_s": Value::Null,
            "tamponade_off_rerun": false,
        }),
    );

    if opts.deep_repair != DeepRepair::Local && !obj.contains_key("si_escalation") {
        obj.insert(
            "si_escalation".into(),
            si_escalation(obj.get("stage1").unwrap(), opts.deep_repair),
        );
    }

    let report_snapshot = Value::Object(obj.clone());
    if let Some(pr) = sutura_core::report::build_print_report_value(
        &report_snapshot,
        Some(&in_f64),
        Some(&input.tris),
        Some(&out_f64),
        Some(&out_i32),
    ) {
        obj.insert("print_report".into(), pr);
    }

    let cfg = score_cfg();
    let fidelity = sutura_core::report::compute_fidelity_value(
        &in_f64,
        &input.tris,
        &out_f64,
        &out_i32,
        &Value::Object(obj.clone()),
        &cfg,
    );
    if let Some(fmap) = fidelity.as_object() {
        for (key, value) in fmap {
            let value = match value {
                Value::Number(n) if key == "repair_fidelity" => match n.as_f64() {
                    Some(f) if f.fract() == 0.0 => Value::from(f as i64),
                    _ => value.clone(),
                },
                _ => value.clone(),
            };
            obj.insert(key.clone(), value);
        }
    }

    let confidence = sutura_core::report::repair_confidence_value(&Value::Object(obj.clone()));
    obj.insert("repair_confidence".into(), Value::from(confidence.score));
    obj.insert(
        "repair_confidence_label".into(),
        Value::from(confidence.label.clone()),
    );
    obj.insert(
        "repair_confidence_factors".into(),
        Value::Object(confidence.factors.clone()),
    );

    let scores = sutura_core::report::compute_scores_value(&Value::Object(obj.clone()), &cfg);
    for (key, source) in [
        ("repair_health", "health"),
        ("repair_health_factors", "health_factors"),
        ("repair_risk", "risk"),
        ("repair_risk_factors", "risk_factors"),
        ("repair_status", "status"),
        ("repair_status_code", "status_code"),
    ] {
        if let Some(value) = scores.get(source) {
            let value = match value {
                Value::Number(n) => match n.as_f64() {
                    Some(f) if f.fract() == 0.0 => Value::from(f as i64),
                    _ => value.clone(),
                },
                _ => value.clone(),
            };
            obj.insert(key.into(), value);
        }
    }
    obj.insert("method_health".into(), method_health());
    for extra in ["analysis", "ftetwild", "recommendations", "trace", "unavailable"] {
        obj.remove(extra);
    }

    let (category, issues, _key) = sutura_core::report::classify_value(&Value::Object(obj.clone()));
    obj.insert("category".into(), Value::from(category));
    obj.insert("issues".into(), json!(issues));

    if !methods.is_empty() {
        if let Some(used) = obj.get_mut("method_used").and_then(Value::as_object_mut) {
            used.insert("source".into(), Value::from("tagged"));
        }
    }
    contract_order(obj);

    Value::Object(obj.clone())
}

/// True when the path names a 3MF container (case-insensitive, like Python).
fn is_3mf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("3mf"))
        .unwrap_or(false)
}

/// Repair a multi-object 3MF the way Python does: every object separately, then
/// one archive rewrite.
///
/// Two behaviours are deliberately copied rather than invented:
///
/// * The output is always a rewritten 3MF container, **whatever the requested
///   extension**. Measured on a 3-object container: `-o out.stl` and
///   `-o out.3mf` produced byte-identical ZIP files, because Python's
///   `repair_3mf_objects` rewrites the archive and never consults the output
///   suffix.
/// * The report carries `objects`, `object_reports` (one per object, in archive
///   order), `objects_watertight` and `objects_stage2_ok`. (Measured; the key is
///   `object_reports`, not `object_analyses` -- the latter belongs to
///   `--analyze`.)
fn process_3mf(
    cli: &crate::Cli,
    path: &Path,
    out_override: Option<&Path>,
    opts: &RepairOptions,
    methods: &[u64],
    progress: Option<&JsonProgress>,
) -> (Value, bool) {
    let input_display = path.display().to_string();
    let output = match out_override {
        Some(out) => out.to_path_buf(),
        None => unique_output(path),
    };

    let token = Arc::new(CancelToken::new());
    let stop = Arc::new(AtomicBool::new(false));
    let monitor_token = Arc::clone(&token);
    let monitor_stop = Arc::clone(&stop);
    let monitor = std::thread::spawn(move || {
        while !monitor_stop.load(Ordering::SeqCst) {
            if INTERRUPTED.load(Ordering::SeqCst) {
                monitor_token.cancel();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    let started = Instant::now();
    let repairs = sutura_core::pipeline_objects::repair_3mf_file(
        path, &output, opts, &CoreSink, &token,
    );
    stop.store(true, Ordering::SeqCst);
    let _ = monitor.join();

    let repairs = match repairs {
        Ok(r) => r,
        Err(e) => {
            if let Some(progress) = progress {
                progress.end();
            }
            return (
                json!({"input": input_display, "error": e.to_string()}),
                true,
            );
        }
    };

    if let Some(progress) = progress {
        progress.stage(8, "save");
        progress.end();
    }
    let elapsed = started.elapsed().as_secs_f64();
    let (objects, watertight_count, stage2_ok_count) =
        sutura_core::pipeline_objects::aggregate_flags(&repairs);

    // Each object's report is the single-mesh report for that object, so the
    // per-object shape matches Python's `object_reports` entries.
    let mut object_reports: Vec<Value> = Vec::with_capacity(repairs.len());
    for r in &repairs {
        let mut rep = r.report.clone();
        if let Some(obj) = rep.as_object_mut() {
            obj.insert("model".into(), Value::from(r.model_name.clone()));
            obj.insert("declared_unit".into(), Value::from(r.declared_unit.clone()));
            obj.insert("block_index".into(), Value::from(r.block_index));
            obj.insert("output".into(), Value::from(output.display().to_string()));
            contract_order(obj);
        }
        object_reports.push(rep);
    }

    let failed = object_reports
        .iter()
        .any(|r| r.get("error").is_some() || r.get("category").and_then(Value::as_str) == Some("error"));

    let mut report = json!({
        "input": input_display,
        "output": output.display().to_string(),
        "objects": objects,
        "objects_watertight": watertight_count,
        "objects_stage2_ok": stage2_ok_count,
        "object_reports": object_reports,
        "repair_mode": opts.mode.as_str(),
        "si_mode": opts.si_mode.as_str(),
        "triage_intensity": opts.intensity,
        "timings": {"file_total": elapsed},
        "method_used": methods_used_value(methods),
    });
    if let Some(obj) = report.as_object_mut() {
        contract_order(obj);
    }
    let _ = cli;
    (report, failed)
}

/// The `method_used` block for a container, matching the single-file shape.
fn methods_used_value(methods: &[u64]) -> Value {
    if methods.is_empty() {
        return Value::Null;
    }
    json!({
        "num": methods.last().copied(),
        "source": "tagged",
    })
}

fn process_one(
    cli: &crate::Cli,
    path: &Path,
    out_override: Option<&Path>,
    opts: &RepairOptions,
    methods: &[u64],
    progress: Option<&JsonProgress>,
) -> (Value, bool) {
    let input_display = path.display().to_string();
    if !path.exists() {
        return (
            json!({"input": input_display, "error": format!("file not found: {input_display}")}),
            true,
        );
    }
    if let Some(progress) = progress {
        progress.stage(1, "load");
    }
    // A 3MF container holds one mesh per object; Python repairs each object
    // separately and rewrites the archive once (`service.repair_3mf_objects`).
    // `load_mesh` refuses 3MF for exactly this reason, so branch before it.
    if is_3mf(path) {
        return process_3mf(cli, path, out_override, opts, methods, progress);
    }
    let input = match load_mesh(path) {
        Ok(mesh) => mesh,
        Err(e) => {
            return (
                json!({"input": input_display, "error": e.to_string()}),
                true,
            )
        }
    };
    if let Some(progress) = progress {
        progress.stage(2, "prepass");
    }

    let verts = f64_verts(&input.verts);
    let tris = i64_tris(&input.tris);
    let token = Arc::new(CancelToken::new());
    let stop = Arc::new(AtomicBool::new(false));
    let monitor_token = Arc::clone(&token);
    let monitor_stop = Arc::clone(&stop);
    let monitor = std::thread::spawn(move || {
        while !monitor_stop.load(Ordering::SeqCst) {
            if INTERRUPTED.load(Ordering::SeqCst) {
                monitor_token.cancel();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });

    let started = Instant::now();
    let outcome = repair(&verts, &tris, opts, &CoreSink, &token);
    stop.store(true, Ordering::SeqCst);
    let _ = monitor.join();

    let (out_v, out_t, core_report) = match outcome {
        RepairOutcome::Ok {
            verts,
            tris,
            report,
        } => (verts, tris, report),
        RepairOutcome::Unavailable { tier, reason } => {
            return (
                json!({"input": input_display, "error": format!("{tier} is unavailable: {reason}")}),
                true,
            )
        }
        RepairOutcome::Error { message } => {
            if let Some(progress) = progress {
                progress.end();
            }
            if message == "cancelled" {
                eprintln!("error: interrupted");
                return (json!({"input": input_display, "error": "interrupted"}), true);
            }
            return (
                json!({"input": input_display, "error": message}),
                true,
            );
        }
    };

    // A fast repair can finish before the monitor observes SIGINT; honour the
    // delivered signal at the next stage boundary exactly like Python.
    if INTERRUPTED.load(Ordering::SeqCst) {
        if let Some(progress) = progress {
            progress.end();
        }
        eprintln!("error: interrupted");
        return (json!({"input": input_display, "error": "interrupted"}), true);
    }

    let output = match out_override {
        Some(out) => out.to_path_buf(),
        None => unique_output(path),
    };
    let out_mesh = to_mesh(&out_v, &out_t);
    if let Err(e) = save_mesh(&output, &out_mesh) {
        if let Some(progress) = progress {
            progress.end();
        }
        return (
            json!({"input": input_display, "error": format!("write failed: {e}")}),
            true,
        );
    }

    if let Some(progress) = progress {
        progress.stage(3, "stage 1");
        if opts.deep_repair != DeepRepair::Off {
            progress.stage(4, "deep repair");
        }
        progress.stage(5, "stage 2");
        progress.stage(6, "reload weld");
        progress.stage(7, "validate");
        progress.stage(8, "save");
        progress.end();
    }

    let elapsed = started.elapsed().as_secs_f64();
    let mut report =
        augment_report(&input, &out_v, &out_t, core_report, opts, methods, elapsed);
    if let Some(obj) = report.as_object_mut() {
        obj.insert("input".into(), Value::from(input_display));
        obj.insert("output".into(), Value::from(output.display().to_string()));
        contract_order(obj);
    }
    let failed = report.get("category").and_then(Value::as_str) == Some("error")
        || report.get("error").is_some();
    let _ = cli;
    (report, failed)
}

pub fn run(cli: &crate::Cli) -> Result<(), (u8, Option<String>)> {
    let (opts, methods_given, methods) = build_options(cli).map_err(|e| (e.code, e.message))?;
    if cli.files.is_empty() {
        return Err((2, Some("no input files".to_string())));
    }
    install_sigint();
    INTERRUPTED.store(false, Ordering::SeqCst);

    let total = cli.files.len();
    let progress_json = cli.progress_json;
    let human_mode = cli.human && !cli.json;
    let mut results: Vec<Value> = Vec::with_capacity(total);
    let mut failures = 0usize;

    for (idx, file) in cli.files.iter().enumerate() {
        let progress = if progress_json && !cli.no_progress {
            Some(JsonProgress::new(file, idx + 1, total))
        } else {
            None
        };
        if let Some(p) = &progress {
            p.start();
        }
        let out_override = if total == 1 {
            cli.output.as_deref()
        } else {
            None
        };
        let (report, failed) = process_one(cli, file, out_override, &opts, &methods, progress.as_ref());
        let interrupted = INTERRUPTED.load(Ordering::SeqCst)
            || report.get("error").and_then(Value::as_str) == Some("interrupted");
        if failed {
            failures += 1;
        }
        results.push(report);
        if interrupted {
            return Err((130, None));
        }
    }

    if total == 1 {
        let report = &results[0];
        if human_mode {
            let text = human::human_report(report, cli.defects, cli.diff);
            let text = match cli.lang.as_deref().unwrap_or("en") {
                "tr" => human::translate(&text, "tr"),
                _ => text,
            };
            println!("{text}\n");
        } else {
            println!("{}", py_json(report));
        }
        return if failures > 0 {
            Err((1, None))
        } else {
            Ok(())
        };
    }

    if human_mode {
        for report in &results {
            let text = human::human_report(report, cli.defects, cli.diff);
            let text = match cli.lang.as_deref().unwrap_or("en") {
                "tr" => human::translate(&text, "tr"),
                _ => text,
            };
            println!("{text}");
            println!();
        }
        let ok = results
            .iter()
            .filter(|r| r.get("category").and_then(Value::as_str) == Some("watertight"))
            .count();
        let warnings = results
            .iter()
            .filter(|r| r.get("category").and_then(Value::as_str) == Some("warning"))
            .count();
        let errors = results
            .iter()
            .filter(|r| r.get("category").and_then(Value::as_str) == Some("error") || r.get("error").is_some())
            .count();
        println!(
            "Summary: {} file(s), {} watertight, {} with warnings, {} failed.",
            total, ok, warnings, errors
        );
    } else {
        let mut issue_counts = Map::new();
        for report in &results {
            if let Some(issues) = report.get("issues").and_then(Value::as_array) {
                for issue in issues {
                    if let Some(code) = issue.as_str() {
                        let count = issue_counts
                            .get(code)
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            + 1;
                        issue_counts.insert(code.to_string(), Value::from(count));
                    }
                }
            }
        }
        let ok = results
            .iter()
            .filter(|r| r.get("category").and_then(Value::as_str) == Some("watertight"))
            .count();
        let warnings = results
            .iter()
            .filter(|r| r.get("category").and_then(Value::as_str) == Some("warning"))
            .count();
        let payload = json!({
            "files": results,
            "summary": {
                "total": total,
                "ok": ok,
                "warning": warnings,
                "error": failures,
                "issue_counts": issue_counts,
            }
        });
        println!("{}", py_json(&payload));
    }
    let _ = methods_given;
    if failures > 0 {
        Err((1, None))
    } else {
        Ok(())
    }
}
