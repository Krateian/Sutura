// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! H7b: a deterministic TEMPORARY cap for the boundary loops of the input.
//!
//! The cap closes every open boundary loop of the input so that the
//! generalized winding number of the *capped* surface reads ~1 inside the
//! solid, giving the H7 min-cut a real interior instead of having to infer the
//! opening from a fractional (open-mesh) winding. The cap triangles are used
//! **only** to evaluate the winding field: they are never part of the basis or
//! the emitted mesh.
//!
//! The fill reuses the hole closer itself (`hole::close_holes_core`): a planar,
//! simple loop is capped flat in its own plane (boundary-cancelling winding) and
//! every other loop is closed with the weight-based ear clipper that the repair
//! pipeline already ships. That closer only emits triangles over the original
//! vertices, so the capped point set is the input point set and no extra
//! vertices are introduced. No RNG is read, so the result is a pure function of
//! the input.

pub(crate) struct TempCap {
    /// Always empty: the hole closer reuses the input vertices, so the capped
    /// point set equals the input point set. Kept for the caller's symmetry.
    pub(crate) extra_verts: Vec<[f64; 3]>,
    /// Cap triangles: indices address the input vertices.
    pub(crate) tris: Vec<[usize; 3]>,
    /// Boundary loops closed by the fill.
    pub(crate) loops: usize,
    /// Loops that the planar fill caps flat.
    pub(crate) planar: usize,
    /// Loops closed by the weight-based ear clipper (non-planar openings).
    pub(crate) fanned: usize,
}

impl TempCap {
    fn empty() -> Self {
        Self {
            extra_verts: Vec::new(),
            tris: Vec::new(),
            loops: 0,
            planar: 0,
            fanned: 0,
        }
    }
}

/// Build a temporary cap for the open boundary loops of `verts`/`tris`.
pub(crate) fn temporary_cap(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> TempCap {
    if verts.is_empty() || tris.is_empty() {
        return TempCap::empty();
    }
    let tris_i64: Vec<[i64; 3]> = tris
        .iter()
        .map(|t| [t[0] as i64, t[1] as i64, t[2] as i64])
        .collect();

    // The fill: planar loops flat, non-planar loops via the ear clipper. The
    // returned point set equals the input point set, so `rep.tris` past the
    // input length is exactly the cap.
    let rep = match crate::hole::close_holes_core(verts, &tris_i64, i64::MAX, false) {
        Ok(r) => r,
        Err(_) => return TempCap::empty(),
    };
    if rep.tris.len() <= tris.len() {
        return TempCap::empty();
    }
    let cap: Vec<[usize; 3]> = rep.tris[tris.len()..]
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();

    // Diagnostics: how many of the loops the planar fill would cap on its own.
    let mut planar = 0usize;
    if let Ok(topo) = crate::hole::boundary_topology(&tris_i64, false) {
        for lp in &topo.loops {
            let vids: Vec<i64> = lp.iter().map(|&he| tris_i64[he / 3][he % 3]).collect();
            if vids.len() >= 3 && crate::hole::planar_cap(verts, &tris_i64, &[], &vids).is_some() {
                planar += 1;
            }
        }
    }

    TempCap {
        extra_verts: Vec::new(),
        tris: cap,
        loops: rep.closed_holes,
        planar,
        fanned: rep.closed_holes.saturating_sub(planar),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_missing_top() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        // Unit cube with the +z face (triangles 4-5-6 and 4-6-7) removed, so the
        // boundary is the planar square loop 4-5-6-7.
        let v = vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ];
        let t = vec![
            [0, 2, 1],
            [0, 3, 2],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ];
        (v, t)
    }

    #[test]
    fn planar_loop_is_capped_deterministically() {
        let (v, t) = cube_missing_top();
        let a = temporary_cap(&v, &t);
        let b = temporary_cap(&v, &t);
        assert_eq!(a.loops, 1);
        assert_eq!(a.planar, 1);
        assert_eq!(a.fanned, 0);
        assert_eq!(a.extra_verts.len(), 0);
        assert_eq!(a.tris.len(), 2);
        assert_eq!(a.tris, b.tris);
    }

    #[test]
    fn non_planar_loop_is_closed_on_the_input_vertices() {
        let (mut v, t) = cube_missing_top();
        v[6][2] += 0.5; // push one loop vertex out of the plane
        let a = temporary_cap(&v, &t);
        let b = temporary_cap(&v, &t);
        assert_eq!(a.loops, 1);
        assert_eq!(a.fanned, 1);
        assert_eq!(a.planar, 0);
        assert_eq!(a.extra_verts.len(), 0);
        assert_eq!(a.tris.len(), 2);
        assert_eq!(a.tris, b.tris);
    }

    #[test]
    fn closed_mesh_has_no_cap() {
        // Fully closed tetrahedron: no boundary loops.
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let t = vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]];
        let c = temporary_cap(&v, &t);
        assert_eq!(c.loops, 0);
        assert_eq!(c.tris.len(), 0);
        assert_eq!(c.extra_verts.len(), 0);
    }
}
