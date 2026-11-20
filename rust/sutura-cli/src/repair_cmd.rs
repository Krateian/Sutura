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
    repair, CancelToken, DeepRepair, ProgressSink, RepairOptions, RepairOutcome,
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

/// Collects the core's stage/tier events for `--trace`.
///
/// The core already emits both (`repair.rs`: `emit_stage` / `emit_tier`) and the
/// CLI threw them away, so `--trace` had nothing to write even though the
/// pipeline reports every boundary. Keeping the whole event stream in memory is
/// cheap -- one small object per stage or tier, ~25 for a full repair.
///
/// Python's `trace.record_stage` is the model (`trace.py:226`): one record per
/// boundary with its measures, sha256, gate decision and elapsed time. The
/// measures cannot be taken here -- the sink never sees geometry -- so they are
/// added by the caller that does (see `trace_record`), which is also why this
/// type only collects.
struct CoreSink {
    events: std::sync::Mutex<Vec<Value>>,
    started: Instant,
}

impl CoreSink {
    fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            started: Instant::now(),
        }
    }

    fn take(&self) -> Vec<Value> {
        self.events
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }

    fn elapsed(&self) -> f64 {
        (self.started.elapsed().as_secs_f64() * 10000.0).round() / 10000.0
    }
}

impl ProgressSink for CoreSink {
    fn emit(&self, event: Value) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

/// Topological measures of a mesh, for a trace record.
///
/// Mirrors Python's `trace.measure_mesh`: face/vertex counts, holes,
/// non-manifold edges and vertices, components, and a sha256 over the normalized
/// arrays. Only called when tracing is on.
fn measure_trace(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Value {
    // `topology_summary` is the same computation Python's trace calls
    // `mesh_topology` + `boundary_loop_stats` for, and it already reports
    // `connected_components_number`'s face-union count as `components`.
    let top = sutura_core::tiers::common::topology_summary(verts, tris);
    let watertight = top.non_two_manifold_edges == 0
        && top.non_two_manifold_vertices == 0
        && top.boundary_edges == 0;
    json!({
        "faces": tris.len(),
        "verts": verts.len(),
        "holes": top.boundary_edges,
        "nm_edges": top.non_two_manifold_edges,
        "nm_verts": top.non_two_manifold_vertices,
        "components": top.components,
        // Python only computes volume on a watertight mesh, because the
        // divergence integral is expensive and meaningless on an open one.
        "volume": if watertight {
            Value::from(
                (sutura_core::tiers::common::signed_volume(verts, tris) * 1e6).round() / 1e6,
            )
        } else {
            Value::Null
        },
        "sha256": mesh_sha256(verts, tris),
    })
}

/// SHA-256 over the normalized geometry arrays, exactly as Python computes it.
///
/// Vertices as contiguous little-endian `f32` with `-0.0` folded to `+0.0`,
/// triangles as `i32`, concatenated in that order (`trace.py:51`).
fn mesh_sha256(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for v in verts {
        for c in v {
            let f = *c as f32;
            h.update(if f == 0.0 { 0.0f32 } else { f }.to_le_bytes());
        }
    }
    for t in tris {
        for i in t {
            h.update((*i as i32).to_le_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()
}

/// Write a trace file, or `None` when `--trace` was not given.
///
/// Python's `finish_trace` writes `{version, created_at, input_file,
/// total_elapsed_seconds, command_args, stages, final_summary}`
/// (`trace.py:288`); the shape is matched so `scripts/trace_diff.py` reads a
/// Rust trace without changes.
fn write_trace(
    path: &Path,
    input: &Path,
    stages: Vec<Value>,
    elapsed: f64,
    command_args: Value,
) -> Result<(), String> {
    let final_summary = stages
        .last()
        .and_then(|s| s.get("output"))
        .cloned()
        .unwrap_or(Value::Null);
    let payload = json!({
        "version": 1,
        "created_at": sutura_core::report::history::now_ts(),
        "input_file": input.display().to_string(),
        "total_elapsed_seconds": elapsed,
        "command_args": command_args,
        "stages": stages,
        "final_summary": final_summary,
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Atomic replace, so a killed run leaves the last complete snapshot rather
    // than half-written JSON -- the same rule Python states at `trace.py:309`.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&payload).unwrap_or_default())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn parse_point(raw: &str, flag: &str) -> Result<[f64; 3], RepairFailure> {
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(RepairFailure::new(
            2,
            format!("argument {flag}: expected X,Y,Z"),
        ));
    }
    let mut out = [0.0f64; 3];
    for (i, part) in parts.iter().enumerate() {
        out[i] = part.parse::<f64>().map_err(|_| {
            RepairFailure::new(2, format!("argument {flag}: coordinates must be numbers"))
        })?;
    }
    Ok(out)
}

pub fn build_options(cli: &crate::Cli) -> Result<(RepairOptions, bool, Vec<u64>), RepairFailure> {
    let engines_sel: Option<Vec<String>> = match cli.engines.as_deref() {
        None | Some("") => None,
        Some(raw) => {
            let parts: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            if parts.is_empty() {
                return Err(RepairFailure::new(2, "argument --engines: empty list"));
            }
            let (configured, _warnings) =
                sutura_core::config::external_engines::load_all_engines(None);
            let unknown: Vec<&str> = parts
                .iter()
                .filter(|n| !configured.contains_key(n.as_str()))
                .map(String::as_str)
                .collect();
            if !unknown.is_empty() {
                return Err(RepairFailure::new(
                    2,
                    format!(
                        "argument --engines: unknown engine(s): {} (see `sutura engines list`)",
                        unknown.join(", ")
                    ),
                ));
            }
            Some(parts)
        }
    };
    // Only flags with no Rust implementation are refused. Everything that IS
    // implemented is handled below and must not be rejected here: this block
    // used to reject `--repeat-source/-target`, `--ftetwild-optimize` and all
    // five dressing options, which the rest of this function implements and
    // `repair.rs` consumes. A merge resolution restored the stale rejections
    // and made implemented features unreachable from the CLI.
    //
    // `--wall-min-thickness` was refused for the same reason and is now wired:
    // the builder is ported (`sutura_geom::wall`, byte-identical to `wall.py`),
    // so the flag sets the target and turns the tier on.
    //
    // p10/core's opt-in ports (edge-tiebreak, autorefine, indirect autorefine)
    // are wired after `options_from` below, so they are not refused here.
    // History, learning-triage and anticipation flags are accepted and wired or
    // honoured elsewhere; the ones below have no consumer yet.
    // `--diagnostics` is handled in `main.rs` before any repair. `--trace` used
    // to be refused here; it is now implemented (`run()` writes the file from
    // the sink's stage/tier events), so there is nothing left to refuse. The
    // list stays as an empty slice rather than being deleted so the next
    // unimplemented flag has a place to go that is not a silent no-op.
    for (flag, set) in [] as [(&str, bool); 0] {
        if set {
            return Err(RepairFailure::new(
                2,
                format!("{flag} is not implemented in the Rust CLI"),
            ));
        }
    }
    // `--self-repair` is handled in `run()` (it reinstalls rather than
    // repairing a mesh), so it is not refused here.
    if let Some(v) = cli.max_geometry_change {
        if v < 0.0 {
            return Err(RepairFailure::new(
                2,
                "--max-geometry-change must be >= 0",
            ));
        }
    }
    if let Some(v) = cli.max_risk {
        if !(0.0..=100.0).contains(&v) {
            return Err(RepairFailure::new(
                2,
                "--max-risk must be between 0 and 100",
            ));
        }
    }

    // Everything below is resolved by `options_from` from the settings table --
    // including validation, with the same messages and the same exit code
    // (`repair_settings.rs:173,187,203,214`). The CLI therefore does NOT
    // pre-parse mode/intensity/deep/tiers here: duplicated parsing is what left
    // eleven dead locals behind after the p10/core merge, and it would drift
    // from the shared table the GUI also goes through.
    //
    // The two repeat points are the exception: they arrive as `X,Y,Z` text, and
    // `RepairSettings` stores `[f64; 3]`, so the conversion (and its
    // error message) belongs on the CLI side where the flag is spelled.
    let repeat_source = match cli.repeat_source.as_deref() {
        Some(raw) => Some(parse_point(raw, "--repeat-source")?),
        None => None,
    };
    let repeat_target = match cli.repeat_target.as_deref() {
        Some(raw) => Some(parse_point(raw, "--repeat-target")?),
        None => None,
    };

    // Go through the SAME settings table the GUI edits, so a control in either
    // front end cannot set an option the other cannot. Values that are derived
    // rather than user-set (mode/intensity parsing, the tri-state tier
    // switches, method parsing) are resolved by `options_from`.
    let settings = sutura_core::repair_settings::RepairSettings {
        mode: cli.mode.clone(),
        intensity: cli.intensity.clone(),
        profile: cli.profile.clone(),
        deep_repair: cli.deep_repair.clone(),
        si_mode: cli.si_mode.clone(),
        solid_engine: cli.solid_engine.clone(),
        classifier_engine: cli.classifier_engine.clone(),
        methods: cli.methods.clone(),
        engines: engines_sel,
        max_geometry_change: cli.max_geometry_change,
        max_risk: cli.max_risk,
        force: cli.force,
        no_graft: cli.no_graft,
        experimental_graft: cli.experimental_graft,
        no_tamponade: cli.no_tamponade,
        // Python's `resolve_tamponade(no_tamponade, force=args.tamponade)`, and
        // the same for flap and coaptation: the forcing flag was declared but
        // hard-coded to `false` here, so `--flap` / `--tamponade` /
        // `--coaptation` were accepted and ignored. `tri()` implements exactly
        // Python's precedence (off wins, then force, then the default), and
        // `SUTURA_*` env handling lives in the core, as it does for the others.
        experimental_tamponade: cli.tamponade,
        no_flap: cli.no_flap,
        experimental_flap: cli.flap,
        no_coaptation: cli.no_coaptation,
        experimental_coaptation: cli.coaptation,
        experimental_splint: cli.experimental_splint,
        no_splint: cli.no_splint,
        no_fallback_ftetwild: cli.no_fallback_ftetwild,
        experimental_fallback_ftetwild: cli.experimental_fallback_ftetwild,
        repeat_source,
        repeat_target,
        time_budget_s: None,
        ftetwild_optimize: cli.ftetwild_optimize,
        // Both are read by `options_from` now, so they belong in the literal
        // rather than being assigned onto `opts` afterwards.
        auto_reorder: cli.auto_reorder,
        extra_features: cli.experimental_edge_tiebreak,
        join_components: cli.experimental_join_components,
        // Declared on the CLI since the beginning, and  already
        // had both; the settings table was the only place they were missing, so
        // the GUI could not reach them either.
        autorefine: cli.experimental_autorefine,
        indirect_autorefine: cli.experimental_indirect_autorefine,
        ftetwild_max_faces: cli.ftetwild_max_faces,
        ftetwild_timeout: cli.ftetwild_timeout,
        ftetwild_max_hausdorff_rel: cli.ftetwild_max_hausdorff,
        // Two flags, one field: `--ftetwild-optimize` is Python's user-facing
        // switch (it sets `SUTURA_FTETWILD_OPTIMIZE`, which the tier reads), and
        // `--ftetwild-optimize-retry` is the retry-after-a-rejected-candidate
        // variant. Both mean "let fTetWild optimise", so either one turns it on.
        ftetwild_optimize_retry: if cli.ftetwild_optimize_retry || cli.ftetwild_optimize {
            Some(true)
        } else {
            None
        },
        graft_max_faces: cli.graft_max_faces,
        graft_timeout: cli.graft_timeout,
        dense_ratio: cli.dense_ratio,
        dense_min_faces: cli.dense_min_faces,
        auto_escalation: None,
        // Dressing: the producer is ported (`dressing.rs`, `coat`/`run` with its
        // own adoption gate), so these now come from the CLI instead of being
        // hardcoded off. `--experimental-dressing` turns it on and `--no-dressing`
        // turns it off -- Python's `resolve_dressing(no_dressing, force=...)`
        // gives the negative flag precedence, and `--no-dressing` was declared
        // but never read, so it did nothing.
        dressing: cli.experimental_dressing && !cli.no_dressing,
        // `--dressing-drain` accepts a mode name (`none`/`half`/`full`/`deep`)
        // or a bare number, and `dressing::resolve_drain` handles both shapes
        // (`Value::String` vs a number). Parse a numeric argument as a number so
        // the report says "1.25" rather than "1.25 is not a mode".
        dressing_drain: cli.dressing_drain.as_deref().map(|raw| {
            let t = raw.trim();
            match t.parse::<f64>() {
                Ok(n) => serde_json::json!(n),
                Err(_) => serde_json::Value::String(t.to_lowercase()),
            }
        }),
        dressing_defects: cli.dressing_defects.clone(),
        dressing_rmax_scale: cli.dressing_rmax_scale,
        dressing_sigma_scale: cli.dressing_sigma_scale,
        dressing_force_adopt: cli.dressing_force_adopt,
        // `--wall-min-thickness` implies the tier: a target thickness with the
        // tier off would silently do nothing, which is the bug class this
        // session has been removing. Python's CLI has no separate on/off flag
        // for wall-thicken -- the target IS the switch.
        wall_thicken: cli.wall_min_thickness.is_some(),
        wall_min_thickness: cli.wall_min_thickness,
    };
    // `options_from` returns `(options, methods_given, methods)` -- the same
    // triple this function returns, computed once from the shared table.
    let (mut opts, methods_given, methods) = sutura_core::repair_settings::options_from(&settings)
        .map_err(|e| RepairFailure::new(e.code, e.message))?;
    // p10/core opt-in ports that are not columns in the shared settings table
    // (they are CLI-only today). `options_from` owns everything else.
    opts.autorefine = cli.experimental_autorefine;
    opts.indirect_autorefine = cli.experimental_indirect_autorefine;
    opts.ladder_depth = 0;
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

const FLAP_ORDER: [&str; 36] = [
    "ran", "adopted", "reason", "baseline_holes", "baseline_non_manifold",
    "baseline_damage", "candidate_holes", "candidate_non_manifold",
    "candidate_damage", "loops_found", "loops_filled", "loops_skipped",
    "loops_skipped_by_reason", "patch_faces", "new_vertices", "time_s",
    "cache_hit", "work_units", "work_budget", "max_orig_vertex_move_mm",
    "input_faces", "input_vertices", "candidate_faces", "candidate_vertices",
    "loop_vertex_total", "slack_units", "baseline_self_intersections",
    "candidate_self_intersections", "si_flagged", "si_checked", "si_policy",
    "guard_rejected", "guard_reason",
    // The safety-net fields, appended last in Python's order
    // (`service.py:466-478`): `budget_exceeded` first, then the two the net
    // writes. Leaving them out of this list is why `retain` dropped them.
    "budget_exceeded", "adopted_final", "final_fallback",
];

const TAMPONADE_ORDER: [&str; 72] = [
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
    "stent_rejected", "plug_metrics", "base_local_si", "cand_local_si",
    "shell_prepass",
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
    // fTetWild is available exactly when `find_binary()` finds a runnable
    // sidecar (FTETWILD_BIN / SUTURA_FTETWILD_BIN / bundled / PATH).  The
    // hardcoded table said "skip" even when the tier ran, which reported a
    // missing fTetWild to the user (and the GUI health warning).
    let ftetwild_ok = sutura_core::ftetwild::find_binary().is_some();
    for (id, status) in METHOD_HEALTH_IDS {
        let status = if id == "ftetwild" && ftetwild_ok {
            "ok"
        } else {
            status
        };
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

fn geom_change_pct(stage1: &Value) -> Option<f64> {
    let mut values = Vec::new();
    for key in ["volume_change_percent", "surface_area_change_percent"] {
        if let Some(v) = stage1.get(key).and_then(Value::as_f64) {
            values.push(v.abs());
        }
    }
    values.into_iter().reduce(f64::max)
}

fn budget_check_value(
    report: &Value,
    max_geom: Option<f64>,
    max_risk: Option<f64>,
) -> Option<Value> {
    if max_geom.is_none() && max_risk.is_none() {
        return None;
    }
    let mut geoms: Vec<f64> = Vec::new();
    let mut risks: Vec<f64> = Vec::new();
    if let Some(objs) = report.get("object_reports").and_then(Value::as_array) {
        for rep in objs {
            if let Some(g) = geom_change_pct(rep.get("stage1").unwrap_or(&Value::Null)) {
                geoms.push(g);
            }
            if let Some(r) = rep.get("repair_risk").and_then(Value::as_f64) {
                risks.push(r);
            }
        }
    } else {
        if let Some(g) = geom_change_pct(report.get("stage1").unwrap_or(&Value::Null)) {
            geoms.push(g);
        }
        if let Some(r) = report.get("repair_risk").and_then(Value::as_f64) {
            risks.push(r);
        }
    }
    let geom = geoms.into_iter().reduce(f64::max);
    let risk = risks.into_iter().reduce(f64::max);
    let mut exceeded = Vec::new();
    if let (Some(budget), Some(value)) = (max_geom, geom) {
        if value > budget {
            exceeded.push(json!({
                "metric": "geometry_change",
                "value": (value * 100.0).round() / 100.0,
                "budget": budget,
            }));
        }
    }
    if let (Some(budget), Some(value)) = (max_risk, risk) {
        if value > budget {
            exceeded.push(json!({
                "metric": "risk",
                "value": value.round() as i64,
                "budget": budget,
            }));
        }
    }
    Some(json!({
        "geometry_change_pct": geom.map(|g| (g * 100.0).round() / 100.0),
        "risk_score": risk.map(|r| r.round() as i64),
        "max_geometry_change_pct": max_geom,
        "max_risk_score": max_risk,
        "within_budget": exceeded.is_empty(),
        "exceeded": exceeded,
    }))
}

fn budget_message(budget: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(items) = budget.get("exceeded").and_then(Value::as_array) {
        for item in items {
            match item.get("metric").and_then(Value::as_str) {
                Some("geometry_change") => {
                    let value = item.get("value").and_then(Value::as_f64).unwrap_or(0.0);
                    let cap = item.get("budget").and_then(Value::as_f64).unwrap_or(0.0);
                    parts.push(format!("geometry change {value:.2}% (budget {cap:.2}%)"));
                }
                Some("risk") => {
                    let value = item.get("value").and_then(Value::as_i64).unwrap_or(0);
                    let cap = item.get("budget").and_then(Value::as_f64).unwrap_or(0.0);
                    parts.push(format!("risk {value} (budget {cap})"));
                }
                _ => {}
            }
        }
    }
    parts.join("; ")
}

fn budget_temp_path(output: &Path) -> PathBuf {
    let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let ext = output.extension().and_then(|s| s.to_str()).unwrap_or("stl");
    output.with_file_name(format!(
        "{stem}.sutura-tmp-{}.{ext}",
        std::process::id()
    ))
}

fn si_escalation(
    stage1: &Value,
    exact_si: &Value,
    deep: DeepRepair,
    dev_pct: Value,
    vol_pct: Value,
) -> Value {
    let stage1_si = stage1
        .get("self_intersections_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let exact = exact_si.as_i64();
    let holes = stage1.get("holes_remaining").and_then(Value::as_i64).unwrap_or(0);
    let nm = stage1
        .get("non_manifold_edges_remaining")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let parts = stage1.get("components").and_then(Value::as_i64).unwrap_or(1);
    let reason = if deep == DeepRepair::Off {
        "disabled by user settings"
    } else if exact_si.is_null() {
        "baseline SI unmeasurable"
    } else if exact == Some(0) {
        "baseline SI is zero"
    } else {
        "escalation not run in the Rust pipeline"
    };
    let baseline_si = if deep == DeepRepair::Off {
        Value::from(stage1_si)
    } else {
        exact_si.clone()
    };
    let mut baseline = json!({"si": baseline_si, "holes": holes, "nm": nm,
                              "parts": parts, "dev_pct": dev_pct,
                              "vol_pct": vol_pct});
    if deep != DeepRepair::Off {
        baseline["si_stage1"] = Value::from(stage1_si);
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
    no_fidelity: bool,
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
        if s1.get("surface_area_before").is_none() {
            s1.insert("surface_area_before".into(), Value::from((before_area * 1000.0).round() / 1000.0));
        }
        if s1.get("surface_area_after").is_none() {
            s1.insert("surface_area_after".into(), Value::from((after_area * 1000.0).round() / 1000.0));
        }
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
        if s1.get("surface_area_change_percent").is_none() {
            s1.insert(
                "surface_area_change_percent".into(),
                surface_change.map(|v| Value::from(round2(v))).unwrap_or(Value::Null),
            );
        }
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
    let produced = obj
        .get("solid_engine")
        .and_then(|s| s.get("produced"))
        .cloned();
    if let Some(deep) = obj.get_mut("deep_repair").and_then(Value::as_object_mut) {
        if deep.get("cad_likeness").is_none() {
            deep.insert(
                "cad_likeness".into(),
                if opts.deep_repair == DeepRepair::Full {
                    cad_likeness(&in_f64, &input.tris)
                } else {
                    Value::Null
                },
            );
        }
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

    // Keep the core's measured stage timings; the CLI only adds the
    // fallback-run flags and a total when the core did not provide one.
    let mut timings = obj
        .get("timings")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // `flap_off_rerun` reports what the safety net actually did, exactly as
    // Python derives it from `flap.final_fallback.ran` (`service.py:522-525`).
    // It used to be hard-coded `false`, which said "no fallback was needed" on
    // every file — including the ones where Python ran one.
    let fallback = obj
        .get("flap")
        .and_then(|f| f.get("final_fallback"))
        .cloned()
        .unwrap_or(Value::Null);
    timings.insert(
        "flap_off_rerun".into(),
        Value::Bool(
            fallback
                .get("ran")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    );
    timings.insert(
        "flap_off_rerun_s".into(),
        fallback
            .get("seconds")
            .cloned()
            .unwrap_or(Value::Null),
    );
    timings.insert("tamponade_off_rerun".into(), Value::Bool(false));
    if !timings.contains_key("file_total") {
        timings.insert(
            "file_total".into(),
            Value::from((elapsed_s * 1000.0).round() / 1000.0),
        );
    }
    obj.insert("timings".into(), Value::Object(timings));

    let report_snapshot = Value::Object(obj.clone());
    let cfg = sutura_core::report::score_cfg();
    // Fidelity only reads the pipeline fields; feeding it the pre-print
    // snapshot avoids serialising/traversing the large print_report.
    //
    // `--no-fidelity` skips it entirely, exactly as Python does
    // (`repair.py:389`: the block is not entered, so the keys are absent rather
    // than null). It is report-only and never changes the mesh, so skipping it
    // cannot alter the saved output.
    let fidelity = if no_fidelity {
        Value::Null
    } else {
        sutura_core::report::compute_fidelity_value(
            &in_f64,
            &input.tris,
            &out_f64,
            &out_i32,
            &report_snapshot,
            &cfg,
        )
    };
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
    if let Some(pr) = sutura_core::report::build_print_report_value(
        &report_snapshot,
        Some(&in_f64),
        Some(&input.tris),
        Some(&out_f64),
        Some(&out_i32),
    ) {
        obj.insert("print_report".into(), pr);
    }
    if opts.deep_repair != DeepRepair::Local && !obj.contains_key("si_escalation") {
        let dev = obj
            .get("print_report")
            .and_then(|p| p.get("geometry"))
            .and_then(|g| g.get("deviation_max_pct"))
            .cloned()
            .unwrap_or(Value::Null);
        let vol = obj
            .get("stage1")
            .and_then(|s| s.get("volume_change_percent"))
            .cloned()
            .unwrap_or(Value::Null);
        obj.insert(
            "si_escalation".into(),
            si_escalation(
                obj.get("stage1").unwrap(),
                &si_value,
                opts.deep_repair,
                dev,
                vol,
            ),
        );
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
    let mut health = method_health();
    if let Some(core) = obj.get("method_health").and_then(Value::as_object) {
        // Keep both lists: the core slice adds Stage-1 errors, the engine slice
        // adds availability skips. Copying only `errors` silently drops the
        // engine entries, which is exactly what a byte diff against Python
        // caught here.
        if let Some(h) = health.as_object_mut() {
            for key in ["errors", "skips"] {
                if let Some(value) = core.get(key) {
                    h.insert(key.into(), value.clone());
                }
            }
        }
    }
    obj.insert("method_health".into(), health);
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

/// True when the path names an OBJ file (case-insensitive, like Python).
fn is_obj(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("obj"))
        .unwrap_or(false)
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
///
/// This was lost in the p10/core merge (`9debf8b`): the CLI stopped calling
/// `pipeline_objects::repair_3mf_file`, so a multi-object container was loaded
/// as a single mesh and only object 0 was repaired. The io-gate caught it.
fn process_3mf(
    cli: &crate::Cli,
    path: &Path,
    out_override: Option<&Path>,
    opts: &RepairOptions,
    methods: &[u64],
    progress: Option<&JsonProgress>,
    sink: &CoreSink,
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
        path, &output, opts, sink, &token,
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
            // Python does not put `model` at the top level of an object report;
            // it lives inside `triangle_attributes`, which is also where the
            // io-gate reads the per-object archive order from. Emitting it at
            // the top level made the order unreadable and the shape wrong.
            obj.insert("declared_unit".into(), Value::from(r.declared_unit.clone()));
            obj.insert("block_index".into(), Value::from(r.block_index));
            obj.insert("output".into(), Value::from(output.display().to_string()));
            if let Some(stats) = &r.triangle_attrs {
                obj.insert("triangle_attributes".into(), stats.to_json());
            }
            contract_order(obj);
        }
        // One history record per object, exactly like `history.write` for a
        // multi-object 3MF: every object report gets its own line, fingerprinted
        // from that object's own mesh, and the container's format ('3mf') is
        // passed in because the per-object report carries no input path.
        if !cli.no_history {
            sutura_core::report::history::write_record(
                &rep,
                &r.mesh.verts,
                &r.mesh.tris,
                env!("SUTURA_PRODUCT_VERSION"),
                (elapsed * 1000.0).round() as i64,
                Some("3mf"),
                &sutura_core::report::history::now_ts(),
            );
        }
        object_reports.push(rep);
    }

    let failed = object_reports.iter().any(|r| {
        r.get("error").is_some() || r.get("category").and_then(Value::as_str) == Some("error")
    });

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
    (report, failed)
}

/// The Flap **final safety net** (Python's HIGH-1, `service.py:422-478`).
///
/// The Flap adoption gate is topological-only and decided on the pre-chain mesh,
/// so it cannot promise the FINAL result beats Flap-OFF. When Flap was adopted
/// and the final mesh is not strict-watertight, has more parts than the input,
/// or the Flap candidate raised the exact self-intersection count, Python runs
/// the whole file ONCE more with Flap off and keeps the better result.
///
/// Without it the Rust CLI reported a different mesh from Python on
/// On a private sample: Python's net chose the flap-OFF run (stage 1 9478 faces), the
/// Rust CLI had no net at all (12914).
///
/// This is orchestration only. The decision rules are the core's own
/// (`tiers::flap::{si_rose, final_off, off_preferred}`), which Python calls
/// through PyO3 too, so the two cannot drift. `repair.rs` is not touched: the
/// core is simply called twice.
fn run_with_flap_safety_net(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    opts: &RepairOptions,
    token: &CancelToken,
    sink: &CoreSink,
) -> RepairOutcome {
    let on = match repair(verts, tris, opts, sink, token) {
        RepairOutcome::Ok {
            verts: out_v,
            tris: out_t,
            report,
        } => (out_v, out_t, report),
        // An error or an unavailable tier is not a candidate to compare.
        other => return other,
    };

    sutura_core::repair::stage_fp_log("after_flap_primary", &on.0, &on.1);
    // Only a Flap-adopted run can be worse *because of* Flap.
    let flap_adopted = on
        .2
        .get("flap")
        .and_then(|f| f.get("adopted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !flap_adopted || opts.flap != Some(true) {
        sutura_core::repair::stage_fp_log("after_flap_net_choice", &on.0, &on.1);
        return RepairOutcome::Ok {
            verts: on.0,
            tris: on.1,
            report: on.2,
        };
    }

    // A candidate SI rise is only a signal to compare the two finals; it is not
    // on its own a rejection, because Stage 1/2 can bring the count back down.
    let si_rise = on
        .2
        .get("flap")
        .map(sutura_core::tiers::flap::si_rose)
        .unwrap_or(false);
    let (on_reason, on_metrics) = sutura_core::tiers::flap::final_off(
        verts, tris, &on.0, &on.1, si_rise, si_rise,
    );
    let Some(on_reason) = on_reason else {
        sutura_core::repair::stage_fp_log("after_flap_net_choice", &on.0, &on.1);
        return RepairOutcome::Ok {
            verts: on.0,
            tris: on.1,
            report: on.2,
        };
    };

    // Re-run the file with Flap off — the same options otherwise, so the
    // fallback cannot diverge from a plain flap-OFF repair.
    let mut off_opts = opts.clone();
    off_opts.flap = Some(false);
    let t_off = std::time::Instant::now();
    let off = match repair(verts, tris, &off_opts, sink, token) {
        RepairOutcome::Ok {
            verts: out_v,
            tris: out_t,
            report,
        } => (out_v, out_t, report),
        other => {
            // The net never crashes a repair: keep the flap-ON result and say
            // the comparison could not be made.
            let mut report = on.2.clone();
            if let Some(flap) = report.get_mut("flap").and_then(Value::as_object_mut) {
                flap.insert(
                    "final_fallback".into(),
                    json!({"ran": true, "used": "on", "reason": on_reason,
                           "on": on_metrics, "error": "flap-off rerun failed"}),
                );
            }
            let _ = other;
            sutura_core::repair::stage_fp_log("after_flap_net_choice", &on.0, &on.1);
            return RepairOutcome::Ok {
                verts: on.0,
                tris: on.1,
                report,
            };
        }
    };

    sutura_core::repair::stage_fp_log("after_flap_off_rerun", &off.0, &off.1);
    let (_, off_metrics) = sutura_core::tiers::flap::final_off(verts, tris, &off.0, &off.1, si_rise, false);
    let use_off = sutura_core::tiers::flap::off_preferred(
        &on_reason,
        on_metrics.as_ref().unwrap_or(&Value::Null),
        off_metrics.as_ref().unwrap_or(&Value::Null),
    );

    let tamponade_skipped = opts.tamponade != Some(false);
    // Python records the re-run's cost so the report separates it from the
    // primary run (`service.py:452-455`).
    let off_seconds = Value::from((t_off.elapsed().as_secs_f64() * 1000.0).round() / 1000.0);
    let mut fallback = json!({
        "ran": true,
        "used": if use_off { "off" } else { "on" },
        "reason": on_reason,
        "on": on_metrics,
        "off": off_metrics,
        "seconds": off_seconds,
        "tamponade_skipped": tamponade_skipped,
    });

    sutura_core::repair::stage_fp_log(
        "after_flap_net_choice",
        if use_off { &off.0 } else { &on.0 },
        if use_off { &off.1 } else { &on.1 },
    );
    if use_off {
        // Report the primary run's Flap verdict, marked as not adopted in the
        // end, together with the fallback record.
        let mut on_flap = on.2.get("flap").cloned().unwrap_or_else(|| json!({}));
        if let Some(obj) = on_flap.as_object_mut() {
            obj.insert("adopted_final".into(), Value::Bool(false));
            obj.insert("final_fallback".into(), fallback.clone());
        }
        let mut report = off.2.clone();
        if let Some(obj) = report.as_object_mut() {
            obj.insert("flap".into(), on_flap);
        }
        // Carry the primary run's tamponade verdict when it did not change the
        // geometry (a no-op record); an adopted tier changed the flap-ON mesh,
        // which is not the mesh now being reported.
        let on_tamp = on.2.get("tamponade").cloned();
        let tamponade_adopted = on_tamp
            .as_ref()
            .and_then(|t| t.get("adopted"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if tamponade_skipped && !tamponade_adopted {
            if let Some(tamp) = on_tamp {
                if let Some(obj) = report.as_object_mut() {
                    obj.insert("tamponade".into(), tamp);
                }
            }
        }
        RepairOutcome::Ok {
            verts: off.0,
            tris: off.1,
            report,
        }
    } else {
        let mut report = on.2.clone();
        if let Some(obj) = fallback.as_object_mut() {
            obj.insert("used".into(), Value::from("on"));
        }
        if let Some(flap) = report.get_mut("flap").and_then(Value::as_object_mut) {
            flap.insert("final_fallback".into(), fallback);
        }
        RepairOutcome::Ok {
            verts: on.0,
            tris: on.1,
            report,
        }
    }
}

fn process_one(
    cli: &crate::Cli,
    path: &Path,
    out_override: Option<&Path>,
    opts: &RepairOptions,
    methods: &[u64],
    progress: Option<&JsonProgress>,
    sink: &CoreSink,
) -> (Value, bool) {
    let t_cli = Instant::now();
    let cli_t = |label: &str, d: std::time::Duration| {
        if std::env::var_os("SUTURA_CLI_TIMINGS").is_some() {
            eprintln!("[cli-t] {label} {:.3}", d.as_secs_f64());
        }
    };
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
        return process_3mf(cli, path, out_override, opts, methods, progress, sink);
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
    cli_t("load", t_cli.elapsed());
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
    let outcome = run_with_flap_safety_net(&verts, &tris, opts, &token, sink);
    stop.store(true, Ordering::SeqCst);
    let _ = monitor.join();
    cli_t("repair", started.elapsed());

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
    sutura_core::repair::stage_fp_log("before_write", &out_v, &out_t);
    let out_mesh = to_mesh(&out_v, &out_t);
    // Python treats 0 as "budget disabled" (``args.max_risk or None``).
    let max_geom = cli.max_geometry_change.filter(|v| *v > 0.0);
    let max_risk = cli.max_risk.filter(|v| *v > 0.0);
    let budgeted = max_geom.is_some() || max_risk.is_some();
    let save_path = if budgeted {
        budget_temp_path(&output)
    } else {
        output.clone()
    };
    let write_result = save_mesh(&save_path, &out_mesh);
    sutura_core::repair::stage_fp_log("after_write", &out_v, &out_t);
    if let Err(e) = write_result {
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
        augment_report(
            &input,
            &out_v,
            &out_t,
            core_report,
            opts,
            methods,
            elapsed,
            cli.no_fidelity,
        );
    if let Some(obj) = report.as_object_mut() {
        obj.insert("input".into(), Value::from(input_display));
        obj.insert("output".into(), Value::from(output.display().to_string()));
        // OBJ can carry `usemtl`/`mtllib` references, and the repaired OBJ is
        // written without them, so the report says whether anything was
        // dropped. Python sets this for `.obj` inputs only, unconditionally
        // (`service.py`: `if ext == '.obj': report['material_discarded'] = ...`).
        // The field was read by `human.rs` but never written after the merge.
        if is_obj(path) {
            obj.insert(
                "material_discarded".into(),
                Value::Bool(sutura_core::obj_has_material_refs(path)),
            );
        }
        contract_order(obj);
    }
    if budgeted {
        let budget = budget_check_value(&report, max_geom, max_risk);
        let within = budget
            .as_ref()
            .and_then(|b| b.get("within_budget"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if within || cli.force {
            if let Err(e) = std::fs::rename(&save_path, &output) {
                let _ = std::fs::remove_file(&save_path);
                if let Some(obj) = report.as_object_mut() {
                    obj.insert("error".into(), Value::from(format!("write failed: {e}")));
                }
            }
        } else {
            let _ = std::fs::remove_file(&save_path);
            if let Some(obj) = report.as_object_mut() {
                let message = if let Some(b) = &budget {
                    format!(
                        "Save declined: the repair exceeded your budget ({}). No output was written; re-run with --force to save anyway.",
                        budget_message(b)
                    )
                } else {
                    "Save declined: the repair exceeded your budget. No output was written; re-run with --force to save anyway.".to_string()
                };
                obj.insert("status".into(), Value::from("budget_declined"));
                obj.insert("error".into(), Value::from(message));
            }
        }
        if let Some(budget) = budget {
            if let Some(obj) = report.as_object_mut() {
                obj.insert("budget".into(), budget);
            }
        }
    }
    let failed = report.get("category").and_then(Value::as_str) == Some("error")
        || report.get("error").is_some();

    // History: the writer existed and was byte-verified (D1) but nothing in the
    // CLI called it, so `history.jsonl` was never extended by a Rust repair.
    // `--no-history` is the switch, matching Python. Failures and declined saves
    // are not recorded -- Python records a successful repair only, and a record
    // for a mesh that was never written would corrupt the learning stats.
    //
    // WARNING for anyone writing a gate or a benchmark around this binary: this
    // write is ON by default and lands in `$HOME/.local/share/sutura/data`.
    // A gate that runs 26 meshes per pass therefore appends 26 records to the
    // real user directory on a machine that may have no installation at all --
    // which happened, twice, before the runner was fenced off with
    // `SUTURA_DATA_DIR` plus `--no-history`. Both are applied by
    // `/tmp/p10-io-gate.sh`; a new gate must do the same.
    if !cli.no_history && !failed {
        let wrote_output = report.get("status").and_then(Value::as_str) != Some("budget_declined");
        if wrote_output {
            let ts = sutura_core::report::history::now_ts();
            sutura_core::report::history::write_record(
                &report,
                &input.verts,
                &input.tris,
                env!("SUTURA_PRODUCT_VERSION"),
                (elapsed * 1000.0).round() as i64,
                None,
                &ts,
            );
        }
    }
    (report, failed)
}

pub fn run(cli: &crate::Cli) -> Result<(), (u8, Option<String>)> {
    // `--self-repair [--repair-install]` reinstalls the running version; it has
    // no mesh to repair, so it short-circuits before any file handling. Python's
    // `self_repair.py` did the same job by rewriting its install tree; here the
    // work is the updater's own download/verify/swap/rollback path, reused.
    if cli.self_repair || cli.repair_install {
        return crate::self_repair_command(cli);
    }
    let (opts, methods_given, methods) = build_options(cli).map_err(|e| (e.code, e.message))?;
    if cli.files.is_empty() {
        return Err((2, Some("no input files".to_string())));
    }

    // `--no-learning-triage` / `--no-cache` are flags whose consumers read the
    // environment, not an argument. Python sets the variable at the same point
    // (`repair.py:1063-1067`), and `config::learning::learning_enabled` reads
    // it. Setting it here is what makes the flag mean something; before this it
    // was parsed and dropped.
    if cli.no_learning_triage {
        std::env::set_var("SUTURA_LEARNING_TRIAGE", "0");
    }
    if cli.no_cache {
        std::env::set_var("SUTURA_NO_CACHE", "1");
    }

    install_sigint();
    INTERRUPTED.store(false, Ordering::SeqCst);

    // `--trace PATH` records every stage boundary the core emits. When the flag
    // is absent the sink still exists but nothing is read from it and no file is
    // written, so the cost is one mutex push per boundary -- the same "zero extra
    // work when disabled" contract Python states at `trace.py:16`.
    let trace_sink = CoreSink::new();

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
        let (report, failed) =
            process_one(cli, file, out_override, &opts, &methods, progress.as_ref(), &trace_sink);
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

    if let Some(trace_path) = &cli.trace {
        let mut stages = trace_sink.take();
        // The core emits boundaries only; the measures and the sha256 belong to
        // the mesh at that boundary, and the sink never sees geometry. Attach the
        // final mesh's measures to the last stage and leave the per-boundary ones
        // empty rather than inventing them -- a wrong sha is worse than a missing
        // one for `scripts/trace_diff.py`, which compares them by value.
        for (i, stage) in stages.iter_mut().enumerate() {
            if let Some(obj) = stage.as_object_mut() {
                obj.insert("step".into(), Value::from(i as u64));
                obj.entry("object_id").or_insert(Value::Null);
                obj.entry("attempt").or_insert(Value::Null);
                obj.entry("elapsed_seconds").or_insert(Value::from(0.0));
                obj.entry("input").or_insert(Value::Null);
                obj.entry("output").or_insert(Value::Null);
            }
        }
        let command_args = json!({
            "mode": cli.mode.clone(),
            "intensity": cli.intensity.clone(),
            "profile": cli.profile.clone(),
            "methods": cli.methods.clone(),
            "files": cli.files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
        });
        if let Err(e) = write_trace(
            trace_path,
            &cli.files[0],
            stages,
            trace_sink.elapsed(),
            command_args,
        ) {
            return Err((1, Some(format!("trace write failed: {e}"))));
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

#[cfg(test)]
mod history_guard_tests {
    /// `SUTURA_DATA_DIR` must redirect the history write away from the real
    /// home directory.
    ///
    /// This is the guard the gates depend on: the CLI records history by
    /// default, so a gate that forgets the variable appends to
    /// `$HOME/.local/share/sutura/data/history.jsonl` on a machine that may have
    /// no installation. That happened twice before the gates were fenced; this
    /// test makes the redirect a pinned invariant rather than a convention.
    ///
    /// Sets the process-global variable, which is why there is exactly one test
    /// here -- two would race under the parallel harness.
    #[test]
    fn data_dir_env_redirects_history() {
        let dir = std::env::temp_dir().join(format!("sutura-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Restore the previous value on the way out. Leaving it set made every
        // later test in this binary see a directory that this one then deletes --
        // which failed a full `cargo test --workspace` run while the same binary
        // passed on its own, three times in a row.
        let previous = std::env::var_os(sutura_core::config::learning::DATA_ENV);
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.0 {
                    Some(v) => std::env::set_var(sutura_core::config::learning::DATA_ENV, v),
                    None => std::env::remove_var(sutura_core::config::learning::DATA_ENV),
                }
            }
        }
        let _restore = Restore(previous);
        std::env::set_var(sutura_core::config::learning::DATA_ENV, &dir);

        let path = sutura_core::report::history::history_path();
        assert!(
            path.starts_with(&dir),
            "history must land under SUTURA_DATA_DIR, got {path:?}"
        );
        assert!(!path.starts_with(std::env::var("HOME").unwrap_or_default() + "/.local"));

        // And an actual append goes there.
        let mut record = serde_json::json!({"format": "stl", "faces_before": 1});
        sutura_core::report::history::append_record(&mut record);
        let text = std::fs::read_to_string(&path).expect("the record must be written");
        assert!(text.contains("\"format\": \"stl\""), "got {text}");
        assert!(text.ends_with('\n'), "one record per line");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod trace_tests {
    use super::*;

    /// The sha must be the same bytes Python hashes, or `trace_diff.py` would
    /// compare two different digests for identical geometry.
    ///
    /// Verified by construction: the digest of an empty mesh is SHA-256 of no
    /// input at all, which is a known constant and therefore a real check that
    /// the shape of the loop is right.
    #[test]
    fn mesh_sha256_of_an_empty_mesh_is_the_empty_digest() {
        assert_eq!(
            mesh_sha256(&[], &[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// `-0.0` must fold to `+0.0`: Python normalizes before hashing
    /// (`trace.py:44`), so two geometrically identical buffers that differ only
    /// in the sign of zero have to produce one digest.
    #[test]
    fn mesh_sha256_normalizes_negative_zero() {
        let a = [[0.0f64, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let b = [[-0.0f64, 0.0, 0.0], [1.0, -0.0, 0.0], [0.0, 1.0, 0.0]];
        assert_eq!(mesh_sha256(&a, &[]), mesh_sha256(&b, &[]));
    }

    /// The measures carry exactly the keys `scripts/trace_diff.py` reads.
    #[test]
    fn measure_trace_reports_the_fields_trace_diff_needs() {
        let verts = vec![[0.0f64, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0i64, 1, 2]];
        let m = measure_trace(&verts, &tris);
        for key in ["faces", "verts", "holes", "nm_edges", "nm_verts", "components", "sha256"] {
            assert!(m.get(key).is_some(), "measure is missing {key}");
        }
        assert_eq!(m["faces"], 1);
        assert_eq!(m["verts"], 3);
        // An open triangle has three boundary EDGES (one per side), no
        // non-manifold edges, and is therefore not watertight. The field is
        // named `holes` for Python parity (`trace.py:88` calls it
        // `boundary_loop_stats()[0]`), but `topology_summary` counts edges --
        // which is what the value has always been.
        assert_eq!(m["nm_edges"], 0);
        assert_eq!(m["holes"], 3);
        // Not watertight, so volume stays null rather than being computed.
        assert_eq!(m["volume"], Value::Null);
    }

    /// The sink collects what the core emits and hands it over exactly once.
    #[test]
    fn core_sink_collects_and_drains() {
        let sink = CoreSink::new();
        assert!(sink.take().is_empty(), "nothing emitted yet");
        sink.emit(json!({"event": "stage", "stage": 1, "name": "validate"}));
        sink.emit(json!({"event": "tier", "tier": "si_excise", "ran": true}));
        let events = sink.take();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["name"], "validate");
        assert_eq!(events[1]["tier"], "si_excise");
        assert!(sink.take().is_empty(), "take drains, so nothing is duplicated");
    }
}

#[cfg(test)]
mod equivalence_tests {
    use sutura_core::repair_settings::RepairSettings;

    /// The GUI and the CLI must produce identical `RepairOptions` from the same
    /// user intent. Both already call `options_from`, so what this pins is the
    /// *wiring*: a GUI control bound to the wrong settings field, or a CLI flag
    /// that parses but never copies across, fails here.
    ///
    /// `RepairOptions` has no `PartialEq`, so values are compared through serde;
    /// field order is fixed by the struct, so equal JSON means equal options.
    fn json_of(cli_args: &[&str]) -> String {
        let cli = <crate::Cli as clap::Parser>::try_parse_from(cli_args)
            .expect("CLI args must parse");
        let (opts, _, _) = super::build_options(&cli).expect("must convert");
        serde_json::to_string(&opts).expect("options must serialise")
    }

    fn json_of_settings(s: &RepairSettings) -> String {
        let (opts, _, _) =
            sutura_core::repair_settings::options_from(s).expect("must convert");
        serde_json::to_string(&opts).expect("options must serialise")
    }

    #[test]
    fn cli_flags_and_gui_settings_agree_on_every_limit() {
        // The CLI spelling of the same intent the GUI panel expresses.
        let from_cli = json_of(&[
            "sutura",
            "in.stl",
            "--max-risk", "0.25",
            "--max-geometry-change", "0.0125",
            "--graft-max-faces", "5001",
            "--graft-timeout", "31.5",
            "--dense-ratio", "4",
            "--dense-min-faces", "2002",
            "--ftetwild-max-faces", "9003",
            "--ftetwild-timeout", "32.5",
            "--ftetwild-max-hausdorff", "0.0075",
            "--ftetwild-optimize-retry",
            "--experimental-join-components",
        ]);

        // What the GUI builds when the user types those numbers into the panel.
        let gui = RepairSettings {
            max_risk: Some(0.25),
            max_geometry_change: Some(0.0125),
            graft_max_faces: Some(5001),
            graft_timeout: Some(31.5),
            dense_ratio: Some(4),
            dense_min_faces: Some(2002),
            ftetwild_max_faces: Some(9003),
            ftetwild_timeout: Some(32.5),
            ftetwild_max_hausdorff_rel: Some(0.0075),
            ftetwild_optimize_retry: Some(true),
            join_components: true,
            ..RepairSettings::default()
        };
        assert_eq!(
            from_cli,
            json_of_settings(&gui),
            "CLI flags and GUI settings must produce the same RepairOptions"
        );
    }

    #[test]
    fn gui_defaults_match_a_no_flag_cli_run() {
        let from_cli = json_of(&["sutura", "in.stl"]);
        assert_eq!(
            from_cli,
            json_of_settings(&RepairSettings::from_intensity("balanced", false)),
            "an untouched GUI must match the CLI's no-flag run"
        );
    }

    #[test]
    fn a_cleared_box_is_not_a_zero_budget() {
        let mut s = RepairSettings::from_intensity("balanced", false);
        let cleared = json_of_settings(&s);
        s.ftetwild_timeout = Some(0.0);
        assert_ne!(
            cleared,
            json_of_settings(&s),
            "None (keep the tier's value) must not equal Some(0.0): a zero \
             timeout would give the tier no time at all"
        );
    }

    /// One table for flag reachability, rather than a test per flag.
    ///
    /// Two properties per row: the CLI must **accept** the flag (a stale
    /// rejection block made several implemented flags unreachable), and the flag
    /// must **change** the resulting options. A flag that parses but reaches
    /// nothing is the same bug in a quieter form.
    ///
    /// `refused` rows are the opposite assertion: flags with no Rust
    /// implementation must fail with exit code 2 and name the flag, so the
    /// refusal is visible rather than silent.
    #[test]
    fn every_flag_is_either_reachable_or_explicitly_refused() {
        use clap::Parser;

        // (argv, JSON pointer that must differ from the no-flag run)
        let reachable: &[(&[&str], &str)] = &[
            (&["--repeat-source", "0,0,0", "--repeat-target", "1,1,1", "--methods", "12"], "/repeat"),
            (&["--ftetwild-optimize"], "/ftetwild_optimize_retry"),
            (&["--experimental-dressing"], "/dressing"),
            (&["--dressing-drain", "half"], "/dressing_drain"),
            (&["--dressing-defects", "holes_nm"], "/dressing_defects"),
            (&["--dressing-rmax-scale", "1.5"], "/dressing_rmax_scale"),
            (&["--dressing-sigma-scale", "0.75"], "/dressing_sigma_scale"),
            (&["--dressing-force-adopt"], "/dressing_force_adopt"),
            (&["--graft-timeout", "31.5"], "/graft_timeout"),
            (&["--dense-ratio", "4"], "/dense_ratio"),
            // The wall builder is ported, so the target thickness now reaches
            // the options and turns the tier on.
            (&["--wall-min-thickness", "0.8"], "/wall_min_thickness"),
            // p10/core opt-in ports are wired after `options_from`.
            (&["--experimental-edge-tiebreak"], "/extra_features"),
            (&["--experimental-autorefine"], "/autorefine"),
            (&["--experimental-indirect-autorefine"], "/indirect_autorefine"),
            (&["--auto-reorder"], "/auto_reorder"),
        ];
        // `--max-risk` / `--max-geometry-change` are deliberately absent from
        // this table: they are budgets enforced in the CLI layer
        // (`budget_check_value`), not `RepairOptions` fields, so there is no
        // option to point at. Their reachability is covered by the budget tests.

        let baseline = {
            let cli = <crate::Cli as Parser>::try_parse_from(["sutura", "in.stl"])
                .expect("baseline parses");
            serde_json::to_value(
                super::build_options(&cli).expect("baseline converts").0,
            )
            .expect("serialises")
        };

        for (args, pointer) in reachable {
            let mut argv = vec!["sutura", "in.stl"];
            argv.extend_from_slice(args);
            let cli = <crate::Cli as Parser>::try_parse_from(&argv)
                .unwrap_or_else(|e| panic!("{args:?} must parse: {e}"));
            let opts = super::build_options(&cli)
                .unwrap_or_else(|e| {
                    panic!("{args:?} must be accepted, not refused: {:?}", e.message)
                })
                .0;
            let value = serde_json::to_value(&opts).expect("serialises");
            let got = value.pointer(pointer);
            let had = baseline.pointer(pointer);
            assert_ne!(
                got, had,
                "{args:?} parsed but did not reach {pointer}: got {got:?}, \
                 baseline {had:?}"
            );
        }

        // Flags with no Rust implementation must be refused, loudly.
        // The three p10/core opt-in ports used to live here; they are now
        // reachable rows above, so the refusal table is intentionally empty.
        let refused: &[&[&str]] = &[];
        for args in refused {
            let mut argv = vec!["sutura", "in.stl"];
            argv.extend_from_slice(args);
            let cli = <crate::Cli as Parser>::try_parse_from(&argv)
                .unwrap_or_else(|e| panic!("{args:?} must parse: {e}"));
            let err = super::build_options(&cli)
                .err()
                .unwrap_or_else(|| panic!("{args:?} must be refused"));
            assert_eq!(err.code, 2, "{args:?} must be a usage error");
            let msg = err.message.unwrap_or_default();
            assert!(
                args.iter().any(|a| msg.contains(a)),
                "{args:?}: the message must name the flag, got {msg:?}"
            );
        }
    }

    /// Flags whose consumer reads the environment rather than an argument.
    ///
    /// `--no-learning-triage` and `--no-cache` are read by
    /// `config::learning::learning_enabled` / `cache::enabled`, so `run()` sets
    /// the variable exactly where Python does (`repair.py:1063-1067`). Before
    /// they were wired the flags parsed and were dropped, which is the quietest
    /// possible way to be wrong.
    #[test]
    fn env_backed_flags_parse_to_a_switch() {
        use clap::Parser;

        let on = <crate::Cli as Parser>::try_parse_from([
            "sutura",
            "--no-learning-triage",
            "--no-cache",
            "--no-history",
            "--no-fidelity",
            "in.stl",
        ])
        .expect("parses");
        assert!(on.no_learning_triage);
        assert!(on.no_cache);
        assert!(on.no_history);
        assert!(on.no_fidelity);

        // The inverse: absent flags stay false, so a default run is untouched.
        let off = <crate::Cli as Parser>::try_parse_from(["sutura", "in.stl"])
            .expect("parses");
        assert!(!off.no_learning_triage);
        assert!(!off.no_cache);
        assert!(!off.no_history);
        assert!(!off.no_fidelity);
    }
}
