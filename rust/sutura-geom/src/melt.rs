// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Melt (L2b): inside-out removal of residual self-intersection folds.
//!
//! This is the melting half of Tamponade.  After the L2a hole plugs are in
//! place, the residual self-intersecting faces are grouped into edge-connected
//! *sectors* (local folds).  Sectors are visited from the object's deepest
//! interior point **outward** (their depth is the median interior-side winding
//! field sample over the sector's own face centroids).  A buried fold is excised
//! and its exposed rim is welded to the coincident opposite sheet; every input
//! triangle that is not a buried fold face is carried through verbatim.  Each
//! sector is one *layer*: it is committed only when the reload-honest hole and
//! non-manifold counts do not rise and the exact self-intersection count
//! strictly falls, otherwise the layer is rolled back byte-for-byte.  A small
//! non-manifold spot is first repaired by a local micro-weld instead of being
//! rolled back.
//!
//! The module is deliberately self-contained: `tamponade.rs` owns exactly one
//! call into it, so the melting logic cannot collide with the budget/gate work
//! in `tamponade.rs` or the whole-shell analysis in `shell.rs`.  It has no
//! global side effects and no parallel reductions, so a run is deterministic.

use std::time::Instant;

/// A self-intersection *proper* face mask, from the crate's exact classifier.
/// Returns `(mask, count, skipped)`; `skipped` is true when the mesh exceeds the
/// classifier's face cap (the mask is then all-false and `count` is `-1`).
pub fn scan_si_faces(verts: &[[f64; 3]], tris: &[[u32; 3]]) -> (Vec<bool>, i64, bool) {
    let t: Vec<[usize; 3]> = tris
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    crate::self_intersections_core(verts, &t)
}

/// Tunables for the melt pass.  Every value is a starting point to be tuned on
/// the real-world SI meshes in the measurement phase.
#[derive(Clone, Debug)]
pub struct MeltParams {
    /// Minimum exterior-side winding for a face to count as buried.
    pub buried_w_out: f64,
    /// Minimum interior-side winding for a face to count as buried when it has
    /// no self-intersection partner on its own sector (the thin-wall guard).
    pub buried_w_in: f64,
    /// Hard cap on the faces one sector may excise.
    pub max_sector_faces: usize,
    /// Hard cap on the fraction of the mesh one sector may excise.
    pub max_face_fraction: f64,
    /// A new non-manifold cluster at or below this many edges may be
    /// micro-welded instead of rolled back.
    pub micro_weld_max_edges: usize,
    /// ...and whose total edge length is within this fraction of the diagonal.
    pub micro_weld_max_rel: f64,
    /// Hard cap on the global depth-grid cell count.
    pub grid_max_cells: usize,
}

impl Default for MeltParams {
    fn default() -> Self {
        Self {
            buried_w_out: 0.5,
            buried_w_in: 1.5,
            max_sector_faces: 5000,
            max_face_fraction: 0.02,
            micro_weld_max_edges: 8,
            micro_weld_max_rel: 0.005,
            grid_max_cells: 8_000_000,
        }
    }
}

/// Result of a melt pass.  `verts`/`tris` equal the input when nothing was
/// accepted.
#[derive(Clone, Debug, Default)]
pub struct MeltOutcome {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub layers_tried: usize,
    pub layers_accepted: usize,
    pub layers_rolled_back: usize,
    pub micro_welds_ok: usize,
    pub micro_welds_failed: usize,
    pub excised_faces: usize,
    /// When empty the caller keeps its own reason.
    pub reason: String,
}

/// Run the inside-out melt.  For now this is the scaffold: the SI scan is
/// implemented and everything else is a guarded no-op, so the output is
/// byte-identical to the L2a result and the phase can be committed green.
pub fn melt_si(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    _h: f64,
    _model_diag: f64,
    _time_budget_s: Option<f64>,
    _params: &MeltParams,
) -> MeltOutcome {
    let t0 = Instant::now();
    let mut out = MeltOutcome {
        verts: verts.to_vec(),
        tris: tris.to_vec(),
        ..Default::default()
    };

    if verts.is_empty() || tris.is_empty() {
        return out;
    }

    let (_mask, si_count, skipped) = scan_si_faces(verts, tris);
    if skipped || si_count <= 0 {
        // Nothing to melt; leave the L2a output untouched.
        return out;
    }

    // P1+ implements the depth field, sectorisation, excision and weld here.
    // Until that lands the pass is a deterministic no-op so the scaffold is
    // safe to ship behind `melt_si`.
    let _ = (t0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_cube() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
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
            [0, 2, 1], [0, 3, 2], // bottom
            [4, 5, 6], [4, 6, 7], // top
            [0, 1, 5], [0, 5, 4], // -y
            [1, 2, 6], [1, 6, 5], // +x
            [2, 3, 7], [2, 7, 6], // +y
            [3, 0, 4], [3, 4, 7], // -x
        ];
        (verts, tris)
    }

    /// Two triangles that cross in a proper (non-degenerate) segment.
    fn crossing_pair() -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.7, -1.0, -1.0],
            [0.7, 3.0, -1.0],
            [0.7, 1.0, 3.0],
        ];
        let tris = vec![[0, 1, 2], [3, 4, 5]];
        (verts, tris)
    }

    #[test]
    fn si_scan_crossing_pair() {
        let (verts, tris) = crossing_pair();
        let (mask, count, skipped) = scan_si_faces(&verts, &tris);
        assert!(!skipped);
        assert_eq!(count, 2, "both crossing faces are flagged");
        assert!(mask.iter().all(|&b| b));
    }

    #[test]
    fn clean_mesh_no_melt() {
        let (verts, tris) = clean_cube();
        let (mask, count, skipped) = scan_si_faces(&verts, &tris);
        assert!(!skipped);
        assert_eq!(count, 0);
        assert!(mask.iter().all(|&b| !b));

        let out = melt_si(&verts, &tris, 0.5, 1.7, None, &MeltParams::default());
        assert_eq!(out.layers_tried, 0);
        assert_eq!(out.layers_accepted, 0);
        assert_eq!(out.verts, verts);
        assert_eq!(out.tris, tris);
    }

    #[test]
    fn scaffold_is_byte_identical_on_si_mesh() {
        // P0 is a guarded no-op, so an SI mesh is currently returned unchanged.
        let (verts, tris) = crossing_pair();
        let out = melt_si(&verts, &tris, 0.5, 3.0, None, &MeltParams::default());
        assert_eq!(out.verts, verts);
        assert_eq!(out.tris, tris);
    }
}
