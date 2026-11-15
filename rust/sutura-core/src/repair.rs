// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Rust pipeline orchestration skeleton (Phase 6, slices 0 and 1).
//!
//! Mirrors the Stage-1 half of `sutura/sutura_engine/pipeline/pipeline.py`
//! `repair_mesh_from_arrays`: input validation, Stage-1 chain, the Tamponade /
//! Flap pre-passes, pinch splitting, the Manifold Stage-2 rebuild and P-WELD.
//!
//! This is deliberately a slice, not the finished pipeline. Everything that is
//! not wired here yet is reported under `report.unavailable`; a tier is never
//! dropped silently. No Python is called and no per-tier callback exists: the
//! `ProgressSink` only sees stage and tier boundaries.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sutura_geom::manifold::{stage2_rebuild_core, Stage2Report};

use crate::config::intensity::{resolve_intensity, IntensitySpec};
use crate::config::policy::{load_policy, LoadedPolicy};
use crate::p_weld::{enforce_reload_verdict, p_weld_final};
use crate::stage1::chain::{
    apply_chain, cleanup_chain, delete_fallback_chain, extreme_extra_passes, stage1_chain,
};
use crate::stage2::split_pinched_vertices;
use crate::tiers::{common, flap, tamponade};

/// Repair mode, matching the Python `REPAIR_MODES` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairMode {
    Auto,
    Low,
    Medium,
    Aggressive,
    Extreme,
}

impl Default for RepairMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl RepairMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::Aggressive => "aggressive",
            Self::Extreme => "extreme",
        }
    }
}

/// Deep-repair ladder mode, matching the Python `DEEP_REPAIR_MODES` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeepRepair {
    Off,
    Local,
    Full,
}

impl Default for DeepRepair {
    fn default() -> Self {
        Self::Full
    }
}

impl DeepRepair {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Local => "local",
            Self::Full => "full",
        }
    }
}

/// Standalone scan-closing mode (registry methods 8/9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosingMode {
    Poisson,
    FlatBack,
    Mirror,
}

/// Repeated-element transplant mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    Manual,
    Auto,
}

/// Last-resort solid engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SolidEngine {
    Ftetwild,
    Cdt,
}

impl Default for SolidEngine {
    fn default() -> Self {
        Self::Ftetwild
    }
}

impl SolidEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ftetwild => "ftetwild",
            Self::Cdt => "cdt",
        }
    }
}

/// Self-intersection policy, matching `resolve_si_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiMode {
    Repair,
    Report,
    Off,
}

impl Default for SiMode {
    fn default() -> Self {
        Self::Report
    }
}

impl SiMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::Report => "report",
            Self::Off => "off",
        }
    }
}

/// Options for [`repair`], with serde defaults equal to the Python CLI
/// defaults (`--mode auto`, Balanced intensity, Tamponade/Flap/Coaptation on,
/// Splint/Dressing/Wall off, fTetWild solid engine, SI mode `report`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RepairOptions {
    pub mode: RepairMode,
    pub intensity: String,
    pub profile: Option<String>,
    pub deep_repair: DeepRepair,
    pub closing: Option<ClosingMode>,
    pub proxy_template: bool,
    pub graft: Option<bool>,
    pub tamponade: Option<bool>,
    pub flap: Option<bool>,
    pub coaptation: Option<bool>,
    pub splint: bool,
    pub dressing: bool,
    pub wall_thicken: bool,
    pub repeat: Option<RepeatMode>,
    pub solid_engine: SolidEngine,
    pub si_mode: SiMode,
    pub time_budget_s: Option<f64>,
    pub seed: u64,
}

impl Default for RepairOptions {
    fn default() -> Self {
        Self {
            mode: RepairMode::Auto,
            intensity: "balanced".to_string(),
            profile: None,
            deep_repair: DeepRepair::Full,
            closing: None,
            proxy_template: false,
            graft: Some(true),
            tamponade: Some(true),
            flap: Some(true),
            coaptation: Some(true),
            splint: false,
            dressing: false,
            wall_thicken: false,
            repeat: None,
            solid_engine: SolidEngine::Ftetwild,
            si_mode: SiMode::Report,
            time_budget_s: None,
            seed: 0,
        }
    }
}

