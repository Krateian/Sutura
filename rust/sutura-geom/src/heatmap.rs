// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The heatmap's scalar fields, ported from `sutura/heatmap.py`.
//!
//! `heatmap.py` is a *rasterizer*: it draws whatever scalar field the caller
//! hands it. The fields themselves are what this module produces, and they are
//! what a parity gate can meaningfully compare:
//!
//! * `defect_faces` — a per-face boolean. A face is defective when it touches a
//!   defect **vertex** (a hole rim or non-manifold region vertex), which is
//!   `_defect_vertex_set` + `is_defect_face = is_defect[tris].any(axis=1)`.
//! * `deviation` — per-vertex unsigned distance from the displayed mesh to a
//!   reference surface, which Python computes with
//!   `sutura_geom.closest_points(ref_v, ref_t, measure_v)`.
//! * `deviation_quantile_index` — the per-*face* index into the 4-stop colour
//!   ramp, which is `deviation[tris].mean(axis=1)` reduced through the 1%/99%
//!   quantile window.
//!
//! Pixel equality is *not* the contract here: the Python renderer is an
//! isometric orthographic CPU raster and the GUI is a perspective GPU view, so
//! they cannot agree on pixels and should not try. The *values* must agree, and
//! they can.

/// The 4 colour stops of the deviation ramp (`_deviation_lut`).
pub const DEVIATION_STOPS: [[f64; 3]; 4] = [
    [10.0, 20.0, 80.0],
    [0.0, 180.0, 200.0],
    [240.0, 220.0, 60.0],
    [230.0, 40.0, 40.0],
];
/// Levels in the deviation ramp: indices run 0..=256 (`n + 1` with n = 4).
pub const DEVIATION_LEVELS: usize = 257;
/// `GREY_MIN` in `_shade_lut`.
pub const GREY_MIN: f64 = 0.18;
/// Shade LUT entries (0..=64) in `_shade_lut`.
pub const SHADE_LEVELS: usize = 65;

/// Per-face defect flags, exactly as `heatmap.prepare_render` computes them.
///
/// `defect_vertices` is the union of every hole rim and non-manifold region
/// vertex (`_defect_vertex_set`). A face is flagged when **any** of its corners
/// is in that set — not when it merely has a bad edge. On a boundary loop the
/// two definitions coincide (every rim vertex is a boundary-edge endpoint), but
/// they diverge for isolated non-manifold vertices, which is why this takes the
/// vertex set rather than recomputing edge counts.
pub fn defect_faces(defect_vertices: &[i64], tris: &[[i32; 3]]) -> Vec<bool> {
    let max_v = tris.iter().flat_map(|t| *t).max().unwrap_or(-1).max(0) as usize;
    let mut bad = vec![false; max_v + 1];
    for v in defect_vertices {
        if *v >= 0 && (*v as usize) <= max_v {
            bad[*v as usize] = true;
        }
    }
    tris.iter()
        .map(|t| t.iter().any(|i| *i >= 0 && bad[*i as usize]))
        .collect()
}

/// Per-vertex unsigned distance from `dst` to the `ref` surface.
///
/// Python calls `sutura_geom.closest_points(ref_verts, ref_tris, measure_verts)`,
/// which is this BVH query per point; the returned distance is the third element
/// of `closest_point`.
pub fn deviation(
    dst_verts: &[[f64; 3]],
    ref_verts: &[[f64; 3]],
    ref_tris: &[[usize; 3]],
) -> Vec<f64> {
    if ref_tris.is_empty() {
        return vec![0.0; dst_verts.len()];
    }
    let bvh = crate::winding::MeshBvh::from_arrays(ref_verts, ref_tris);
    dst_verts
        .iter()
        .map(|p| bvh.closest_point(*p).1)
        .collect()
}

