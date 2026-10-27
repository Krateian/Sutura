//! Exact Hermite data for dual contouring of a closed (morphologically
//! processed) signed field.
//!
//! The closing used by Graft re-derives its field from a binary occupancy, so
//! the crossings and gradient normals of that field are quantised to the voxel
//! grid.  Where the closing left the surface untouched, the original mesh
//! gives the exact surface point and face normal instead.  An edge is such a
//! "healthy" edge when the closest point of the original lies within
//! `HEALTHY_DIST` voxels of the edge's linear crossing in the closed field.
//! (An end-node sign test against the original field was tried and discarded:
//! the occupancy quantisation flips about 80 % of the surface nodes, so it
//! rejected almost every healthy edge.)  Every other sign-change edge keeps the field-gradient Hermite data.
//!
//! Hermite samples are `(point on the surface, outward unit normal)` pairs;
//! the point need not lie on the grid edge (Ju et al. 2002 only requires an
//! exact surface sample and its normal).  The outward direction is taken from
//! the field's own inside/outside labelling, so inconsistent face orientation
//! in the input cannot flip a normal.
//!
//! Pure function of the grid and the mesh: rayon collects in index order, so
//! the result is deterministic.

use crate::winding::MeshBvh;
use rayon::prelude::*;
use std::collections::HashMap;

/// An edge is exact when the original surface passes within this many voxels
/// of the edge's linear crossing: half the cell diagonal, `sqrt(3)/2`.  The
/// occupancy closing places the surface up to about that far from the true
/// one; a bridged concavity lies farther and keeps gradient data.
const HEALTHY_DIST: f64 = 0.866_025_403_784_438_6;

/// Minimum `|n . edge_dir|` for a face normal to be trusted on an edge.
const MIN_ALIGN: f64 = 0.1;

#[derive(Default, Clone, Copy, Debug)]
pub struct HermiteStats {
    /// Sign-change grid edges of the closed field.
    pub edges_total: usize,
    /// Edges that received exact Hermite data from the original mesh.
    pub edges_exact: usize,
    /// Closest-point queries issued.
    pub closest_queries: usize,
}

pub struct HermiteEdges {
    map: HashMap<u64, ([f64; 3], [f64; 3])>,
    nx: usize,
    ny: usize,
    pub stats: HermiteStats,
}