/// The result of one Rust pipeline run.
pub enum RepairOutcome {
    Ok {
        verts: Vec<[f64; 3]>,
        tris: Vec<[i64; 3]>,
        report: Value,
    },
    Unavailable {
        tier: &'static str,
        reason: String,
    },
    Error {
        message: String,
    },
}

/// Stage/tier progress events. There is deliberately no per-tier callback.
pub trait ProgressSink {
    fn emit(&self, event: Value);
}

/// A sink that discards every event.
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn emit(&self, _event: Value) {}
}

/// Cooperative cancellation flag shared with the caller.
pub struct CancelToken {
    flag: AtomicBool,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

fn emit_stage(sink: &dyn ProgressSink, stage: u32, name: &str) {
    sink.emit(json!({"event": "stage", "stage": stage, "name": name}));
}

fn emit_tier(sink: &dyn ProgressSink, tier: &str, stage: u32, report: &Value) {
    sink.emit(json!({
        "event": "tier",
        "tier": tier,
        "stage": stage,
        "ran": report.get("ran").cloned().unwrap_or(Value::Bool(false)),
        "adopted": report.get("adopted").cloned().unwrap_or(Value::Bool(false)),
        "reason": report.get("reason").cloned().unwrap_or(Value::Null),
    }));
}

fn cancelled() -> RepairOutcome {
    RepairOutcome::Error {
        message: "cancelled".to_string(),
    }
}

fn error(message: impl Into<String>) -> RepairOutcome {
    RepairOutcome::Error {
        message: message.into(),
    }
}

fn to_f64(v: &[[f32; 3]]) -> Vec<[f64; 3]> {
    v.iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect()
}

fn to_i64(t: &[[i32; 3]]) -> Vec<[i64; 3]> {
    t.iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect()
}

fn to_i32(t: &[[i64; 3]]) -> Vec<[i32; 3]> {
    t.iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect()
}

/// Quantize to the float32 output rule used by the Python pipeline.
fn quantize(verts: &[[f64; 3]]) -> Vec<[f64; 3]> {
    verts
        .iter()
        .map(|p| [p[0] as f32 as f64, p[1] as f32 as f64, p[2] as f32 as f64])
        .collect()
}

/// `boundary_loop_stats` (count, largest loop edge count) without numpy.
fn boundary_loop_stats(tris: &[[i64; 3]]) -> (usize, usize) {
    if tris.is_empty() {
        return (0, 0);
    }
    let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let k = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(k).or_insert(0) += 1;
        }
    }
    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    for (&(a, b), &c) in &counts {
        if c == 1 {
            adj.entry(a).or_default().push(b);
            adj.entry(b).or_default().push(a);
        }
    }
    if adj.is_empty() {
        return (0, 0);
    }
    let mut seen: HashMap<i64, bool> = HashMap::new();
    let mut loops = Vec::new();
    for &start in adj.keys() {
        if seen.get(&start).copied().unwrap_or(false) {
            continue;
        }
        let mut n = 0usize;
        let mut cur = start;
        let mut prev: Option<i64> = None;
        loop {
            if seen.get(&cur).copied().unwrap_or(false) {
                break;
            }
            seen.insert(cur, true);
            n += 1;
            let next = adj
                .get(&cur)
                .and_then(|list| list.iter().copied().find(|x| Some(*x) != prev));
            match next {
                Some(x) => {
                    prev = Some(cur);
                    cur = x;
                }
                None => break,
            }
        }
        loops.push(n);
    }
    let max = loops.iter().copied().max().unwrap_or(0);
    (loops.len(), max)
}

fn read_thresholds(policy: Option<&LoadedPolicy>, section: &str, name: &str) -> Option<(i64, i64)> {
    let table = policy?.get(section)?.get(name)?.as_table()?;
    let min = table.get("mincomponentsize")?.as_i64()?;
    let max = table.get("maxholesize")?.as_i64()?;
    Some((min, max))
}

