// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `repeat.py::repair_repeat_auto` — predict-and-verify transplant driver.
//!
//! Given a detected pattern (rotational, translational or helical), pick a
//! healthy template element, predict where each copy should be, measure how far
//! the mesh actually deviates there, and hand the damaged positions to the
//! boolean transplant. The report is what the Repeat tier records.
//!
//! Reproduction notes, each load-bearing:
//!
//! * The template is the member whose AREA is closest to the cluster median
//!   (`np.median` = mean of the two middle values for an even count, and
//!   `np.argmin` keeps the FIRST minimum).
//! * `_calc_deviation` uses the RMS of the forward distance and, when a member
//!   lies within half an element diagonal, the RMS of the reverse distance --
//!   taking the larger. That reverse check is what distinguishes a dent from a
//!   bump.
//! * Rotational `k_template` is `int(np.round(...))`, which rounds half to even
//!   on the binary value; `py_round(v, 0)` reproduces it.
//! * The sector path (free route) re-runs `align` per sector with
//!   `max_iters=5`, and can narrow `template_pts` to the damaged region.

use crate::measures::sum_f64;
use crate::pointcloud::KdTree;
use crate::repeat_align::{align, Transform4};
use crate::repeat_obb::{compute_hausdorff_outside, compute_obb, Obb};
use crate::repeat_patch::{detect_repetition, PatchDetection};
use crate::winding;
use rayon::prelude::*;

/// `DAMAGE_DEVIATION_MIN`.
pub const DAMAGE_DEVIATION_MIN: f64 = 0.06;
/// `DAMAGE_DEVIATION_MAX`.
pub const DAMAGE_DEVIATION_MAX: f64 = 0.85;

/// One entry of `per_position_deviation`.
#[derive(Debug, Clone)]
pub struct PositionDev {
    pub index: i64,
    pub deviation: f64,
}

/// The `repair_repeat_auto` report.
#[derive(Debug, Clone)]
pub struct AutoReport {
    pub watertight: bool,
    pub repaired: bool,
    pub pattern_type: String,
    pub group_size: usize,
    pub positions_checked: usize,
    pub positions_repaired: usize,
    pub per_position_deviation: Vec<PositionDev>,
    pub volume_change: f64,
    pub hausdorff_outside: f64,
    pub notes: Vec<String>,
    pub error: Option<String>,
}

