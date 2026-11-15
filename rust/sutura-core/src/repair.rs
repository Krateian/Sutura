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

use crate::analysis::{analyze_mesh, Analysis as MeshAnalysis};
use crate::classify::classify_mesh;
use crate::closing::{flat_back_close, poisson_close};
use crate::config::intensity::{resolve_intensity, IntensitySpec};
use crate::config::policy::{load_policy, LoadedPolicy, PolicyValue};
use crate::graft::shell::{shell_wrap, ShellWrapOptions};
use crate::local_remesh;
use crate::mesh::Mesh;
use crate::mirror::mirror_close;
use crate::p_weld::{enforce_reload_verdict, p_weld_final};
use crate::proxy::proxy_template_repair;
use crate::ranking::{rank_methods, recommendations_json, Analysis as RankingAnalysis};
use crate::report::health::build_report_value;
use crate::stage1::chain::{
    apply_chain, cleanup_chain, delete_fallback_chain, extreme_extra_passes, stage1_chain,
};
use crate::stage1::si_excise;
use crate::stage2::split_pinched_vertices;
use crate::tiers::closing as closing_tier;
use crate::tiers::{cdt, coaptation, common, flap, graft as graft_tier, splint, tamponade};

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
    pub ftetwild: Option<Value>,
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
            ftetwild: None,
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
pub(crate) fn boundary_loop_stats(tris: &[[i64; 3]]) -> (usize, usize) {
    let t32: Vec<[i32; 3]> = tris
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    crate::validate::boundary_loop_stats(0, &t32)
}

fn read_thresholds(policy: Option<&LoadedPolicy>, section: &str, name: &str) -> Option<(i64, i64)> {
    let table = policy?.get(section)?.get(name)?.as_table()?;
    let min = table.get("mincomponentsize")?.as_i64()?;
    let max = table.get("maxholesize")?.as_i64()?;
    Some((min, max))
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

/// Classifier + triage result for one input, mirroring the Python
/// `classify_with_engine` / `resolve_mode_params` pair.
struct Triage {
    mesh_type: String,
    confidence: f64,
    engine: String,
    min_component: i64,
    max_hole: i64,
    tuning_applied: bool,
}

fn policy_number(policy: Option<&LoadedPolicy>, section: &str, key: &str, default: f64) -> f64 {
    let node = policy.and_then(|p| {
        let mut parts = section.split('.');
        let first = parts.next()?;
        let mut node = p.get(first)?;
        for part in parts {
            node = node.get(part)?;
        }
        Some(node)
    });
    node.and_then(|n| n.get(key))
        .and_then(PolicyValue::as_f64)
        .unwrap_or(default)
}

fn tuning_applied_for(mesh_type: &str, confidence: f64, policy: Option<&LoadedPolicy>) -> bool {
    match mesh_type {
        "mechanical" => confidence >= policy_number(policy, "stage1_tuning", "mech_gate", 0.75),
        "organic" => confidence >= policy_number(policy, "stage1_tuning", "org_gate", 0.70),
        _ => false,
    }
}

/// `adopt_guard.policy_for('ftetwild')`: the shared adoption policy plus the
/// fTetWild absolute floor and the improved-topology growth allowance.
fn ftetwild_guard_policy(policy: Option<&LoadedPolicy>) -> common::GuardPolicy {
    let floor = policy_number(policy, "gates.adoption", "ftetwild_abs_floor", 50_000.0) as i64;
    common::GuardPolicy {
        face_ratio: policy_number(policy, "gates.adoption", "face_ratio", 4.0),
        vertex_ratio: policy_number(policy, "gates.adoption", "vertex_ratio", 4.0),
        min_slack: policy_number(policy, "gates.adoption", "min_slack", 256.0) as i64,
        loop_face_slack: policy_number(policy, "gates.adoption", "loop_face_slack", 16.0) as i64,
        loop_vertex_slack: policy_number(policy, "gates.adoption", "loop_vertex_slack", 16.0)
            as i64,
        si_policy: "improved_flag".to_string(),
        si_rel_tol: 0.0,
        si_abs_tol: 0,
        face_abs_floor: floor,
        vertex_abs_floor: floor,
        allow_growth_if_improved: true,
    }
}

/// Resolve the classifier verdict and the Stage-1 thresholds exactly as
/// `pipeline/presets.py` does. The classifier is the merged Rust v2 head with
/// the classic fallback baked in; the policy supplies every threshold table.
fn resolve_triage(
    opts: &RepairOptions,
    policy: Option<&LoadedPolicy>,
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
) -> Triage {
    let tri_usize: Vec<[usize; 3]> = tris
        .iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect();
    let cls = classify_mesh(verts, &tri_usize, "experimental");
    let mesh_type = cls
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let confidence = cls.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);

    let fixed = match opts.mode {
        RepairMode::Low => read_thresholds(policy, "modes", "low").or(Some((8, 200))),
        RepairMode::Medium => read_thresholds(policy, "modes", "medium").or(Some((8, 1000))),
        RepairMode::Aggressive => {
            read_thresholds(policy, "modes", "aggressive").or(Some((12, 3000)))
        }
        RepairMode::Extreme => read_thresholds(policy, "modes", "extreme").or(Some((20, 10_000))),
        RepairMode::Auto => None,
    };
    if let Some((min_component, max_hole)) = fixed {
        return Triage {
            mesh_type,
            confidence,
            engine: "experimental".to_string(),
            min_component,
            max_hole,
            tuning_applied: false,
        };
    }
    if let Some(profile) = &opts.profile {
        let (min_component, max_hole) =
            read_thresholds(policy, "stage1_profiles", profile).unwrap_or((8, 1000));
        return Triage {
            mesh_type,
            confidence,
            engine: "experimental".to_string(),
            min_component,
            max_hole,
            tuning_applied: false,
        };
    }
    let applied = tuning_applied_for(&mesh_type, confidence, policy);
    let key = if applied {
        mesh_type.as_str()
    } else {
        "unknown"
    };
    let (min_component, max_hole) =
        read_thresholds(policy, "stage1_types", key).unwrap_or((8, 1000));
    Triage {
        mesh_type,
        confidence,
        engine: "experimental".to_string(),
        min_component,
        max_hole,
        tuning_applied: applied,
    }
}

