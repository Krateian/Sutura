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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sutura_geom::manifold::{outer_shell_union_core, stage2_rebuild_core, Stage2Report};
use sutura_geom::shell::{shell_analyze_and_filter, ShellAnalysisResult, ShellConfig};

use crate::analysis::{analyze_mesh, Analysis as MeshAnalysis};
use crate::classify::classify_mesh_with;
use crate::closing::{flat_back_close_with_cancel, poisson_close_with_cancel};
use crate::config::intensity::{resolve_intensity, IntensitySpec};
use crate::config::policy::{load_policy, LoadedPolicy, PolicyValue};
use crate::graft::shell::{shell_wrap_with_cancel, ShellWrapOptions};
use crate::local_remesh;
use crate::mesh::Mesh;
use crate::mirror::mirror_close;
use crate::p_weld::{enforce_reload_verdict, p_weld_final, reload_strict_holes_nm};
use crate::proxy::proxy_template_repair;
use crate::ranking::{
    order_graft_before_ftetwild, prune_avoid_nums, rank_methods, recommendations_json,
    Analysis as RankingAnalysis, Recommendation,
};
use crate::report::defects::detect_defects;
use crate::report::health::build_report_value;
use crate::stage1::chain::{
    apply_chain_with_cancel, cleanup_chain, delete_fallback_chain,
    extreme_extra_passes, stage1_chain,
};
use crate::stage1::si_excise;
use crate::stage2::split_pinched_vertices;
use crate::tiers::closing as closing_tier;
use crate::dressing as dressing_tier;
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
    pub dressing_drain: Option<Value>,
    pub dressing_defects: Option<String>,
    pub dressing_rmax_scale: Option<f64>,
    pub dressing_sigma_scale: Option<f64>,
    pub dressing_force_adopt: bool,
    pub wall_thicken: bool,
    /// `--wall-min-thickness`: the target wall thickness in mm. `None` uses
    /// `wall.DEFAULT_MIN_THICKNESS_REL * bbox_diagonal` (1%), as Python does.
    /// Only meaningful when `wall_thicken` is on.
    pub wall_min_thickness: Option<f64>,
    pub repeat: Option<RepeatMode>,
    /// Method 12 (`RepeatMode::Manual`): the two 3D points the transplant
    /// moves between. `--repeat-source` / `--repeat-target`; required together
    /// when method 12 is selected, as in the Python CLI.
    pub repeat_source: Option<[f64; 3]>,
    pub repeat_target: Option<[f64; 3]>,
    pub solid_engine: SolidEngine,
    pub si_mode: SiMode,
    pub ftetwild: Option<Value>,
    pub time_budget_s: Option<f64>,
    /// Opt-in Stage-1 `meshing_join_components` pass (Python
    /// `--experimental-join-components`).
    #[serde(default)]
    pub join_components: bool,
    /// Per-run overrides for the intensity spec (None keeps the policy value).
    #[serde(default)]
    pub ftetwild_max_faces: Option<i64>,
    #[serde(default)]
    pub ftetwild_timeout: Option<f64>,
    #[serde(default)]
    pub graft_max_faces: Option<i64>,
    #[serde(default)]
    pub graft_timeout: Option<f64>,
    #[serde(default)]
    pub dense_ratio: Option<i64>,
    #[serde(default)]
    pub dense_min_faces: Option<i64>,
    #[serde(default)]
    pub ftetwild_max_hausdorff_rel: Option<f64>,
    #[serde(default)]
    pub ftetwild_optimize_retry: Option<bool>,
    pub seed: u64,
    #[serde(default)]
    pub forced_methods: Vec<u8>,
    /// `--engines`: tag only these configured external engines.
    #[serde(default)]
    pub engines: Option<Vec<String>>,
    /// An explicit `--engines` selection keeps Python's non-escalated
    /// baseline.  When false (and no method is forced), the Rust service skips
    /// the auto deep-repair ladder so it does not force Graft/fTetWild.
    #[serde(default)]
    pub auto_escalation: Option<bool>,
    /// Opt-in `--experimental-edge-tiebreak` classifier head.
    #[serde(default)]
    pub extra_features: bool,
    /// Opt-in `--experimental-autorefine` snap-rounding pass.
    #[serde(default)]
    pub autorefine: bool,
    /// Opt-in `--experimental-indirect-autorefine` exact arrangement split.
    #[serde(default)]
    pub indirect_autorefine: bool,
    /// Opt-in `--auto-reorder`: reorder the ranked fallback by the suggestion
    /// order before the first attempt.
    #[serde(default)]
    pub auto_reorder: bool,
    /// Opt-in `--ftetwild-optimize`: run the fTetWild optimizing attempt first
    /// (Python `ftetwild_params()`), including when the SI trigger forces it.
    #[serde(default)]
    pub ftetwild_optimize: bool,
    /// `--classifier-engine classic|experimental`; the policy default is
    /// experimental, matching Python.
    #[serde(default)]
    pub classifier_engine: Option<String>,
    /// Internal guard: depth of a nested auto-ladder attempt.  Not part of the
    /// Python-facing option JSON.
    #[serde(skip)]
    pub ladder_depth: u8,
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
            dressing_drain: None,
            dressing_defects: None,
            dressing_rmax_scale: None,
            dressing_sigma_scale: None,
            dressing_force_adopt: false,
            wall_thicken: false,
            wall_min_thickness: None,
            repeat: None,
            repeat_source: None,
            repeat_target: None,
            solid_engine: SolidEngine::Ftetwild,
            si_mode: SiMode::Report,
            ftetwild: None,
            time_budget_s: None,
            join_components: false,
            ftetwild_max_faces: None,
            ftetwild_timeout: None,
            graft_max_faces: None,
            graft_timeout: None,
            dense_ratio: None,
            dense_min_faces: None,
            ftetwild_max_hausdorff_rel: None,
            ftetwild_optimize_retry: None,
            seed: 0,
            forced_methods: Vec::new(),
            engines: None,
            auto_escalation: None,
            extra_features: false,
            autorefine: false,
            indirect_autorefine: false,
            auto_reorder: false,
            ftetwild_optimize: false,
            classifier_engine: None,
            ladder_depth: 0,
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
        sutura_geom::cancel::request();
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

impl sutura_geom::cancel::Cancel for CancelToken {
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
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

fn to_f32(v: &[[f64; 3]]) -> Vec<[f32; 3]> {
    v.iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
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

/// Port of `mesh_io.should_extract_outer_shell`.
///
/// True only for a mechanical (or heavy-SI, `si_count >= 200`) mesh whose
/// face-connected components are mostly closed and at least two component
/// bounding boxes overlap or touch. Components are joined only by edges used
/// by exactly two faces, matching the Python `face_adjacency` rule.
fn should_extract_outer_shell(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    mesh_type: &str,
    si_count: usize,
) -> bool {
    let is_mech = mesh_type == "mechanical";
    let is_heavy_si = si_count >= 200;
    if !(is_mech || is_heavy_si) {
        return false;
    }
    let n_faces = tris.len();
    if n_faces == 0 || verts.is_empty() {
        return false;
    }

    let mut edge_faces: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if a < 0 || b < 0 || a as usize >= verts.len() || b as usize >= verts.len() {
                return false;
            }
            let key = if a <= b { (a, b) } else { (b, a) };
            edge_faces.entry(key).or_default().push(fi);
        }
    }

    let mut parent: Vec<usize> = (0..n_faces).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        let mut root = x;
        while p[root] != root {
            root = p[root];
        }
        while p[x] != root {
            let next = p[x];
            p[x] = root;
            x = next;
        }
        root
    }
    for faces in edge_faces.values() {
        if faces.len() == 2 {
            let a = find(&mut parent, faces[0]);
            let b = find(&mut parent, faces[1]);
            if a != b {
                parent[b] = a;
            }
        }
    }

    let mut label_of_root: HashMap<usize, usize> = HashMap::new();
    let mut comp_of_face = vec![0usize; n_faces];
    for (fi, slot) in comp_of_face.iter_mut().enumerate() {
        let root = find(&mut parent, fi);
        let next = label_of_root.len();
        let label = *label_of_root.entry(root).or_insert(next);
        *slot = label;
    }
    let n_comp = label_of_root.len();
    if n_comp <= 1 {
        return false;
    }

    let mut comp_faces = vec![0usize; n_comp];
    for &c in &comp_of_face {
        comp_faces[c] += 1;
    }

    let mut comp_edge_counts: HashMap<(usize, (i64, i64)), usize> = HashMap::new();
    for (fi, t) in tris.iter().enumerate() {
        let c = comp_of_face[fi];
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let key = if a <= b { (a, b) } else { (b, a) };
            *comp_edge_counts.entry((c, key)).or_insert(0) += 1;
        }
    }
    let mut boundary_edges = vec![0usize; n_comp];
    let mut not_closed = vec![false; n_comp];
    for (&(c, _), &count) in &comp_edge_counts {
        if count == 1 {
            boundary_edges[c] += 1;
        }
        if count != 2 {
            not_closed[c] = true;
        }
    }
    for c in 0..n_comp {
        if comp_faces[c] >= 4 && boundary_edges[c] >= 3 {
            return false;
        }
    }
    let closed_count = not_closed.iter().filter(|&&closed| !closed).count();
    if (closed_count as f64) < (n_comp as f64) * 0.7 {
        return false;
    }

    let mut lo = vec![[f64::INFINITY; 3]; n_comp];
    let mut hi = vec![[f64::NEG_INFINITY; 3]; n_comp];
    for (fi, t) in tris.iter().enumerate() {
        let c = comp_of_face[fi];
        for &vi in t {
            let p = verts[vi as usize];
            for k in 0..3 {
                if p[k] < lo[c][k] {
                    lo[c][k] = p[k];
                }
                if p[k] > hi[c][k] {
                    hi[c][k] = p[k];
                }
            }
        }
    }

    let mut gmin = [f64::INFINITY; 3];
    let mut gmax = [f64::NEG_INFINITY; 3];
    for p in verts {
        for k in 0..3 {
            if p[k] < gmin[k] {
                gmin[k] = p[k];
            }
            if p[k] > gmax[k] {
                gmax[k] = p[k];
            }
        }
    }
    let diag =
        ((gmax[0] - gmin[0]).powi(2) + (gmax[1] - gmin[1]).powi(2) + (gmax[2] - gmin[2]).powi(2))
            .sqrt();
    let eps = 1e-4 * if diag > 1e-6 { diag } else { 1.0 };
    for i in 0..n_comp {
        for j in (i + 1)..n_comp {
            if hi[i][0].min(hi[j][0]) >= lo[i][0].max(lo[j][0]) - eps
                && hi[i][1].min(hi[j][1]) >= lo[i][1].max(lo[j][1]) - eps
                && hi[i][2].min(hi[j][2]) >= lo[i][2].max(lo[j][2]) - eps
            {
                return true;
            }
        }
    }
    false
}