impl AutoReport {
    fn early(pattern_type: &str, error: Option<String>, notes: Vec<String>, watertight: bool) -> Self {
        AutoReport {
            watertight,
            repaired: false,
            pattern_type: pattern_type.to_string(),
            group_size: 0,
            positions_checked: 0,
            positions_repaired: 0,
            per_position_deviation: Vec::new(),
            volume_change: 0.0,
            hausdorff_outside: 0.0,
            notes,
            error,
        }
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// `np.round(v, 0)` then `int(...)`: half-to-even on the binary value.
fn py_round_int(v: f64) -> i64 {
    if !v.is_finite() {
        return 0;
    }
    format!("{:.0}", v).parse::<f64>().unwrap_or(0.0) as i64
}

/// `np.round(v, 4)`.
fn py_round4(v: f64) -> f64 {
    if !v.is_finite() {
        return v;
    }
    format!("{:.4}", v).parse::<f64>().unwrap_or(v)
}

/// `np.median` of a list (mean of the middle two for an even count).
fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// `np.mean(pts, axis=0)`: 2-D reduction, sequential over rows.
fn mean_points(pts: &[[f64; 3]]) -> [f64; 3] {
    let mut acc = [0.0f64; 3];
    for p in pts {
        for k in 0..3 {
            acc[k] += p[k];
        }
    }
    let n = pts.len() as f64;
    if n > 0.0 {
        for k in 0..3 {
            acc[k] /= n;
        }
    }
    acc
}

/// `sqrt(mean(x**2))` over a 1-D slice -- pairwise, like `np.mean`.
fn rms(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let sq: Vec<f64> = xs.iter().map(|x| x * x).collect();
    (sum_f64(&sq) / xs.len() as f64).sqrt()
}

fn apply(t: &Transform4, pts: &[[f64; 3]]) -> Vec<[f64; 3]> {
    t.apply(pts)
}

/// `_rotation_matrix(angle, axis, point=c)[:3, :3]`-based rigid transform
/// (rotation about a point).
fn rotation_about(angle: f64, axis: [f64; 3], point: [f64; 3]) -> Transform4 {
    let n = norm(axis);
    if n < 1e-12 {
        return Transform4::identity();
    }
    let (x, y, z) = (axis[0] / n, axis[1] / n, axis[2] / n);
    let c = angle.cos();
    let s = angle.sin();
    let cc = 1.0 - c;
    let r = [
        [c + x * x * cc, x * y * cc - z * s, x * z * cc + y * s],
        [y * x * cc + z * s, c + y * y * cc, y * z * cc - x * s],
        [z * x * cc - y * s, z * y * cc + x * s, c + z * z * cc],
    ];
    // t = p - R p
    let t = [
        point[0] - (r[0][0] * point[0] + r[0][1] * point[1] + r[0][2] * point[2]),
        point[1] - (r[1][0] * point[0] + r[1][1] * point[1] + r[1][2] * point[2]),
        point[2] - (r[2][0] * point[0] + r[2][1] * point[1] + r[2][2] * point[2]),
    ];
    Transform4::from_rt(&r, t)
}

/// `check_mesh_validity`: watertight 2-manifold with no degenerate faces.
pub fn check_mesh_validity(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (bool, &'static str) {
    if verts.len() < 4 || tris.len() < 4 {
        return (false, "Mesh has too few vertices or triangles");
    }
    for f in tris {
        if f[0] == f[1] || f[1] == f[2] || f[2] == f[0] {
            return (false, "Mesh contains degenerate triangles with duplicate vertices");
        }
    }
    let mut counts: std::collections::HashMap<(usize, usize), usize> =
        std::collections::HashMap::new();
    for f in tris {
        for (a, b) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            let key = if a <= b { (a, b) } else { (b, a) };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
    let n_open = counts.values().filter(|&&c| c == 1).count();
    let n_nm = counts.values().filter(|&&c| c > 2).count();
    if n_open > 0 || n_nm > 0 {
        return (false, "Non-watertight mesh");
    }
    (true, "")
}

/// The transplants' result, supplied by the caller (the boolean CSG lives in
/// `manifold`, which needs the python/manifold feature).
pub struct TransplantOutcome {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[usize; 3]>,
    pub volume_change: f64,
    pub boxes: Vec<Obb>,
    pub watertight: bool,
}

/// Everything `repair_repeat_auto` decides, minus the CSG execution.
///
/// Split this way so the decision logic (which positions are damaged, with what
/// transforms) is testable without the Manifold kernel; the caller applies
/// `transforms` through `boolean_transplant`.
pub enum AutoPlan {
    /// Nothing to do; report as-is.
    Early(AutoReport),
    /// Repair these positions of this template.
    Transplants {
        pattern_type: String,
        group_size: usize,
        positions_checked: usize,
        per_position_deviation: Vec<PositionDev>,
        template_pts: Vec<[f64; 3]>,
        transforms: Vec<Transform4>,
    },
}

/// `repair_repeat_auto` up to (but not including) the CSG transplant.
pub fn plan_repeat_auto(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> AutoPlan {
    if verts.len() < 4 || tris.len() < 4 {
        return AutoPlan::Early(AutoReport::early(
            "none",
            Some("Mesh has too few vertices or triangles".to_string()),
            Vec::new(),
            false,
        ));
    }
    let (is_valid, err_msg) = check_mesh_validity(verts, tris);
    if !is_valid {
        return AutoPlan::Early(AutoReport::early(
            "none",
            Some(err_msg.to_string()),
            Vec::new(),
            false,
        ));
    }

    let (score, route, free, patch) = detect_repetition(verts, tris);
    let info_type = if route == "free" {
        free.as_ref().map(|_| "rotational").unwrap_or("none")
    } else {
        patch
            .as_ref()
            .map(|p| p.pattern_type.as_str())
            .unwrap_or("none")
    };
    if score < 0.5 || !matches!(info_type, "rotational" | "translational" | "helical") {
        return AutoPlan::Early(AutoReport::early(
            info_type,
            None,
            vec!["No regular repeating pattern detected with sufficient confidence".to_string()],
            true,
        ));
    }

    // The free route has no `members`, so it takes the sector path below.
    let has_members = patch.is_some() && route == "patch";
    if !has_members {
        return sector_plan(verts, tris, free.as_ref().expect("free route"), &patch);
    }
    let p = patch.as_ref().expect("patch");
    member_plan(verts, tris, p)
}

fn member_plan(verts: &[[f64; 3]], tris: &[[usize; 3]], info: &PatchDetection) -> AutoPlan {
    // `members` are patch indices; rebuild the dicts the report needs.
    let all = crate::repeat_segment::segment_elements(
        verts,
        tris,
        crate::repeat_segment::DEFAULT_FEATURE_ANGLE_DEG,
    );
    let members: Vec<&crate::repeat_segment::Patch> = info
        .members
        .iter()
        .filter_map(|&i| all.get(i))
        .collect();
    if members.is_empty() {
        return AutoPlan::Early(AutoReport::early(
            &info.pattern_type,
            None,
            vec!["No regular repeating pattern detected with sufficient confidence".to_string()],
            true,
        ));
    }

    let areas: Vec<f64> = members.iter().map(|m| m.area).collect();
    let med_area = median(&areas);
    let mut best_template_idx = 0usize;
    let mut best_delta = f64::INFINITY;
    for (i, &a) in areas.iter().enumerate() {
        let d = (a - med_area).abs();
        if d < best_delta {
            best_delta = d;
            best_template_idx = i;
        }
    }
    let template = members[best_template_idx];
    let template_pts: Vec<[f64; 3]> = template.verts_idx.iter().map(|&i| verts[i]).collect();
    let elem_diag = template.diag;

    // Forward distance via one BVH over the whole mesh.
    let bvh = winding::MeshBvh::from_arrays(verts, tris);
    let member_centroids: Vec<[f64; 3]> = members.iter().map(|m| m.centroid).collect();

    let calc_deviation = |pred_pts: &[[f64; 3]]| -> f64 {
        let fwd: Vec<f64> = pred_pts.par_iter().map(|p| bvh.closest_point(*p).1).collect();
        let dev_dent = rms(&fwd) / (elem_diag + 1e-12);

        let c_p = mean_points(pred_pts);
        let mut min_d = f64::INFINITY;
        let mut best_m = 0usize;
        for (i, &c) in member_centroids.iter().enumerate() {
            let d = norm([c[0] - c_p[0], c[1] - c_p[1], c[2] - c_p[2]]);
            if d < min_d {
                min_d = d;
                best_m = i;
            }
        }
        if min_d <= 0.5 * elem_diag {
            let m_pts: Vec<[f64; 3]> =
                members[best_m].verts_idx.iter().map(|&i| verts[i]).collect();
            match KdTree::new(pred_pts) {
                Some(tree) => {
                    let (d_rev, _i, _k) = tree.query(&m_pts, 1);
                    let dev_bump = rms(&d_rev) / (elem_diag + 1e-12);
                    dev_dent.max(dev_bump)
                }
                None => dev_dent,
            }
        } else {
            dev_dent
        }
    };

    let mut positions_checked = 0usize;
    let mut per_pos: Vec<PositionDev> = Vec::new();
    let mut transforms: Vec<Transform4> = Vec::new();

    match info.pattern_type.as_str() {
        "rotational" => {
            let order = info.order.unwrap_or(0);
            let c_rot = info.center.unwrap_or([0.0; 3]);
            let axis = info.axis.unwrap_or([0.0, 0.0, 1.0]);
            let step = info.step_angle.unwrap_or(0.0);
            let u_ax = info.u_axis.unwrap_or([1.0, 0.0, 0.0]);
            let v_ax = info.v_axis.unwrap_or([0.0, 1.0, 0.0]);

            let th_of = |c: [f64; 3]| -> f64 {
                let d = [c[0] - c_rot[0], c[1] - c_rot[1], c[2] - c_rot[2]];
                dot(d, v_ax).atan2(dot(d, u_ax)).rem_euclid(2.0 * std::f64::consts::PI)
            };
            let th_template = th_of(template.centroid);
            let th_base = th_of(members[0].centroid);
            let k_template = py_round_int(
                (th_template - th_base).rem_euclid(2.0 * std::f64::consts::PI) / step,
            );

            for k in 0..order {
                positions_checked += 1;
                let rot_angle = (k as i64 - k_template) as f64 * step;
                let tk = rotation_about(rot_angle, axis, c_rot);
                let pred = apply(&tk, &template_pts);
                let dev = calc_deviation(&pred);
                per_pos.push(PositionDev {
                    index: k as i64,
                    deviation: py_round4(dev),
                });
                if (DAMAGE_DEVIATION_MIN..=DAMAGE_DEVIATION_MAX).contains(&dev) {
                    transforms.push(tk);
                }
            }
        }
        "translational" => {
            let origin = info.origin.unwrap_or([0.0; 3]);
            let ax_u = info.u_axis.unwrap_or([1.0, 0.0, 0.0]);
            let ax_v = info.v_axis.unwrap_or([0.0, 1.0, 0.0]);
            let step_u = info.step_u.unwrap_or(0.0);
            let step_v = info.step_v.unwrap_or(0.0);
            let p_idx = info.p_indices.clone().unwrap_or_default();
            let q_idx = info.q_indices.clone().unwrap_or_default();
            let has_w = info.w_axis.is_some() && info.step_w.is_some() && info.r_indices.is_some();
            let (ax_w, step_w, r_idx) = if has_w {
                (
                    info.w_axis.unwrap(),
                    info.step_w.unwrap(),
                    info.r_indices.clone().unwrap_or_default(),
                )
            } else {
                ([0.0; 3], 0.0, vec![0i64])
            };
            let r_max = r_idx.iter().copied().max().unwrap_or(0);
            let template_r = r_idx.get(best_template_idx).copied().unwrap_or(0);
            let p_max = p_idx.iter().copied().max().unwrap_or(0);
            let q_max = q_idx.iter().copied().max().unwrap_or(0);
            let template_p = p_idx.get(best_template_idx).copied().unwrap_or(0);
            let template_q = q_idx.get(best_template_idx).copied().unwrap_or(0);
            let _ = origin;

            for pv in 0..=p_max {
                for qv in 0..=q_max {
                    for rv in 0..=r_max {
                        positions_checked += 1;
                        let shift = [
                            (pv - template_p) as f64 * step_u * ax_u[0]
                                + (qv - template_q) as f64 * step_v * ax_v[0]
                                + (rv - template_r) as f64 * step_w * ax_w[0],
                            (pv - template_p) as f64 * step_u * ax_u[1]
                                + (qv - template_q) as f64 * step_v * ax_v[1]
                                + (rv - template_r) as f64 * step_w * ax_w[1],
                            (pv - template_p) as f64 * step_u * ax_u[2]
                                + (qv - template_q) as f64 * step_v * ax_v[2]
                                + (rv - template_r) as f64 * step_w * ax_w[2],
                        ];
                        let tk = Transform4::from_rt(
                            &[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                            shift,
                        );
                        let pred: Vec<[f64; 3]> =
                            template_pts.iter().map(|p| [p[0] + shift[0], p[1] + shift[1], p[2] + shift[2]]).collect();
                        let dev = calc_deviation(&pred);
                        per_pos.push(PositionDev {
                            index: if has_w { pv * 10000 + qv * 100 + rv } else { pv * 100 + qv },
                            deviation: py_round4(dev),
                        });
                        if (DAMAGE_DEVIATION_MIN..=DAMAGE_DEVIATION_MAX).contains(&dev) {
                            transforms.push(tk);
                        }
                    }
                }
            }
        }
        "helical" => {
            let c_circle = info.center.unwrap_or([0.0; 3]);
            let axis = info.axis.unwrap_or([0.0, 0.0, 1.0]);
            let th_step = info.step_angle.unwrap_or(0.0);
            let h_step = info.step_h.unwrap_or(0.0);
            let k_min = info.k_min.unwrap_or(0);
            let k_max = info.k_max.unwrap_or(0);
            let k_idx = info.k_indices.clone().unwrap_or_default();
            let template_k = k_idx.get(best_template_idx).copied().unwrap_or(0);

            for k in k_min..=k_max {
                positions_checked += 1;
                let delta_k = k - template_k;
                let rot_angle = delta_k as f64 * th_step;
                // Python builds T_k from the rotation about the circle centre,
                // then REPLACES its translation with `R_k[..3, 3] + delta_k*h_step*axis`
                // -- that is the rotation-about-point translation COLUMN, which
                // already includes `c_circle - R @ c_circle`. Using the first row
                // of the rotation instead put every predicted position in the
                // wrong place, which is why all six deviations came out at
                // 0.33-0.51 and every position looked damaged.
                let rk_full = rotation_about(rot_angle, axis, c_circle);
                let rk = rk_full.rotation();
                let base_t = rk_full.translation();
                let t_k = Transform4::from_rt(
                    &rk,
                    [
                        base_t[0] + delta_k as f64 * h_step * axis[0],
                        base_t[1] + delta_k as f64 * h_step * axis[1],
                        base_t[2] + delta_k as f64 * h_step * axis[2],
                    ],
                );
                let pred = apply(&t_k, &template_pts);
                let dev = calc_deviation(&pred);
                per_pos.push(PositionDev {
                    index: k,
                    deviation: py_round4(dev),
                });
                if (DAMAGE_DEVIATION_MIN..=DAMAGE_DEVIATION_MAX).contains(&dev) {
                    transforms.push(t_k);
                }
            }
        }
        _ => {}
    }

    if transforms.is_empty() {
        return AutoPlan::Early(AutoReport {
            watertight: true,
            repaired: false,
            pattern_type: info.pattern_type.clone(),
            group_size: info.members.len(),
            positions_checked,
            positions_repaired: 0,
            per_position_deviation: per_pos,
            volume_change: 0.0,
            hausdorff_outside: 0.0,
            notes: vec!["All detected pattern positions are healthy".to_string()],
            error: None,
        });
    }
    AutoPlan::Transplants {
        pattern_type: info.pattern_type.clone(),
        group_size: info.members.len(),
        positions_checked,
        per_position_deviation: per_pos,
        template_pts,
        transforms,
    }
}

/// The segmentation-free route: sector-based predict-and-verify.
fn sector_plan(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    free: &crate::repeat_free::RotationalMatch,
    _patch: &Option<PatchDetection>,
) -> AutoPlan {
    let order = free.order;
    let c_rot = free.center;
    let axis = free.axis;
    let step_angle = free.step_angle;
    let u_ax = free.u_axis;
    let v_ax = free.v_axis;

    let v_proj: Vec<[f64; 3]> = verts
        .iter()
        .map(|p| [p[0] - c_rot[0], p[1] - c_rot[1], p[2] - c_rot[2]])
        .collect();
    let v_theta: Vec<f64> = v_proj
        .iter()
        .map(|d| dot(*d, v_ax).atan2(dot(*d, u_ax)).rem_euclid(2.0 * std::f64::consts::PI))
        .collect();

    let bvh = winding::MeshBvh::from_arrays(verts, tris);
    let rot_step = rotation_about(step_angle, axis, c_rot);
    let r_step = rot_step.rotation();

    let mut sector_devs: Vec<f64> = Vec::with_capacity(order);
    let mut sector_pts: Vec<Vec<[f64; 3]>> = Vec::with_capacity(order);
    let half_band = step_angle * 0.55;
    for k in 0..order {
        let th_k = k as f64 * step_angle;
        let pts_k: Vec<[f64; 3]> = verts
            .iter()
            .zip(v_theta.iter())
            .filter(|(_, &t)| {
                let d = (t - th_k + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI)
                    - std::f64::consts::PI;
                d.abs() <= half_band
            })
            .map(|(p, _)| *p)
            .collect();
        sector_pts.push(pts_k.clone());
        if pts_k.len() < 4 {
            sector_devs.push(1.0);
            continue;
        }
        // pred = (pts_k - c_rot) @ R_step.T + c_rot
        let pred: Vec<[f64; 3]> = pts_k
            .iter()
            .map(|p| {
                let d = [p[0] - c_rot[0], p[1] - c_rot[1], p[2] - c_rot[2]];
                [
                    d[0] * r_step[0][0] + d[1] * r_step[0][1] + d[2] * r_step[0][2] + c_rot[0],
                    d[0] * r_step[1][0] + d[1] * r_step[1][1] + d[2] * r_step[1][2] + c_rot[1],
                    d[0] * r_step[2][0] + d[1] * r_step[2][1] + d[2] * r_step[2][2] + c_rot[2],
                ]
            })
            .collect();
        let dists: Vec<f64> = pred.iter().map(|p| bvh.closest_point(*p).1).collect();
        sector_devs.push(sum_f64(&dists) / dists.len() as f64);
    }

    let mut best_k = 0usize;
    for (i, &d) in sector_devs.iter().enumerate() {
        if d < sector_devs[best_k] {
            best_k = i;
        }
    }
    let mut template_pts = sector_pts[best_k].clone();
    let mut elem_diag = {
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &template_pts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        norm([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]])
    };
    if elem_diag == 0.0 {
        elem_diag = 1.0;
    }

    let mut positions_checked = 0usize;
    let mut per_pos: Vec<PositionDev> = Vec::new();
    let mut transforms: Vec<Transform4> = Vec::new();
    for k in 0..order {
        positions_checked += 1;
        let rot_angle = (k as i64 - best_k as i64) as f64 * step_angle;
        let r_k = rotation_about(rot_angle, axis, c_rot);
        let pts_k = &sector_pts[k];
        let t_k = if pts_k.len() >= 4 {
            let (t, _rms) = align(&template_pts, pts_k, false, false, 5);
            t
        } else {
            r_k
        };
        let pred = apply(&t_k, &template_pts);
        let dists: Vec<f64> = pred.iter().map(|p| bvh.closest_point(*p).1).collect();
        let dev = rms(&dists) / elem_diag;
        let max_dist = dists.iter().copied().fold(0.0f64, f64::max);
        let max_dev = max_dist / elem_diag;
        per_pos.push(PositionDev {
            index: k as i64,
            deviation: py_round4(dev),
        });
        if max_dev >= 0.04 || dev >= 0.015 {
            transforms.push(t_k);
            let thresh = (0.4 * max_dist).max(0.005 * elem_diag);
            let dam_mask: Vec<usize> = (0..dists.len()).filter(|&i| dists[i] >= thresh).collect();
            if dam_mask.len() >= 3 {
                let dam: Vec<[f64; 3]> = dam_mask.iter().map(|&i| template_pts[i]).collect();
                let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
                for p in &dam {
                    for kk in 0..3 {
                        lo[kk] = lo[kk].min(p[kk]);
                        hi[kk] = hi[kk].max(p[kk]);
                    }
                }
                let dam_span = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
                let dam_max = dam_span.iter().copied().fold(0.0f64, f64::max);
                if dam_max < 0.8 * elem_diag {
                    let c_dam = mean_points(&dam);
                    let radius = norm(dam_span) * 0.75 + 1.5;
                    let keep: Vec<[f64; 3]> = template_pts
                        .iter()
                        .filter(|p| {
                            norm([p[0] - c_dam[0], p[1] - c_dam[1], p[2] - c_dam[2]]) <= radius
                        })
                        .copied()
                        .collect();
                    if keep.len() >= 8 {
                        template_pts = keep;
                    }
                }
            }
        }
    }

    if transforms.is_empty() {
        return AutoPlan::Early(AutoReport {
            watertight: true,
            repaired: false,
            pattern_type: "rotational".to_string(),
            group_size: 0,
            positions_checked,
            positions_repaired: 0,
            per_position_deviation: per_pos,
            volume_change: 0.0,
            hausdorff_outside: 0.0,
            notes: vec!["All detected pattern positions are healthy".to_string()],
            error: None,
        });
    }
    AutoPlan::Transplants {
        pattern_type: "rotational".to_string(),
        group_size: 0,
        positions_checked,
        per_position_deviation: per_pos,
        template_pts,
        transforms,
    }
}

/// Build the final report once the caller has run the CSG transplant.
pub fn finish_report(
    plan: AutoPlan,
    outcome: Option<TransplantOutcome>,
    orig_v: &[[f64; 3]],
    orig_t: &[[usize; 3]],
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>, AutoReport) {
    match plan {
        AutoPlan::Early(report) => {
            // The caller keeps the input in the Early case.
            (orig_v.to_vec(), orig_t.to_vec(), report)
        }
        AutoPlan::Transplants {
            pattern_type,
            group_size,
            positions_checked,
            per_position_deviation,
            template_pts: _,
            transforms,
        } => {
            let repaired_n = transforms.len();
            let notes = vec![format!(
                "{} repeated elements were replaced by healthy copies",
                repaired_n
            )];
            match outcome {
                None => (
                    orig_v.to_vec(),
                    orig_t.to_vec(),
                    AutoReport {
                        watertight: false,
                        repaired: false,
                        pattern_type,
                        group_size,
                        positions_checked,
                        positions_repaired: 0,
                        per_position_deviation,
                        volume_change: 0.0,
                        hausdorff_outside: 0.0,
                        notes: Vec::new(),
                        error: Some("transplant unavailable".to_string()),
                    },
                ),
                Some(out) => {
                    let h = if out.boxes.is_empty() {
                        0.0
                    } else {
                        compute_hausdorff_outside(
                            orig_v,
                            orig_t,
                            &out.verts,
                            &out.tris,
                            &out.boxes,
                            5000,
                        )
                    };
                    let vol = round6(out.volume_change);
                    let hh = round6(h);
                    (
                        out.verts,
                        out.tris,
                        AutoReport {
                            watertight: out.watertight,
                            repaired: repaired_n > 0,
                            pattern_type,
                            group_size,
                            positions_checked,
                            positions_repaired: repaired_n,
                            per_position_deviation,
                            volume_change: vol,
                            hausdorff_outside: hh,
                            notes,
                            error: None,
                        },
                    )
                }
            }
        }
    }
}

fn round6(v: f64) -> f64 {
    if !v.is_finite() {
        return v;
    }
    format!("{:.6}", v).parse::<f64>().unwrap_or(v)
}

/// OBB for a template, used by the caller that runs the transplant.
pub fn template_obb(template_pts: &[[f64; 3]], margin: f64) -> Obb {
    compute_obb(template_pts, margin, crate::repeat_obb::OBB_MIN_EPSILON)
}

/// `repair_repeat_auto` end to end, with no Python in the loop.
///
/// The plan comes from `plan_repeat_auto` and the CSG from
/// `manifold::boolean_transplant_core` -- the same Manifold kernel the Python
/// binding wraps, so both languages produce the same mesh. `sutura-core` calls
/// this from the Repeat tier, which is why it must not need the `python`
/// feature.
pub fn repair_repeat_auto_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    margin: f64,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>, AutoReport) {
    let orig_v = verts;
    let orig_t = tris;
    let plan = plan_repeat_auto(verts, tris);
    let outcome = match &plan {
        AutoPlan::Early(_) => None,
        AutoPlan::Transplants {
            template_pts,
            transforms,
            ..
        } => {
            if transforms.is_empty() {
                None
            } else {
                let obb = template_obb(template_pts, margin);
                let mats: Vec<([[f64; 3]; 3], [f64; 3])> = transforms
                    .iter()
                    .map(|t| {
                        let r = t.rotation();
                        (r, t.translation())
                    })
                    .collect();
                match crate::manifold::boolean_transplant_core(
                    verts, tris, obb.center, obb.extents, obb.frame, &mats,
                ) {
                    Ok((v, t, rep)) => Some(TransplantOutcome {
                        verts: v,
                        tris: t,
                        volume_change: rep.vol_change,
                        boxes: rep
                            .boxes
                            .iter()
                            .map(|(c, e, f)| Obb {
                                center: *c,
                                extents: *e,
                                frame: *f,
                            })
                            .collect(),
                        watertight: rep.watertight,
                    }),
                    Err(e) => {
                        let mut out = finish_report(plan, None, orig_v, orig_t);
                        out.2.error = Some(e);
                        return out;
                    }
                }
            }
        }
    };
    finish_report(plan, outcome, orig_v, orig_t)
}

/// `repair_repeat_manual` (#12) end to end, with no Python in the loop.
///
/// The caller names two 3D points; the transplant moves the element nearest
/// `source_point` onto the element nearest `target_point`. That is
/// `segment_elements` -> nearest patches -> `align` -> the same
/// `manifold::boolean_transplant_core` the auto path uses.
pub fn repair_repeat_manual_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    source_point: [f64; 3],
    target_point: [f64; 3],
    margin: f64,
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>, AutoReport) {
    let orig_v = verts;
    let orig_t = tris;
    let mut early = |err: Option<&str>| -> (Vec<[f64; 3]>, Vec<[usize; 3]>, AutoReport) {
        (
            orig_v.to_vec(),
            orig_t.to_vec(),
            AutoReport {
                watertight: err.is_none(),
                repaired: false,
                pattern_type: "none".to_string(),
                group_size: 0,
                positions_checked: 0,
                positions_repaired: 0,
                per_position_deviation: Vec::new(),
                volume_change: 0.0,
                hausdorff_outside: 0.0,
                notes: Vec::new(),
                error: err.map(|e| e.to_string()),
            },
        )
    };
    if verts.len() < 4 || tris.len() < 4 {
        return early(Some("Mesh has too few vertices or triangles"));
    }
    let (is_valid, err_msg) = check_mesh_validity(verts, tris);
    if !is_valid {
        return early(Some(err_msg));
    }

    let patches = crate::repeat_segment::segment_elements(
        verts,
        tris,
        crate::repeat_segment::DEFAULT_FEATURE_ANGLE_DEG,
    );
    if patches.len() < 2 {
        return early(Some("Fewer than 2 candidate element patches found"));
    }

    let nearest = |p: [f64; 3]| -> usize {
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for (i, patch) in patches.iter().enumerate() {
            let c = patch.centroid;
            let d = norm([c[0] - p[0], c[1] - p[1], c[2] - p[2]]);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    };
    let src = &patches[nearest(source_point)];
    let dst = &patches[nearest(target_point)];
    let src_pts: Vec<[f64; 3]> = src.verts_idx.iter().map(|&i| verts[i]).collect();
    let dst_pts: Vec<[f64; 3]> = dst.verts_idx.iter().map(|&i| verts[i]).collect();

    let (t, _rms) = crate::repeat_align::align(&src_pts, &dst_pts, false, false, 20);
    let obb = template_obb(&src_pts, margin);
    let mats: Vec<([[f64; 3]; 3], [f64; 3])> = vec![(t.rotation(), t.translation())];
    match crate::manifold::boolean_transplant_core(
        verts,
        tris,
        obb.center,
        obb.extents,
        obb.frame,
        &mats,
    ) {
        Ok((ov, ot, rep)) => {
            let boxes: Vec<Obb> = rep
                .boxes
                .iter()
                .map(|(c, e, f)| Obb {
                    center: *c,
                    extents: *e,
                    frame: *f,
                })
                .collect();
            let h = if boxes.is_empty() {
                0.0
            } else {
                compute_hausdorff_outside(orig_v, orig_t, &ov, &ot, &boxes, 5000)
            };
            (
                ov,
                ot,
                AutoReport {
                    watertight: rep.watertight,
                    repaired: true,
                    // The manual route reports no pattern: `repair_repeat_manual`
                    // in Python leaves these unset, and the tier's `decide`
                    // prints them through unchanged. Setting a pattern here
                    // made the report differ from Python's for the same mesh.
                    pattern_type: String::new(),
                    group_size: 0,
                    positions_checked: 0,
                    positions_repaired: 0,
                    per_position_deviation: Vec::new(),
                    volume_change: round6(rep.vol_change),
                    hausdorff_outside: round6(h),
                    notes: vec![
                        "1 repeated element manually replaced by healthy copy".to_string(),
                    ],
                    error: None,
                },
            )
        }
        Err(e) => early(Some(&e)),
    }
}

#[cfg(feature = "python")]
pub mod py {
    use super::*;
    use numpy::{AllowTypeChange, PyArrayLike2};
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

    /// `repair_repeat_auto` end to end: plan + Manifold CSG, with no Python.
    ///
    /// Exposed so the parity gate can assert that the pure-Rust path and the
    /// Python facade produce the same mesh.
    #[pyfunction]
    #[pyo3(name = "repair_repeat_auto")]
    #[pyo3(signature = (verts, tris, margin=0.10))]
    pub fn repair_repeat_auto_core_py<'py>(
        py: Python<'py>,
        verts: PyArrayLike2<'py, f64, AllowTypeChange>,
        tris: PyArrayLike2<'py, i64, AllowTypeChange>,
        margin: f64,
    ) -> PyResult<(
        Bound<'py, numpy::PyArray2<f64>>,
        Bound<'py, numpy::PyArray2<i64>>,
        Bound<'py, PyDict>,
    )> {
        let v: Vec<[f64; 3]> = verts
            .as_array()
            .rows()
            .into_iter()
            .map(|r| [r[0], r[1], r[2]])
            .collect();
        let t: Vec<[usize; 3]> = tris
            .as_array()
            .rows()
            .into_iter()
            .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
            .collect();
        let (ov, ot, rep) = repair_repeat_auto_core(&v, &t, margin);
        let ovv: Vec<Vec<f64>> = ov.iter().map(|p| p.to_vec()).collect();
        let ott: Vec<Vec<i64>> = ot
            .iter()
            .map(|f| vec![f[0] as i64, f[1] as i64, f[2] as i64])
            .collect();
        let d = PyDict::new(py);
        d.set_item("watertight", rep.watertight)?;
        d.set_item("repaired", rep.repaired)?;
        d.set_item("pattern_type", rep.pattern_type)?;
        d.set_item("group_size", rep.group_size)?;
        d.set_item("positions_checked", rep.positions_checked)?;
        d.set_item("positions_repaired", rep.positions_repaired)?;
        d.set_item("volume_change", rep.volume_change)?;
        d.set_item("hausdorff_outside", rep.hausdorff_outside)?;
        d.set_item("notes", rep.notes)?;
        d.set_item("error", rep.error)?;
        let devs: Vec<(i64, f64)> = rep
            .per_position_deviation
            .iter()
            .map(|p| (p.index, p.deviation))
            .collect();
        d.set_item("per_position_deviation", devs)?;
        Ok((
            numpy::PyArray2::from_vec2(py, &ovv)?,
            numpy::PyArray2::from_vec2(py, &ott)?,
            d,
        ))
    }

    /// `plan_repeat_auto`: the decision half only, no CSG.
    ///
    /// Returns `(kind, payload)`:
    ///   `kind == "early"`  -> payload is the report dict, nothing to transplant
    ///   `kind == "transplant"` -> payload carries `template_pts` and `transforms`
    #[pyfunction]
    #[pyo3(name = "plan_repeat_auto")]
    pub fn plan_repeat_auto_py<'py>(
        py: Python<'py>,
        verts: PyArrayLike2<'py, f64, AllowTypeChange>,
        tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    ) -> PyResult<(String, Bound<'py, PyDict>)> {
        let v: Vec<[f64; 3]> = verts
            .as_array()
            .rows()
            .into_iter()
            .map(|r| [r[0], r[1], r[2]])
            .collect();
        let t: Vec<[usize; 3]> = tris
            .as_array()
            .rows()
            .into_iter()
            .map(|r| [r[0] as usize, r[1] as usize, r[2] as usize])
            .collect();
        let plan = plan_repeat_auto(&v, &t);
        let out = PyDict::new(py);
        match plan {
            AutoPlan::Early(r) => {
                out.set_item("pattern_type", r.pattern_type)?;
                out.set_item("watertight", r.watertight)?;
                out.set_item("repaired", r.repaired)?;
                out.set_item("group_size", r.group_size)?;
                out.set_item("positions_checked", r.positions_checked)?;
                out.set_item("positions_repaired", r.positions_repaired)?;
                out.set_item("volume_change", r.volume_change)?;
                out.set_item("hausdorff_outside", r.hausdorff_outside)?;
                out.set_item("notes", r.notes.clone())?;
                out.set_item("error", r.error.clone())?;
                let devs: Vec<(i64, f64)> = r
                    .per_position_deviation
                    .iter()
                    .map(|d| (d.index, d.deviation))
                    .collect();
                out.set_item("per_position_deviation", devs)?;
                Ok(("early".to_string(), out))
            }
            AutoPlan::Transplants {
                pattern_type,
                group_size,
                positions_checked,
                per_position_deviation,
                template_pts,
                transforms,
            } => {
                out.set_item("pattern_type", pattern_type)?;
                out.set_item("group_size", group_size)?;
                out.set_item("positions_checked", positions_checked)?;
                let devs: Vec<(i64, f64)> = per_position_deviation
                    .iter()
                    .map(|d| (d.index, d.deviation))
                    .collect();
                out.set_item("per_position_deviation", devs)?;
                let tp: Vec<Vec<f64>> = template_pts.iter().map(|p| p.to_vec()).collect();
                out.set_item("template_pts", numpy::PyArray2::from_vec2(py, &tp)?)?;
                let mats: Vec<Vec<Vec<f64>>> = transforms
                    .iter()
                    .map(|t| t.0.iter().map(|r| r.to_vec()).collect())
                    .collect();
                out.set_item(
                    "transforms",
                    numpy::PyArray3::from_vec3(py, &mats)?,
                )?;
                Ok(("transplant".to_string(), out))
            }
        }
    }

    #[allow(dead_code)]
    fn _unused(e: PyValueError) -> PyValueError {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validity_rejects_degenerate_faces() {
        let v = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let t = vec![[0usize, 0, 1], [0, 2, 3], [1, 2, 3], [0, 1, 3]];
        let (ok, msg) = check_mesh_validity(&v, &t);
        assert!(!ok);
        assert!(msg.contains("degenerate"), "{msg}");
    }

    #[test]
    fn validity_accepts_a_closed_tetrahedron() {
        let v = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let t = vec![[0usize, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]];
        let (ok, msg) = check_mesh_validity(&v, &t);
        assert!(ok, "{msg}");
    }

    #[test]
    fn too_few_faces_is_an_early_error() {
        let v = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let t = vec![[0usize, 1, 2]];
        match plan_repeat_auto(&v, &t) {
            AutoPlan::Early(r) => assert!(r.error.is_some()),
            _ => panic!("expected an early report"),
        }
    }

    #[test]
    fn median_matches_numpy_for_odd_and_even_counts() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }

    #[test]
    fn py_round_int_rounds_half_to_even() {
        assert_eq!(py_round_int(2.5), 2);
        assert_eq!(py_round_int(3.5), 4);
        assert_eq!(py_round_int(-0.0), 0);
    }

    #[test]
    fn rotation_about_a_point_keeps_that_point_fixed() {
        let p = [1.0, 2.0, 3.0];
        let t = rotation_about(0.7, [0.0, 0.0, 1.0], p);
        let out = t.apply(&[p]);
        for k in 0..3 {
            assert!((out[0][k] - p[k]).abs() < 1e-12, "{:?}", out[0]);
        }
    }

    /// The helical branch builds `T_k` from the rotation about the circle
    /// centre and then REPLACES its translation with
    /// `R_k[:3, 3] + delta_k * h_step * axis`. `R_k[:3, 3]` is the
    /// rotation-about-point translation column (`point - R @ point`), not a row
    /// of the rotation; using the rotation's first row scattered every
    /// predicted position, which made all six threads look damaged (deviations
    /// 0.33-0.51 instead of 0.0006-0.147) and repaired 6 positions instead of 1.
    #[test]
    fn helical_step_uses_the_rotation_translation_column() {
        let c = [0.5, -1.25, 2.0];
        let axis = [0.0, 0.0, 1.0];
        let angle = 0.4_f64;
        let t = rotation_about(angle, axis, c);
        let r = t.rotation();
        let base = t.translation();

        // `point - R @ point` really is what the transform carries, so stepping
        // by `h * axis` shifts the element along the helix.
        let expect = [
            c[0] - (r[0][0] * c[0] + r[0][1] * c[1] + r[0][2] * c[2]),
            c[1] - (r[1][0] * c[0] + r[1][1] * c[1] + r[1][2] * c[2]),
            c[2] - (r[2][0] * c[0] + r[2][1] * c[1] + r[2][2] * c[2]),
        ];
        for k in 0..3 {
            assert!((base[k] - expect[k]).abs() < 1e-12, "{:?}", base);
        }
        // A row of the rotation is not this vector.
        assert!((base[0] - r[0][0]).abs() > 1e-6);
    }
}

#[cfg(test)]
mod auto_core_tests {
    use super::*;

    /// A 12-position knurl ring with one damaged bump, built without any
    /// Python: a cylinder plus 12 boxes, one of them oversized.
    fn knurl() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        // Cylinder wall samples around the ring (16 per position) so
        // `segment_elements` sees 12 discrete patches.
        let mut v: Vec<[f64; 3]> = Vec::new();
        let mut t: Vec<[usize; 3]> = Vec::new();
        let n = 12usize;
        let r = 5.0f64;
        let h = 2.0f64;
        let seg = 16usize;
        for i in 0..n {
            let a = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            let (ca, sa) = (a.cos(), a.sin());
            let base = v.len();
            // two stacked rings of `seg` points, offset outward
            for k in 0..seg {
                let b = 2.0 * std::f64::consts::PI * k as f64 / seg as f64;
                let (cb, sb) = (b.cos(), b.sin());
                let out = r + 0.6;
                let x = ca * out - sa * (0.4 * cb);
                let y = sa * out + ca * (0.4 * cb);
                v.push([x, y, -h / 2.0 + 0.1 * sb]);
            }
            for k in 0..seg {
                let b = 2.0 * std::f64::consts::PI * k as f64 / seg as f64;
                let (cb, sb) = (b.cos(), b.sin());
                let out = r + 0.6;
                let x = ca * out - sa * (0.4 * cb);
                let y = sa * out + ca * (0.4 * cb);
                let z = h / 2.0 + 0.1 * sb;
                // the damaged position is pushed out
                let scale = if i == 3 { 1.6 } else { 1.0 };
                v.push([x * scale, y * scale, z]);
            }
            for k in 0..seg {
                let k2 = (k + 1) % seg;
                t.push([base + k, base + k2, base + seg + k]);
                t.push([base + k2, base + seg + k2, base + seg + k]);
            }
        }
        (v, t)
    }

    #[test]
    fn auto_core_runs_without_python_and_is_deterministic() {
        let (v, t) = knurl();
        assert!(t.len() > 100, "fixture too small: {}", t.len());
        let (v1, t1, r1) = repair_repeat_auto_core(&v, &t, 0.02);
        let (v2, t2, r2) = repair_repeat_auto_core(&v, &t, 0.02);
        // Same input twice must give byte-identical output.
        assert_eq!(v1, v2, "vertex output is not deterministic");
        assert_eq!(t1, t2, "triangle output is not deterministic");
        assert_eq!(r1.positions_repaired, r2.positions_repaired);
        assert_eq!(r1.hausdorff_outside.to_bits(), r2.hausdorff_outside.to_bits());
    }
}