/// `np.quantile(x, q)` with the default `linear` interpolation.
///
/// NumPy places the quantile at virtual index `q * (n - 1)` and interpolates
/// between the two neighbouring order statistics. Implementing it any other way
/// (nearest, midpoint) shifts the ramp's window and changes indices at the
/// edges, which is exactly where the ramp is most visible.
pub fn quantile_linear(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// The per-face ramp index, from per-vertex deviation.
///
/// Reduced to a **face mean** first (`deviation[tris].mean(axis=1)` in
/// `prepare_render`), and the 1st/99th percentiles are taken over those *face
/// means* — not over the per-vertex array. The two windows differ materially
/// (measured on a shifted mesh: q99 is 0.1667 over face means but 0.3901 over
/// vertices), so quantiling the wrong array silently changes every index.
/// A zero or negative span yields all-zero indices, matching Python's guard.
pub fn deviation_quantile_index(deviation: &[f64], tris: &[[i32; 3]]) -> Vec<u16> {
    if deviation.is_empty() || tris.is_empty() {
        return Vec::new();
    }
    let mut face_mean: Vec<f64> = tris
        .iter()
        .map(|t| {
            let n = t.len() as f64;
            t.iter()
                .map(|i| deviation.get(*i as usize).copied().unwrap_or(0.0))
                .sum::<f64>()
                / n
        })
        .collect();

    let mut sorted = face_mean.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo = quantile_linear(&sorted, 0.01);
    let hi = quantile_linear(&sorted, 0.99);
    let span = hi - lo;

    if !(span > 0.0) {
        face_mean.iter_mut().for_each(|v| *v = 0.0);
        return vec![0; tris.len()];
    }
    face_mean
        .iter()
        .map(|d| {
            let t = ((d - lo) / span * 256.0).floor();
            t.clamp(0.0, 256.0) as u16
        })
        .collect()
}

/// The 257-entry deviation ramp (`_deviation_lut`): 4 stops, linearly blended.
pub fn deviation_lut() -> Vec<[u8; 3]> {
    let n = DEVIATION_STOPS.len();
    // `np.interp` over `n + 1` levels: positions 0, n/3, 2n/3, n.
    let positions: Vec<f64> = (0..n)
        .map(|i| i as f64 * (DEVIATION_LEVELS - 1) as f64 / (n - 1) as f64)
        .collect();
    let mut lut = Vec::with_capacity(DEVIATION_LEVELS);
    for level in 0..DEVIATION_LEVELS {
        let x = level as f64;
        // Clamp outside the stop range, like np.interp.
        if x <= positions[0] {
            lut.push(to_rgb(DEVIATION_STOPS[0]));
            continue;
        }
        if x >= positions[n - 1] {
            lut.push(to_rgb(DEVIATION_STOPS[n - 1]));
            continue;
        }
        let hi = positions.iter().position(|p| *p >= x).unwrap_or(n - 1);
        let lo = hi - 1;
        let frac = (x - positions[lo]) / (positions[hi] - positions[lo]);
        let mut rgb = [0u8; 3];
        for k in 0..3 {
            let v = DEVIATION_STOPS[lo][k] * (1.0 - frac) + DEVIATION_STOPS[hi][k] * frac;
            rgb[k] = round_clamp(v);
        }
        lut.push(rgb);
    }
    lut
}

/// The 65-entry shade LUT (`_shade_lut`) for one base colour.
///
/// Python stores this per base colour and indexes it by `rint(shade * 64)`.
pub fn shade_lut(base: [u8; 3]) -> Vec<[u8; 3]> {
    (0..SHADE_LEVELS)
        .map(|i| {
            let v = GREY_MIN + (1.0 - GREY_MIN) * (i as f64 / (SHADE_LEVELS - 1) as f64);
            [
                round_clamp(base[0] as f64 * v),
                round_clamp(base[1] as f64 * v),
                round_clamp(base[2] as f64 * v),
            ]
        })
        .collect()
}

/// `_shade_index`: `clip(rint(shade * 64), 0, 64)`.
pub fn shade_index(shade: f64) -> usize {
    // Python's `round` is banker's rounding on .5; Rust's `round` is half-away.
    // The shade values come from a dot product, so exact .5 is vanishingly rare,
    // but `round_ties_even` is the correct match and free.
    let v = (shade * 64.0).round_ties_even();
    v.clamp(0.0, 64.0) as usize
}

fn to_rgb(c: [f64; 3]) -> [u8; 3] {
    [round_clamp(c[0]), round_clamp(c[1]), round_clamp(c[2])]
}

fn round_clamp(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defect_faces_flags_faces_touching_a_defect_vertex() {
        let tris = vec![[0, 1, 2], [2, 3, 4], [5, 6, 7]];
        // Vertex 2 is a rim vertex: the first two faces touch it, the third not.
        let flags = defect_faces(&[2], &tris);
        assert_eq!(flags, vec![true, true, false]);
        // No defect vertices at all: nothing flagged.
        assert_eq!(defect_faces(&[], &tris), vec![false, false, false]);
        // Out-of-range indices are ignored rather than panicking.
        assert_eq!(defect_faces(&[-1, 99], &tris), vec![false, false, false]);
    }

    /// NumPy's `linear` interpolation, pinned against `np.quantile`.
    #[test]
    fn quantile_linear_matches_numpy() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        // np.quantile([1..5], 0.0/0.5/1.0) == 1.0/3.0/5.0
        assert_eq!(quantile_linear(&v, 0.0), 1.0);
        assert_eq!(quantile_linear(&v, 0.5), 3.0);
        assert_eq!(quantile_linear(&v, 1.0), 5.0);
        // np.quantile([1..5], 0.25) == 2.0; 0.75 == 4.0
        assert_eq!(quantile_linear(&v, 0.25), 2.0);
        assert_eq!(quantile_linear(&v, 0.75), 4.0);
        // Non-integral position interpolates: 4 values, q=0.5 -> pos 1.5.
        let w = vec![0.0, 10.0, 20.0, 30.0];
        assert_eq!(quantile_linear(&w, 0.5), 15.0);
        // Degenerate inputs.
        assert_eq!(quantile_linear(&[], 0.5), 0.0);
        assert_eq!(quantile_linear(&[7.0], 0.9), 7.0);
    }

    /// The ramp index: face means windowed to the 1st/99th percentile.
    #[test]
    fn quantile_index_windows_and_scales() {
        let dev = vec![0.0, 1.0, 2.0, 3.0];
        let tris = vec![[0, 1, 2], [1, 2, 3]];
        let idx = deviation_quantile_index(&dev, &tris);
        // Face means: (0+1+2)/3 = 1.0 and (1+2+3)/3 = 2.0
        // lo/hi from those two values at 1%/99% ~= 1.01 / 1.99
        assert_eq!(idx.len(), 2);
        assert!(idx[0] <= idx[1], "larger deviation must not get a lower index");
    }

    /// A flat field has no span, so every face indexes 0 (Python's guard).
    #[test]
    fn flat_deviation_is_all_zero() {
        let dev = vec![5.0; 6];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        assert_eq!(deviation_quantile_index(&dev, &tris), vec![0, 0]);
    }

    #[test]
    fn deviation_lut_shape_and_endpoints() {
        let lut = deviation_lut();
        assert_eq!(lut.len(), DEVIATION_LEVELS);
        // Endpoints are the first and last stops, pinned from numpy.interp.
        assert_eq!(lut[0], [10, 20, 80]);
        assert_eq!(lut[DEVIATION_LEVELS - 1], [230, 40, 40]);
        // The second entry blends towards cyan: blue rises before it falls.
        // (An earlier version of this test asserted the opposite and was wrong —
        // the navy stop is [10,20,80], and cyan's blue is 200, not 0.)
        assert_eq!(lut[1], [10, 22, 81]);
        // Green rises monotonically out of navy towards cyan.
        assert!(lut[1][1] >= lut[0][1]);
    }

    #[test]
    fn shade_lut_ranges() {
        let lut = shade_lut([178, 186, 194]);
        assert_eq!(lut.len(), SHADE_LEVELS);
        // Index 0 is GREY_MIN * base, the last is the base itself.
        assert_eq!(lut[SHADE_LEVELS - 1], [178, 186, 194]);
        assert_eq!(lut[0], [round_clamp(178.0 * GREY_MIN), round_clamp(186.0 * GREY_MIN), round_clamp(194.0 * GREY_MIN)]);
        // Clamping at both ends.
        assert_eq!(shade_index(-1.0), 0);
        assert_eq!(shade_index(2.0), 64);
        assert_eq!(shade_index(0.5), 32);
    }

    /// A vertex already on the reference surface has distance ~0; one moved away
    /// has a larger distance. This is the metric the gate compares.
    #[test]
    fn deviation_measures_distance_to_the_reference() {
        let ref_v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let ref_t = vec![[0usize, 1, 2]];
        // Coplanar points: distance 0 when inside the triangle.
        let dst = vec![[0.2, 0.2, 0.0], [0.2, 0.2, 2.0]];
        let d = deviation(&dst, &ref_v, &ref_t);
        assert!(d[0].abs() < 1e-12, "on-surface point: {}", d[0]);
        assert!((d[1] - 2.0).abs() < 1e-9, "lifted point: {}", d[1]);
        // An empty reference cannot be measured; zeros rather than a panic.
        assert_eq!(deviation(&dst, &ref_v, &[]), vec![0.0, 0.0]);
    }
}