/// Stage-1 thresholds for the requested mode/profile. The classifier is not
/// ported in this slice, so `auto` without a profile uses the conservative
/// `unknown` type defaults (8/1000), exactly the Python fallback.
fn resolve_thresholds(opts: &RepairOptions, policy: Option<&LoadedPolicy>) -> (i64, i64) {
    let fixed = match opts.mode {
        RepairMode::Low => read_thresholds(policy, "modes", "low").or(Some((8, 200))),
        RepairMode::Medium => read_thresholds(policy, "modes", "medium").or(Some((8, 1000))),
        RepairMode::Aggressive => {
            read_thresholds(policy, "modes", "aggressive").or(Some((12, 3000)))
        }
        RepairMode::Extreme => read_thresholds(policy, "modes", "extreme").or(Some((20, 10000))),
        RepairMode::Auto => None,
    };
    if let Some(pair) = fixed {
        return pair;
    }
    if let Some(profile) = &opts.profile {
        if let Some(pair) = read_thresholds(policy, "stage1_profiles", profile) {
            return pair;
        }
    }
    read_thresholds(policy, "stage1_types", "unknown").unwrap_or((8, 1000))
}

/// Resolve the triage intensity (CLI > env > config.json > Balanced).
fn resolve_intensity_spec(opts: &RepairOptions, policy: Option<&LoadedPolicy>) -> IntensitySpec {
    match policy {
        Some(loaded) => resolve_intensity(
            loaded,
            Some(opts.intensity.as_str()),
            None,
            None,
            None,
            None,
        ),
        None => IntensitySpec {
            name: opts.intensity.clone(),
            ..IntensitySpec::default()
        },
    }
}

const TAMPONADE_WORK: [(&str, i64); 4] = [
    ("quick", 0),
    ("balanced", 2_000_000),
    ("thorough", 8_000_000),
    ("extreme", 25_000_000),
];
const TAMPONADE_DEFAULT_WORK: i64 = 2_000_000;
const FLAP_DEFAULT_WORK: i64 = 75_000_000;

fn tamponade_work(intensity: &str) -> i64 {
    TAMPONADE_WORK
        .iter()
        .find(|(name, _)| *name == intensity)
        .map(|(_, work)| *work)
        .unwrap_or(TAMPONADE_DEFAULT_WORK)
}

fn flap_work(policy: Option<&LoadedPolicy>, intensity: &str) -> i64 {
    let by_intensity = policy
        .and_then(|p| p.get("tiers"))
        .and_then(|t| t.get("flap"))
        .and_then(|f| f.get("work_by_intensity"));
    if let Some(v) = by_intensity
        .and_then(|v| v.get(intensity))
        .and_then(|v| v.as_i64())
    {
        return v;
    }
    policy
        .and_then(|p| p.get("tiers"))
        .and_then(|t| t.get("flap"))
        .and_then(|f| f.get("default_work"))
        .and_then(|v| v.as_i64())
        .unwrap_or(FLAP_DEFAULT_WORK)
}

fn melt_si_enabled(intensity: &str) -> bool {
    if let Ok(value) = std::env::var("SUTURA_TAMPONADE_MELT") {
        return value.trim() == "1";
    }
    matches!(intensity, "thorough" | "extreme")
}

fn tamponade_params(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    intensity: &str,
    si_mode: SiMode,
    prepass: bool,
) -> Value {
    let work = tamponade_work(intensity);
    json!({
        "prepass": prepass,
        "tamponade_force": false,
        "intensity": intensity,
        "melt_si": melt_si_enabled(intensity),
        "h_min": common::median_edge_length(verts, tris),
        "budget": work,
        "prepass_budget": if prepass { Value::from(work) } else { Value::Null },
        "si_mode": si_mode.as_str(),
        "shape_gate_enabled": true,
        "genus": Value::Null,
        "input_faces": tris.len(),
    })
}

fn flap_params(tris: &[[i64; 3]], intensity: &str, policy: Option<&LoadedPolicy>) -> Value {
    json!({
        "work_budget": flap_work(policy, intensity),
        "intensity": intensity,
        "input_faces": tris.len(),
    })
}