/// Holes, non-manifold region count and vertex-connected parts, as the Python
/// outer-shell phase gates measure them.
fn shell_topo_counts(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (usize, usize, usize) {
    let holes = boundary_loop_stats(tris).0;
    let v32: Vec<[f32; 3]> = verts
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = tris
        .iter()
        .map(|r| [r[0] as i32, r[1] as i32, r[2] as i32])
        .collect();
    let nm = detect_defects(&v32, &t32, false).non_manifold.len();
    let parts = common::count_vertex_components(verts.len(), tris);
    (holes, nm, parts)
}

fn u32_tris(tris: &[[i64; 3]]) -> Vec<[u32; 3]> {
    tris.iter()
        .map(|r| [r[0] as u32, r[1] as u32, r[2] as u32])
        .collect()
}

fn usize_tris(tris: &[[i64; 3]]) -> Vec<[usize; 3]> {
    tris.iter()
        .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
        .collect()
}

fn i64_from_u32_tris(tris: &[[u32; 3]]) -> Vec<[i64; 3]> {
    tris.iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect()
}

fn i64_from_usize_tris(tris: &[[usize; 3]]) -> Vec<[i64; 3]> {
    tris.iter()
        .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
        .collect()
}

/// Port of the Python Stage-1 outer-shell pre-step.
///
/// The caller must gate this on `mode == auto`; the Tamponade switch is part
/// of the feature (a disabled Tamponade records `outer_shell_skipped`).
fn run_outer_shell_prepass(
    cur_v: &mut Vec<[f64; 3]>,
    cur_t: &mut Vec<[i64; 3]>,
    mesh_type: &str,
    tamponade_enabled: bool,
    stats: &mut Map<String, Value>,
) {
    let t_shell = Instant::now();
    let mut shell_analyze_s = 0.0f64;
    let mut topo_stats_s = 0.0f64;
    if !should_extract_outer_shell(cur_v, cur_t, mesh_type, 0) {
        return;
    }
    if !tamponade_enabled {
        stats.insert(
            "outer_shell_skipped".into(),
            Value::from("tamponade_disabled"),
        );
        return;
    }

    let config = ShellConfig {
        delta: 0.1,
        keep_cavities: true,
        split_intersections: true,
        ..Default::default()
    };
    let ut = u32_tris(cur_t);
    let t_analyze = Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shell_analyze_and_filter(cur_v, &ut, &config)
    }));
    shell_analyze_s = t_analyze.elapsed().as_secs_f64();
    let t_topo = Instant::now();

    let mut shell_adopted = false;
    let mut shell_budget_exceeded = false;
    let shell_phase: Option<String>;
    let shell_reason: Option<String>;
    let sinfo: Option<ShellAnalysisResult> = match outcome {
        Ok((cand_v, cand_t, res)) => {
            let in_counts = shell_topo_counts(cur_v, cur_t);
            shell_budget_exceeded = res.budget_exceeded;
            if res.is_disjoint {
                shell_adopted = true;
                shell_phase = Some("P-a_disjoint".into());
                shell_reason = Some("disjoint_parts_no_overlap".into());
            } else if res.coincident_opp_count > 0
                && !res.has_internal_walls
                && res.internal_walls_count == 0
                && res.unresolved_coincident_count == 0
                && res.reason == "ok"
            {
                let cand_counts = shell_topo_counts(&cand_v, &i64_from_u32_tris(&cand_t));
                if cand_counts.0 <= in_counts.0
                    && cand_counts.1 <= in_counts.1
                    && cand_counts.2 <= in_counts.2
                {
                    shell_adopted = true;
                    shell_phase = Some("P-b_coincident".into());
                    shell_reason = Some("coincident_opposite_dissolved".into());
                    *cur_v = quantize(&cand_v);
                    *cur_t = i64_from_u32_tris(&cand_t);
                } else {
                    shell_phase = Some("P-b_coincident".into());
                    shell_reason = Some("rejected_holes_or_parts_rose".into());
                }
            } else if !res.has_internal_walls && res.reason == "ok" {
                let cand_counts = shell_topo_counts(&cand_v, &i64_from_u32_tris(&cand_t));
                if cand_counts.0 <= in_counts.0
                    && cand_counts.1 <= in_counts.1
                    && cand_counts.2 <= in_counts.2
                {
                    shell_adopted = true;
                    shell_phase = Some("P-c_penetrating".into());
                    shell_reason = Some("penetrating_bodies_clipped_and_stitched".into());
                    *cur_v = quantize(&cand_v);
                    *cur_t = i64_from_u32_tris(&cand_t);
                } else {
                    shell_phase = Some("P-c_penetrating".into());
                    shell_reason = Some("rejected_holes_or_parts_rose".into());
                }
            } else {
                shell_phase = Some(
                    if res.reason == "unresolved_coplanar_overlapping" {
                        "P-c_penetrating"
                    } else {
                        "fallback"
                    }
                    .into(),
                );
                shell_reason = Some(res.reason.clone());
            }
            Some(res)
        }
        Err(_) => {
            shell_phase = Some("error".into());
            shell_reason = Some("shell_analyze_panicked".into());
            None
        }
    };

    topo_stats_s = t_topo.elapsed().as_secs_f64();
    let info = sinfo.as_ref();
    let phase_value = shell_phase.clone().map(Value::from).unwrap_or(Value::Null);
    let reason_value = shell_reason.clone().map(Value::from).unwrap_or(Value::Null);
    stats.insert(
        "tamponade_shell_prepass".into(),
        json!({
            "shell_adopted": shell_adopted,
            "shell_phase": phase_value.clone(),
            "shell_reason": reason_value.clone(),
            "time_s": (t_shell.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
            "shell_analyze_s": (shell_analyze_s * 1000.0).round() / 1000.0,
            "topo_stats_s": (topo_stats_s * 1000.0).round() / 1000.0,
            "outer_count": info.map(|r| r.outer_count).unwrap_or(0),
            "buried_count": info.map(|r| r.buried_count).unwrap_or(0),
            "cavity_count": info.map(|r| r.cavity_count).unwrap_or(0),
            "coincident_opp_count": info.map(|r| r.coincident_opp_count).unwrap_or(0),
            "coincident_same_count": info.map(|r| r.coincident_same_count).unwrap_or(0),
            "internal_walls_count": info.map(|r| r.internal_walls_count).unwrap_or(0),
            "unresolved_coincident_count": info.map(|r| r.unresolved_coincident_count).unwrap_or(0),
            "has_internal_walls": info.map(|r| r.has_internal_walls).unwrap_or(false),
            "is_disjoint": info.map(|r| r.is_disjoint).unwrap_or(false),
            "budget_exceeded": shell_budget_exceeded,
            "max_coplanar_cluster_faces": config.max_coplanar_cluster_faces,
            "max_coplanar_constraints": config.max_coplanar_constraints,
            "max_plane_faces": config.max_plane_faces,
            "max_coplanar_total_faces": config.max_coplanar_total_faces,
        }),
    );
    stats.insert("shell_adopted".into(), Value::Bool(shell_adopted));
    stats.insert("shell_phase".into(), phase_value);
    stats.insert("shell_reason".into(), reason_value);

    if shell_adopted {
        return;
    }

    if shell_budget_exceeded {
        stats.insert(
            "outer_shell_skipped".into(),
            Value::from("shell_budget_exceeded"),
        );
        return;
    }
    let (in_rholes, in_rnm) = reload_strict_holes_nm(cur_v, cur_t);
    if in_rholes == 0 && in_rnm == 0 {
        stats.insert(
            "outer_shell_skipped".into(),
            Value::from("healthy_multipart_input"),
        );
        return;
    }

    let ut = usize_tris(cur_t);
    let union_out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        outer_shell_union_core(cur_v, &ut, None)
    }));
    if let Ok(Some((shell_v, shell_t))) = union_out {
        if !shell_t.is_empty() {
            let shell_t64 = i64_from_usize_tris(&shell_t);
            let (out_rholes, out_rnm) = reload_strict_holes_nm(&shell_v, &shell_t64);
            if out_rholes <= in_rholes && out_rnm <= in_rnm {
                *cur_v = quantize(&shell_v);
                *cur_t = shell_t64;
                stats.insert("outer_shell_extracted".into(), Value::Bool(true));
            } else {
                stats.insert(
                    "outer_shell_rejected".into(),
                    Value::from("topology_regressed"),
                );
            }
        }
    }
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
    let engine = opts
        .classifier_engine
        .as_deref()
        .unwrap_or("experimental")
        .to_string();
    let cls = classify_mesh_with(verts, &tri_usize, &engine, opts.extra_features);
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
            engine,
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
            engine,
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
        engine,
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
    policy: Option<&LoadedPolicy>,
) -> Value {
    let work = tamponade_work(intensity);
    let mut used: Vec<i64> = tris.iter().flat_map(|r| r.iter().copied()).collect();
    used.sort_unstable();
    used.dedup();
    let mut edges: Vec<(i64, i64)> = Vec::new();
    for r in tris {
        for (a, b) in [(r[0], r[1]), (r[1], r[2]), (r[2], r[0])] {
            edges.push(if a <= b { (a, b) } else { (b, a) });
        }
    }
    edges.sort_unstable();
    edges.dedup();
    let chi = used.len() as i64 - edges.len() as i64 + tris.len() as i64;
    // Mirror `topology.mesh_topology`: genus is defined only for a 2-manifold
    // mesh, where g = components - (holes + chi) / 2 across all shells.  The
    // closed-single-shell shortcut (2 - chi)/2 is wrong for multi-part and
    // open meshes and disagreed with the Python report on identical output.
    let summary = common::topology_summary(verts, tris);
    let genus = if summary.non_two_manifold_edges == 0
        && summary.non_two_manifold_vertices == 0
    {
        let holes = boundary_loop_stats(tris).0 as i64;
        summary.components as i64 - (holes + chi) / 2
    } else {
        -1
    };
    let gate = |key: &str, default: f64| -> f64 {
        policy_number(policy, "tiers.tamponade.gates", key, default)
    };
    let guard = json!({
        "face_ratio": policy_number(policy, "gates.adoption", "face_ratio", 4.0),
        "vertex_ratio": policy_number(policy, "gates.adoption", "vertex_ratio", 4.0),
        "min_slack": policy_number(policy, "gates.adoption", "min_slack", 256.0) as i64,
        "loop_face_slack": policy_number(policy, "gates.adoption", "loop_face_slack", 16.0) as i64,
        "loop_vertex_slack": policy_number(policy, "gates.adoption", "loop_vertex_slack", 16.0) as i64,
        "si_policy": "flag",
        "si_rel_tol": 0.0,
        "si_abs_tol": 0,
        "face_abs_floor": 0,
        "vertex_abs_floor": 0,
        "allow_growth_if_improved": false,
    });
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
        "shape_compactness": gate("shape_compactness", 0.10),
        "shape_open_area": gate("shape_open_area", 0.10),
        "shape_thin": gate("shape_thin", 0.30),
        "shape_thickness": gate("shape_effective_thickness", 0.004),
        "shape_depth_cells": gate("shape_depth_cells", 32768.0) as i64,
        "shape_perimeter_ratio": gate("shape_perimeter_ratio", 1.0),
        "shape_solid_area": gate("shape_solid_area", 0.10),
        "shape_winding": gate("shape_winding", 0.90),
        "shape_side_samples": gate("shape_side_samples", 20000.0) as i64,
        "shape_side_eps": gate("shape_side_eps", 1e-4),
        "leak_max_faces": policy_number(policy, "tiers.tamponade", "leak_max_faces", 150000.0) as i64,
        "collapse_bbox_frac": gate("collapse_bbox_frac", 0.5),
        "leak_trigger_frac": gate("leak_trigger_frac", 0.01),
        "guard": guard,
        "genus": genus,
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

fn flap_debris_params(policy: Option<&LoadedPolicy>) -> Value {
    json!({
        "debris_volume_eps": policy_number(
            policy, "tiers.flap.gates", "debris_volume_eps", 1e-3),
        "debris_volume_rel": policy_number(
            policy, "tiers.flap.gates", "debris_volume_rel", 1e-9),
        "debris_flatness": policy_number(
            policy, "tiers.flap.gates", "debris_flatness", 1e-4),
    })
}

fn ensure_tamponade_defaults(rec: &mut Value) {
    if let Some(obj) = rec.as_object_mut() {
        for key in [
            "patch_vertices",
            "trimmed_vertices",
            "reference_used",
            "reference_fallback",
            "stent_used",
            "stent_subplugs",
            "stent_rejected",
        ] {
            obj.entry(key).or_insert_with(|| json!(0));
        }
        let melt = obj
            .get("melt_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        obj.insert("si_checked".into(), json!(melt));
        obj.entry("plug_metrics").or_insert(Value::Null);
    }
}

fn adopted(report: &Value) -> bool {
    report
        .get("adopted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

thread_local! {
    static FTW_NOTICE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

const SI_METHOD_ID: &str = "scaffold";
const SI_METHOD_NAME: &str = "Scaffold";
const DEFAULT_GRAFT_GRID_BUDGET: i64 = 120_000_000;

#[derive(Clone, Debug, Default)]
struct SiMetrics {
    si: Option<i64>,
    holes: Option<i64>,
    nm: Option<i64>,
    parts: Option<i64>,
    dev_pct: Option<f64>,
    vol_pct: Option<f64>,
    si_stage1: Option<i64>,
    volume_mm3: Option<f64>,
    shape_max_mm: Option<f64>,
    shape_p95_mm: Option<f64>,
}

impl SiMetrics {
    fn to_value(&self) -> Value {
        json!({
            "si": self.si,
            "holes": self.holes,
            "nm": self.nm,
            "parts": self.parts,
            "dev_pct": self.dev_pct,
            "vol_pct": self.vol_pct,
            "si_stage1": self.si_stage1,
            "volume_mm3": self.volume_mm3,
            "shape_max_mm": self.shape_max_mm,
            "shape_p95_mm": self.shape_p95_mm,
        })
    }

    fn apply_facts(&mut self, facts: &SiFacts) {
        self.holes = Some(facts.holes);
        self.nm = Some(facts.nm);
        self.parts = Some(facts.parts);
        self.volume_mm3 = facts.volume_mm3;
        self.shape_max_mm = Some(facts.shape_max_mm);
        self.shape_p95_mm = Some(facts.shape_p95_mm);
    }
}

#[derive(Clone, Debug)]
struct SiFacts {
    holes: i64,
    nm: i64,
    parts: i64,
    volume_mm3: Option<f64>,
    shape_max_mm: f64,
    shape_p95_mm: f64,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

fn si_exact(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Option<i64> {
    if tris.is_empty() {
        return Some(0);
    }
    let (_mask, count, skipped) = sutura_geom::self_intersections_core(verts, &usize_tris(tris));
    if skipped || count < 0 {
        None
    } else {
        Some(count)
    }
}

fn percentile_linear(values: &mut [f64], pct: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n == 1 {
        return values[0];
    }
    let rank = (pct / 100.0) * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        values[lo]
    } else {
        let frac = rank - lo as f64;
        values[lo] + frac * (values[hi] - values[lo])
    }
}

fn two_sided_shape(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> Option<(f64, f64)> {
    if in_v.is_empty() || in_t.is_empty() || out_v.is_empty() || out_t.is_empty() {
        return None;
    }
    let o2i = common::closest_points(in_v, in_t, out_v);
    let i2o = common::closest_points(out_v, out_t, in_v);
    if o2i.is_empty() || i2o.is_empty() {
        return None;
    }
    let mut max_d = 0.0f64;
    let mut all = Vec::with_capacity(o2i.len() + i2o.len());
    for (_, d, _) in o2i {
        max_d = max_d.max(d);
        all.push(d);
    }
    for (_, d, _) in i2o {
        max_d = max_d.max(d);
        all.push(d);
    }
    Some((round4(max_d), round4(percentile_linear(&mut all, 95.0))))
}

fn si_facts(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> Option<SiFacts> {
    if out_v.is_empty() || out_t.is_empty() {
        return None;
    }
    let shape = two_sided_shape(in_v, in_t, out_v, out_t)?;
    let defects = detect_defects(&to_f32(out_v), &to_i32(out_t), false);
    let topo = common::topology_summary(out_v, out_t);
    Some(SiFacts {
        holes: defects.holes.len() as i64,
        nm: defects.non_manifold.len() as i64,
        parts: topo.components as i64,
        volume_mm3: Some(round6(common::signed_volume(out_v, out_t))),
        shape_max_mm: shape.0,
        shape_p95_mm: shape.1,
    })
}

fn si_tolerance(in_v: &[[f64; 3]], policy: Option<&LoadedPolicy>) -> Option<f64> {
    if in_v.is_empty() {
        return None;
    }
    let diag = common::bbox_diagonal(in_v);
    if !(diag > 0.0) {
        return None;
    }
    let div = policy_number(policy, "si_escalation", "dist_tol_div", 10_000.0);
    let min = policy_number(policy, "si_escalation", "dist_tol_min_mm", 0.0125);
    let max = policy_number(policy, "si_escalation", "dist_tol_max_mm", 0.05);
    Some((diag / div).max(min).min(max))
}

fn si_method_num(policy: Option<&LoadedPolicy>) -> i64 {
    policy
        .and_then(|p| p.get("si_escalation"))
        .and_then(|t| t.get("method_num"))
        .and_then(PolicyValue::as_i64)
        .unwrap_or(10)
}

fn si_vol_rel_tol(policy: Option<&LoadedPolicy>) -> f64 {
    policy_number(policy, "si_escalation", "vol_rel_tol", 0.005)
}

fn graft_grid_budget() -> Option<i64> {
    let raw = std::env::var("SUTURA_GRAFT_GRID_BUDGET").ok();
    let Some(raw) = raw else {
        return Some(DEFAULT_GRAFT_GRID_BUDGET);
    };
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Some(DEFAULT_GRAFT_GRID_BUDGET);
    }
    if matches!(text.as_str(), "0" | "false" | "no" | "off") {
        return None;
    }
    match text.parse::<i64>() {
        Ok(value) if value > 0 => Some(value),
        Ok(_) => None,
        Err(_) => Some(DEFAULT_GRAFT_GRID_BUDGET),
    }
}

const DEFAULT_FALLBACK_WORK_BUDGET: i64 = 6_000_000;
const PIPELINE_FACE_COST: i64 = 8;

#[allow(dead_code)]
fn resolve_budget_env(key: &str, default: i64) -> Option<i64> {
    let Ok(raw) = std::env::var(key) else {
        return Some(default);
    };
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Some(default);
    }
    if matches!(text.as_str(), "0" | "false" | "no" | "off") {
        return None;
    }
    match text.parse::<i64>() {
        Ok(value) if value > 0 => Some(value),
        Ok(_) => None,
        Err(_) => Some(default),
    }
}

#[allow(dead_code)]
fn fallback_work_budget() -> Option<i64> {
    resolve_budget_env("SUTURA_FALLBACK_BUDGET", DEFAULT_FALLBACK_WORK_BUDGET)
}

#[allow(dead_code)]
fn work_class(method_id: &str) -> &'static str {
    match method_id {
        "quick_clean" | "local_mend" | "join" | "transplant" | "transplant_plus" => "faces",
        "graft" | "dressing" => "grid",
        _ => "damage",
    }
}

#[allow(dead_code)]
fn candidate_work(method_id: &str, damage: i64, grid: i64, faces: i64) -> i64 {
    match work_class(method_id) {
        "grid" if grid > 0 => grid,
        "faces" if faces > 0 => faces * PIPELINE_FACE_COST,
        _ => damage,
    }
}

#[allow(dead_code)]
fn fits_budget(work: i64, budget: Option<i64>) -> bool {
    budget.map(|b| work <= b).unwrap_or(true)
}

#[allow(dead_code)]
fn grid_work_units(faces: i64, cells: i64) -> i64 {
    (faces.max(0) * cells.max(0) + 999) / 1000
}

#[allow(dead_code)]
fn grid_work_with_holes(faces: i64, cells: i64, holes: i64) -> i64 {
    grid_work_units(faces, cells) * (1 + holes.max(0) / 100)
}

/// Python `triage._fallback_grid_work` for one mesh: the morphology grid
/// estimate at the resolved intensity budget, with the hole multiplier.
#[allow(dead_code)]
fn fallback_grid_work(v: &[[f64; 3]], t: &[[i64; 3]], intensity: &str) -> i64 {
    if t.is_empty() {
        return 0;
    }
    let budget = crate::graft::shell::resolve_grid_budget(Some(intensity));
    let voxel = crate::graft::helpers::auto_voxel(v, t, Some(budget));
    let cells = crate::graft::helpers::grid_cell_count(v, voxel);
    let holes = boundary_loop_stats(t).0 as i64;
    grid_work_with_holes(t.len() as i64, cells, holes)
}

#[cfg(test)]
mod work_estimate_tests {
    use super::*;

    #[test]
    fn work_classes_match_python() {
        assert_eq!(work_class("quick_clean"), "faces");
        assert_eq!(work_class("autorefine"), "damage");
        assert_eq!(work_class("graft"), "grid");
        assert_eq!(candidate_work("quick_clean", 7, 0, 10), 80);
        assert_eq!(candidate_work("autorefine", 7, 0, 10), 7);
        assert_eq!(candidate_work("graft", 7, 99, 10), 99);
    }

    #[test]
    fn budget_and_grid_math() {
        assert!(fits_budget(6_000_000, None));
        assert!(fits_budget(6_000_000, Some(6_000_000)));
        assert!(!fits_budget(6_000_001, Some(6_000_000)));
        assert_eq!(grid_work_units(10, 1), 1);
        assert_eq!(grid_work_with_holes(10, 1000, 250), 30);
    }
}

fn input_damage_work(v: &[[f64; 3]], t: &[[i64; 3]]) -> i64 {
    let topo = common::topology_summary(v, t);
    let holes = boundary_loop_stats(t).0 as i64;
    t.len() as i64
        * (1 + topo.non_two_manifold_edges as i64 + topo.non_two_manifold_vertices as i64 + holes)
}

fn volume_change_pct(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> Option<f64> {
    let before = common::signed_volume(in_v, in_t);
    let after = common::signed_volume(out_v, out_t);
    if before == 0.0 || after == 0.0 {
        return None;
    }
    Some(round2((after - before) / before.abs() * 100.0))
}

fn geom_change_pct(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> Option<f64> {
    let before_area = common::surface_area(in_v, in_t);
    let after_area = common::surface_area(out_v, out_t);
    let mut values = Vec::new();
    if let Some(vol) = volume_change_pct(in_v, in_t, out_v, out_t) {
        values.push(vol.abs());
    }
    if before_area != 0.0 && after_area != 0.0 {
        values.push(round2((after_area - before_area) / before_area.abs() * 100.0).abs());
    }
    if values.is_empty() {
        None
    } else {
        Some(values.into_iter().fold(0.0f64, f64::max))
    }
}

fn report_geometry_metrics(
    report: &Value,
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
) -> (Option<f64>, Option<f64>) {
    let it = to_i32(in_t);
    let ot = to_i32(out_t);
    let Some(pr) = crate::report::build_print_report_value(
        report,
        Some(in_v),
        Some(&it),
        Some(out_v),
        Some(&ot),
    ) else {
        return (None, None);
    };
    let geom = pr.get("geometry");
    (
        geom.and_then(|g| g.get("deviation_max_pct"))
            .and_then(Value::as_f64),
        geom.and_then(|g| g.get("volume_change_pct"))
            .and_then(Value::as_f64),
    )
}

fn si_metrics_from_report(report: &Value) -> SiMetrics {
    let s1 = report.get("stage1");
    let int = |key: &str| s1.and_then(|m| m.get(key)).and_then(Value::as_i64);
    SiMetrics {
        si: int("self_intersections_remaining"),
        holes: int("holes_remaining"),
        nm: int("non_manifold_edges_remaining"),
        parts: int("components"),
        si_stage1: None,
        ..Default::default()
    }
}

fn si_verdict(
    base: &SiMetrics,
    cand: &SiMetrics,
    tolerance: Option<f64>,
    vol_rel_tol: f64,
) -> (bool, String) {
    let Some(base_si) = base.si else {
        return (false, "baseline SI unmeasurable".to_string());
    };
    if base_si <= 0 {
        return (false, "baseline SI is zero".to_string());
    }
    let Some(cand_si) = cand.si else {
        return (false, "candidate SI unmeasurable".to_string());
    };
    if cand_si >= base_si {
        return (false, "SI not strictly lower".to_string());
    }
    for (key, label) in [
        ("holes", "holes"),
        ("nm", "non-manifold"),
        ("parts", "parts"),
    ] {
        let b = match key {
            "holes" => base.holes,
            "nm" => base.nm,
            _ => base.parts,
        };
        let c = match key {
            "holes" => cand.holes,
            "nm" => cand.nm,
            _ => cand.parts,
        };
        let Some(b) = b else {
            return (false, format!("baseline {label} unmeasurable"));
        };
        let Some(c) = c else {
            return (false, format!("candidate {label} unmeasurable"));
        };
        if c > b {
            return (false, format!("{label} worse ({b} -> {c})"));
        }
    }
    let Some(tol) = tolerance else {
        return (false, "shape tolerance unmeasurable".to_string());
    };
    for (key, label) in [
        ("shape_max_mm", "two-sided max"),
        ("shape_p95_mm", "two-sided p95"),
    ] {
        let b = match key {
            "shape_max_mm" => base.shape_max_mm,
            _ => base.shape_p95_mm,
        };
        let c = match key {
            "shape_max_mm" => cand.shape_max_mm,
            _ => cand.shape_p95_mm,
        };
        let Some(b) = b else {
            return (false, format!("baseline {label} unmeasurable"));
        };
        let Some(c) = c else {
            return (false, format!("candidate {label} unmeasurable"));
        };
        if c > b + tol {
            return (
                false,
                format!("{label} worse ({b:.4} -> {c:.4} mm > {b:.4} + {tol:.4})"),
            );
        }
    }
    let (Some(bv), Some(cv)) = (base.volume_mm3, cand.volume_mm3) else {
        return (false, "volume unmeasurable".to_string());
    };
    if bv.abs() <= 1e-12 {
        return (false, "baseline volume is zero".to_string());
    }
    let rel = (cv - bv).abs() / bv.abs();
    if rel > vol_rel_tol {
        return (
            false,
            format!(
                "volume change {:.3}% > {:.2}% ({:.3} -> {:.3} mm3)",
                rel * 100.0,
                vol_rel_tol * 100.0,
                bv,
                cv
            ),
        );
    }
    (
        true,
        format!("never-worse under the two-sided SI-escalation rule (tol {tol:.4} mm)"),
    )
}

fn stl_sha256(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Option<String> {
    let mesh = Mesh::new(to_f32(verts), to_i32(tris));
    let bytes = crate::mesh_io::write_stl_binary_bytes(&mesh).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn si_escalation_allowed(opts: &RepairOptions) -> bool {
    opts.si_mode != SiMode::Off
        && opts.deep_repair == DeepRepair::Full
        && !matches!(opts.ftetwild.as_ref(), Some(Value::Bool(false)))
}

fn apply_si_escalation(
    out: &mut Map<String, Value>,
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    base_v: &[[f64; 3]],
    base_t: &[[i64; 3]],
    opts: &RepairOptions,
    policy: Option<&LoadedPolicy>,
    stage1_snapshot: Option<&(Vec<[f64; 3]>, Vec<[i64; 3]>)>,
    cancel: &CancelToken,
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>, Value)> {
    let base_si = si_exact(base_v, base_t)?;
    if base_si <= 0 {
        return None;
    }
    let tolerance = si_tolerance(input_v, policy)?;
    let base_facts = si_facts(input_v, input_t, base_v, base_t)?;
    if base_facts.holes > 0 || base_facts.nm > 0 {
        return None;
    }

    let baseline_entry = out
        .get("methods_tried")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);

    let base_report = Value::Object(out.clone());
    let (base_dev, base_vol) =
        report_geometry_metrics(&base_report, input_v, input_t, base_v, base_t);
    let mut base = si_metrics_from_report(&base_report);
    base.si = Some(base_si);
    base.si_stage1 = stage1_snapshot.and_then(|(v, t)| si_exact(v, t));
    base.dev_pct = base_dev;
    base.vol_pct = base_vol.or_else(|| volume_change_pct(input_v, input_t, base_v, base_t));
    base.apply_facts(&base_facts);

    let method_num = si_method_num(policy);
    let work_budget = graft_grid_budget();
    let work_units = input_damage_work(input_v, input_t);
    let vol_rel_tol = si_vol_rel_tol(policy);

    let mut field = Map::new();
    field.insert("triggered".into(), Value::Bool(true));
    field.insert("method".into(), Value::Null);
    field.insert("num".into(), Value::Null);
    field.insert("name".into(), Value::Null);
    field.insert("candidates".into(), json!([]));
    field.insert("si_source".into(), Value::from("exact_output"));
    field.insert("baseline".into(), base.to_value());
    field.insert("tolerance_mm".into(), json!(tolerance));
    field.insert(
        "reason".into(),
        Value::from("final output watertight with SI > 0"),
    );
    field.insert(
        "work_budget".into(),
        work_budget.map(Value::from).unwrap_or(Value::Null),
    );
    field.insert("budget_scope".into(), Value::from("si_escalation_120m"));

    let mut cand_field = json!({
        "num": method_num,
        "id": Value::Null,
        "name": Value::Null,
        "outcome": Value::Null,
        "reason": Value::Null,
    });

    if method_num != 10 {
        cand_field["reason"] = Value::from("candidate method unavailable");
        push_candidate(&mut field, cand_field);
        finalize_rejected(&mut field, &base);
        out.insert("si_escalation".into(), Value::Object(field));
        return None;
    }
    cand_field["id"] = Value::from(SI_METHOD_ID);
    cand_field["name"] = Value::from(SI_METHOD_NAME);
    cand_field["work_units"] = Value::from(work_units);
    cand_field["work_budget"] = work_budget.map(Value::from).unwrap_or(Value::Null);
    if let Some(budget) = work_budget {
        if work_units > budget {
            cand_field["outcome"] = Value::from("skipped_budget");
            cand_field["reason"] = Value::from(format!(
                "estimated damage work {work_units} (budget {budget}) is above the SI-escalation cap"
            ));
            push_candidate(&mut field, cand_field);
            finalize_rejected(&mut field, &base);
            out.insert("si_escalation".into(), Value::Object(field));
            return None;
        }
    }

    let started = Instant::now();
    let mut cand_opts = opts.clone();
    cand_opts.deep_repair = DeepRepair::Off;
    cand_opts.proxy_template = true;
    cand_opts.ftetwild = Some(Value::Bool(false));
    let candidate = repair_impl(input_v, input_t, &cand_opts, &NoopSink, cancel, false);
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (cand_v, cand_t, cand_report) = match candidate {
        RepairOutcome::Ok {
            verts,
            tris,
            report,
        } => (verts, tris, report),
        RepairOutcome::Unavailable { tier, reason } => {
            cand_field["outcome"] = Value::from("error");
            cand_field["reason"] = Value::from(format!("unavailable tier '{tier}': {reason}"));
            cand_field["elapsed_ms"] = Value::from(elapsed_ms.round() as i64);
            push_candidate(&mut field, cand_field);
            finalize_rejected(&mut field, &base);
            out.insert("si_escalation".into(), Value::Object(field));
            return None;
        }
        RepairOutcome::Error { message } => {
            cand_field["outcome"] = Value::from("error");
            cand_field["reason"] = Value::from(message);
            cand_field["elapsed_ms"] = Value::from(elapsed_ms.round() as i64);
            push_candidate(&mut field, cand_field);
            finalize_rejected(&mut field, &base);
            out.insert("si_escalation".into(), Value::Object(field));
            return None;
        }
    };

    let cand_facts = si_facts(input_v, input_t, &cand_v, &cand_t);
    let cand_si = si_exact(&cand_v, &cand_t);
    let (cand_dev, cand_vol) =
        report_geometry_metrics(&cand_report, input_v, input_t, &cand_v, &cand_t);
    let mut cand = si_metrics_from_report(&cand_report);
    cand.si = cand_si;
    cand.si_stage1 = cand_si;
    cand.dev_pct = cand_dev;
    cand.vol_pct = cand_vol.or_else(|| volume_change_pct(input_v, input_t, &cand_v, &cand_t));
    if let Some(facts) = cand_facts.as_ref() {
        cand.apply_facts(facts);
    }
    let cand_sha = stl_sha256(&cand_v, &cand_t);
    cand_field["candidate"] = cand.to_value();
    cand_field["candidate_sha256"] = cand_sha.map(Value::from).unwrap_or(Value::Null);
    cand_field["elapsed_ms"] = Value::from(elapsed_ms.round() as i64);

    // Evaluate the SI candidate with the same reload-strict holes/nm rule as
    // the Python ladder (`_evaluate`); the raw `method_reached_watertight` flag
    // is pre-reload and rejected an accepted scaffold on broken_40261.
    let (watertight, eval_holes, eval_nm, eval_reason) =
        ladder_eval(&cand_v, &cand_t, &cand_report);
    let geom_change = geom_change_pct(input_v, input_t, &cand_v, &cand_t);
    let tried_entry = json!({
        "num": method_num,
        "id": SI_METHOD_ID,
        "name": SI_METHOD_NAME,
        "outcome": if watertight { "accepted" } else { "rejected" },
        "watertight": watertight,
        "holes": if watertight { 0 } else { eval_holes },
        "non_manifold": if watertight { 0 } else { eval_nm },
        "hausdorff_rel": Value::Null,
        "geom_change_pct": geom_change.map(Value::from).unwrap_or(Value::Null),
        "guard_rejected": Value::Null,
        "guard_reason": Value::Null,
        "si_flagged": false,
        "adopt_guard": Value::Null,
        "reason": eval_reason,
        "template": Value::Null,
        "elapsed_ms": elapsed_ms.round() as i64,
    });

    if !watertight {
        cand_field["outcome"] = Value::from("rejected");
        cand_field["reason"] = Value::from(format!("candidate rejected: {eval_reason}"));
        push_candidate(&mut field, cand_field);
        append_tried(out, &tried_entry);
        finalize_rejected(&mut field, &base);
        out.insert("si_escalation".into(), Value::Object(field));
        return None;
    }

    let (ok, why) = si_verdict(&base, &cand, Some(tolerance), vol_rel_tol);
    if !ok {
        cand_field["outcome"] = Value::from("rejected");
        cand_field["reason"] = Value::from(why.clone());
        push_candidate(&mut field, cand_field);
        append_tried(out, &tried_entry);
        finalize_rejected(&mut field, &base);
        out.insert("si_escalation".into(), Value::Object(field));
        return None;
    }

    cand_field["outcome"] = Value::from("adopted");
    cand_field["reason"] = Value::from(why.clone());
    push_candidate(&mut field, cand_field);
    field.insert("method".into(), Value::from(SI_METHOD_ID));
    field.insert("num".into(), Value::from(method_num));
    field.insert("name".into(), Value::from(SI_METHOD_NAME));
    field.insert("outcome".into(), Value::from("adopted"));
    field.insert("reason".into(), Value::from(why.clone()));
    let field_value = Value::Object(field);
    out.insert("si_escalation".into(), field_value.clone());

    let mut adopted = match cand_report {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    adopted.insert(
        "methods_tried".into(),
        Value::Array(vec![baseline_entry, tried_entry]),
    );
    adopted.insert(
        "method_used".into(),
        json!({
            "num": method_num,
            "id": SI_METHOD_ID,
            "name": SI_METHOD_NAME,
            "source": "auto_si_escalated",
            "note": "SI escalation",
        }),
    );
    adopted.insert("method_reached_watertight".into(), Value::Bool(true));
    adopted.insert("si_escalation".into(), field_value);
    for key in [
        "fallback_work_budget",
        "fallback_grid_work_budget",
        "fallback_work_input",
        "fallback_work_estimate",
        "fallback_skipped_budget",
    ] {
        if !adopted.contains_key(key) {
            if let Some(value) = out.get(key) {
                adopted.insert(key.to_string(), value.clone());
            }
        }
    }
    Some((cand_v, cand_t, Value::Object(adopted)))
}

fn push_candidate(field: &mut Map<String, Value>, cand: Value) {
    if let Some(list) = field.get_mut("candidates").and_then(Value::as_array_mut) {
        list.push(cand);
    }
}

fn append_tried(out: &mut Map<String, Value>, entry: &Value) {
    if let Some(list) = out.get_mut("methods_tried").and_then(Value::as_array_mut) {
        list.push(entry.clone());
    }
}

fn finalize_rejected(field: &mut Map<String, Value>, base: &SiMetrics) {
    let last = field
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .rev()
                .find(|c| c.get("candidate").is_some())
                .or_else(|| list.last())
        })
        .cloned();
    if let Some(last) = last {
        field.insert(
            "method".into(),
            last.get("id").cloned().unwrap_or(Value::Null),
        );
        field.insert(
            "num".into(),
            last.get("num").cloned().unwrap_or(Value::Null),
        );
        field.insert(
            "name".into(),
            last.get("name").cloned().unwrap_or(Value::Null),
        );
        field.insert(
            "reason".into(),
            last.get("reason").cloned().unwrap_or(Value::Null),
        );
        if let Some(candidate) = last.get("candidate") {
            field.insert("candidate".into(), candidate.clone());
        }
        if let Some(sha) = last.get("candidate_sha256") {
            field.insert("candidate_sha256".into(), sha.clone());
        }
        if let Some(ms) = last.get("elapsed_ms") {
            field.insert("elapsed_ms".into(), ms.clone());
        }
    }
    field.insert("outcome".into(), Value::from("rejected"));
    field.insert("baseline".into(), base.to_value());
}

fn stage_fp(v: &[[f64; 3]], t: &[[i64; 3]]) -> u64 {
    let mut h: u64 = 0;
    for p in v {
        for x in p {
            for b in x.to_le_bytes() {
                h = h.wrapping_mul(131).wrapping_add(b as u64);
            }
        }
    }
    for r in t {
        for x in r {
            for b in x.to_le_bytes() {
                h = h.wrapping_mul(131).wrapping_add(b as u64);
            }
        }
    }
    h
}

pub fn stage_fp_log(label: &str, v: &[[f64; 3]], t: &[[i64; 3]]) {
    if std::env::var_os("SUTURA_STAGE_FP").is_some() {
        eprintln!(
            "[stage-fp] {} {} {} {}",
            label,
            stage_fp(v, t),
            t.len(),
            v.len()
        );
    }
}

fn cad_likeness(v: &[[f64; 3]], t: &[[i64; 3]]) -> Value {
    let f = t.len();
    if f == 0 || v.is_empty() {
        return json!({"score": 0.0, "planar_fraction": 0.0,
                      "sharp_fraction": 0.0, "cad_like": false});
    }
    let mut normals: Vec<[f64; 3]> = Vec::with_capacity(f);
    for r in t {
        let a = v[r[0] as usize];
        let b = v[r[1] as usize];
        let c = v[r[2] as usize];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let ln = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let d = if ln == 0.0 { 1.0 } else { ln };
        normals.push([n[0] / d, n[1] / d, n[2] / d]);
    }
    let mut edges: Vec<((i64, i64), usize)> = Vec::with_capacity(f * 3);
    for (fi, r) in t.iter().enumerate() {
        for k in 0..3 {
            let a = r[k];
            let b = r[(k + 1) % 3];
            edges.push(((a.min(b), a.max(b)), fi));
        }
    }
    edges.sort_by_key(|(key, _)| *key);
    let mut angles: Vec<f64> = Vec::new();
    let mut i = 0usize;
    while i < edges.len() {
        let mut j = i + 1;
        while j < edges.len() && edges[j].0 == edges[i].0 {
            j += 1;
        }
        if j - i == 2 {
            let n1 = normals[edges[i].1];
            let n2 = normals[edges[i + 1].1];
            let dot = (n1[0] * n2[0] + n1[1] * n2[1] + n1[2] * n2[2])
                .abs()
                .clamp(0.0, 1.0);
            angles.push(dot.acos().to_degrees());
        }
        i = j;
    }
    let n = angles.len();
    let flat = angles.iter().filter(|a| **a < 1.0).count();
    let sharp = angles.iter().filter(|a| **a > 30.0).count();
    let planar_fraction = if n == 0 { 0.0 } else { flat as f64 / n as f64 };
    let sharp_fraction = if n == 0 { 0.0 } else { sharp as f64 / n as f64 };
    let score = planar_fraction.max(sharp_fraction);
    json!({
        "score": score,
        "planar_fraction": planar_fraction,
        "sharp_fraction": sharp_fraction,
        "cad_like": score >= 0.5,
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
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
    report_stage1_counts: bool,
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
    let produced = match shell_wrap_with_cancel(input_v, input_t, &opts, cancel) {
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
        "stage1_holes": if report_stage1_counts { json!(holes) } else { Value::Null },
        "stage1_nm": if report_stage1_counts { json!(nm) } else { Value::Null },
        "guard": json!({"si_policy": "improved_flag"}),
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

fn run_dressing_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    intensity: &str,
    opts: &RepairOptions,
    policy: Option<&LoadedPolicy>,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let params = json!({
        "intensity": intensity,
        "gates": {
            "max_volume_delta": policy_number(
                policy, "tiers.dressing.gates", "max_volume_delta", 0.10),
            "max_parts_slack": policy_number(
                policy, "tiers.dressing.gates", "max_parts_slack", 1.0) as i64,
            "max_normal_angle_p95": policy_number(
                policy, "tiers.dressing.gates", "max_normal_angle_p95", 30.0),
        },
        "drain": opts.dressing_drain,
        "defects": opts.dressing_defects,
        "r_max_scale": opts.dressing_rmax_scale,
        "sigma_scale": opts.dressing_sigma_scale,
        "force_adopt": opts.dressing_force_adopt,
    });
    dressing_tier::run_with_cancel(
        cur_v, cur_t, input_v, input_t, intensity, &params, cancel)
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
        "guard": json!({"si_policy": "non_rise"}),
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
        "guard": json!({"si_policy": "strict_zero"}),
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
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>, Value) {
    let (cand_v, cand_t, rep, tier) = match mode {
        ClosingMode::Poisson => {
            let out = poisson_close_with_cancel(input_v, input_t, None, cancel);
            (out.verts, out.tris, out.report, "poisson")
        }
        ClosingMode::FlatBack => {
            let out = flat_back_close_with_cancel(input_v, input_t, None, cancel);
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
    _cancel: Option<&dyn sutura_geom::cancel::Cancel>,
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
        "dressing",
    ] {
        availability.insert(tier.to_string(), json!(["ok", Value::Null]));
    }
    availability.insert("local_remesh".to_string(), json!(["ok", Value::Null]));
    let mut skips: Vec<Value> = Vec::new();
    if let Some(entries) = unavailable.as_object() {
        for tier in [
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
        "repeat_builder": entry("unused: the Repeat producer now runs in sutura-core (tiers::repeat::repair_auto_report)"),
        "wall_builder": entry("the Wall-thicken producer stays in Python; only the Rust adoption gate exists"),
        "cad_likeness": entry("cad_likeness is a Python scoring helper; no sutura-core implementation yet"),
        "ranking_scores": entry("unused: repetition_score, single_side_score and relief_score are produced by analysis.rs"),
        "method_escalation_multi": entry("multi-object 3MF/OBJ escalation stays in the Python service; sutura-core runs the ranked ladder for single meshes"),
    })
    .as_object()
    .cloned()
    .expect("unavailable tier map");
    if let Some(reason) = ftetwild_reason {
        tiers.insert("ftetwild".to_string(), entry(reason));
    }
    Value::Object(tiers)
}

/// Re-run the Stage-1 chain on an opt-in pre-processed candidate.
fn stage1_variant(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    max_hole: i64,
    min_component: i64,
    join_components: bool,
    cancel: &CancelToken,
) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let chain = Value::Array(stage1_chain(max_hole, min_component, join_components));
    let (out_v, out_t, _applied, _skipped, _small) = apply_chain_with_cancel(
        verts,
        tris,
        &chain,
        Some(cancel as &dyn sutura_geom::cancel::Cancel),
    );
    (out_v, out_t)
}

/// Python's candidate guard: holes and non-manifold edges no worse than the
/// baseline chain output.
fn candidate_no_worse(
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
    base_v: &[[f64; 3]],
    base_t: &[[i64; 3]],
) -> bool {
    let cand_holes = boundary_loop_stats(cand_t).0;
    let cand_nm = common::topology_summary(cand_v, cand_t).non_two_manifold_edges;
    let base_holes = boundary_loop_stats(base_t).0;
    let base_nm = common::topology_summary(base_v, base_t).non_two_manifold_edges;
    cand_holes <= base_holes && cand_nm <= base_nm
}

/// Map one registry method number to the option overrides Python passes via
/// `method.kwargs`, and mark the attempt as non-escalating.  Returns `None`
/// for methods whose producer still lives only in Python (Repeat #11/#12,
/// Wall #15, Dressing #16, Tamponade #17, Splint #18) or unknown numbers.
pub(crate) fn method_options(base: &RepairOptions, num: u8) -> Option<RepairOptions> {
    if !matches!(num, 1..=10 | 13 | 14 | 19) {
        return None;
    }
    let mut opts = base.clone();
    opts.forced_methods = vec![num];
    opts.auto_escalation = Some(false);
    opts.ladder_depth = base.ladder_depth.saturating_add(1);
    match num {
        1 => {
            opts.deep_repair = DeepRepair::Off;
            opts.ftetwild = Some(Value::Bool(false));
        }
        2 => {
            opts.deep_repair = DeepRepair::Local;
            opts.ftetwild = Some(Value::Bool(false));
        }
        3 => {
            opts.deep_repair = DeepRepair::Full;
            opts.ftetwild = Some(Value::from("auto"));
        }
        4 => opts.join_components = true,
        5 => opts.autorefine = true,
        6 => opts.indirect_autorefine = true,
        7 => {
            opts.deep_repair = DeepRepair::Full;
            opts.ftetwild = Some(Value::Bool(true));
        }
        8 => {
            opts.closing = Some(ClosingMode::Poisson);
            opts.deep_repair = DeepRepair::Off;
            opts.ftetwild = Some(Value::Bool(false));
        }
        9 => {
            opts.closing = Some(ClosingMode::FlatBack);
            opts.deep_repair = DeepRepair::Off;
            opts.ftetwild = Some(Value::Bool(false));
        }
        10 => {
            opts.proxy_template = true;
            opts.deep_repair = DeepRepair::Off;
            opts.ftetwild = Some(Value::Bool(false));
        }
        13 => {
            opts.graft = Some(true);
            opts.deep_repair = DeepRepair::Off;
            opts.ftetwild = Some(Value::Bool(false));
        }
        14 => {
            opts.closing = Some(ClosingMode::Mirror);
            opts.deep_repair = DeepRepair::Off;
            opts.ftetwild = Some(Value::Bool(false));
        }
        19 => {
            opts.coaptation = Some(true);
            opts.deep_repair = DeepRepair::Full;
            opts.ftetwild = Some(Value::Bool(false));
        }
        _ => return None,
    }
    Some(opts)
}

/// Run one ranked-fallback attempt as a nested repair with the method's option
/// overrides.  The attempt never re-enters the auto ladder
/// (`auto_escalation=false`) and shares the caller's cancellation token.
#[allow(dead_code)]
pub(crate) fn attempt_with_method(
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    base: &RepairOptions,
    method_num: u8,
    cancel: &CancelToken,
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>, Value)> {
    let opts = method_options(base, method_num)?;
    if cancel.is_cancelled() {
        return None;
    }
    match repair_impl(input_v, input_t, &opts, &NoopSink, cancel, false) {
        RepairOutcome::Ok {
            verts,
            tris,
            report,
        } => Some((verts, tris, report)),
        _ => None,
    }
}

/// Python `_auto_escalation_allowed`: the ranked ladder is only enabled for
/// the full preset and while fTetWild is not explicitly off.
fn auto_ladder_allowed(opts: &RepairOptions) -> bool {
    opts.auto_escalation.unwrap_or(true)
        && opts.ladder_depth == 0
        && opts.forced_methods.is_empty()
        && matches!(opts.deep_repair, DeepRepair::Full)
        && !matches!(opts.ftetwild, Some(Value::Bool(false)))
}

fn method_invents_geometry(num: u8) -> bool {
    matches!(num, 7 | 10 | 13 | 15 | 16)
}

/// Python `_evaluate` for a nested attempt: strict-watertight + reloaded
/// holes/non-manifold edges.
fn ladder_eval(v: &[[f64; 3]], t: &[[i64; 3]], report: &Value) -> (bool, usize, usize, String) {
    let s1 = report.get("stage1");
    let two = s1
        .and_then(|s| s.get("two_manifold"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let holes_remaining = s1
        .and_then(|s| s.get("holes_remaining"))
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let stage2_ok = report
        .get("stage2")
        .and_then(|s| s.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let watertight = two && holes_remaining == 0 && stage2_ok;
    let (holes, nm) = if watertight {
        (0usize, 0usize)
    } else {
        reload_strict_holes_nm(v, t)
    };
    let reason = if watertight {
        "strict-watertight".to_string()
    } else if holes > 0 || nm > 0 {
        format!("holes={holes} non-manifold={nm} remain")
    } else {
        "stage 1 closed but stage 2 did not confirm the solid".to_string()
    };
    (watertight, holes, nm, reason)
}

fn ladder_guard_policy(policy: Option<&LoadedPolicy>, method_id: &str) -> common::GuardPolicy {
    let read = |key: &str, default: f64| policy_number(policy, "gates.adoption", key, default);
    let si_policy = match method_id {
        "graft" | "ftetwild" => "improved_flag",
        "splint" | "transplant" | "transplant_plus" | "wall_thicken" => "non_rise",
        "coaptation" | "dressing" => "strict_zero",
        "flap" | "tamponade" => "flag",
        _ => "flag",
    };
    common::GuardPolicy {
        face_ratio: read("face_ratio", 4.0),
        vertex_ratio: read("vertex_ratio", 4.0),
        min_slack: read("min_slack", 256.0) as i64,
        loop_face_slack: read("loop_face_slack", 16.0) as i64,
        loop_vertex_slack: read("loop_vertex_slack", 16.0) as i64,
        si_policy: si_policy.to_string(),
        si_rel_tol: 0.0,
        si_abs_tol: 0,
        face_abs_floor: if method_id == "ftetwild" {
            read("ftetwild_abs_floor", 50_000.0) as i64
        } else {
            0
        },
        vertex_abs_floor: if method_id == "ftetwild" {
            read("ftetwild_abs_floor", 50_000.0) as i64
        } else {
            0
        },
        allow_growth_if_improved: method_id == "ftetwild",
    }
}

fn ladder_record(
    num: u8,
    id: &str,
    name: &str,
    outcome: &str,
    watertight: bool,
    holes: usize,
    nm: usize,
    reason: &str,
    work_units: Option<i64>,
    work_budget: Option<i64>,
    budget_scope: Option<&str>,
) -> Value {
    let mut rec = json!({
        "num": num,
        "id": id,
        "name": name,
        "outcome": outcome,
        "watertight": watertight,
        "holes": holes,
        "non_manifold": nm,
        "hausdorff_rel": Value::Null,
        "geom_change_pct": Value::Null,
        "guard_rejected": Value::Null,
        "guard_reason": Value::Null,
        "si_flagged": false,
        "adopt_guard": Value::Null,
        "reason": reason,
        "template": Value::Null,
        "elapsed_ms": 0,
    });
    if let Some(units) = work_units {
        rec["work_units"] = Value::from(units);
    }
    if let Some(budget) = work_budget {
        rec["work_budget"] = Value::from(budget);
    }
    if let Some(scope) = budget_scope {
        rec["budget_scope"] = Value::from(scope);
    }
    rec
}

fn ladder_attach_budgets(
    report: &mut Map<String, Value>,
    work_budget: Option<i64>,
    grid_budget: Option<i64>,
    damage: i64,
    grid: i64,
    counts: &(i64, i64, i64, i64),
) {
    report.insert(
        "fallback_work_budget".into(),
        work_budget.map(Value::from).unwrap_or(Value::Null),
    );
    report.insert(
        "fallback_grid_work_budget".into(),
        grid_budget.map(Value::from).unwrap_or(Value::Null),
    );
    report.insert(
        "fallback_work_input".into(),
        json!({
            "faces": counts.0,
            "nm_edges": counts.1,
            "nm_vertices": counts.2,
            "holes": counts.3,
        }),
    );
    report.insert(
        "fallback_work_estimate".into(),
        json!({"damage_units": damage, "grid_units": grid}),
    );
}

/// Run the ranked fallback ladder on a non-watertight baseline.  Returns the
/// adopted candidate when one wins; otherwise annotates the baseline report
/// and returns `None` (the baseline output is kept).
#[allow(clippy::too_many_arguments)]
fn run_auto_ladder(
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    opts: &RepairOptions,
    policy: Option<&LoadedPolicy>,
    ranking: &(Vec<Recommendation>, HashSet<u8>),
    base_report: &mut Map<String, Value>,
    cancel: &CancelToken,
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>, Map<String, Value>)> {
    let (base_num, _, _) = baseline_method(opts.deep_repair);
    let faces = input_t.len() as i64;
    let topo = common::topology_summary(input_v, input_t);
    let holes = boundary_loop_stats(input_t).0 as i64;
    let counts = (
        faces,
        topo.non_two_manifold_edges as i64,
        topo.non_two_manifold_vertices as i64,
        holes,
    );
    let damage = input_damage_work(input_v, input_t);
    let grid = fallback_grid_work(input_v, input_t, &opts.intensity);
    let work_budget = fallback_work_budget();
    let grid_budget = graft_grid_budget();
    let max_attempts = policy_number(policy, "auto", "max_attempts", 3.0).max(0.0) as usize;
    let invent_min = policy_number(policy, "auto", "invent_min_score", 0.8);
    let baseline_entry = base_report
        .get("methods_tried")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);
    let mut tried: Vec<Value> = Vec::new();
    let mut attempts = 0usize;
    let mut deferred_graft: Option<(
        Recommendation,
        Vec<[f64; 3]>,
        Vec<[i64; 3]>,
        Map<String, Value>,
        bool,
    )> = None;

    let mut adopt = |rec: &Recommendation,
                     v: Vec<[f64; 3]>,
                     t: Vec<[i64; 3]>,
                     cand_report: Value,
                     tried: &[Value]|
     -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>, Map<String, Value>)> {
        let mut m = cand_report.as_object().cloned().unwrap_or_default();
        let mut all = Vec::with_capacity(tried.len() + 1);
        all.push(baseline_entry.clone());
        all.extend(tried.iter().cloned());
        m.insert("methods_tried".into(), Value::Array(all));
        m.insert(
            "method_used".into(),
            json!({
                "num": rec.num,
                "id": rec.id,
                "name": rec.name,
                "source": "auto_escalated",
                "note": null,
            }),
        );
        m.insert("method_reached_watertight".into(), Value::Bool(true));
        ladder_attach_budgets(&mut m, work_budget, grid_budget, damage, grid, &counts);
        Some((v, t, m))
    };

    for rec in &ranking.0 {
        if rec.num == base_num || ranking.1.contains(&rec.num) {
            continue;
        }
        let Some(_) = method_options(opts, rec.num) else {
            continue;
        };
        let work = candidate_work(rec.id, damage, grid, faces);
        let cls = work_class(rec.id);
        let budget = if cls == "grid" { grid_budget } else { work_budget };
        let scope = if cls == "grid" { "grid" } else { "fallback" };
        if !fits_budget(work, budget) {
            tried.push(ladder_record(
                rec.num,
                rec.id,
                rec.name,
                "skipped_budget",
                false,
                0,
                0,
                &format!(
                    "estimated {cls} work {work} (budget {}) is above the cap; method not started, stage-1 result kept",
                    budget.map(|b| b.to_string()).unwrap_or_else(|| "none".into())
                ),
                Some(work),
                budget,
                Some(scope),
            ));
            continue;
        }
        if method_invents_geometry(rec.num) && rec.score < invent_min {
            continue;
        }
        if attempts >= max_attempts {
            break;
        }
        attempts += 1;
        let Some((cv, ct, crep)) = attempt_with_method(input_v, input_t, opts, rec.num, cancel) else {
            continue;
        };
        let (mut watertight, holes, nm, reason) = ladder_eval(&cv, &ct, &crep);
        if watertight {
            let guard_policy = ladder_guard_policy(policy, rec.id);
            if let Some(g) = common::blowup_reason(
                input_t.len(),
                input_v.len(),
                ct.len(),
                cv.len(),
                0,
                &guard_policy,
            ) {
                watertight = false;
                tried.push(ladder_record(
                    rec.num,
                    rec.id,
                    rec.name,
                    "rejected",
                    false,
                    holes,
                    nm,
                    &format!("adopt guard: {g}"),
                    None,
                    None,
                    None,
                ));
                continue;
            }
        }
        tried.push(ladder_record(
            rec.num,
            rec.id,
            rec.name,
            if watertight { "accepted" } else { "rejected" },
            watertight,
            holes,
            nm,
            &reason,
            None,
            None,
            None,
        ));
        if !watertight {
            continue;
        }
        // Python `_evaluate`: generative methods and Join get a one-sided
        // input->output Hausdorff gate (Graft is self-guarded by its own
        // fidelity/coverage gate below).
        let shape_gated = method_invents_geometry(rec.num) || rec.id == "join";
        if shape_gated && rec.num != 13 {
            let gen_max = policy_number(
                policy,
                "auto",
                "generative_max_hausdorff_rel",
                0.01,
            );
            let hd = crate::proxy::one_sided_hausdorff_rel(
                input_v, input_t, &cv, &ct,
            )
            .0;
            if let Some(hd) = hd {
                if hd > gen_max {
                    tried.push(ladder_record(
                        rec.num,
                        rec.id,
                        rec.name,
                        "rejected",
                        false,
                        holes,
                        nm,
                        &format!(
                            "shape changed (one-sided Hausdorff {hd:.3} > {gen_max:.3})"
                        ),
                        None,
                        None,
                        None,
                    ));
                    continue;
                }
            }
        }
        if rec.num == 13 {
            let g = crep.get("graft");
            let fidelity_failed = g
                .and_then(|g| g.get("fidelity_ok"))
                .and_then(Value::as_bool)
                == Some(false);
            let coverage_failed = g
                .and_then(|g| g.get("coverage_ok"))
                .and_then(Value::as_bool)
                == Some(false);
            if fidelity_failed || coverage_failed {
                deferred_graft = Some((
                    rec.clone(),
                    cv,
                    ct,
                    crep.as_object().cloned().unwrap_or_default(),
                    coverage_failed,
                ));
                continue;
            }
        }
        return adopt(rec, cv, ct, crep, &tried);
    }

    if let Some((rec, cv, ct, mut cand, coverage_failed)) = deferred_graft {
        if !coverage_failed {
            return adopt(&rec, cv, ct, Value::Object(cand), &tried);
        }
        // Coverage failure: never adopt this Graft; surface it on the baseline.
        let g = cand.remove("graft").unwrap_or(Value::Null);
        if let Some(gm) = g.as_object() {
            let mut surf = gm.clone();
            surf.insert("ran".into(), Value::Bool(true));
            surf.insert("adopted".into(), Value::Bool(false));
            surf.insert("reject_reason".into(), Value::from("coverage"));
            base_report.insert("graft".into(), Value::Object(surf));
        }
    }

    // No candidate won: keep the baseline bytes but record the attempts.
    let mut all = Vec::with_capacity(tried.len() + 1);
    all.push(baseline_entry);
    all.extend(tried.iter().cloned());
    base_report.insert("methods_tried".into(), Value::Array(all));
    ladder_attach_budgets(base_report, work_budget, grid_budget, damage, grid, &counts);
    None
}

/// Repair one mesh given as float64/int64 arrays.
pub fn repair(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    opts: &RepairOptions,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
) -> RepairOutcome {
    // One reset per top-level run: `new()` is also used for short-lived score
    // tokens, and resetting there would swallow a running repair's cancel.
    sutura_geom::cancel::reset();
    repair_impl(verts, tris, opts, sink, cancel, true)
}

/// Python-compatible total surface area: f32 per-face areas, NumPy pairwise
/// summation (`validate::surface_area`).  The stage-1 report uses this shared
/// kernel so its value matches `sutura_engine.pipeline.utils.surface_area`.
fn py_surface_area(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> f64 {
    let v32: Vec<[f32; 3]> = verts
        .iter()
        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let t32: Vec<[i32; 3]> = tris
        .iter()
        .map(|t| [t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    crate::validate::surface_area(&v32, &t32)
}

fn repair_impl(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    opts: &RepairOptions,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
    allow_si_escalation: bool,
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

    let t_start = Instant::now();
    let mut t_prepass = 0.0f64;
    let mut t_stage1 = 0.0f64;
    let mut t_deep = 0.0f64;
    let mut t_stage2 = 0.0f64;
    let mut t_reload = 0.0f64;

    emit_stage(sink, 1, "validate");
    let policy = load_policy(None).ok();
    let mut spec = resolve_intensity_spec(opts, policy.as_ref());
    if let Some(v) = opts.ftetwild_max_faces {
        spec.ftetwild_max_faces = Some(v);
    }
    if let Some(v) = opts.ftetwild_timeout {
        spec.ftetwild_timeout = v;
    }
    if let Some(v) = opts.graft_max_faces {
        spec.graft_max_faces = Some(v);
    }
    if let Some(v) = opts.graft_timeout {
        spec.graft_timeout = v;
    }
    if let Some(v) = opts.dense_ratio {
        spec.dense_ratio = v;
    }
    if let Some(v) = opts.dense_min_faces {
        spec.dense_min_faces = v;
    }
    if let Some(v) = opts.ftetwild_optimize_retry {
        spec.ftetwild_optimize_retry_on_dense_fail = v;
    }
    let intensity_for_tamponade = spec.base.clone().unwrap_or_else(|| spec.name.clone());
    let triage = resolve_triage(opts, policy.as_ref(), verts, tris);
    if cancel.is_cancelled() {
        return cancelled();
    }
    let min_component = triage.min_component;
    let mut max_hole = triage.max_hole;
    let (_, largest_loop) = boundary_loop_stats(tris);
    max_hole = max_hole.max(2 * largest_loop as i64);

    let input_v = verts.to_vec();
    let input_t = tris.to_vec();
    let mut cur_v = verts.to_vec();
    let mut cur_t = tris.to_vec();
    // Python keeps a second pair, `v`/`t`, that starts as the input rounded to
    // float32 (`pipeline.py:151`) and is rebound **only** by the external-engine
    // adoption and by Flap — not by the Stage-1 chain, which writes to
    // `verts`/`tris`. The wall tier measures `v`/`t` (`pipeline.py:897`), so it
    // sees the Flap output but never the Stage-1 output. Tracking it separately
    // is what makes the tier read the same mesh as Python: measuring `cur_v`
    // gave 17250 faces on broken_37011 where Python measured 17308.
    let mut py_v = quantize(verts);
    let mut py_t = tris.to_vec();
    let mut stage1_snapshot: Option<(Vec<[f64; 3]>, Vec<[i64; 3]>)> = None;
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
    let ftetwild_bin = crate::ftetwild::find_binary();
    let (selected_engines, engine_warnings) =
        crate::engines::selected_engines(opts.engines.as_deref());
    let auto_escalation = opts.auto_escalation.unwrap_or(true);
    let skip_auto_ladder = !auto_escalation && opts.forced_methods.is_empty();
    let mut engine_entries: Vec<Value> = Vec::new();

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
    if cancel.is_cancelled() {
        return cancelled();
    }
    let ranking: Option<(Vec<Recommendation>, HashSet<u8>)> =
        match (policy.as_ref(), mesh_analysis.as_ref()) {
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
                diagnosis_v2: analysis.diagnosis_v2.clone(),
                ..RankingAnalysis::default()
            };
            let mut recs = rank_methods(loaded, &rank_analysis, None, None);
            if opts.auto_reorder {
                recs = crate::ranking::apply_auto_reorder(recs, loaded, &rank_analysis);
            }
            let recs = order_graft_before_ftetwild(recs);
            let avoid = prune_avoid_nums(&rank_analysis);
            Some((recs, avoid))
        }
        _ => None,
    };
    let recommendations = ranking
        .as_ref()
        .map(|(recs, _)| recommendations_json(recs));

    if cancel.is_cancelled() {
        return cancelled();
    }
    // --- Outer-shell pre-pass ---------------------------------------------
    // Python runs this only in `auto` mode, before the Tamponade prepass, on
    // mechanical (or heavy-SI) multi-shell inputs. It is part of the Tamponade
    // feature, so an explicit `--no-tamponade` skips it.
    let t_prepass_start = Instant::now();
    if opts.mode == RepairMode::Auto {
        let tamponade_enabled = opts.tamponade != Some(false);
        let shell_before = (cur_v.clone(), cur_t.clone());
        run_outer_shell_prepass(
            &mut cur_v,
            &mut cur_t,
            &triage.mesh_type,
            tamponade_enabled,
            &mut stats,
        );
        // Python's `v, t = verts, tris` after an adopted outer-shell extraction
        // (`pipeline.py:394`); when it is rejected, `cur` is unchanged too.
        if cur_t != shell_before.1 {
            py_v = quantize(&cur_v);
            py_t = cur_t.clone();
        }
    }
    let before = common::topology_summary(&cur_v, &cur_t);
    let input_components = common::topology_summary(&input_v, &input_t).components;
    let holes_before = boundary_loop_stats(&cur_t).0;
    let nm_before = before.non_two_manifold_edges;
    let before_verts = cur_v.len();

    let stage1_volume_before_v = cur_v.clone();
    let stage1_volume_before_t = cur_t.clone();

    // --- Tamponade pre-pass ------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 2, "prepass");
    let mut tamponade_prepass_adopted = false;
    let mut tamponade_gate_skipped = false;
    if opts.tamponade != Some(false) {
        let hygiene = Value::Array(cleanup_chain(true));
        let (clean_v, clean_t, _, _, _) = apply_chain_with_cancel(
            &cur_v, &cur_t, &hygiene, Some(cancel as &dyn sutura_geom::cancel::Cancel));
        if cancel.is_cancelled() {
            return cancelled();
        }
        let params = tamponade_params(
            &clean_v,
            &clean_t,
            &intensity_for_tamponade,
            opts.si_mode,
            true,
            policy.as_ref(),
        );
        let (out_v, out_t, mut rec) = tamponade::run_with_cancel(
            &clean_v,
            &clean_t,
            &params,
            Some(cancel as &dyn sutura_geom::cancel::Cancel),
        );
        ensure_tamponade_defaults(&mut rec);
        tamponade_prepass_adopted = adopted(&rec);
        tamponade_gate_skipped = rec
            .get("gate")
            .and_then(Value::as_str)
            .map(|g| g == "shape_card")
            .unwrap_or(false);
        if tamponade_prepass_adopted {
            cur_v = quantize(&out_v);
            cur_t = out_t;
            // Python's `verts, tris = v, t` after an adopted prepass
            // (`pipeline.py:471`).
            py_v = cur_v.clone();
            py_t = cur_t.clone();
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
        let (out_v, out_t, rec) = flap::run_with_cancel(
            &cur_v,
            &cur_t,
            &params,
            Some(cancel as &dyn sutura_geom::cancel::Cancel),
        );
        if adopted(&rec) {
            cur_v = quantize(&out_v);
            cur_t = out_t;
            // Python rebinds `v`/`t` here too (`pipeline.py:506`), which is why
            // the wall tier sees the Flap output but not the Stage-1 output.
            py_v = cur_v.clone();
            py_t = cur_t.clone();
        }
        if std::env::var_os("SUTURA_PIPELINE_DEBUG").is_some() {
            eprintln!(
                "[pipeline-rust] flap faces={} verts={} work_budget={} v0={:?}",
                cur_t.len(),
                cur_v.len(),
                rec.get("work_budget").cloned().unwrap_or(Value::Null),
                cur_v.first()
            );
        }
        emit_tier(sink, "flap_prepass", 2, &rec);
        stats.insert("flap".into(), rec);
    }

    stage_fp_log("after_prepass", &cur_v, &cur_t);
    t_prepass = t_prepass_start.elapsed().as_secs_f64();
    crate::engines::run_and_replace(
        &selected_engines,
        "before_stage1",
        &mut cur_v,
        &mut cur_t,
        Some(cancel as &dyn sutura_geom::cancel::Cancel),
        0.01,
        &mut engine_entries,
    );
    // --- Stage 1 -----------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 3, "stage 1");
    let s1_in_v = cur_v.clone();
    let s1_in_t = cur_t.clone();
    if std::env::var_os("SUTURA_PIPELINE_DEBUG").is_some() {
        eprintln!(
            "[pipeline-rust] stage1 params min={} maxhole={} faces={} verts={} v0={:?}",
            min_component,
            max_hole,
            cur_t.len(),
            cur_v.len(),
            cur_v.first()
        );
    }
    let chain = Value::Array(stage1_chain(max_hole, min_component, opts.join_components));
    let mut stage1_errors: Vec<Value> = Vec::new();
    let (v1, t1, mut applied, mut skipped, removed_small) =
        crate::stage1::chain::apply_chain_with_cancel_errors(
            &cur_v,
            &cur_t,
            &chain,
            Some(cancel as &dyn sutura_geom::cancel::Cancel),
            &mut stage1_errors,
        );
    cur_v = v1;
    cur_t = t1;
    if cancel.is_cancelled() {
        return cancelled();
    }
    if opts.join_components {
        let (vj, tj, moved, remaining) = crate::stage1::chain::join_small_components(
            &cur_v,
            &cur_t,
            min_component,
            max_hole,
        );
        cur_v = vj;
        cur_t = tj;
        stats.insert(
            "experimental_join_components".into(),
            json!({"moved": moved, "remaining_small": remaining}),
        );
    }

    // Opt-in autorefine prototypes: re-run the SAME Stage-1 chain on the
    // pre-processed input and adopt the candidate only when holes and
    // non-manifold edges are no worse. Python order is autorefine, then the
    // exact indirect arrangement, then delete-fallback/join.
    if opts.autorefine {
        let (ar_v, ar_t, rec) = crate::autorefine::autorefine(&s1_in_v, &s1_in_t);
        let mut rep = rec;
        if ar_t.is_empty() {
            rep["skipped"] = Value::Bool(true);
        } else {
            let (av, at) = stage1_variant(
                &ar_v, &ar_t, max_hole, min_component, opts.join_components, cancel,
            );
            let adopted = candidate_no_worse(&av, &at, &cur_v, &cur_t);
            rep["adopted"] = Value::Bool(adopted);
            rep["skipped"] = Value::Bool(false);
            if adopted {
                cur_v = av;
                cur_t = at;
            }
        }
        stats.insert("experimental_autorefine".into(), rep);
        if cancel.is_cancelled() {
            return cancelled();
        }
    }
    if opts.indirect_autorefine {
        let mut rep = Map::new();
        rep.insert("skipped".into(), Value::Bool(false));
        let source_t: Vec<[usize; 3]> = s1_in_t
            .iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect();
        match sutura_geom::arrangement::arrangement_lite_core(&s1_in_v, &source_t) {
            Ok((pool, out_t, report)) => {
                let av: Vec<[f64; 3]> = pool
                    .iter()
                    .map(|p| {
                        let p = sutura_geom::arrangement::rat3_to_f64(p);
                        [p[0] as f32 as f64, p[1] as f32 as f64, p[2] as f32 as f64]
                    })
                    .collect();
                let at: Vec<[i64; 3]> = out_t
                    .iter()
                    .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
                    .collect();
                rep.insert("ran".into(), Value::Bool(true));
                rep.insert(
                    "si_before".into(),
                    Value::from(report.si_pairs_detected as i64),
                );
                rep.insert("si_after".into(), Value::from(0i64));
                rep.insert("faces_before".into(), Value::from(report.input_faces as i64));
                rep.insert("faces_after".into(), Value::from(report.output_faces as i64));
                let mut degen = Map::new();
                for (key, value) in report.degenerate_cases {
                    degen.insert(key, Value::from(value as i64));
                }
                rep.insert("degenerate_cases".into(), Value::Object(degen));
                rep.insert("error".into(), Value::Null);
                if !at.is_empty() {
                    let (cv, ct) = stage1_variant(
                        &av, &at, max_hole, min_component, opts.join_components, cancel,
                    );
                    let adopted = candidate_no_worse(&cv, &ct, &cur_v, &cur_t);
                    rep.insert("adopted".into(), Value::Bool(adopted));
                    if adopted {
                        cur_v = cv;
                        cur_t = ct;
                    }
                }
            }
            Err(err) => {
                rep.insert(
                    "error".into(),
                    Value::from(format!(
                        "indirect arrangement-lite failed: {err}"
                    )),
                );
            }
        }
        stats.insert(
            "experimental_indirect_autorefine".into(),
            Value::Object(rep),
        );
        if cancel.is_cancelled() {
            return cancelled();
        }
    }

    let mut after = common::topology_summary(&cur_v, &cur_t);
    if after.non_two_manifold_edges > 0 || after.non_two_manifold_vertices > 0 {
        let fallback = Value::Array(delete_fallback_chain(
            max_hole,
            min_component,
            opts.join_components,
        ));
        let (fv, ft, f_applied, f_skipped, _) = apply_chain_with_cancel(
            &cur_v, &cur_t, &fallback, Some(cancel as &dyn sutura_geom::cancel::Cancel));
        if cancel.is_cancelled() {
            return cancelled();
        }
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

    if opts.join_components {
        let (vj, tj, moved, remaining) = crate::stage1::chain::join_small_components(
            &cur_v,
            &cur_t,
            min_component,
            max_hole,
        );
        cur_v = vj;
        cur_t = tj;
        after = common::topology_summary(&cur_v, &cur_t);
        stats.insert(
            "experimental_join_components".into(),
            json!({"moved": moved, "remaining_small": remaining}),
        );
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
    stage_fp_log("before_si_excise", &cur_v, &cur_t);
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
    stage_fp_log("after_si_excise", &cur_v, &cur_t);

    stats.insert("extreme_passes_applied".into(), Value::Bool(false));
    if opts.mode == RepairMode::Extreme {
        let params = json!({
            "maxholesize": max_hole,
            "mincomponentsize": min_component,
            "join_components": opts.join_components,
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

    // `after_stage1` engines run on the Stage-1 result, before the closing
    // / Tamponade-post / deep ladder, exactly where Python's pipeline runs
    // them.  Keeping the placement here (not after the ladder) is what makes
    // the engine see the same input array on both sides.
    stage_fp_log("after_stage1", &cur_v, &cur_t);
    if crate::engines::run_and_replace(
        &selected_engines,
        "after_stage1",
        &mut cur_v,
        &mut cur_t,
        Some(cancel as &dyn sutura_geom::cancel::Cancel),
        0.01,
        &mut engine_entries,
    ) {
        after = common::topology_summary(&cur_v, &cur_t);
    }
    t_stage1 = t_start.elapsed().as_secs_f64();
    let t_deep_start = Instant::now();

    // Tamponade second chance when the pre-pass did not adopt.
    if opts.tamponade == Some(true) && !tamponade_prepass_adopted && !tamponade_gate_skipped {
        let params = tamponade_params(
            &cur_v,
            &cur_t,
            &intensity_for_tamponade,
            opts.si_mode,
            false,
            policy.as_ref(),
        );
        let (out_v, out_t, mut rec) = tamponade::run_with_cancel(
            &cur_v,
            &cur_t,
            &params,
            Some(cancel as &dyn sutura_geom::cancel::Cancel),
        );
        ensure_tamponade_defaults(&mut rec);
        if adopted(&rec) {
            cur_v = quantize(&out_v);
            cur_t = out_t;
        }
        emit_tier(sink, "tamponade_post", 3, &rec);
        stats.insert("tamponade".into(), rec);
        if cancel.is_cancelled() {
            return cancelled();
        }
    }
    stage_fp_log("after_tamponade_post", &cur_v, &cur_t);

    // --- Registry methods 8/9/10: closing / proxy-template / mirror -------
    // They run on the ORIGINAL input and replace the current result only with
    // a strict-watertight candidate.
    if opts.closing.is_some() || opts.proxy_template {
        let cl_in_v = cur_v.clone();
        let cl_in_t = cur_t.clone();
        let (cv, ct, rec) = if opts.proxy_template {
            run_proxy_tier(
                &cur_v, &cur_t, &s1_in_v, &s1_in_t,
                Some(cancel as &dyn sutura_geom::cancel::Cancel),
            )
        } else {
            run_closing_tier(
                &cur_v,
                &cur_t,
                &s1_in_v,
                &s1_in_t,
                opts.closing.expect("closing mode present"),
                Some(cancel as &dyn sutura_geom::cancel::Cancel),
            )
        };
        if adopted(&rec) {
            cur_v = cv;
            cur_t = ct;
            after = common::topology_summary(&cur_v, &cur_t);
        }
        emit_tier(sink, "closing", 3, &rec);
        if cancel.is_cancelled() {
            return cancelled();
        }
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
    stage_fp_log("after_closing", &cur_v, &cur_t);

    let mut forced_graft_done = false;
    if opts.forced_methods.contains(&13) && opts.graft != Some(false) {
        let mut holes = boundary_loop_stats(&cur_t).0;
        let mut nm = after.non_two_manifold_edges;
        if holes > 0 || nm > 0
            || opts.forced_methods.contains(&13)
        {
            let g_in_v = cur_v.clone();
            let g_in_t = cur_t.clone();
            let (gv, gt, rec) = run_graft_tier(
                &cur_v, &cur_t, &s1_in_v, &s1_in_t, holes, nm,
                &intensity_for_tamponade, &spec, opts.time_budget_s,
                Some(cancel as &dyn sutura_geom::cancel::Cancel),
                false,
            );
            if adopted(&rec) {
                cur_v = quantize(&gv);
                cur_t = gt;
                after = common::topology_summary(&cur_v, &cur_t);
                holes = boundary_loop_stats(&cur_t).0;
                nm = after.non_two_manifold_edges;
            }
            emit_tier(sink, "graft", 4, &rec);
            trace.record("graft", "tiers.graft.graft_tier", &g_in_v, &g_in_t,
                         &cur_v, &cur_t, rec.clone());
            stats.insert("graft".into(), rec);
            forced_graft_done = true;
        }
    }
    stage_fp_log("after_graft", &cur_v, &cur_t);

    // --- Deep-repair ladder ------------------------------------------------
    let ladder_holes_before = boundary_loop_stats(&cur_t).0;
    let ladder_nm_before = after.non_two_manifold_edges;
    let mut tiers_run: Vec<Value> = Vec::new();
    if opts.deep_repair != DeepRepair::Off && !skip_auto_ladder {
        emit_stage(sink, 4, "deep repair");
        if opts.deep_repair == DeepRepair::Full {
            let mut holes = ladder_holes_before;
            let mut nm = ladder_nm_before;

            // Graft (#13): shell wrap, only when Stage 1 still leaves damage.
            let mut graft_ok = false;
            if !forced_graft_done
                && opts.graft != Some(false)
                && (holes > 0 || nm > 0)
            {
                let g_in_v = cur_v.clone();
                let g_in_t = cur_t.clone();
                let (gv, gt, mut rec) = run_graft_tier(
                    &cur_v,
                    &cur_t,
                    &s1_in_v,
                    &s1_in_t,
                    holes,
                    nm,
                    &intensity_for_tamponade,
                    &spec,
                    opts.time_budget_s,
                    Some(cancel as &dyn sutura_geom::cancel::Cancel),
                    true,
                );
                let cov_failed = rec
                    .get("coverage_ok")
                    .and_then(Value::as_bool)
                    == Some(false);
                let fid_failed = rec
                    .get("fidelity_ok")
                    .and_then(Value::as_bool)
                    == Some(false);
                if adopted(&rec) && !cov_failed && !fid_failed {
                    cur_v = quantize(&gv);
                    cur_t = gt;
                    after = common::topology_summary(&cur_v, &cur_t);
                    holes = boundary_loop_stats(&cur_t).0;
                    nm = after.non_two_manifold_edges;
                    graft_ok = true;
                    tiers_run.push(Value::from("graft"));
                } else if adopted(&rec) {
                    // The ladder holds a low-coverage/low-fidelity candidate
                    // aside and lets fTetWild/the next tier run, exactly like
                    // the Python deep-repair ladder.
                    if let Some(obj) = rec.as_object_mut() {
                        obj.insert("adopted".into(), json!(false));
                        obj.insert(
                            "reason".into(),
                            json!(if cov_failed {
                                "superseded by fTetWild (input coverage below threshold)"
                            } else {
                                "superseded by fTetWild (fidelity below threshold)"
                            }),
                        );
                    }
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

            // Dressing (#16): variable-viscosity coat, after Graft and before
            // Splint (the Python ladder order).
            let mut dressing_ok = false;
            if opts.dressing || opts.dressing_force_adopt {
                let d_in_v = cur_v.clone();
                let d_in_t = cur_t.clone();
                // Python's ladder receives the post-prepass surface as its
                // `input_v`, not the raw file arrays (outer-shell / Tamponade /
                // Flap may already have replaced them).
                let (dv, dt, rec) = run_dressing_tier(
                    &cur_v,
                    &cur_t,
                    &s1_in_v,
                    &s1_in_t,
                    &intensity_for_tamponade,
                    opts,
                    policy.as_ref(),
                    Some(cancel as &dyn sutura_geom::cancel::Cancel),
                );
                if adopted(&rec) {
                    cur_v = dv;
                    cur_t = dt;
                    after = common::topology_summary(&cur_v, &cur_t);
                    holes = boundary_loop_stats(&cur_t).0;
                    nm = after.non_two_manifold_edges;
                    dressing_ok = true;
                    tiers_run.push(Value::from("dressing"));
                }
                if std::env::var_os("SUTURA_PIPELINE_DEBUG").is_some() {
                    eprintln!(
                        "[pipeline-rust] dressing adopted={} voxel={:?} r_base={:?} r_max={:?} faces={:?} holes={:?} nm={:?}",
                        dressing_ok,
                        rec.get("voxel").cloned().unwrap_or(Value::Null),
                        rec.get("r_base").cloned().unwrap_or(Value::Null),
                        rec.get("r_max").cloned().unwrap_or(Value::Null),
                        rec.get("faces").cloned().unwrap_or(Value::Null),
                        rec.get("holes").cloned().unwrap_or(Value::Null),
                        rec.get("non_manifold").cloned().unwrap_or(Value::Null),
                    );
                }
                emit_tier(sink, "dressing", 4, &rec);
                trace.record(
                    "dressing",
                    "tiers.dressing.dressing_tier",
                    &d_in_v,
                    &d_in_t,
                    &cur_v,
                    &cur_t,
                    rec.clone(),
                );
                stats.insert("dressing".into(), rec);
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
            if ft_trigger != crate::ftetwild::Trigger::Disabled && ftetwild_bin.is_some() {
                let earlier = if graft_ok {
                    "graft"
                } else if splint_ok {
                    "splint"
                } else if coaptation_ok {
                    "coaptation"
                } else if cdt_ok {
                    "cdt"
                } else {
                    ""
                };
                if !earlier.is_empty() {
                    stats.insert(
                        "ftetwild_reason".into(),
                        json!(format!("skipped: earlier tier succeeded ({earlier})")),
                    );
                }
            }
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
                    max_hausdorff_rel: opts.ftetwild_max_hausdorff_rel.unwrap_or_else(|| {
                        policy_number(
                            policy.as_ref(),
                            "gates.tuning",
                            "ftetwild_max_hausdorff_rel",
                            0.01,
                        )
                    }),
                    decimate_hd_margin: policy_number(
                        policy.as_ref(),
                        "gates.tuning",
                        "decimate_hd_margin",
                        0.005,
                    ),
                    guard: ftetwild_guard_policy(policy.as_ref()),
                };
                let replace_ftw = crate::engines::run_and_replace(
                    &selected_engines,
                    "replace_ftetwild",
                    &mut cur_v,
                    &mut cur_t,
                    Some(cancel as &dyn sutura_geom::cancel::Cancel),
                    0.01,
                    &mut engine_entries,
                );
                if replace_ftw {
                    after = common::topology_summary(&cur_v, &cur_t);
                }
                let ft_result = if replace_ftw {
                    None
                } else {
                    crate::ftetwild::run_tier_with_cancel(
                        &cur_v,
                        &cur_t,
                        &input_v,
                        &input_t,
                        &after,
                        bin,
                        &spec,
                        &tuning,
                        ft_trigger == crate::ftetwild::Trigger::Always,
                        crate::ftetwild::params_for_optimize(opts.ftetwild_optimize),
                        Some(cancel as &dyn sutura_geom::cancel::Cancel),
                    )
                };
                if let Some(result) = ft_result {
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

    // --- Repeated-element transplant (#11/#12) -------------------------------
    //
    // The whole producer -- pattern detection, template choice, Manifold
    // transplant and the Hausdorff guard -- is Rust (`tiers::repeat::
    // repair_auto_report`), so this no longer defers to Python.
    if let Some(mode) = opts.repeat {
        let margin = policy_number(policy.as_ref(), "tiers.repeat", "obb_margin", 0.10);
        let (rv, rt, report) = match (mode, opts.repeat_source, opts.repeat_target) {
            // Method 12: the caller named the two points to transplant between.
            (RepeatMode::Manual, Some(src), Some(dst)) => {
                let tu: Vec<[usize; 3]> = cur_t
                    .iter()
                    .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
                    .collect();
                let (mv, mt, rep) = sutura_geom::repeat_repair::repair_repeat_manual_core(
                    &cur_v, &tu, src, dst, margin,
                );
                let mt_i: Vec<[i64; 3]> = mt
                    .iter()
                    .map(|f| [f[0] as i64, f[1] as i64, f[2] as i64])
                    .collect();
                (mv, mt_i, rep)
            }
            // Manual without both points is a caller error; report it rather
            // than silently falling back to the auto route.
            (RepeatMode::Manual, _, _) => (
                cur_v.to_vec(),
                cur_t.to_vec(),
                crate::tiers::repeat::error_report(
                    "manual repeat needs both --repeat-source and --repeat-target",
                ),
            ),
            (RepeatMode::Auto, _, _) => crate::tiers::repeat::repair_auto_raw(
                &cur_v,
                &cur_t,
                margin,
            ),
        };
        // `decide` owns the adoption gate (defect counts, reload-honest
        // watertight, Hausdorff outside, reason), so route through it rather
        // than re-deriving the verdict here -- that is what the Python tier
        // does, and it keeps one report shape.
        let params = json!({
            "gates": {
                "max_hausdorff_outside": policy_number(
                    policy.as_ref(), "tiers.repeat.gates",
                    "max_hausdorff_outside", 0.02),
            },
        });
        let (rv, rt, rec) = crate::tiers::repeat::decide(
            &cur_v, &cur_t, &rv, &rt, &crate::tiers::repeat::report_to_value(report),
            &params,
        );
        let mut rec = rec;
        rec["mode"] = Value::from(match mode {
            RepeatMode::Manual => "manual",
            RepeatMode::Auto => "auto",
        });
        // `decide` sets `adopted`; the mesh it returns is already the one to
        // keep when it did.
        let ran = rec.get("adopted").and_then(Value::as_bool).unwrap_or(false);
        rec["ran"] = Value::from(ran);
        let in_v = cur_v.clone();
        let in_t = cur_t.clone();
        if ran {
            cur_v = rv;
            cur_t = rt;
            tiers_run.push(Value::from("repeat"));
        }
        let r = rec.clone();
        emit_tier(sink, "repeat", 4, &r);
        trace.record(
            "repeat",
            "tiers.repeat.repeat_tier",
            &in_v,
            &in_t,
            &cur_v,
            &cur_t,
            r.clone(),
        );
        stats.insert("repeat".into(), r);
    }
    if opts.wall_thicken {
        // The producer is now ported (sutura-geom, byte-identical to
        // wall.py), so this is a real tier rather than an error record.
        //
        // The adoption baseline is the *current ladder* mesh's hole and
        // non-manifold counts, matching Python's `base_holes`/`base_nm`.
        let base_holes = boundary_loop_stats(&cur_t).0;
        let base_nm = after.non_two_manifold_edges;
        // Python's tier input is `v`/`t`: the input rounded to float32 at
        // pipeline entry, rebound by Flap but **not** by the Stage-1 chain. That
        // is why the measured face count follows the Flap output (17308 on
        // broken_37011) rather than the Stage-1 output (17250).
        let outcome = crate::wall_tier::run(
            &py_v,
            &py_t,
            // Ladder mesh: used only for the adoption gate's base counts.
            &cur_v,
            &cur_t,
            opts.wall_min_thickness,
            crate::wall_tier::DEFAULT_RESOLUTION,
            crate::wall_tier::DEFAULT_SAMPLES,
            0,
            base_holes,
            base_nm,
        );
        let rec = outcome.record;
        emit_tier(sink, "wall_thicken", 4, &rec);
        if outcome.adopted {
            cur_v = outcome.verts;
            cur_t = outcome.tris;
            after = common::topology_summary(&cur_v, &cur_t);
            tiers_run.push(Value::from("wall_thicken"));
        }
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

    stage_fp_log("after_deep", &cur_v, &cur_t);

    let produced = produced_tier(&stats);
    if opts.solid_engine.as_str() == "ftetwild"
        && produced != "ftetwild"
        && !FTW_NOTICE.with(|c| c.replace(true))
    {
        eprintln!(
            "sutura: fTetWild not needed: {} repaired the mesh",
            produced
        );
    }
    stats.insert(
        "solid_engine".into(),
        json!({
            "selected": opts.solid_engine.as_str(),
            "produced": produced,
            "reason": stats
                .get("ftetwild_reason")
                .or_else(|| stats.get("cdt").and_then(|r| r.get("reason")))
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
    deep.insert(
        "cad_likeness".into(),
        if opts.deep_repair == DeepRepair::Full {
            cad_likeness(&input_v, &input_t)
        } else {
            Value::Null
        },
    );
    deep.insert("dressing_damage".into(), Value::Null);
    deep.insert("available".into(), Value::Null);
    stats.insert("deep_repair".into(), Value::Object(deep));

    t_deep = t_deep_start.elapsed().as_secs_f64();

    // Pinch split before Stage 2, matching the pipeline's P1a split.
    let pinch_in_v = cur_v.clone();
    let pinch_in_t = cur_t.clone();
    let pinch = split_pinched_vertices(&cur_v, &cur_t);
    // Match Python: only take the split result when the tier actually ran.
    // On the no-pinch path Python keeps the input arrays, while the core
    // returns them with a cyclic rotation of each triangle.
    let pinch_report = if let Some(rep) = pinch.report {
        cur_v = to_f64(&pinch.verts);
        cur_t = to_i64(&pinch.tris);
        rep
    } else {
        Value::Bool(false)
    };
    // Python's pipeline repairs a globally inverted winding before Stage 2
    // (mesh_volume < 0 -> invert once; invert back if still negative).
    if common::signed_volume(&cur_v, &cur_t) < 0.0 {
        cur_t = sutura_geom::topology::invert_face_orientation_core(&cur_t);
        if common::signed_volume(&cur_v, &cur_t) < 0.0 {
            cur_t = sutura_geom::topology::invert_face_orientation_core(&cur_t);
        }
    }
    stage_fp_log("after_pinch", &cur_v, &cur_t);
    after = common::topology_summary(&cur_v, &cur_t);
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
    stage1.insert("vertices_before".into(), Value::from(before_verts));
    stage1.insert("vertices_after".into(), Value::from(cur_v.len()));
    let geom_volume = |v: &[[f64; 3]], t: &[[i64; 3]]| -> f64 {
        let v32: Vec<[f32; 3]> = v
            .iter()
            .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            .collect();
        sutura_geom::topology::mesh_geometric_measures_core(&v32, t)
            .0
            .unwrap_or(0.0)
    };
    stage1.insert(
        "volume_before".into(),
        Value::from(geom_volume(&stage1_volume_before_v, &stage1_volume_before_t)),
    );
    stage1.insert("volume_after".into(), Value::from(geom_volume(&cur_v, &cur_t)));
    {
        let vb = geom_volume(&stage1_volume_before_v, &stage1_volume_before_t);
        let va = geom_volume(&cur_v, &cur_t);
        stage1.insert("volume_change_percent".into(), Value::Null);
        if vb != 0.0 {
            let pct = (va - vb) / vb.abs() * 100.0;
            stage1.insert(
                "volume_change_percent".into(),
                Value::from((pct * 100.0).round() / 100.0),
            );
        }
    }
    {
        let sa_before = py_surface_area(&stage1_volume_before_v, &stage1_volume_before_t);
        let sa_after = py_surface_area(&cur_v, &cur_t);
        let round3 = |x: f64| (x * 1000.0).round() / 1000.0;
        stage1.insert("surface_area_before".into(), Value::from(round3(sa_before)));
        stage1.insert("surface_area_after".into(), Value::from(round3(sa_after)));
        stage1.insert("surface_area_change_percent".into(), Value::Null);
        if sa_before != 0.0 {
            let pct = (sa_after - sa_before) / sa_before * 100.0;
            stage1.insert(
                "surface_area_change_percent".into(),
                Value::from((pct * 100.0).round() / 100.0),
            );
        }
    }
    stage1.insert("components_before".into(), Value::from(input_components));
    {
        let ut: Vec<[usize; 3]> = cur_t
            .iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect();
        // SI excise already measured the post-excision count.  Reuse it only
        // when nothing after SI excise could have changed the mesh: no deep
        // tier, no post-SI tamponade/closing/proxy/mirror/repeat/wall adoption
        // and no pinch split.  Otherwise recompute exactly as before.
        let post_si_adopted = |key: &str| {
            stats
                .get(key)
                .map(|r| adopted(r))
                .unwrap_or(false)
        };
        let deep_changed = stats
            .get("deep_repair")
            .and_then(|d| d.get("tiers_run"))
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        let pinch_changed = stats
            .get("pinched_vertices_split")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reuse_si = stats
            .get("si_excise")
            .filter(|r| {
                r.get("si_after").and_then(Value::as_i64).is_some()
                    && !deep_changed
                    && !pinch_changed
                    && !post_si_adopted("tamponade")
                    && !post_si_adopted("closing")
                    && !post_si_adopted("proxy_template")
                    && !post_si_adopted("mirror_complete")
                    && !post_si_adopted("repeat")
                    && !post_si_adopted("wall_thicken")
            })
            .and_then(|r| r.get("si_after"))
            .and_then(Value::as_i64);
        let (si, skipped) = match reuse_si {
            Some(si) => (si, false),
            None => {
                let (_m, si, skipped) =
                    sutura_geom::self_intersections_core(&cur_v, &ut);
                (si, skipped)
            }
        };
        stage1.insert(
            "self_intersections_remaining".into(),
            if skipped || si < 0 {
                Value::Null
            } else {
                Value::from(si)
            },
        );
        let si_work =
            cur_t.len() as i64 * (1 + nm_after as i64 + after.non_two_manifold_vertices as i64 + holes_after as i64);
        stage1.insert("si_work_units".into(), Value::from(si_work));
        stage1.insert("si_work_budget".into(), Value::from(15_000_000i64));
    }
    stage1.insert("applied_filters".into(), Value::from(applied));
    stage1.insert(
        "removed_small_parts".into(),
        removed_small.unwrap_or(Value::Null),
    );
    if skipped.as_object().map(|m| !m.is_empty()).unwrap_or(false) {
        stage1.insert("skipped".into(), skipped);
    }
    let possible = crate::report::possible_debris(
        &cur_v,
        &to_i32(&cur_t),
        min_component as usize,
        0.005,
    )
    .map(|d| {
        json!({
            "count": d.count,
            "capped": d.capped,
            "parts": d.parts.iter().map(|p| json!({
                "faces": p.faces,
                "volume_pct": p.volume_pct,
                "size_pct": p.size_pct,
                "distance_mm": p.distance_mm,
            })).collect::<Vec<_>>(),
        })
    });
    stage1.insert(
        "possible_debris".into(),
        possible.unwrap_or(Value::Null),
    );
    stats.insert("stage1".into(), Value::Object(stage1));
    if allow_si_escalation && si_escalation_allowed(opts) {
        stage1_snapshot = Some((cur_v.clone(), cur_t.clone()));
    }

    // Python `_deep_repair_offer`: name the final tier and list the tiers a
    // deeper mode could still run when damage remains.
    let final_tier = [
        "ftetwild", "cdt", "graft", "dressing", "splint", "coaptation", "local",
    ]
    .iter()
    .find(|tier| {
        let rec = match **tier {
            "ftetwild" => stats.get("experimental_ftetwild"),
            "local" => stats.get("local_remesh"),
            other => stats.get(other),
        };
        rec.map(adopted).unwrap_or(false)
    })
    .copied()
    .unwrap_or("stage1");
    if let Some(deep) = stats.get_mut("deep_repair").and_then(Value::as_object_mut) {
        deep.insert("final_tier".into(), Value::from(final_tier));
        if holes_after > 0 || nm_after > 0 {
            let mut tiers: Vec<&str> = Vec::new();
            if opts.deep_repair == DeepRepair::Off {
                tiers.push("local");
            }
            let within_cap = spec
                .ftetwild_max_faces
                .map(|cap| cur_t.len() <= cap as usize)
                .unwrap_or(true);
            if (opts.deep_repair == DeepRepair::Off
                || opts.deep_repair == DeepRepair::Local)
                && within_cap
                && crate::ftetwild::find_binary().is_some()
            {
                tiers.push("ftetwild");
            }
            if !tiers.is_empty() {
                let n_faces = input_t.len();
                let n_regions = (holes_after + nm_after) as f64;
                let mut estimate = Map::new();
                if tiers.contains(&"local") {
                    let e = 0.5 + 0.05 * n_regions + 0.02 * n_faces as f64 / 1000.0;
                    estimate.insert(
                        "local".into(),
                        Value::from((e * 10.0).round() / 10.0),
                    );
                }
                if tiers.contains(&"ftetwild") {
                    let e = (1e-3 * n_faces as f64).min(spec.ftetwild_timeout);
                    estimate.insert(
                        "ftetwild".into(),
                        Value::from((e * 10.0).round() / 10.0),
                    );
                }
                deep.insert(
                    "available".into(),
                    json!({
                        "holes_remaining": holes_after,
                        "nm_remaining": nm_after,
                        "tiers": tiers,
                        "estimate_s": Value::Object(estimate),
                    }),
                );
            }
        }
    }

    // --- Stage 2 -----------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 5, "stage 2");
    let t_stage2_start = Instant::now();
    if two_manifold && holes_after == 0 {
        let s2_in_v = cur_v.clone();
        let s2_in_t = cur_t.clone();
        let t32 = to_i32(&cur_t);
        let (s2_v, s2_t, s2_rep) = stage2_rebuild_core(&cur_v, &t32);
        if s2_rep.ok && !s2_t.is_empty() {
            cur_v = to_f64(&s2_v);
            cur_t = to_i64(&s2_t);
        }
        stage_fp_log("after_stage2", &cur_v, &cur_t);
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

    t_stage2 = t_stage2_start.elapsed().as_secs_f64();

    // --- P-WELD ------------------------------------------------------------
    if cancel.is_cancelled() {
        return cancelled();
    }
    emit_stage(sink, 6, "reload weld");
    let pw_in_v = cur_v.clone();
    let pw_in_t = cur_t.clone();
    crate::engines::run_and_replace(
        &selected_engines,
        "after_stage2",
        &mut cur_v,
        &mut cur_t,
        Some(cancel as &dyn sutura_geom::cancel::Cancel),
        0.01,
        &mut engine_entries,
    );
    let t_reload_start = Instant::now();
    let pw = crate::p_weld::p_weld_final_with_cancel(
        &cur_v,
        &cur_t,
        Some(cancel as &dyn sutura_geom::cancel::Cancel),
    );
    t_reload = t_reload_start.elapsed().as_secs_f64();
    if cancel.is_cancelled() {
        return cancelled();
    }
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
    stage_fp_log("after_pweld", &cur_v, &cur_t);

    // Flap debris cleanup: the Python path applies `drop_flap_debris` after
    // P-WELD when Flap was adopted. Mirror it here so the native CLI saves the
    // same mesh as the Python service path.
    let flap_adopted = stats.get("flap").map(adopted).unwrap_or(false);
    if flap_adopted {
        let params = flap_debris_params(policy.as_ref());
        let (cl_v, cl_t, info) = flap::debris(&input_v, &input_t, &cur_v, &cur_t, &params);
        if let Some(info) = info {
            let (bh, bnm) = reload_strict_holes_nm(&cur_v, &cur_t);
            let (ch, cnm) = reload_strict_holes_nm(&cl_v, &cl_t);
            if ch <= bh && cnm <= bnm {
                cur_v = cl_v;
                cur_t = cl_t;
                if let Some(obj) = stats.get_mut("flap").and_then(Value::as_object_mut) {
                    obj.insert("debris_cleanup".into(), info);
                }
            }
        }
    }
    stage_fp_log("after_flap_debris", &cur_v, &cur_t);

    crate::engines::run_and_replace(
        &selected_engines,
        "final_fallback",
        &mut cur_v,
        &mut cur_t,
        Some(cancel as &dyn sutura_geom::cancel::Cancel),
        0.01,
        &mut engine_entries,
    );
    stage_fp_log("after_final_fallback", &cur_v, &cur_t);
    let final_holes = boundary_loop_stats(&cur_t).0;
    let final_nm = common::topology_summary(&cur_v, &cur_t).non_two_manifold_edges;
    let reachable = common::is_two_manifold(cur_v.len(), &cur_t) && final_holes == 0;
    stats.insert("method_reached_watertight".into(), Value::Bool(reachable));
    stats.insert(
        "timings".into(),
        json!({
            "prepass": (t_prepass * 1000.0).round() / 1000.0,
            "stage1": (t_stage1 * 1000.0).round() / 1000.0,
            "deep_repair": (t_deep * 1000.0).round() / 1000.0,
            "stage2": (t_stage2 * 1000.0).round() / 1000.0,
            "reload_weld": (t_reload * 1000.0).round() / 1000.0,
            "file_total": ((t_start.elapsed().as_secs_f64()) * 1000.0).round() / 1000.0,
        }),
    );

    let mut unavailable = unavailable_tiers(ftetwild_bin.is_none().then_some(
        "fTetWild binary not found; set FTETWILD_BIN/SUTURA_FTETWILD_BIN or install third_party/ftetwild/bin/ftetwild",
    ));
    // Both sides of this merge added to the method-health block: go records the
    // Stage-1 method errors, and the engine slice marks the requested external
    // engines as unavailable. They are independent, so both are kept.
    if let Some(selected) = opts.engines.as_ref() {
        if let Some(obj) = unavailable.as_object_mut() {
            for name in selected {
                obj.insert(
                    name.clone(),
                    json!({
                        "ran": false,
                        "unavailable": true,
                        "reason": "external engine execution is not implemented in the Rust pipeline",
                    }),
                );
            }
        }
    }
    let (mut method_health, mut method_errors) = method_health_value(&stats, &unavailable);
    if let Some(arr) = method_errors.as_array_mut() {
        arr.extend(stage1_errors.clone());
    }
    if let Some(arr) = method_health
        .get_mut("errors")
        .and_then(Value::as_array_mut)
    {
        arr.extend(stage1_errors);
    }
    if let Some(selected) = opts.engines.as_ref() {
        if let Some(skips) = method_health
            .get_mut("skips")
            .and_then(Value::as_array_mut)
        {
            for name in selected {
                skips.push(json!({
                    "method": name,
                    "stage": "availability",
                    "kind": "skipped",
                    "message": "external engine execution is not implemented in the Rust pipeline",
                }));
            }
        }
    }
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
    if !engine_entries.is_empty() {
        let shape_changed = engine_entries.iter().any(|e| {
            e.get("shape_changed").and_then(Value::as_bool) == Some(true)
        });
        stats.insert("engines".into(), Value::Array(engine_entries));
        if shape_changed {
            stats.insert("shape_changed".into(), Value::Bool(true));
        }
    }
    if !engine_warnings.is_empty() {
        stats.insert(
            "engine_warnings".into(),
            Value::Array(engine_warnings.into_iter().map(Value::from).collect()),
        );
    }
    let mut report = Value::Object(stats);
    enforce_reload_verdict(&mut report, &cur_v, &cur_t, false);

    let mut out = match report {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let geom_change = {
        let mut worst: Option<f64> = None;
        if let Some(s1) = out.get("stage1").and_then(Value::as_object) {
            for key in ["volume_change_percent", "surface_area_change_percent"] {
                if let Some(v) = s1.get(key).and_then(Value::as_f64) {
                    let a = v.abs();
                    worst = Some(worst.map_or(a, |w| w.max(a)));
                }
            }
        }
        worst
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
            "geom_change_pct": geom_change.map(Value::from).unwrap_or(Value::Null),
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

    // Ranked fallback ladder (Python `triage._auto`), before the final SI
    // escalation.  Nested attempts disable auto_escalation, so this cannot
    // recurse; an adopted candidate replaces the whole baseline result.
    if let Some(ranking) = ranking.as_ref() {
        if !reachable && auto_ladder_allowed(opts) && !cancel.is_cancelled() {
            if let Some((adopted_v, adopted_t, adopted_report)) = run_auto_ladder(
                &input_v,
                &input_t,
                opts,
                policy.as_ref(),
                ranking,
                &mut out,
                cancel,
            ) {
                cur_v = adopted_v;
                cur_t = adopted_t;
                out = adopted_report;
            }
        }
    }
    stage_fp_log("after_auto_ladder", &cur_v, &cur_t);

    if allow_si_escalation && si_escalation_allowed(opts) {
        if let Some((adopted_v, adopted_t, adopted_report)) = apply_si_escalation(
            &mut out,
            &input_v,
            &input_t,
            &cur_v,
            &cur_t,
            opts,
            policy.as_ref(),
            stage1_snapshot.as_ref(),
            cancel,
        ) {
            stage_fp_log("after_si_final", &adopted_v, &adopted_t);
            return RepairOutcome::Ok {
                verts: adopted_v,
                tris: adopted_t,
                report: adopted_report,
            };
        }
    }
    stage_fp_log("after_si_final", &cur_v, &cur_t);

    RepairOutcome::Ok {
        verts: cur_v,
        tris: cur_t,
        report: Value::Object(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_ladder_gate_matches_python() {
        let mut o = RepairOptions::default();
        assert!(auto_ladder_allowed(&o));
        o.deep_repair = DeepRepair::Off;
        assert!(!auto_ladder_allowed(&o));
        o = RepairOptions::default();
        o.ftetwild = Some(Value::Bool(false));
        assert!(!auto_ladder_allowed(&o));
        o = RepairOptions::default();
        o.forced_methods = vec![3];
        assert!(!auto_ladder_allowed(&o));
        o = RepairOptions::default();
        o.ladder_depth = 1;
        assert!(!auto_ladder_allowed(&o));
        o = RepairOptions::default();
        o.auto_escalation = Some(false);
        assert!(!auto_ladder_allowed(&o));
    }

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

    fn push_box(verts: &mut Vec<[f64; 3]>, tris: &mut Vec<[i64; 3]>, origin: [f64; 3], size: f64) {
        let (v, t) = cube();
        let base = verts.len() as i64;
        for p in v {
            verts.push([
                origin[0] + p[0] * size,
                origin[1] + p[1] * size,
                origin[2] + p[2] * size,
            ]);
        }
        for tri in t {
            tris.push([tri[0] + base, tri[1] + base, tri[2] + base]);
        }
    }

    fn two_shells(nested: bool) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        push_box(&mut verts, &mut tris, [0.0, 0.0, 0.0], 2.0);
        if nested {
            push_box(&mut verts, &mut tris, [0.5, 0.5, 0.5], 1.0);
        } else {
            push_box(&mut verts, &mut tris, [1.5, 1.5, 1.5], 2.0);
        }
        (verts, tris)
    }

    #[test]
    fn si_escalation_verdict_rejects_not_strictly_lower() {
        let base = SiMetrics {
            si: Some(3),
            holes: Some(0),
            nm: Some(0),
            parts: Some(1),
            volume_mm3: Some(100.0),
            shape_max_mm: Some(0.1),
            shape_p95_mm: Some(0.0),
            ..SiMetrics::default()
        };
        let cand = SiMetrics {
            si: Some(4),
            ..base.clone()
        };
        let (ok, reason) = si_verdict(&base, &cand, Some(0.0125), 0.005);
        assert!(!ok);
        assert_eq!(reason, "SI not strictly lower");
    }

    #[test]
    fn si_escalation_triggers_on_small_positive_mesh() {
        let (verts, tris) = two_shells(false);
        assert!(si_exact(&verts, &tris).unwrap_or(0) > 0);
        let opts = RepairOptions {
            tamponade: Some(false),
            flap: Some(false),
            coaptation: Some(false),
            graft: Some(false),
            ..RepairOptions::default()
        };
        let mut out = Map::new();
        out.insert(
            "stage1".into(),
            json!({
                "holes_remaining": 0,
                "non_manifold_edges_remaining": 0,
                "components": 2,
            }),
        );
        out.insert(
            "methods_tried".into(),
            json!([{
                "num": 3,
                "id": "full_mend",
                "name": "Full Mend",
                "outcome": "accepted",
                "watertight": true,
            }]),
        );
        let snapshot = (verts.clone(), tris.clone());
        let adopted = apply_si_escalation(
            &mut out,
            &verts,
            &tris,
            &verts,
            &tris,
            &opts,
            None,
            Some(&snapshot),
            &CancelToken::new(),
        );
        let field = out
            .get("si_escalation")
            .and_then(Value::as_object)
            .expect("si_escalation object");
        assert_eq!(field.get("triggered").and_then(Value::as_bool), Some(true));
        let baseline_si = field
            .get("baseline")
            .and_then(|b| b.get("si"))
            .and_then(Value::as_i64)
            .expect("baseline SI");
        assert!(baseline_si > 0);
        let candidates = field
            .get("candidates")
            .and_then(Value::as_array)
            .expect("candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].get("num").and_then(Value::as_i64), Some(10));
        let tried = out
            .get("methods_tried")
            .and_then(Value::as_array)
            .expect("methods_tried");
        assert_eq!(tried.len(), 2);
        assert_eq!(tried[1].get("num").and_then(Value::as_i64), Some(10));
        if let Some((v, t, report)) = adopted {
            assert!(!v.is_empty() && !t.is_empty());
            assert!(report.get("methods_tried").is_some());
        }
    }

    #[test]
    fn should_extract_outer_shell_accepts_nested_and_overlapping_shells() {
        for nested in [true, false] {
            let (verts, tris) = two_shells(nested);
            assert!(
                should_extract_outer_shell(&verts, &tris, "mechanical", 0),
                "nested={nested} must be considered"
            );
        }
    }

    #[test]
    fn should_extract_outer_shell_rejects_single_open_and_organic() {
        let (verts, tris) = cube();
        assert!(!should_extract_outer_shell(&verts, &tris, "mechanical", 0));

        let (verts, tris) = two_shells(true);
        assert!(!should_extract_outer_shell(&verts, &tris, "organic", 0));

        let (mut verts, mut tris) = two_shells(true);
        tris.pop();
        assert!(!should_extract_outer_shell(&verts, &tris, "mechanical", 0));

        verts.clear();
        tris.clear();
        assert!(!should_extract_outer_shell(&verts, &tris, "mechanical", 0));
    }

    #[test]
    fn shell_prepass_noops_when_not_considered() {
        let (verts, tris) = cube();
        let mut got_v = verts.clone();
        let mut got_t = tris.clone();
        let mut stats = Map::new();
        run_outer_shell_prepass(&mut got_v, &mut got_t, "mechanical", true, &mut stats);
        assert_eq!(got_v, verts);
        assert_eq!(got_t, tris);
        assert!(!stats.contains_key("tamponade_shell_prepass"));
        assert!(!stats.contains_key("outer_shell_skipped"));
    }

    #[test]
    fn shell_prepass_skips_when_tamponade_disabled() {
        let (verts, tris) = two_shells(true);
        let mut got_v = verts.clone();
        let mut got_t = tris.clone();
        let mut stats = Map::new();
        run_outer_shell_prepass(&mut got_v, &mut got_t, "mechanical", false, &mut stats);
        assert_eq!(got_v, verts);
        assert_eq!(got_t, tris);
        assert_eq!(
            stats.get("outer_shell_skipped").and_then(Value::as_str),
            Some("tamponade_disabled")
        );
        assert!(!stats.contains_key("tamponade_shell_prepass"));
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
            wall_min_thickness: None,
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
        assert!(
            unavailable.get("dressing").is_none(),
            "dressing is compiled into sutura-core"
        );
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
    fn external_copy_engine_is_adopted_and_reported() {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "sutura-engine-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let engines = dir.join("sutura").join("engines");
        std::fs::create_dir_all(&engines).unwrap();
        std::fs::write(
            engines.join("copy.toml"),
            "name = \"copy\"\ncommand = [\"/bin/cp\", \"{input}\", \"{output}\"]\nplacement = \"after_stage1\"\n",
        )
        .unwrap();
        let old = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let (verts, tris) = cube();
        let opts = RepairOptions {
            engines: Some(vec!["copy".into()]),
            tamponade: Some(false),
            flap: Some(false),
            coaptation: Some(false),
            deep_repair: DeepRepair::Off,
            ..RepairOptions::default()
        };
        let outcome = repair(&verts, &tris, &opts, &NoopSink, &CancelToken::new());
        match outcome {
            RepairOutcome::Ok { report, .. } => {
                let entries = report
                    .get("engines")
                    .and_then(Value::as_array)
                    .expect("engines report");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0]["name"], json!("copy"));
                assert_eq!(entries[0]["adopted"], json!(true));
            }
            _ => panic!("unexpected outcome"),
        }
        match old {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
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