fn policy_value_json(value: &PolicyValue) -> Value {
    match value {
        PolicyValue::Bool(b) => Value::Bool(*b),
        PolicyValue::Int(i) => Value::from(*i),
        PolicyValue::Float(f) => Value::from(*f),
        PolicyValue::Str(s) => Value::from(s.clone()),
        PolicyValue::Array(items) => Value::Array(items.iter().map(policy_value_json).collect()),
        PolicyValue::Table(table) => {
            let mut out = Map::new();
            for (key, item) in table {
                out.insert(key.clone(), policy_value_json(item));
            }
            Value::Object(out)
        }
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

fn measure_mesh(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Value {
    let topo = common::topology_summary(verts, tris);
    let holes = boundary_loop_stats(tris).0;
    json!({
        "faces": topo.faces,
        "verts": verts.len(),
        "holes": holes,
        "nm_edges": topo.non_two_manifold_edges,
        "nm_verts": topo.non_two_manifold_vertices,
        "components": topo.components,
        "volume": Value::Null,
        "sha256": Value::Null,
    })
}

/// Stage/tier trace in the same record shape as `sutura_engine.trace`.
struct TraceLog {
    records: Vec<Value>,
    step: u64,
}

impl TraceLog {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            step: 0,
        }
    }

    fn record(
        &mut self,
        stage: &str,
        method: &str,
        vin: &[[f64; 3]],
        tin: &[[i64; 3]],
        vout: &[[f64; 3]],
        tout: &[[i64; 3]],
        gate: Value,
    ) {
        let record = json!({
            "step": self.step,
            "object_id": "main",
            "attempt": Value::Null,
            "stage": stage,
            "method": method,
            "elapsed_seconds": 0.0,
            "input": measure_mesh(vin, tin),
            "output": measure_mesh(vout, tout),
            "sha256": Value::Null,
            "gate": gate,
        });
        self.step += 1;
        self.records.push(record);
    }

    fn into_value(self) -> Value {
        Value::Array(self.records)
    }
}

const SI_EXCISE_BUDGET: [(&str, f64); 4] = [
    ("quick", 0.0),
    ("balanced", 5.0),
    ("thorough", 20.0),
    ("extreme", 60.0),
];

fn si_excise_enabled() -> bool {
    match std::env::var("SUTURA_SI_EXCISE") {
        Ok(raw) => !matches!(
            raw.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

fn si_excise_budget(spec: &IntensitySpec) -> f64 {
    let key = spec.base.as_deref().unwrap_or(spec.name.as_str());
    SI_EXCISE_BUDGET
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, budget)| *budget)
        .unwrap_or(5.0)
}

fn si_excise_guard_mode() -> String {
    match std::env::var("SUTURA_SI_EXCISE_GUARD_MODE") {
        Ok(raw) => {
            let mode = raw.trim().to_lowercase();
            if matches!(mode.as_str(), "full" | "local" | "parity") {
                mode
            } else {
                "local".to_string()
            }
        }
        Err(_) => "local".to_string(),
    }
}

fn baseline_method(deep: DeepRepair) -> (u8, &'static str, &'static str) {
    match deep {
        DeepRepair::Off => (1, "quick_clean", "Quick Clean"),
        DeepRepair::Local => (2, "local_mend", "Local Mend"),
        DeepRepair::Full => (3, "full_mend", "Full Mend"),
    }
}

fn method_used_value(deep: DeepRepair) -> Value {
    let (num, id, name) = baseline_method(deep);
    json!({
        "num": num,
        "id": id,
        "name": name,
        "source": "auto_baseline",
    })
}

fn tier_error(tier: &str, message: impl Into<String>) -> Value {
    json!({
        "tier": tier,
        "ran": false,
        "adopted": false,
        "watertight": false,
        "unavailable": true,
        "reason": message.into(),
    })
}

fn run_graft_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    holes: usize,
    nm: usize,
    intensity: &str,
    spec: &IntensitySpec,
    time_budget: Option<f64>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    if let Some(cap) = spec.graft_max_faces {
        if input_t.len() > cap as usize {
            return (
                cur_v.to_vec(),
                cur_t.to_vec(),
                tier_error(
                    "graft",
                    format!(
                        "input has {} faces, above the Graft cap of {}; the ladder result was kept",
                        input_t.len(),
                        cap
                    ),
                ),
            );
        }
    }
    let opts = ShellWrapOptions {
        intensity: Some(intensity.to_string()),
        time_budget: time_budget.or(Some(spec.graft_timeout)),
        hybrid: true,
        ..ShellWrapOptions::default()
    };
    let produced = match shell_wrap(input_v, input_t, &opts) {
        Ok(out) => out,
        Err(e) => {
            return (
                cur_v.to_vec(),
                cur_t.to_vec(),
                tier_error("graft", format!("shell-wrap producer failed: {e}")),
            )
        }
    };
    let params = json!({
        "max_faces": spec.graft_max_faces,
        "time_budget": time_budget.or(Some(spec.graft_timeout)),
        "stage1_holes": holes,
        "stage1_nm": nm,
        "guard": Value::Null,
    });
    graft_tier::decide(
        cur_v,
        cur_t,
        input_v,
        input_t,
        &produced.verts,
        &produced.tris,
        &produced.report,
        &params,
    )
}

fn run_splint_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    holes: usize,
    nm: usize,
    intensity: &str,
    policy: Option<&LoadedPolicy>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let res = policy_number(policy, "tiers.splint", "res", 128.0) as i64;
    let work = policy
        .and_then(|p| p.get("tiers"))
        .and_then(|t| t.get("splint"))
        .and_then(|s| s.get("work_by_intensity"))
        .and_then(|w| w.get(intensity))
        .and_then(PolicyValue::as_i64)
        .unwrap_or(4_000_000);
    let params = json!({
        "res": res,
        "adaptive": "retry",
        "cap": Value::Null,
        "work_limit": work,
        "stage1_holes": holes,
        "stage1_nm": nm,
        "guard": Value::Null,
    });
    splint::run(cur_v, cur_t, input_v, input_t, &params)
}