fn adopted(report: &Value) -> bool {
    report
        .get("adopted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn stage2_report_json(rep: &Stage2Report) -> Value {
    let mut out = Map::new();
    out.insert(
        "construct_status".into(),
        Value::from(rep.construct_status.clone()),
    );
    out.insert("input_vertices".into(), Value::from(rep.input_vertices));
    out.insert("input_faces".into(), Value::from(rep.input_faces));
    out.insert("input_watertight".into(), Value::Bool(rep.input_watertight));
    out.insert("ok".into(), Value::Bool(rep.ok));
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(v) = value {
            out.insert(key.to_string(), v);
        }
    };
    put("error", rep.error.clone().map(Value::from));
    put("volume_before", rep.volume_before.map(Value::from));
    put(
        "volume_after_union",
        rep.volume_after_union.map(Value::from),
    );
    put(
        "debris_parts_dropped",
        rep.debris_parts_dropped.map(|v| Value::from(v as i64)),
    );
    put(
        "shells_merged",
        rep.shells_merged.map(|v| Value::from(v as i64)),
    );
    put("cavity_bbox_ratio", rep.cavity_bbox_ratio.map(Value::from));
    put("cavity_preserved", rep.cavity_preserved.map(Value::Bool));
    put(
        "cavity_rejected_bbox",
        rep.cavity_rejected_bbox.map(Value::Bool),
    );
    put(
        "cavity_rejected_si",
        rep.cavity_rejected_si.map(Value::Bool),
    );
    put("cavity_si_union", rep.cavity_si_union.map(Value::from));
    put("cavity_si_whole", rep.cavity_si_whole.map(Value::from));
    put(
        "post_union_debris_dropped",
        rep.post_union_debris_dropped.map(|v| Value::from(v as i64)),
    );
    put(
        "shells_found",
        rep.shells_found.map(|v| Value::from(v as i64)),
    );
    put("shells", rep.shells.map(|v| Value::from(v as i64)));
    put("volume_after", rep.volume_after.map(Value::from));
    drop(put);
    out.insert("output_vertices".into(), Value::from(rep.output_vertices));
    out.insert("output_faces".into(), Value::from(rep.output_faces));
    Value::Object(out)
}

/// Tiers this slice does not execute. Every entry is explicit; none is silent.
fn unavailable_tiers() -> Value {
    let entry = |reason: &str| {
        json!({
            "ran": false,
            "unavailable": true,
            "reason": reason,
        })
    };
    json!({
        "local_remesh": entry("local_remesh is a Python deep-repair tier; no sutura-core implementation yet"),
        "dressing": entry("Dressing orchestration is not in sutura-core yet; only the sutura-geom coat producer exists"),
        "ftetwild": entry("fTetWild is an external Python tier; no Rust implementation exists"),
        "graft": entry("Graft shell-wrap producer exists in sutura-core; deep-repair ladder wiring lands in a later slice"),
        "closing": entry("Closing producers exist in sutura-core; closing-tier wiring lands in a later slice"),
        "proxy": entry("Proxy-template producer exists in sutura-core; proxy-tier wiring lands in a later slice"),
        "mirror": entry("Mirror producer exists in sutura-core; mirror-tier wiring lands in a later slice"),
        "repeat": entry("Repeat transplant producer is Python-only; no sutura-core implementation yet"),
        "wall": entry("Wall-thicken producer is Python-only; no sutura-core implementation yet"),
        "splint": entry("Splint producer exists in sutura-core; deep-repair ladder wiring lands in a later slice"),
        "coaptation": entry("Coaptation producer exists in sutura-core; deep-repair ladder wiring lands in a later slice"),
        "cdt": entry("CDT solid engine exists in sutura-core; deep-repair ladder wiring lands in a later slice"),
    })
}

fn method_used() -> Value {
    json!({
        "num": Value::Null,
        "id": Value::Null,
        "name": Value::Null,
        "source": "rust",
    })
}

