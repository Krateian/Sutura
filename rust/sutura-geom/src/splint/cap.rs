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
//! Reuses the hole closer's own machinery: `hole::boundary_topology` walks the
//! boundary loops in half-edge index order and `hole::planar_cap` triangulates a
//! planar, simple loop with the boundary-cancelling winding. When the planar
//! fill declines (non-planar opening) a deterministic fan to the loop centroid
//! is used as the fallback. No RNG is read, so the result is a pure function of
//! the input.

pub(crate) struct TempCap {
    /// Centroid vertices added by the fan fallback (appended after the input
    /// vertices in the capped point set).
    pub(crate) extra_verts: Vec<[f64; 3]>,
    /// Cap triangles: indices `< verts.len()` address the input vertices,
    /// indices `>= verts.len()` address `extra_verts`.
    pub(crate) tris: Vec<[usize; 3]>,
    /// Boundary loops seen.
    pub(crate) loops: usize,
    /// Loops closed with the planar fill.
    pub(crate) planar: usize,
    /// Loops closed with the centroid fan.
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
    let topo = match crate::hole::boundary_topology(&tris_i64, false) {
        Ok(t) => t,
        Err(_) => return TempCap::empty(),
    };

    let mut out = TempCap::empty();
    for lp in &topo.loops {
        // Ordered loop vertices: the start vertex of each boundary half-edge.
        let vids: Vec<i64> = lp.iter().map(|&he| tris_i64[he / 3][he % 3]).collect();
        if vids.len() < 3 {
            continue;
        }
        out.loops += 1;
        // Primary: the hole closer's planar cap (boundary-cancelling winding).
        if let Some(cap) = crate::hole::planar_cap(verts, &tris_i64, &[], &vids) {
            for t in cap {
                out.tris
                    .push([t[0] as usize, t[1] as usize, t[2] as usize]);
            }
            out.planar += 1;
            continue;
        }
        // Fallback: deterministic fan to the loop centroid, reversed winding.
        let k = vids.len();
        let mut c = [0.0f64; 3];
        for &v in &vids {
            let p = verts[v as usize];
            for d in 0..3 {
                c[d] += p[d];
            }
        }
        for d in 0..3 {
            c[d] /= k as f64;
        }
        let ci = verts.len() + out.extra_verts.len();
        out.extra_verts.push(c);
        for i in 0..k {
            let a = vids[i] as usize;
            let b = vids[(i + 1) % k] as usize;
            out.tris.push([b, a, ci]);
        }
        out.fanned += 1;
    }
    out
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
    fn non_planar_loop_falls_back_to_a_fan() {
        let (mut v, t) = cube_missing_top();
        v[6][2] += 0.5; // push one loop vertex out of the plane
        let a = temporary_cap(&v, &t);
        assert_eq!(a.loops, 1);
        assert_eq!(a.fanned, 1);
        assert_eq!(a.planar, 0);
        assert_eq!(a.extra_verts.len(), 1);
        assert_eq!(a.tris.len(), 4);
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