fn run_coaptation_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    intensity: &str,
    policy: Option<&LoadedPolicy>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let ulp_k = policy_number(policy, "tiers.coaptation", "seam_ulp_k", 1.0) as i64;
    let work = policy
        .and_then(|p| p.get("tiers"))
        .and_then(|t| t.get("coaptation"))
        .and_then(|s| s.get("work_by_intensity"))
        .and_then(|w| w.get(intensity))
        .and_then(PolicyValue::as_i64)
        .unwrap_or(1_000_000);
    let params = json!({
        "cap": 0,
        "ulp_k": ulp_k,
        "work_limit": work,
        "guard": Value::Null,
    });
    coaptation::run(cur_v, cur_t, input_v, input_t, &params)
}

fn run_cdt_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let params = json!({
        "cap": Value::Null,
        "wall_timeout_ms": 0,
        "max_tets": 250_000,
    });
    cdt::run(cur_v, cur_t, input_v, input_t, &params)
}

fn run_closing_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    mode: ClosingMode,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let (cand_v, cand_t, rep, tier) = match mode {
        ClosingMode::Poisson => {
            let out = poisson_close(input_v, input_t, None);
            (out.verts, out.tris, out.report, "poisson")
        }
        ClosingMode::FlatBack => {
            let out = flat_back_close(input_v, input_t, None);
            (out.verts, out.tris, out.report, "flat_back")
        }
        ClosingMode::Mirror => {
            let out = mirror_close(input_v, input_t, crate::mirror::MIRROR_MIN_SCORE);
            (out.verts, out.tris, out.report, "mirror")
        }
    };
    let params = json!({"tier": tier});
    closing_tier::decide(
        cur_v, cur_t, input_v, input_t, &cand_v, &cand_t, &rep, &params,
    )
}

fn run_proxy_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let out = proxy_template_repair(input_v, input_t, None, 3);
    let params = json!({"tier": "proxy_template"});
    closing_tier::decide(
        cur_v,
        cur_t,
        input_v,
        input_t,
        &out.verts,
        &out.tris,
        &out.report,
        &params,
    )
}

fn tier_adopted(stats: &Map<String, Value>, tier: &str) -> bool {
    stats.get(tier).map(adopted).unwrap_or(false)
}

fn produced_tier(stats: &Map<String, Value>) -> &'static str {
    for tier in [
        "cdt",
        "ftetwild",
        "coaptation",
        "splint",
        "dressing",
        "graft",
    ] {
        let key = if tier == "ftetwild" {
            "experimental_ftetwild"
        } else {
            tier
        };
        if tier_adopted(stats, key) {
            return match tier {
                "cdt" => "cdt",
                "ftetwild" => "ftetwild",
                "coaptation" => "coaptation",
                "splint" => "splint",
                "dressing" => "dressing",
                _ => "graft",
            };
        }
    }
    "stage1"
}

/// `method_health` assembled from the run's tier reports. Compiled producers
/// are `ok`; absent producers are `skip`; a tier that returned an `error` is
/// `fail`. Missing ranking scores are reported as an explicit skip.
fn method_health_value(stats: &Map<String, Value>, unavailable: &Value) -> (Value, Value) {
    let mut availability = Map::new();
    for tier in [
        "tamponade",
        "flap",
        "graft",
        "splint",
        "coaptation",
        "cdt",
        "closing",
        "repeat",
        "wall_thicken",
        "si_excise",
        "ftetwild",
    ] {
        availability.insert(tier.to_string(), json!(["ok", Value::Null]));
    }
    availability.insert("local_remesh".to_string(), json!(["ok", Value::Null]));
    let mut skips: Vec<Value> = Vec::new();
    if let Some(entries) = unavailable.as_object() {
        for tier in [
            "dressing",
            "ftetwild",
            "repeat_builder",
            "wall_builder",
        ] {
            let Some(entry) = entries.get(tier) else {
                continue;
            };
            let reason = entry
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unavailable");
            availability.insert(tier.to_string(), json!(["skip", reason]));
            skips.push(json!({
                "method": tier,
                "stage": "availability",
                "kind": "skipped",
                "message": reason,
            }));
        }
    }
    let mut errors: Vec<Value> = Vec::new();
    for (tier, report) in stats {
        // `ftetwild` mirrors `experimental_ftetwild`; count the canonical key
        // once so an error is not reported twice.
        if tier == "ftetwild" {
            continue;
        }
        if let Some(message) = report.get("error").and_then(Value::as_str) {
            errors.push(json!({
                "method": tier,
                "stage": "tier",
                "kind": "exception",
                "message": message,
            }));
        }
    }
    let health = build_report_value(&Value::Object(availability), &errors, &skips);
    (health, Value::Array(errors))
}