/// Repair one mesh given as float64/int64 arrays.
pub fn repair(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    opts: &RepairOptions,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
) -> RepairOutcome {
    if verts.is_empty() || tris.is_empty() {
        return error("input mesh is empty (no triangles)");
    }
    if verts.iter().any(|p| p.iter().any(|c| !c.is_finite())) {
        return error("input mesh contains NaN or infinite coordinates");
    }
    let nv = verts.len();
    for (fi, tri) in tris.iter().enumerate() {
        for &index in tri {
            if index < 0 || index as usize >= nv {
                return error(format!(
                    "triangle {fi} references vertex {index} but the mesh has {nv} vertices"
                ));
            }
        }
    }
    if cancel.is_cancelled() {
        return cancelled();
    }

    emit_stage(sink, 1, "validate");
    let policy = load_policy(None).ok();
    let spec = resolve_intensity_spec(opts, policy.as_ref());
    let intensity_for_tamponade = spec.base.clone().unwrap_or_else(|| spec.name.clone());
    let (min_component, mut max_hole) = resolve_thresholds(opts, policy.as_ref());
    let (_, largest_loop) = boundary_loop_stats(tris);
    max_hole = max_hole.max(2 * largest_loop as i64);

    let before = common::topology_summary(verts, tris);
    let holes_before = boundary_loop_stats(tris).0;
    let nm_before = before.non_two_manifold_edges;

    let mut cur_v = verts.to_vec();
    let mut cur_t = tris.to_vec();
    let mut stats = Map::new();
    stats.insert("timings".into(), json!({}));
    stats.insert("si_mode".into(), Value::from(opts.si_mode.as_str()));
    stats.insert("triage_intensity".into(), Value::from(spec.name.clone()));
    stats.insert("classifier_engine".into(), Value::from("rust"));
    stats.insert("detected_type".into(), Value::from("unknown"));
    stats.insert("detected_confidence".into(), Value::from(0.0));
    stats.insert("repair_mode".into(), Value::from(opts.mode.as_str()));
    if let Some(profile) = &opts.profile {
        stats.insert("repair_profile".into(), Value::from(profile.clone()));
    }
    stats.insert("tuning_applied".into(), Value::Bool(false));
    stats.insert(
        "solid_engine".into(),
        Value::from(opts.solid_engine.as_str()),
    );
    stats.insert("deep_repair".into(), Value::from(opts.deep_repair.as_str()));
    stats.insert("experimental_autorefine".into(), Value::Bool(false));
    stats.insert(
        "experimental_indirect_autorefine".into(),
        Value::Bool(false),
    );
    stats.insert("experimental_join_components".into(), Value::Bool(false));
    stats.insert("experimental_ftetwild".into(), Value::Bool(false));

    // --- Tamponade pre-pass ------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 2, "prepass");
    let mut tamponade_prepass_adopted = false;
    let mut tamponade_gate_skipped = false;
    if opts.tamponade == Some(true) {
        let hygiene = Value::Array(cleanup_chain(true));
        let (clean_v, clean_t, _, _, _) = apply_chain(&cur_v, &cur_t, &hygiene);
        let params = tamponade_params(
            &clean_v,
            &clean_t,
            &intensity_for_tamponade,
            opts.si_mode,
            true,
        );
        let (out_v, out_t, rec) = tamponade::run(&clean_v, &clean_t, &params);
        tamponade_prepass_adopted = adopted(&rec);
        tamponade_gate_skipped = rec
            .get("gate")
            .and_then(Value::as_str)
            .map(|g| g == "shape_card")
            .unwrap_or(false);
        if tamponade_prepass_adopted {
            cur_v = out_v;
            cur_t = out_t;
        }
        emit_tier(sink, "tamponade_prepass", 2, &rec);
        stats.insert("tamponade".into(), rec);
    }

    // --- Flap pre-pass -----------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    if opts.flap == Some(true) {
        let params = flap_params(&cur_t, &spec.name, policy.as_ref());
        let (out_v, out_t, rec) = flap::run(&cur_v, &cur_t, &params);
        if adopted(&rec) {
            cur_v = out_v;
            cur_t = out_t;
        }
        emit_tier(sink, "flap_prepass", 2, &rec);
        stats.insert("flap".into(), rec);
    }

    // --- Stage 1 -----------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 3, "stage 1");
    let chain = Value::Array(stage1_chain(max_hole, min_component, false));
    let (v1, t1, mut applied, mut skipped, removed_small) = apply_chain(&cur_v, &cur_t, &chain);
    cur_v = v1;
    cur_t = t1;

    let mut after = common::topology_summary(&cur_v, &cur_t);
    if after.non_two_manifold_edges > 0 || after.non_two_manifold_vertices > 0 {
        let fallback = Value::Array(delete_fallback_chain(max_hole, min_component, false));
        let (fv, ft, f_applied, f_skipped, _) = apply_chain(&cur_v, &cur_t, &fallback);
        let f_after = common::topology_summary(&fv, &ft);
        let damage_before = after.non_two_manifold_edges + after.non_two_manifold_vertices;
        let damage_after = f_after.non_two_manifold_edges + f_after.non_two_manifold_vertices;
        if damage_after < damage_before {
            cur_v = fv;
            cur_t = ft;
            after = f_after;
            applied = f_applied;
            skipped = f_skipped;
        }
    }

    if after.faces == 0 {
        if opts.mode == RepairMode::Extreme {
            return error(
                "Extreme mode removed all geometry (small connected component below the size threshold); try a less aggressive mode (e.g. Auto)",
            );
        }
        return error("all faces are degenerate; nothing to repair");
    }

    stats.insert("extreme_passes_applied".into(), Value::Bool(false));
    if opts.mode == RepairMode::Extreme {
        let params = json!({
            "maxholesize": max_hole,
            "mincomponentsize": min_component,
            "join_components": false,
        });
        let (ran, found, removed, _applied_extra, _skipped_extra, ev, et) =
            extreme_extra_passes(&cur_v, &cur_t, &params);
        if ran {
            cur_v = ev;
            cur_t = et;
            stats.insert("extreme_passes_applied".into(), Value::Bool(true));
            stats.insert("self_intersections_found".into(), Value::from(found));
            stats.insert(
                "self_intersections_removed".into(),
                Value::from(removed as i64),
            );
        }
    }

    // Tamponade second chance when the pre-pass did not adopt.
    if opts.tamponade == Some(true) && !tamponade_prepass_adopted && !tamponade_gate_skipped {
        let params = tamponade_params(
            &cur_v,
            &cur_t,
            &intensity_for_tamponade,
            opts.si_mode,
            false,
        );
        let (out_v, out_t, rec) = tamponade::run(&cur_v, &cur_t, &params);
        if adopted(&rec) {
            cur_v = out_v;
            cur_t = out_t;
        }
        emit_tier(sink, "tamponade_post", 3, &rec);
        stats.insert("tamponade".into(), rec);
    }

    // Pinch split before Stage 2, matching the pipeline's P1a split.
    let pinch = split_pinched_vertices(&cur_v, &cur_t);
    cur_v = to_f64(&pinch.verts);
    cur_t = to_i64(&pinch.tris);
    after = common::topology_summary(&cur_v, &cur_t);
    stats.insert(
        "pinched_vertices_split".into(),
        pinch.report.unwrap_or(Value::Bool(false)),
    );

    // --- Stage 1 report ----------------------------------------------------
    let holes_after = boundary_loop_stats(&cur_t).0;
    let nm_after = after.non_two_manifold_edges;
    let two_manifold = common::is_two_manifold(cur_v.len(), &cur_t);
    let mut stage1 = Map::new();
    stage1.insert(
        "holes_closed".into(),
        Value::from(holes_before.saturating_sub(holes_after)),
    );
    stage1.insert("holes_remaining".into(), Value::from(holes_after));
    stage1.insert(
        "non_manifold_edges_fixed".into(),
        Value::from(nm_before.saturating_sub(nm_after)),
    );
    stage1.insert("non_manifold_edges_remaining".into(), Value::from(nm_after));
    stage1.insert("two_manifold".into(), Value::Bool(two_manifold));
    stage1.insert("components".into(), Value::from(after.components));
    stage1.insert("faces_after".into(), Value::from(after.faces));
    stage1.insert("faces_before".into(), Value::from(before.faces));
    stage1.insert(
        "faces_removed".into(),
        Value::from(before.faces.saturating_sub(after.faces)),
    );
    stage1.insert("vertices_before".into(), Value::from(verts.len()));
    stage1.insert("vertices_after".into(), Value::from(cur_v.len()));
    stage1.insert("components_before".into(), Value::from(before.components));
    stage1.insert("applied_filters".into(), Value::from(applied));
    stage1.insert(
        "removed_small_parts".into(),
        removed_small.unwrap_or(Value::Null),
    );
    if skipped.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
        stage1.insert("skipped".into(), skipped);
    }
    stats.insert("stage1".into(), Value::Object(stage1));

    // --- Stage 2 -----------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 4, "stage 2");
    if two_manifold && holes_after == 0 {
        let t32 = to_i32(&cur_t);
        let (s2_v, s2_t, s2_rep) = stage2_rebuild_core(&cur_v, &t32);
        if s2_rep.ok && !s2_t.is_empty() {
            cur_v = to_f64(&s2_v);
            cur_t = to_i64(&s2_t);
        }
        stats.insert("stage2".into(), stage2_report_json(&s2_rep));
    }

    // --- P-WELD ------------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 5, "reload weld");
    let pw = p_weld_final(&cur_v, &cur_t);
    cur_v = to_f64(&pw.verts);
    cur_t = to_i64(&pw.tris);
    if let Some(rec) = pw.report {
        stats.insert("p_weld".into(), rec);
    }

    cur_v = quantize(&cur_v);

    let reachable =
        common::is_two_manifold(cur_v.len(), &cur_t) && boundary_loop_stats(&cur_t).0 == 0;
    stats.insert("method_reached_watertight".into(), Value::Bool(reachable));
    stats.insert("method_errors".into(), json!([]));
    stats.insert(
        "timings".into(),
        json!({
            "prepass": 0.0,
            "stage1": 0.0,
            "stage2": 0.0,
            "reload_weld": 0.0,
        }),
    );

    // P-HONEST: the verdict describes the mesh actually handed back.
    let mut report = Value::Object(stats);
    enforce_reload_verdict(&mut report, &cur_v, &cur_t, false);

    let mut out = match report {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let final_stats = out.clone();
    out.insert("methods_tried".into(), json!([]));
    out.insert("method_used".into(), method_used());
    out.insert("unavailable".into(), unavailable_tiers());
    // The Python pipeline returns its `stats` dict directly; keep the same
    // blocks addressable as a single object for the Rust callers as well.
    out.insert("stats".into(), Value::Object(final_stats));

    RepairOutcome::Ok {
        verts: cur_v,
        tris: cur_t,
        report: Value::Object(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let tris = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (verts, tris)
    }

    #[test]
    fn empty_mesh_is_an_error() {
        let opts = RepairOptions::default();
        let outcome = repair(&[], &[], &opts, &NoopSink, &CancelToken::new());
        assert!(matches!(outcome, RepairOutcome::Error { .. }));
    }

    #[test]
    fn closed_cube_with_opt_in_tiers_off_is_ok_and_lists_unavailable() {
        let (verts, tris) = cube();
        let opts = RepairOptions {
            tamponade: Some(false),
            flap: Some(false),
            graft: None,
            coaptation: Some(false),
            splint: false,
            dressing: false,
            wall_thicken: false,
            closing: None,
            proxy_template: false,
            repeat: None,
            deep_repair: DeepRepair::Off,
            ..RepairOptions::default()
        };
        let outcome = repair(&verts, &tris, &opts, &NoopSink, &CancelToken::new());
        let report = match outcome {
            RepairOutcome::Ok {
                verts,
                tris,
                report,
            } => {
                assert!(!verts.is_empty());
                assert!(!tris.is_empty());
                report
            }
            RepairOutcome::Unavailable { tier, reason } => {
                panic!("unexpected unavailable {tier}: {reason}")
            }
            RepairOutcome::Error { message } => panic!("unexpected error: {message}"),
        };
        let unavailable = report
            .get("unavailable")
            .and_then(Value::as_object)
            .expect("unavailable object");
        for tier in ["local_remesh", "dressing", "ftetwild"] {
            let entry = unavailable.get(tier).expect(tier);
            assert_eq!(entry.get("ran").and_then(Value::as_bool), Some(false));
            assert_eq!(
                entry.get("unavailable").and_then(Value::as_bool),
                Some(true)
            );
            assert!(entry.get("reason").and_then(Value::as_str).is_some());
        }
        assert_eq!(
            report
                .get("stage1")
                .and_then(|s| s.get("holes_remaining"))
                .and_then(Value::as_i64),
            Some(0)
        );
        assert!(report.get("methods_tried").is_some());
        assert!(report.get("method_used").is_some());
    }
}