impl HermiteEdges {
    /// Hermite sample of the grid edge between nodes `ia < ib`, if exact.
    pub fn get(&self, ia: usize, ib: usize) -> Option<([f64; 3], [f64; 3])> {
        let d = ib.checked_sub(ia)?;
        let axis = if d == 1 {
            0u64
        } else if d == self.nx {
            1
        } else if d == self.nx * self.ny {
            2
        } else {
            return None;
        };
        self.map.get(&(ia as u64 * 3 + axis)).copied()
    }
}

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Compute exact Hermite data for every healthy sign-change edge.
///
/// `field` is the closed field that is contoured (negative inside,
/// `i`-fastest layout).
pub fn build_edges(
    bvh: &MeshBvh,
    field: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
) -> HermiteEdges {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let node = |i: usize, j: usize, k: usize| i + nx * (j + ny * k);
    let pos = |i: usize, j: usize, k: usize| -> [f64; 3] {
        [
            origin[0] + voxel * i as f64,
            origin[1] + voxel * j as f64,
            origin[2] + voxel * k as f64,
        ]
    };

    let per_slab: Vec<(Vec<(u64, [f64; 3], [f64; 3])>, HermiteStats)> = (0..nz)
        .into_par_iter()
        .map(|k| {
            let mut out = Vec::new();
            let mut st = HermiteStats::default();
            for j in 0..ny {
                for i in 0..nx {
                    let ia = node(i, j, k);
                    for axis in 0..3u64 {
                        let (ni, nj, nk) = match axis {
                            0 => (i + 1, j, k),
                            1 => (i, j + 1, k),
                            _ => (i, j, k + 1),
                        };
                        if ni >= nx || nj >= ny || nk >= nz {
                            continue;
                        }
                        let ib = node(ni, nj, nk);
                        let (fa, fb) = (field[ia], field[ib]);
                        if (fa < 0.0) == (fb < 0.0) {
                            continue;
                        }
                        st.edges_total += 1;
                        let pa = pos(i, j, k);
                        let pb = pos(ni, nj, nk);
                        let (fa, fb) = (fa as f64, fb as f64);
                        let t = if (fb - fa).abs() > 1e-12 {
                            (-fa / (fb - fa)).clamp(0.0, 1.0)
                        } else {
                            0.5
                        };
                        let p = [
                            pa[0] + t * (pb[0] - pa[0]),
                            pa[1] + t * (pb[1] - pa[1]),
                            pa[2] + t * (pb[2] - pa[2]),
                        ];
                        st.closest_queries += 1;
                        let (c, d, tid) = bvh.closest_point(p);
                        if !(d <= HEALTHY_DIST * voxel) || tid == usize::MAX {
                            continue;
                        }
                        let mut n = bvh.tri_normal(tid);
                        // Inside -> outside direction along the edge.
                        let dir = if fa < fb { sub(pb, pa) } else { sub(pa, pb) };
                        let dl = dot(dir, dir).sqrt();
                        if dl <= 1e-12 {
                            continue;
                        }
                        let al = dot(n, dir) / dl;
                        if !(al.abs() >= MIN_ALIGN) {
                            continue;
                        }
                        if al < 0.0 {
                            n = [-n[0], -n[1], -n[2]];
                        }
                        st.edges_exact += 1;
                        out.push((ia as u64 * 3 + axis, c, n));
                    }
                }
            }
            (out, st)
        })
        .collect();

    let mut stats = HermiteStats::default();
    let mut map = HashMap::new();
    for (v, st) in per_slab {
        stats.edges_total += st.edges_total;
        stats.edges_exact += st.edges_exact;
        stats.closest_queries += st.closest_queries;
        for (key, c, n) in v {
            map.insert(key, (c, n));
        }
    }
    HermiteEdges { map, nx, ny, stats }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct CrossingStats {
    pub edges: usize,
    pub exact: usize,
    pub distance_queries: usize,
}

/// Maximum distance evaluations per edge (the two bracket ends are free).
const MAX_EVALS: usize = 6;
/// Stop when `|F|` falls below this fraction of a voxel.
const F_TOL_VOXELS: f64 = 1e-3;

/// Exact crossing fractions of `F = s - c` on the given sign-change edges.
///
/// `field` holds `F` at the nodes, `cfield` the smooth offset `c` (so that
/// `s = F + c` is the exact signed distance of the original at a node).  On
/// an edge `F(p) = sign * u(p) - c(p)`, with `u` the exact unsigned distance
/// (BVH) and `c` trilinearly interpolated.  The sign of `s` is that of the end
/// nodes when they agree, else `inside(p)` (the caller's winding oracle), since
/// near the original surface a sign error turns `F` into `|u| - c`.  The root
/// is bracketed by the nodal values and found with the Illinois variant of
/// regula falsi.  An edge whose root cannot be bracketed, evaluated or
/// converged within the evaluation budget keeps no entry (linear crossing).
///
/// The result maps `(lo, hi)` to the fraction from node `lo` toward `hi`.
pub fn exact_offset_crossings(
    bvh: &MeshBvh,
    field: &[f32],
    cfield: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
    edges: &[(usize, usize)],
    inside: &(dyn Fn([f64; 3]) -> bool + Sync),
) -> (HashMap<(usize, usize), f64>, CrossingStats) {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let coords = |n: usize| (n % nx, (n / nx) % ny, n / (nx * ny));
    let c_at = |p: [f64; 3]| -> f64 {
        let g = |d: usize, n: usize| {
            ((p[d] - origin[d]) / voxel).clamp(0.0, (n - 1) as f64 - 1e-9)
        };
        let (gx, gy, gz) = (g(0, nx), g(1, ny), g(2, nz));
        let (i, j, k) = (gx as usize, gy as usize, gz as usize);
        let (fx, fy, fz) = (gx - i as f64, gy - j as f64, gz - k as f64);
        let v = |a: usize, b: usize, c: usize| cfield[a + nx * (b + ny * c)] as f64;
        let l = |a: f64, b: f64, t: f64| a + t * (b - a);
        let x00 = l(v(i, j, k), v(i + 1, j, k), fx);
        let x10 = l(v(i, j + 1, k), v(i + 1, j + 1, k), fx);
        let x01 = l(v(i, j, k + 1), v(i + 1, j, k + 1), fx);
        let x11 = l(v(i, j + 1, k + 1), v(i + 1, j + 1, k + 1), fx);
        l(l(x00, x10, fy), l(x01, x11, fy), fz)
    };

    let per: Vec<(Option<f64>, usize)> = edges
        .par_iter()
        .map(|&(lo, hi)| {
            let (fa, fb) = (field[lo] as f64, field[hi] as f64);
            let (sa, sb) = (fa + cfield[lo] as f64, fb + cfield[hi] as f64);
            let (ia, ja, ka) = coords(lo);
            let (ib, jb, kb) = coords(hi);
            let pa = [
                origin[0] + voxel * ia as f64,
                origin[1] + voxel * ja as f64,
                origin[2] + voxel * ka as f64,
            ];
            let pb = [
                origin[0] + voxel * ib as f64,
                origin[1] + voxel * jb as f64,
                origin[2] + voxel * kb as f64,
            ];
            let mut queries = 0usize;
            let mut eval = |t: f64| -> f64 {
                let p = [
                    pa[0] + t * (pb[0] - pa[0]),
                    pa[1] + t * (pb[1] - pa[1]),
                    pa[2] + t * (pb[2] - pa[2]),
                ];
                queries += 1;
                let u = bvh.unsigned_distance_at(p);
                let sg = if (sa < 0.0) == (sb < 0.0) {
                    if sa < 0.0 { -1.0 } else { 1.0 }
                } else if inside(p) {
                    -1.0
                } else {
                    1.0
                };
                sg * u - c_at(p)
            };
            // Bracket ends use the nodal values (exact by construction).
            let (mut t0, mut f0, mut t1, mut f1) = (0.0f64, fa, 1.0f64, fb);
            if !((f0 < 0.0) != (f1 < 0.0)) {
                return (None, queries);
            }
            let tol = F_TOL_VOXELS * voxel;
            let mut side = 0i32;
            let mut t = if (f1 - f0).abs() > 1e-12 {
                (-f0 / (f1 - f0)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            for _ in 0..MAX_EVALS {
                let ft = eval(t);
                if !ft.is_finite() {
                    return (None, queries);
                }
                if ft.abs() <= tol {
                    return (Some(t), queries);
                }
                if (ft < 0.0) == (f0 < 0.0) {
                    t0 = t;
                    f0 = ft;
                    if side == -1 {
                        f1 *= 0.5;
                    }
                    side = -1;
                } else {
                    t1 = t;
                    f1 = ft;
                    if side == 1 {
                        f0 *= 0.5;
                    }
                    side = 1;
                }
                let d = f1 - f0;
                t = if d.abs() > 1e-300 { (t0 * f1 - t1 * f0) / d } else { 0.5 * (t0 + t1) };
                if !t.is_finite() {
                    return (None, queries);
                }
                t = t.clamp(t0.min(t1), t0.max(t1));
            }
            (None, queries)
        })
        .collect();

    let mut stats = CrossingStats { edges: edges.len(), ..Default::default() };
    let mut map = HashMap::with_capacity(edges.len());
    for (e, (t, q)) in edges.iter().zip(per) {
        stats.distance_queries += q;
        if let Some(t) = t {
            stats.exact += 1;
            map.insert(*e, t);
        }
    }
    (map, stats)
}

/// Marching-tetrahedra coat of `F = s - c` with exact crossings (see
/// `exact_offset_crossings`; unconverged edges keep the linear crossing); topology equals the plain marching-tetrahedra
/// surface of `field`.  Also returns the crossing statistics.
pub fn exact_coat_mesh(
    bvh: &MeshBvh,
    field: &[f32],
    cfield: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
    inside: &(dyn Fn([f64; 3]) -> bool + Sync),
) -> (crate::dual_contour::DcMesh, CrossingStats) {
    let edges = crate::dual_contour::tet_crossing_edges(field, dims);
    let (tab, st) = exact_offset_crossings(bvh, field, cfield, dims, origin, voxel, &edges, inside);
    let m = crate::dual_contour::marching_tets_mesh_exact(field, dims, origin, voxel, Some(&tab));
    (m, st)
}

/// `SUTURA_HERMITE=0|false|no|off` opts out; ON by default.
pub fn enabled_by_default() -> bool {
    match std::env::var("SUTURA_HERMITE") {
        Ok(val) => {
            let v = val.trim().to_ascii_lowercase();
            v != "0" && v != "false" && v != "no" && v != "off"
        }
        Err(_) => true,
    }
}

#[derive(Default, Clone, Debug)]
pub struct HermiteReport {
    pub stats: HermiteStats,
    pub used: bool,
    pub reason: &'static str,
    pub verts_changed: usize,
    pub reverted_fold: usize,
    pub reverted_si: usize,
    pub si_base: i64,
    pub si_hermite: i64,
}

pub fn report_dict<'py>(
    py: pyo3::Python<'py>,
    r: &HermiteReport,
) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::types::PyDict>> {
    use pyo3::types::PyDictMethods;
    let d = pyo3::types::PyDict::new(py);
    d.set_item("used", r.used)?;
    d.set_item("reason", r.reason)?;
    d.set_item("edges_total", r.stats.edges_total)?;
    d.set_item("edges_exact", r.stats.edges_exact)?;
    d.set_item("closest_queries", r.stats.closest_queries)?;
    d.set_item("verts_changed", r.verts_changed)?;
    d.set_item("reverted_fold", r.reverted_fold)?;
    d.set_item("reverted_si", r.reverted_si)?;
    d.set_item("si_base", r.si_base)?;
    d.set_item("si_hermite", r.si_hermite)?;
    Ok(d)
}

#[inline]
fn tri_normal_unit(v: &[[f64; 3]], t: &[u32; 3]) -> Option<[f64; 3]> {
    let (a, b, c) = (v[t[0] as usize], v[t[1] as usize], v[t[2] as usize]);
    let (u, w) = (sub(b, a), sub(c, a));
    let n = [
        u[1] * w[2] - u[2] * w[1],
        u[2] * w[0] - u[0] * w[2],
        u[0] * w[1] - u[1] * w[0],
    ];
    let l = dot(n, n).sqrt();
    if l > 1e-300 {
        Some([n[0] / l, n[1] / l, n[2] / l])
    } else {
        None
    }
}

fn si_count(v: &[[f64; 3]], t: &[[u32; 3]]) -> (Vec<bool>, i64) {
    let ti: Vec<[usize; 3]> = t
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    let b = MeshBvh::from_arrays(v, &ti);
    let (mask, n, _skipped) = b.self_intersections(v, &ti);
    (mask, n)
}

/// Re-place the vertices of `base` (the dual contouring of `field`) with exact
/// Hermite data, keeping the topology, and guard the result:
/// 1. a vertex whose incident triangle flips against the baseline triangle
///    normal reverts to its baseline position (repeated to a fixpoint);
/// 2. self-intersections are counted for both meshes; vertices of faces that
///    newly intersect revert to baseline, and when the Hermite mesh still has
///    more intersections than the baseline the whole baseline is returned.
pub fn refine_vertices(
    bvh: &MeshBvh,
    field: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
    base: crate::dual_contour::DcMesh,
) -> (crate::dual_contour::DcMesh, HermiteReport) {
    let mut rep = HermiteReport {
        si_base: -1,
        si_hermite: -1,
        reason: "no_exact_edges",
        ..Default::default()
    };
    if base.fallback || !base.manifold || base.tris.is_empty() {
        rep.reason = "base_not_dual";
        return (base, rep);
    }
    let he = build_edges(bvh, field, dims, origin, voxel);
    rep.stats = he.stats;
    if he.stats.edges_exact == 0 {
        return (base, rep);
    }
    let hook = |a: usize, b: usize| he.get(a, b);
    let h = crate::dual_contour::dual_contour_hermite(field, dims, origin, voxel, &hook);
    if h.fallback || !h.manifold || h.verts.len() != base.verts.len() || h.tris != base.tris {
        rep.reason = "topology_differs";
        return (base, rep);
    }

    let nv = base.verts.len();
    let mut cur = h.verts;
    let mut changed: Vec<bool> = (0..nv).map(|i| cur[i] != base.verts[i]).collect();
    rep.verts_changed = changed.iter().filter(|&&c| c).count();

    // 1. fold guard against the baseline triangle normals.
    let base_n: Vec<Option<[f64; 3]>> = base.tris.iter().map(|t| tri_normal_unit(&base.verts, t)).collect();
    for _ in 0..8 {
        let mut revert: Vec<usize> = Vec::new();
        for (ti, t) in base.tris.iter().enumerate() {
            if !t.iter().any(|&x| changed[x as usize]) {
                continue;
            }
            let bad = match (tri_normal_unit(&cur, t), base_n[ti]) {
                (Some(c), Some(b)) => dot(c, b) < 0.0,
                (None, Some(_)) => true,
                _ => false,
            };
            if bad {
                revert.extend(t.iter().map(|&x| x as usize).filter(|&x| changed[x]));
            }
        }
        if revert.is_empty() {
            break;
        }
        revert.sort_unstable();
        revert.dedup();
        for x in revert {
            cur[x] = base.verts[x];
            changed[x] = false;
            rep.reverted_fold += 1;
        }
    }

    // 2. self-intersection guard.
    if base.tris.len() <= crate::SI_MAX_FACES {
        let (_m0, si0) = si_count(&base.verts, &base.tris);
        rep.si_base = si0;
        let (mut mask, mut si1) = si_count(&cur, &base.tris);
        for _ in 0..2 {
            if si1 <= si0 {
                break;
            }
            let mut any = false;
            for (ti, t) in base.tris.iter().enumerate() {
                if mask[ti] {
                    for &x in t {
                        let x = x as usize;
                        if changed[x] {
                            cur[x] = base.verts[x];
                            changed[x] = false;
                            rep.reverted_si += 1;
                            any = true;
                        }
                    }
                }
            }
            if !any {
                break;
            }
            let r = si_count(&cur, &base.tris);
            mask = r.0;
            si1 = r.1;
        }
        rep.si_hermite = si1;
        if si1 > si0 {
            rep.reason = "si_rollback";
            return (base, rep);
        }
    } else {
        rep.reason = "si_unchecked";
    }

    rep.verts_changed = changed.iter().filter(|&&c| c).count();
    if rep.verts_changed == 0 {
        rep.reason = "all_reverted";
        return (base, rep);
    }
    rep.used = true;
    if rep.reason == "no_exact_edges" {
        rep.reason = "ok";
    }
    (
        crate::dual_contour::DcMesh {
            verts: cur,
            tris: base.tris,
            fallback: false,
            manifold: true,
        },
        rep,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned wedge-free check: a cube centred in a grid.  Every healthy
    /// edge sample must lie on a cube face and carry that face's outward normal.
    fn cube_fixture() -> (MeshBvh, Vec<f32>, [usize; 3], [f64; 3], f64) {
        let h = 1.0f64;
        let verts: Vec<[f64; 3]> = vec![
            [-2.3, -2.3, -2.3],
            [2.3, -2.3, -2.3],
            [2.3, 2.3, -2.3],
            [-2.3, 2.3, -2.3],
            [-2.3, -2.3, 2.3],
            [2.3, -2.3, 2.3],
            [2.3, 2.3, 2.3],
            [-2.3, 2.3, 2.3],
        ];
        let tris: Vec<[usize; 3]> = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [3, 0, 4],
            [3, 4, 7],
        ];
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        let n = 9usize;
        let dims = [n, n, n];
        let origin = [-4.0, -4.0, -4.0];
        let mut f = vec![0f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let p = [
                        origin[0] + h * i as f64,
                        origin[1] + h * j as f64,
                        origin[2] + h * k as f64,
                    ];
                    let m = p[0].abs().max(p[1].abs()).max(p[2].abs());
                    f[i + n * (j + n * k)] = (m - 2.3) as f32;
                }
            }
        }
        (bvh, f, dims, origin, h)
    }

    /// Hermite dual contouring of an axis-aligned cube whose faces and edges
    /// fall between grid nodes: every vertex must sit on the cube surface, and
    /// the corner/edge cells must reproduce the sharp edges exactly.
    #[test]
    fn cube_vertices_are_on_the_surface_and_deterministic() {
        let (bvh, f, dims, origin, h) = cube_fixture();
        let he = build_edges(&bvh, &f, dims, origin, h);
        let hook = |a: usize, b: usize| he.get(a, b);
        let m = crate::dual_contour::dual_contour_hermite(&f, dims, origin, h, &hook);
        assert!(m.manifold && !m.fallback);
        let mut on_edge = 0;
        for p in &m.verts {
            let a = [p[0].abs(), p[1].abs(), p[2].abs()];
            let mx = a[0].max(a[1]).max(a[2]);
            assert!((mx - 2.3).abs() < 1e-9, "vertex off surface {p:?}");
            if a.iter().filter(|&&x| (x - 2.3).abs() < 1e-9).count() >= 2 {
                on_edge += 1;
            }
        }
        assert!(on_edge > 0, "no vertex reproduced a sharp edge");
        let he2 = build_edges(&bvh, &f, dims, origin, h);
        let hook2 = |a: usize, b: usize| he2.get(a, b);
        let m2 = crate::dual_contour::dual_contour_hermite(&f, dims, origin, h, &hook2);
        assert_eq!(m.verts, m2.verts);
        assert_eq!(m.tris, m2.tris);
    }

    /// Exact crossings of the true signed distance of a cube: every coat
    /// vertex lies on the cube surface (to the solver tolerance), the topology
    /// is the plain marching-tetrahedra one, and each vertex is on its edge.
    #[test]
    fn exact_coat_vertices_lie_on_the_surface() {
        let (bvh, _f, dims, origin, h) = cube_fixture();
        let n = dims[0];
        let mut f = vec![0f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let p = [
                        origin[0] + h * i as f64,
                        origin[1] + h * j as f64,
                        origin[2] + h * k as f64,
                    ];
                    let u = bvh.unsigned_distance_at(p);
                    let inside = p.iter().all(|x| x.abs() < 2.3);
                    f[i + n * (j + n * k)] = if inside { -u } else { u } as f32;
                }
            }
        }
        let c = vec![0f32; n * n * n];
        let inside = |p: [f64; 3]| p.iter().all(|x| x.abs() < 2.3);
        let (m, st) = exact_coat_mesh(&bvh, &f, &c, dims, origin, h, &inside);
        let plain = crate::dual_contour::marching_tets_mesh(&f, dims, origin, h);
        assert!(st.edges > 0 && st.exact * 10 >= st.edges * 9, "{} of {}", st.exact, st.edges);
        assert_eq!(m.tris, plain.tris);
        let mut worst_exact = 0.0f64;
        let mut worst_plain = 0.0f64;
        for (a, b) in m.verts.iter().zip(&plain.verts) {
            worst_exact = worst_exact.max(bvh.unsigned_distance_at(*a));
            worst_plain = worst_plain.max(bvh.unsigned_distance_at(*b));
        }
        assert!(worst_exact < 2e-3 * h, "exact coat off surface: {worst_exact}");
        assert!(worst_exact <= worst_plain + 1e-12);
    }

    #[test]
    fn cube_samples_are_exact() {
        let (bvh, f, dims, origin, h) = cube_fixture();
        let n = dims[0];
        let he = build_edges(&bvh, &f, dims, origin, h);
        assert!(he.stats.edges_exact > 0);
        assert_eq!(he.stats.edges_exact, he.stats.edges_total);
        let mut seen = 0;
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let ia = i + n * (j + n * k);
                    for (di, dj, dk) in [(1, 0, 0), (0, 1, 0), (0, 0, 1)] {
                        if i + di >= n || j + dj >= n || k + dk >= n {
                            continue;
                        }
                        let ib = (i + di) + n * ((j + dj) + n * (k + dk));
                        if let Some((p, nrm)) = he.get(ia, ib) {
                            seen += 1;
                            let m = p[0].abs().max(p[1].abs()).max(p[2].abs());
                            assert!((m - 2.3).abs() < 1e-9, "point off surface {p:?}");
                            let a = nrm[0].abs().max(nrm[1].abs()).max(nrm[2].abs());
                            assert!((a - 1.0).abs() < 1e-9, "normal not axis aligned {nrm:?}");
                            // outward: same sign as the dominant point coordinate
                            let ax = (0..3)
                                .max_by(|&x, &y| nrm[x].abs().partial_cmp(&nrm[y].abs()).unwrap())
                                .unwrap();
                            assert!(nrm[ax] * p[ax] > 0.0, "normal not outward");
                        }
                    }
                }
            }
        }
        assert_eq!(seen, he.stats.edges_exact);
    }
}