/// Tiers this slice does not execute. Every entry is explicit; none is silent.
///
/// fTetWild is compiled in but needs the standalone binary, so it only appears
/// here (with a reason) when no executable was found.
fn unavailable_tiers(ftetwild_reason: Option<&str>) -> Value {
    let entry = |reason: &str| {
        json!({
            "ran": false,
            "unavailable": true,
            "reason": reason,
        })
    };
    let mut tiers = json!({
        "dressing": entry("Dressing orchestration is not in sutura-core yet; only the sutura-geom coat producer exists"),
        "repeat_builder": entry("the Repeat transplant producer stays in Python; only the Rust adoption gate exists"),
        "wall_builder": entry("the Wall-thicken producer stays in Python; only the Rust adoption gate exists"),
        "cad_likeness": entry("cad_likeness is a Python scoring helper; no sutura-core implementation yet"),
        "ranking_scores": entry("repetition_score, single_side_score and relief_score are not ported; ranking treats them as 0.0"),
        "method_escalation": entry("ranked multi-method escalation is file-level (src/out); sutura-core selects the baseline method only"),
    })
    .as_object()
    .cloned()
    .expect("unavailable tier map");
    if let Some(reason) = ftetwild_reason {
        tiers.insert("ftetwild".to_string(), entry(reason));
    }
    Value::Object(tiers)
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
    let triage = resolve_triage(opts, policy.as_ref(), verts, tris);
    let min_component = triage.min_component;
    let mut max_hole = triage.max_hole;
    let (_, largest_loop) = boundary_loop_stats(tris);
    max_hole = max_hole.max(2 * largest_loop as i64);

    let before = common::topology_summary(verts, tris);
    let holes_before = boundary_loop_stats(tris).0;
    let nm_before = before.non_two_manifold_edges;

    let input_v = verts.to_vec();
    let input_t = tris.to_vec();
    let mut cur_v = verts.to_vec();
    let mut cur_t = tris.to_vec();
    let mut trace = TraceLog::new();
    let mut stats = Map::new();
    stats.insert("timings".into(), json!({}));
    stats.insert("si_mode".into(), Value::from(opts.si_mode.as_str()));
    stats.insert("triage_intensity".into(), Value::from(spec.name.clone()));
    if let Some(base) = &spec.base {
        stats.insert("triage_profile_base".into(), Value::from(base.clone()));
        let mut overrides = Map::new();
        for (key, value) in &spec.overrides {
            overrides.insert(key.clone(), policy_value_json(value));
        }
        stats.insert("triage_overrides".into(), Value::Object(overrides));
    }
    stats.insert(
        "classifier_engine".into(),
        Value::from(triage.engine.clone()),
    );
    stats.insert(
        "detected_type".into(),
        Value::from(triage.mesh_type.clone()),
    );
    stats.insert("detected_confidence".into(), Value::from(triage.confidence));
    stats.insert("repair_mode".into(), Value::from(opts.mode.as_str()));
    if let Some(profile) = &opts.profile {
        stats.insert("repair_profile".into(), Value::from(profile.clone()));
    }
    stats.insert("tuning_applied".into(), Value::Bool(triage.tuning_applied));
    stats.insert("experimental_autorefine".into(), Value::Bool(false));
    stats.insert(
        "experimental_indirect_autorefine".into(),
        Value::Bool(false),
    );
    stats.insert("experimental_join_components".into(), Value::Bool(false));
    stats.insert("experimental_ftetwild".into(), Value::Bool(false));
    stats.insert("ftetwild".into(), Value::Bool(false));
    let ftetwild_bin = crate::ftetwild::find_binary();

    // The merged read-only analysis feeds the ranking and the report. Its
    // three behavioural scores are not ported yet; the missing list is exposed
    // under `unavailable` instead of being treated as a real 0.0 signal.
    let mesh32 = Mesh::new(
        verts
            .iter()
            .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            .collect(),
        tris.iter()
            .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
            .collect(),
    );
    let mesh_analysis: Option<MeshAnalysis> =
        analyze_mesh(&mesh32, &triage.engine, opts.si_mode != SiMode::Off).ok();
    let recommendations = match (policy.as_ref(), mesh_analysis.as_ref()) {
        (Some(loaded), Some(analysis)) => {
            let rank_analysis = RankingAnalysis {
                mesh_type: Some(analysis.mesh_type.clone()),
                type_confidence: analysis.type_confidence,
                boundary_loops: analysis.boundary_loops as f64,
                non_manifold_edges: analysis.non_manifold_edges as f64,
                self_intersections: analysis.self_intersections as f64,
                components: analysis.components as f64,
                largest_loop_ratio: analysis.largest_loop_ratio,
                open_area_ratio: analysis.open_area_ratio,
                ..RankingAnalysis::default()
            };
            let recs = rank_methods(loaded, &rank_analysis, None, None);
            Some(recommendations_json(&recs))
        }
        _ => None,
    };

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
    let s1_in_v = cur_v.clone();
    let s1_in_t = cur_t.clone();
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

    trace.record(
        "stage1_chain",
        "stage1.apply_chain",
        &s1_in_v,
        &s1_in_t,
        &cur_v,
        &cur_t,
        json!({"filters": applied, "skipped": skipped}),
    );

    if after.faces == 0 {
        if opts.mode == RepairMode::Extreme {
            return error(
                "Extreme mode removed all geometry (small connected component below the size threshold); try a less aggressive mode (e.g. Auto)",
            );
        }
        return error("all faces are degenerate; nothing to repair");
    }

    // --- Guarded SI excise + re-cap (Full Mend path) ----------------------
    let tamponade_shell_adopted = stats
        .get("tamponade")
        .and_then(|r| r.get("shell_adopted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if opts.deep_repair == DeepRepair::Full
        && opts.mode != RepairMode::Extreme
        && si_excise_enabled()
        && !tamponade_shell_adopted
    {
        let si_in_v = cur_v.clone();
        let si_in_t = cur_t.clone();
        let params = json!({
            "maxholesize": max_hole,
            "max_rounds": 3,
            "time_budget": si_excise_budget(&spec),
            "face_fraction": 0.30,
            "shape_guard": true,
            "guard_mode": si_excise_guard_mode(),
        });
        let (sv, st, rec) = si_excise::run(&cur_v, &cur_t, &input_v, &input_t, &params);
        let guard_blocked = rec
            .get("shape_guard")
            .and_then(|g| g.get("reason"))
            .and_then(Value::as_str)
            .map(|r| r == "shape_guard")
            .unwrap_or(false);
        let si_applied = rec.get("applied").and_then(Value::as_bool).unwrap_or(false);
        if si_applied || guard_blocked {
            cur_v = sv;
            cur_t = st;
            after = common::topology_summary(&cur_v, &cur_t);
        }
        if si_applied || guard_blocked {
            stats.insert("si_excise".into(), rec.clone());
        }
        emit_tier(sink, "si_excise", 3, &rec);
        trace.record(
            "si_excise",
            "stage1.si_excise_recap",
            &si_in_v,
            &si_in_t,
            &cur_v,
            &cur_t,
            rec,
        );
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

    // --- Registry methods 8/9/10: closing / proxy-template / mirror -------
    // They run on the ORIGINAL input and replace the current result only with
    // a strict-watertight candidate.
    if opts.closing.is_some() || opts.proxy_template {
        let cl_in_v = cur_v.clone();
        let cl_in_t = cur_t.clone();
        let (cv, ct, rec) = if opts.proxy_template {
            run_proxy_tier(&cur_v, &cur_t, &input_v, &input_t)
        } else {
            run_closing_tier(
                &cur_v,
                &cur_t,
                &input_v,
                &input_t,
                opts.closing.expect("closing mode present"),
            )
        };
        if adopted(&rec) {
            cur_v = cv;
            cur_t = ct;
            after = common::topology_summary(&cur_v, &cur_t);
        }
        emit_tier(sink, "closing", 3, &rec);
        trace.record(
            "closing_ladder",
            "tiers.closing.closing_ladder",
            &cl_in_v,
            &cl_in_t,
            &cur_v,
            &cur_t,
            rec.clone(),
        );
        stats.insert("closing".into(), rec);
    }

    // --- Deep-repair ladder ------------------------------------------------
    let ladder_holes_before = boundary_loop_stats(&cur_t).0;
    let ladder_nm_before = after.non_two_manifold_edges;
    let mut tiers_run: Vec<Value> = Vec::new();
    if opts.deep_repair != DeepRepair::Off {
        emit_stage(sink, 4, "deep repair");
        if opts.deep_repair == DeepRepair::Full {
            let mut holes = ladder_holes_before;
            let mut nm = ladder_nm_before;

            // Graft (#13): shell wrap, only when Stage 1 still leaves damage.
            let mut graft_ok = false;
            if opts.graft != Some(false) && (holes > 0 || nm > 0) {
                let g_in_v = cur_v.clone();
                let g_in_t = cur_t.clone();
                let (gv, gt, rec) = run_graft_tier(
                    &cur_v,
                    &cur_t,
                    &input_v,
                    &input_t,
                    holes,
                    nm,
                    &intensity_for_tamponade,
                    &spec,
                    opts.time_budget_s,
                );
                if adopted(&rec) {
                    cur_v = gv;
                    cur_t = gt;
                    after = common::topology_summary(&cur_v, &cur_t);
                    holes = boundary_loop_stats(&cur_t).0;
                    nm = after.non_two_manifold_edges;
                    graft_ok = true;
                    tiers_run.push(Value::from("graft"));
                }
                emit_tier(sink, "graft", 4, &rec);
                trace.record(
                    "graft",
                    "tiers.graft.graft_tier",
                    &g_in_v,
                    &g_in_t,
                    &cur_v,
                    &cur_t,
                    rec.clone(),
                );
                stats.insert("graft".into(), rec);
            }

            // Dressing (#16): producer absent in sutura-core.
            let mut dressing_ok = false;
            if opts.dressing {
                let rec = tier_error(
                    "dressing",
                    "Dressing orchestration is not in sutura-core; the ladder result was kept",
                );
                emit_tier(sink, "dressing", 4, &rec);
                trace.record(
                    "dressing",
                    "tiers.dressing.dressing_tier",
                    &cur_v,
                    &cur_t,
                    &cur_v,
                    &cur_t,
                    rec.clone(),
                );
                stats.insert("dressing".into(), rec);
                dressing_ok = false;
            }

            // Splint (#18): exact solid rebuild after Graft/Dressing.
            let mut splint_ok = false;
            if opts.splint && !dressing_ok && !graft_ok && (holes > 0 || nm > 0) {
                let s_in_v = cur_v.clone();
                let s_in_t = cur_t.clone();
                let (sv, st, rec) = run_splint_tier(
                    &cur_v,
                    &cur_t,
                    &input_v,
                    &input_t,
                    holes,
                    nm,
                    &intensity_for_tamponade,
                    policy.as_ref(),
                );
                if adopted(&rec) {
                    cur_v = sv;
                    cur_t = st;
                    after = common::topology_summary(&cur_v, &cur_t);
                    holes = boundary_loop_stats(&cur_t).0;
                    nm = after.non_two_manifold_edges;
                    splint_ok = true;
                    tiers_run.push(Value::from("splint"));
                }
                emit_tier(sink, "splint", 4, &rec);
                trace.record(
                    "splint",
                    "tiers.splint.splint_tier",
                    &s_in_v,
                    &s_in_t,
                    &cur_v,
                    &cur_t,
                    rec.clone(),
                );
                stats.insert("splint".into(), rec);
            }

            // Coaptation (#19): multi-body seam union, not gated on holes.
            let mut coaptation_ok = false;
            if opts.coaptation == Some(true) && !dressing_ok && !graft_ok && !splint_ok {
                let c_in_v = cur_v.clone();
                let c_in_t = cur_t.clone();
                let (cv, ct, rec) = run_coaptation_tier(
                    &cur_v,
                    &cur_t,
                    &input_v,
                    &input_t,
                    &intensity_for_tamponade,
                    policy.as_ref(),
                );
                if adopted(&rec) {
                    cur_v = cv;
                    cur_t = ct;
                    after = common::topology_summary(&cur_v, &cur_t);
                    holes = boundary_loop_stats(&cur_t).0;
                    nm = after.non_two_manifold_edges;
                    coaptation_ok = true;
                    tiers_run.push(Value::from("coaptation"));
                }
                emit_tier(sink, "coaptation", 4, &rec);
                trace.record(
                    "coaptation",
                    "tiers.coaptation.coaptation_tier",
                    &c_in_v,
                    &c_in_t,
                    &cur_v,
                    &cur_t,
                    rec.clone(),
                );
                stats.insert("coaptation".into(), rec);
            }

            // CDT last-resort solid engine.
            let mut cdt_ok = false;
            if opts.solid_engine == SolidEngine::Cdt
                && !dressing_ok
                && !splint_ok
                && !coaptation_ok
                && !graft_ok
                && (holes > 0 || nm > 0)
            {
                let c_in_v = cur_v.clone();
                let c_in_t = cur_t.clone();
                let (cv, ct, rec) = run_cdt_tier(&cur_v, &cur_t, &input_v, &input_t);
                if adopted(&rec) {
                    cur_v = cv;
                    cur_t = ct;
                    after = common::topology_summary(&cur_v, &cur_t);
                    cdt_ok = true;
                    tiers_run.push(Value::from("cdt"));
                }
                emit_tier(sink, "cdt", 4, &rec);
                trace.record(
                    "cdt",
                    "tiers.cdt.cdt_tier",
                    &c_in_v,
                    &c_in_t,
                    &cur_v,
                    &cur_t,
                    rec.clone(),
                );
                stats.insert("cdt".into(), rec);
            }

            // fTetWild (#7): standalone deterministic binary when installed.
            let ft_trigger =
                crate::ftetwild::resolve_trigger(opts.ftetwild.as_ref(), spec.ftetwild_enabled);
            if ft_trigger != crate::ftetwild::Trigger::Disabled
                && !graft_ok
                && !splint_ok
                && !coaptation_ok
                && !cdt_ok
                && ftetwild_bin.is_some()
            {
                let ft_in_v = cur_v.clone();
                let ft_in_t = cur_t.clone();
                let bin = ftetwild_bin.as_deref().expect("checked above");
                let tuning = crate::ftetwild::TierTuning {
                    max_hausdorff_rel: policy_number(
                        policy.as_ref(),
                        "gates.tuning",
                        "ftetwild_max_hausdorff_rel",
                        0.01,
                    ),
                    decimate_hd_margin: policy_number(
                        policy.as_ref(),
                        "gates.tuning",
                        "decimate_hd_margin",
                        0.005,
                    ),
                    guard: ftetwild_guard_policy(policy.as_ref()),
                };
                if let Some(result) = crate::ftetwild::run_tier(
                    &cur_v,
                    &cur_t,
                    &input_v,
                    &input_t,
                    &after,
                    bin,
                    &spec,
                    &tuning,
                    ft_trigger == crate::ftetwild::Trigger::Always,
                ) {
                    if result.adopted {
                        cur_v = result.verts;
                        cur_t = result.tris;
                        after = result.after;
                        tiers_run.push(Value::from("ftetwild"));
                    }
                    if result.shape_changed {
                        stats.insert("shape_changed".into(), Value::Bool(true));
                    }
                    emit_tier(sink, "ftetwild", 4, &result.report);
                    trace.record(
                        "ftetwild",
                        "pipeline._ftetwild_tier",
                        &ft_in_v,
                        &ft_in_t,
                        &cur_v,
                        &cur_t,
                        result.report.clone(),
                    );
                    stats.insert("experimental_ftetwild".into(), result.report.clone());
                    stats.insert("ftetwild".into(), result.report);
                }
            }
        }

        // Local re-mesh (#P1a): Python runs this tier only in `local` mode.
        if opts.deep_repair == DeepRepair::Local
            && (ladder_holes_before > 0 || ladder_nm_before > 0)
        {
            let l_in_v = quantize(&cur_v);
            let l_in_t = cur_t.clone();
            let (lv, lt, _l_after, rec) = local_remesh::local_remesh(&l_in_v, &cur_t, &after);
            cur_v = lv;
            cur_t = lt;
            emit_tier(sink, "local_remesh", 4, &rec);
            trace.record(
                "local_remesh",
                "tiers.local_remesh.local_remesh_tier",
                &l_in_v,
                &l_in_t,
                &cur_v,
                &cur_t,
                rec.clone(),
            );
            stats.insert("local_remesh".into(), rec);
            tiers_run.push(Value::from("local"));
        }
    }

    // --- Repeated-element transplant and wall thicken (producers absent) --
    if let Some(mode) = opts.repeat {
        let rec = tier_error(
            "repeat",
            "the Repeat transplant producer stays in Python; the ladder result was kept",
        );
        emit_tier(sink, "repeat", 4, &rec);
        trace.record(
            "repeat",
            "tiers.repeat.repeat_tier",
            &cur_v,
            &cur_t,
            &cur_v,
            &cur_t,
            rec.clone(),
        );
        let mut rec = rec;
        rec["mode"] = Value::from(match mode {
            RepeatMode::Manual => "manual",
            RepeatMode::Auto => "auto",
        });
        stats.insert("repeat".into(), rec);
    }
    if opts.wall_thicken {
        let rec = tier_error(
            "wall_thicken",
            "the Wall-thicken producer stays in Python; the ladder result was kept",
        );
        emit_tier(sink, "wall_thicken", 4, &rec);
        trace.record(
            "wall_thicken",
            "tiers.wall.wall_thicken_tier",
            &cur_v,
            &cur_t,
            &cur_v,
            &cur_t,
            rec.clone(),
        );
        stats.insert("wall_thicken".into(), rec);
    }

    let produced = produced_tier(&stats);
    stats.insert(
        "solid_engine".into(),
        json!({
            "selected": opts.solid_engine.as_str(),
            "produced": produced,
            "reason": stats
                .get("cdt")
                .and_then(|r| r.get("reason"))
                .cloned()
                .unwrap_or(Value::Null),
        }),
    );
    let mut deep = Map::new();
    deep.insert("mode".into(), Value::from(opts.deep_repair.as_str()));
    deep.insert("si_mode".into(), Value::from(opts.si_mode.as_str()));
    deep.insert("holes_before".into(), Value::from(ladder_holes_before));
    deep.insert("nm_before".into(), Value::from(ladder_nm_before));
    deep.insert("tiers_run".into(), Value::Array(tiers_run));
    deep.insert(
        "local".into(),
        stats.get("local_remesh").cloned().unwrap_or(Value::Null),
    );
    deep.insert(
        "ftetwild".into(),
        match stats.get("experimental_ftetwild") {
            Some(Value::Bool(false)) | None => Value::Null,
            Some(v) => v.clone(),
        },
    );
    deep.insert(
        "graft".into(),
        stats.get("graft").cloned().unwrap_or(Value::Null),
    );
    deep.insert(
        "dressing".into(),
        stats.get("dressing").cloned().unwrap_or(Value::Null),
    );
    deep.insert(
        "splint".into(),
        stats.get("splint").cloned().unwrap_or(Value::Null),
    );
    deep.insert(
        "cdt".into(),
        stats.get("cdt").cloned().unwrap_or(Value::Null),
    );
    deep.insert(
        "solid_engine".into(),
        stats.get("solid_engine").cloned().unwrap_or(Value::Null),
    );
    deep.insert(
        "coaptation".into(),
        stats.get("coaptation").cloned().unwrap_or(Value::Null),
    );
    deep.insert("cad_likeness".into(), Value::Null);
    deep.insert("dressing_damage".into(), Value::Null);
    deep.insert("available".into(), Value::Null);
    stats.insert("deep_repair".into(), Value::Object(deep));

    // Pinch split before Stage 2, matching the pipeline's P1a split.
    let pinch_in_v = cur_v.clone();
    let pinch_in_t = cur_t.clone();
    let pinch = split_pinched_vertices(&cur_v, &cur_t);
    cur_v = to_f64(&pinch.verts);
    cur_t = to_i64(&pinch.tris);
    after = common::topology_summary(&cur_v, &cur_t);
    let pinch_report = pinch.report.unwrap_or(Value::Bool(false));
    trace.record(
        "pinch_split",
        "stage2._split_pinched_vertices",
        &pinch_in_v,
        &pinch_in_t,
        &cur_v,
        &cur_t,
        json!({"report": pinch_report}),
    );
    stats.insert("pinched_vertices_split".into(), pinch_report);

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
    emit_stage(sink, 5, "stage 2");
    if two_manifold && holes_after == 0 {
        let s2_in_v = cur_v.clone();
        let s2_in_t = cur_t.clone();
        let t32 = to_i32(&cur_t);
        let (s2_v, s2_t, s2_rep) = stage2_rebuild_core(&cur_v, &t32);
        if s2_rep.ok && !s2_t.is_empty() {
            cur_v = to_f64(&s2_v);
            cur_t = to_i64(&s2_t);
        }
        let s2_json = stage2_report_json(&s2_rep);
        trace.record(
            "stage2",
            "stage2.maybe_run_stage2",
            &s2_in_v,
            &s2_in_t,
            &cur_v,
            &cur_t,
            s2_json.clone(),
        );
        stats.insert("stage2".into(), s2_json);
    } else {
        trace.record(
            "stage2",
            "stage2.maybe_run_stage2",
            &cur_v,
            &cur_t,
            &cur_v,
            &cur_t,
            json!({"status": "skipped"}),
        );
    }

    // --- P-WELD ------------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 6, "reload weld");
    let pw_in_v = cur_v.clone();
    let pw_in_t = cur_t.clone();
    let pw = p_weld_final(&cur_v, &cur_t);
    cur_v = to_f64(&pw.verts);
    cur_t = to_i64(&pw.tris);
    let pw_gate = pw
        .report
        .clone()
        .unwrap_or_else(|| json!({"status": "noop"}));
    trace.record(
        "p_weld",
        "p_weld.p_weld_final",
        &pw_in_v,
        &pw_in_t,
        &cur_v,
        &cur_t,
        pw_gate.clone(),
    );
    if let Some(rec) = pw.report {
        stats.insert("p_weld".into(), rec);
    }

    cur_v = quantize(&cur_v);

    let final_holes = boundary_loop_stats(&cur_t).0;
    let final_nm = common::topology_summary(&cur_v, &cur_t).non_two_manifold_edges;
    let reachable = common::is_two_manifold(cur_v.len(), &cur_t) && final_holes == 0;
    stats.insert("method_reached_watertight".into(), Value::Bool(reachable));
    stats.insert(
        "timings".into(),
        json!({
            "prepass": 0.0,
            "stage1": 0.0,
            "deep_repair": 0.0,
            "stage2": 0.0,
            "reload_weld": 0.0,
        }),
    );

    let unavailable = unavailable_tiers(ftetwild_bin.is_none().then_some(
        "fTetWild binary not found; set FTETWILD_BIN/SUTURA_FTETWILD_BIN or install third_party/ftetwild/bin/ftetwild",
    ));
    let (method_health, method_errors) = method_health_value(&stats, &unavailable);
    stats.insert("method_health".into(), method_health);
    stats.insert("method_errors".into(), method_errors);
    stats.insert("trace".into(), trace.into_value());
    if let Some(analysis) = mesh_analysis.as_ref() {
        stats.insert("analysis".into(), analysis.to_json());
    }
    if let Some(recs) = recommendations {
        stats.insert("recommendations".into(), recs);
    }

    // P-HONEST: the verdict describes the mesh actually handed back.
    let mut report = Value::Object(stats);
    enforce_reload_verdict(&mut report, &cur_v, &cur_t, false);

    let mut out = match report {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let (num, id, name) = baseline_method(opts.deep_repair);
    let outcome = if reachable { "accepted" } else { "rejected" };
    let reason = if reachable {
        "strict-watertight".to_string()
    } else {
        format!("holes={final_holes} non-manifold={final_nm}")
    };
    out.insert(
        "methods_tried".into(),
        json!([{
            "num": num,
            "id": id,
            "name": name,
            "outcome": outcome,
            "watertight": reachable,
            "holes": final_holes,
            "non_manifold": final_nm,
            "hausdorff_rel": Value::Null,
            "geom_change_pct": Value::Null,
            "guard_rejected": Value::Null,
            "guard_reason": Value::Null,
            "si_flagged": false,
            "adopt_guard": Value::Null,
            "reason": reason,
            "template": Value::Null,
            "elapsed_ms": 0,
        }]),
    );
    out.insert("method_used".into(), method_used_value(opts.deep_repair));
    out.insert("unavailable".into(), unavailable);

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
        for tier in ["dressing"] {
            let entry = unavailable.get(tier).expect(tier);
            assert_eq!(entry.get("ran").and_then(Value::as_bool), Some(false));
            assert_eq!(
                entry.get("unavailable").and_then(Value::as_bool),
                Some(true)
            );
            assert!(entry.get("reason").and_then(Value::as_str).is_some());
        }
        if crate::ftetwild::find_binary().is_none() {
            let entry = unavailable.get("ftetwild").expect("ftetwild");
            assert_eq!(entry.get("ran").and_then(Value::as_bool), Some(false));
            assert_eq!(
                entry.get("unavailable").and_then(Value::as_bool),
                Some(true)
            );
            assert!(entry.get("reason").and_then(Value::as_str).is_some());
        } else {
            assert!(
                unavailable.get("ftetwild").is_none(),
                "an installed fTetWild binary is compiled in"
            );
        }
        assert!(
            unavailable.get("local_remesh").is_none(),
            "local_remesh is compiled into sutura-core"
        );
        assert_eq!(
            report
                .get("stage1")
                .and_then(|s| s.get("holes_remaining"))
                .and_then(Value::as_i64),
            Some(0)
        );
        assert!(report.get("methods_tried").is_some());
        assert!(report.get("method_used").is_some());
        assert!(
            report.get("stats").is_none(),
            "the report must be Python-shaped without a duplicate stats alias"
        );
    }

    #[test]
    fn auto_triage_on_a_closed_cube_keeps_the_conservative_path() {
        let (verts, tris) = cube();
        let opts = RepairOptions {
            tamponade: Some(false),
            flap: Some(false),
            graft: None,
            coaptation: Some(false),
            deep_repair: DeepRepair::Off,
            ..RepairOptions::default()
        };
        let outcome = repair(&verts, &tris, &opts, &NoopSink, &CancelToken::new());
        let report = match outcome {
            RepairOutcome::Ok { report, .. } => report,
            RepairOutcome::Unavailable { tier, reason } => {
                panic!("unexpected unavailable {tier}: {reason}")
            }
            RepairOutcome::Error { message } => panic!("unexpected error: {message}"),
        };
        assert_eq!(
            report.get("repair_mode").and_then(Value::as_str),
            Some("auto")
        );
        assert_eq!(
            report.get("classifier_engine").and_then(Value::as_str),
            Some("experimental")
        );
        assert!(report
            .get("detected_type")
            .and_then(Value::as_str)
            .is_some());
        assert!(report
            .get("detected_confidence")
            .and_then(Value::as_f64)
            .is_some());
        let mesh_type = report
            .get("detected_type")
            .and_then(Value::as_str)
            .expect("detected_type");
        let confidence = report
            .get("detected_confidence")
            .and_then(Value::as_f64)
            .expect("detected_confidence");
        let gate = match mesh_type {
            "mechanical" => 0.75,
            "organic" => 0.70,
            _ => f64::INFINITY,
        };
        assert_eq!(
            report.get("tuning_applied").and_then(Value::as_bool),
            Some(confidence >= gate),
            "tuning must follow the Python classifier confidence gate for {mesh_type}"
        );
        assert_eq!(
            report.get("triage_intensity").and_then(Value::as_str),
            Some("balanced")
        );
        assert!(report.get("stage1").and_then(Value::as_object).is_some());
        assert!(report
            .get("deep_repair")
            .and_then(Value::as_object)
            .is_some());
        assert!(report
            .get("solid_engine")
            .and_then(Value::as_object)
            .is_some());
        assert_eq!(
            report
                .get("method_used")
                .and_then(|m| m.get("source"))
                .and_then(Value::as_str),
            Some("auto_baseline")
        );
        assert_eq!(
            report
                .get("method_used")
                .and_then(|m| m.get("num"))
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            report
                .get("methods_tried")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn default_closed_cube_full_ladder_reports_the_python_keys() {
        let (verts, tris) = cube();
        let opts = RepairOptions {
            tamponade: Some(false),
            flap: Some(false),
            ..RepairOptions::default()
        };
        let outcome = repair(&verts, &tris, &opts, &NoopSink, &CancelToken::new());
        let report = match outcome {
            RepairOutcome::Ok { report, .. } => report,
            other => panic!("unexpected outcome: {}", outcome_name(&other)),
        };
        let deep = report
            .get("deep_repair")
            .and_then(Value::as_object)
            .expect("deep_repair object");
        assert_eq!(deep.get("mode").and_then(Value::as_str), Some("full"));
        assert!(deep.get("tiers_run").and_then(Value::as_array).is_some());
        assert!(deep.get("coaptation").is_some());
        assert_eq!(
            report
                .get("method_used")
                .and_then(|m| m.get("num"))
                .and_then(Value::as_u64),
            Some(3)
        );
        assert_eq!(
            report
                .get("solid_engine")
                .and_then(|s| s.get("selected"))
                .and_then(Value::as_str),
            Some("ftetwild")
        );
        assert!(report.get("trace").and_then(Value::as_array).is_some());
        assert!(report
            .get("method_health")
            .and_then(Value::as_object)
            .is_some());
    }

    #[test]
    fn unavailable_list_is_explicit_and_never_silent() {
        let (verts, tris) = cube();
        let opts = RepairOptions {
            tamponade: Some(false),
            flap: Some(false),
            coaptation: Some(false),
            deep_repair: DeepRepair::Off,
            ..RepairOptions::default()
        };
        let outcome = repair(&verts, &tris, &opts, &NoopSink, &CancelToken::new());
        let report = match outcome {
            RepairOutcome::Ok { report, .. } => report,
            other => panic!("unexpected outcome: {}", outcome_name(&other)),
        };
        let unavailable = report
            .get("unavailable")
            .and_then(Value::as_object)
            .expect("unavailable object");
        for tier in [
            "dressing",
            "repeat_builder",
            "wall_builder",
            "ranking_scores",
        ] {
            let entry = unavailable.get(tier).unwrap_or_else(|| panic!("{tier}"));
            assert_eq!(entry.get("ran").and_then(Value::as_bool), Some(false));
            assert_eq!(
                entry.get("unavailable").and_then(Value::as_bool),
                Some(true)
            );
            assert!(
                entry
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(|r| !r.is_empty())
                    .unwrap_or(false),
                "{tier} needs a non-empty reason"
            );
        }
        if crate::ftetwild::find_binary().is_none() {
            let entry = unavailable.get("ftetwild").expect("ftetwild");
            assert_eq!(entry.get("ran").and_then(Value::as_bool), Some(false));
            assert_eq!(
                entry.get("unavailable").and_then(Value::as_bool),
                Some(true)
            );
            assert!(
                entry
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(|r| !r.is_empty())
                    .unwrap_or(false),
                "ftetwild needs a non-empty reason"
            );
        } else {
            assert!(unavailable.get("ftetwild").is_none());
        }
    }

    #[test]
    fn trace_and_method_health_are_present() {
        let (verts, tris) = cube();
        let opts = RepairOptions {
            tamponade: Some(false),
            flap: Some(false),
            coaptation: Some(false),
            deep_repair: DeepRepair::Off,
            ..RepairOptions::default()
        };
        let outcome = repair(&verts, &tris, &opts, &NoopSink, &CancelToken::new());
        let report = match outcome {
            RepairOutcome::Ok { report, .. } => report,
            other => panic!("unexpected outcome: {}", outcome_name(&other)),
        };
        let trace = report
            .get("trace")
            .and_then(Value::as_array)
            .expect("trace array");
        assert!(!trace.is_empty());
        for record in trace {
            assert!(record.get("stage").and_then(Value::as_str).is_some());
            assert!(record.get("method").and_then(Value::as_str).is_some());
            assert!(record.get("gate").is_some());
        }
        let health = report
            .get("method_health")
            .and_then(Value::as_object)
            .expect("method_health object");
        for key in [
            "total", "ok", "failed", "skipped", "methods", "errors", "skips",
        ] {
            assert!(health.contains_key(key), "method_health missing {key}");
        }
        assert!(health
            .get("total")
            .and_then(Value::as_u64)
            .map(|n| n > 0)
            .unwrap_or(false));
        assert!(health.get("methods").and_then(Value::as_object).is_some());
    }

    fn outcome_name(outcome: &RepairOutcome) -> &'static str {
        match outcome {
            RepairOutcome::Ok { .. } => "ok",
            RepairOutcome::Unavailable { .. } => "unavailable",
            RepairOutcome::Error { .. } => "error",
        }
    }
}
