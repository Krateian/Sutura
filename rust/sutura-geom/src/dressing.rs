//! Dressing (#16): variable-thickness coat extraction from a narrow-band
//! signed field.
//!
//! Pipeline: narrow-band generalized-winding sign + unsigned distance ->
//! variable radius `r(x)` (larger near defects) -> scalar field `F = s - r` ->
//! marching tetrahedra -> watertight, two-manifold coat.
//!
//! The winding number and the unsigned distance are evaluated ONLY for cells
//! whose unsigned distance is below `band = r_max + band_voxels*voxel`.  Cells
//! outside the band get their sign from a flood fill of the free region from
//! the grid boundary; the surface `F = 0` (at `u = r <= r_max`) always lies
//! inside the band, so extraction never needs a field value that was not
//! computed.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::dual_contour::{marching_tets_mesh, mesh_is_manifold};
use crate::morph;
use crate::winding::{voxel_count, MeshBvh};
use crate::MAX_VOXELS;

/// Sentinel for the exact Euclidean distance transform (squared index units).
const EDT_INF: f32 = 1.0e20;

/// Coarse sampling stride used to seed the narrow-band distance query.  A fine
/// cell is only queried when the 1-Lipschitz bound over its coarse block leaves
/// it within the band (see [`narrow_band_distance`]).
const COARSE_STRIDE: usize = 3;

/// Far-field tolerance for the cheap winding prescreen.  A cell whose loose
/// value is farther than [`PRESCREEN_MARGIN`] from 0.5 is decided from it; only
/// cells inside the margin pay for the exact winding number.
const PRESCREEN_TOL: f64 = 0.1;
const PRESCREEN_MARGIN: f64 = 0.15;

/// Parameters for [`dressing_coat`].  `r_base`/`r_max`/`sigma` may be `None`,
/// in which case they are derived from the input's median edge length.
#[derive(Clone, Copy, Debug)]
pub struct DressingParams {
    pub voxel: Option<f64>,
    pub r_base: Option<f64>,
    pub r_max: Option<f64>,
    pub sigma: Option<f64>,
    pub band_voxels: f64,
    pub margin_voxels: f64,
}

impl Default for DressingParams {
    fn default() -> Self {
        DressingParams {
            voxel: None,
            r_base: None,
            r_max: None,
            sigma: None,
            band_voxels: 2.0,
            margin_voxels: 3.0,
        }
    }
}

/// Bookkeeping reported back to the caller.
#[derive(Clone, Debug)]
pub struct DressingInfo {
    pub dims: [usize; 3],
    pub origin: [f64; 3],
    pub voxel: f64,
    pub r_base: f64,
    pub r_max: f64,
    pub sigma: f64,
    pub band: f64,
    pub voxels: u64,
    pub band_cells: usize,
    pub sign_cells: usize,
    pub gwn_cells: usize,
    pub gwn_exact_cells: usize,
    pub coarsened: bool,
    pub fallback: bool,
    pub manifold: bool,
    pub seconds: f64,
    pub band_seconds: f64,
    pub field_seconds: f64,
    pub extract_seconds: f64,
}

/// Extracted coat mesh plus its report.
pub struct DressingMesh {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub info: DressingInfo,
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Median edge length over all triangle edges (0 for an empty mesh).
pub fn median_edge_length(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> f64 {
    if tris.is_empty() {
        return 0.0;
    }
    let mut lens: Vec<f64> = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let d = [
                verts[a][0] - verts[b][0],
                verts[a][1] - verts[b][1],
                verts[a][2] - verts[b][2],
            ];
            lens.push(dot(d, d).sqrt());
        }
    }
    lens.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    lens[lens.len() / 2]
}

/// Resolve the radius parameters the prototype used.
fn resolve_radius(me: f64, voxel: f64, p: &DressingParams) -> (f64, f64, f64) {
    let r_base = p
        .r_base
        .unwrap_or_else(|| (0.3 * me).clamp(0.25 * voxel, 0.8 * voxel));
    let r_max = p.r_max.unwrap_or_else(|| (3.5 * me).min(2.5));
    let sigma = p.sigma.unwrap_or_else(|| (2.0 * me).max(2.0 * voxel));
    (r_base, r_max, sigma)
}

/// Plan the padded grid, coarsening the voxel until it fits `MAX_VOXELS`.
fn plan_dressing_grid(
    bmin: [f64; 3],
    bmax: [f64; 3],
    voxel_opt: Option<f64>,
    band: f64,
    pad: usize,
) -> ([f64; 3], f64, [usize; 3], bool) {
    let ext = [
        (bmax[0] - bmin[0]).max(1e-12),
        (bmax[1] - bmin[1]).max(1e-12),
        (bmax[2] - bmin[2]).max(1e-12),
    ];
    let maxext = ext[0].max(ext[1]).max(ext[2]);
    let mut voxel = voxel_opt.filter(|v| *v > 0.0).unwrap_or(maxext / 96.0);
    if !voxel.is_finite() || voxel <= 0.0 {
        voxel = maxext / 96.0;
    }
    let _ = band;
    let mut coarsened = false;
    loop {
        let mut dims = [0usize; 3];
        for i in 0..3 {
            dims[i] = ((bmax[i] - bmin[i]) / voxel).ceil() as usize + 1 + 2 * pad;
        }
        if voxel_count(dims) <= MAX_VOXELS {
            let origin = [
                bmin[0] - pad as f64 * voxel,
                bmin[1] - pad as f64 * voxel,
                bmin[2] - pad as f64 * voxel,
            ];
            return (origin, voxel, dims, coarsened);
        }
        voxel *= 1.25;
        coarsened = true;
    }
}

/// Narrow-band unsigned distance: a coarse `closest_point` pass marks candidate
/// blocks, then only candidate fine cells get an exact query.  Returns
/// `(u, band_mask)` where `u` is `INF` outside the band.
fn narrow_band_distance(
    bvh: &MeshBvh,
    origin: [f64; 3],
    voxel: f64,
    dims: [usize; 3],
    band: f32,
) -> (Vec<f32>, Vec<u8>) {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let n = nx * ny * nz;
    let slice = nx * ny;
    let m = COARSE_STRIDE;
    let cd0 = (nx + m - 1) / m;
    let cd1 = (ny + m - 1) / m;
    let cd2 = (nz + m - 1) / m;
    let cslice = cd0 * cd1;

    let mut u_coarse = vec![0f32; cd0 * cd1 * cd2];
    u_coarse
        .par_chunks_mut(cslice)
        .enumerate()
        .for_each(|(ck, chunk)| {
            let kz = ck * m;
            if kz >= nz {
                return;
            }
            let z = origin[2] + voxel * kz as f64;
            for cj in 0..cd1 {
                let jy = cj * m;
                if jy >= ny {
                    continue;
                }
                let y = origin[1] + voxel * jy as f64;
                let row = cj * cd0;
                for ci in 0..cd0 {
                    let ix = ci * m;
                    if ix >= nx {
                        continue;
                    }
                    let x = origin[0] + voxel * ix as f64;
                    chunk[row + ci] = bvh.closest_point([x, y, z]).1 as f32;
                }
            }
        });

    // A fine cell in a block is at most the block diagonal away from the
    // block's min-corner sample, and the unsigned distance is 1-Lipschitz.
    let bound = (m as f64 * voxel * 3.0f64.sqrt()) as f32;
    let cutoff = band + bound;

    let mut u = vec![f32::INFINITY; n];
    let mut mask = vec![0u8; n];
    u.par_chunks_mut(slice)
        .zip(mask.par_chunks_mut(slice))
        .enumerate()
        .for_each(|(k, (urow, mrow))| {
            let ck = k / m;
            let z = origin[2] + voxel * k as f64;
            for j in 0..ny {
                let cj = j / m;
                let y = origin[1] + voxel * j as f64;
                let row = j * nx;
                for i in 0..nx {
                    let ci = i / m;
                    let uc = u_coarse[ci + cd0 * (cj + cd1 * ck)];
                    if uc >= cutoff {
                        continue;
                    }
                    let x = origin[0] + voxel * i as f64;
                    let d = bvh.closest_point([x, y, z]).1;
                    if (d as f32) < band {
                        urow[row + i] = d as f32;
                        mrow[row + i] = 1;
                    }
                }
            }
        });
    (u, mask)
}

/// Exact Euclidean distance (in voxels) to the nearest seed cell.
fn defect_distance(dims: [usize; 3], origin: [f64; 3], voxel: f64, pts: &[[f64; 3]]) -> Vec<f32> {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let mut d = vec![EDT_INF; nx * ny * nz];
    for p in pts {
        let i = ((p[0] - origin[0]) / voxel).round();
        let j = ((p[1] - origin[1]) / voxel).round();
        let k = ((p[2] - origin[2]) / voxel).round();
        if i < 0.0 || j < 0.0 || k < 0.0 {
            continue;
        }
        let (i, j, k) = (i as isize, j as isize, k as isize);
        if i as usize >= nx || j as usize >= ny || k as usize >= nz {
            continue;
        }
        d[i as usize + nx * (j as usize + ny * k as usize)] = 0.0;
    }
    morph::edt_3d(&mut d, dims);
    d
}

/// Flood-fill the free region reachable from the grid boundary.
fn flood_exterior(
    dims: [usize; 3],
    band_mask: &[u8],
    u: &[f32],
    r: &[f32],
    barrier_floor: f32,
) -> Vec<u8> {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let n = nx * ny * nz;
    let mut free = vec![0u8; n];
    free.par_iter_mut().enumerate().for_each(|(idx, f)| {
        // The barrier that stops the exterior flood must be at least one voxel
        // thick: with a sub-voxel (or zero) radius the `u >= r` test alone
        // leaves no closed shell and the flood leaks into the solid, adding a
        // spurious inner surface.
        *f = if band_mask[idx] == 0 || u[idx] >= r[idx].max(barrier_floor) {
            1
        } else {
            0
        };
    });

    let mut ext = vec![0u8; n];
    let mut stack: Vec<u32> = Vec::with_capacity(n / 4);
    let push = |idx: usize, ext: &mut [u8], stack: &mut Vec<u32>| {
        if free[idx] != 0 && ext[idx] == 0 {
            ext[idx] = 1;
            stack.push(idx as u32);
        }
    };
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                if i == 0 || i + 1 == nx || j == 0 || j + 1 == ny || k == 0 || k + 1 == nz {
                    push(i + nx * (j + ny * k), &mut ext, &mut stack);
                }
            }
        }
    }
    while let Some(p) = stack.pop() {
        let p = p as usize;
        let i = p % nx;
        let j = (p / nx) % ny;
        let k = p / (nx * ny);
        if i > 0 {
            push(p - 1, &mut ext, &mut stack);
        }
        if i + 1 < nx {
            push(p + 1, &mut ext, &mut stack);
        }
        if j > 0 {
            push(p - nx, &mut ext, &mut stack);
        }
        if j + 1 < ny {
            push(p + nx, &mut ext, &mut stack);
        }
        if k > 0 {
            push(p - nx * ny, &mut ext, &mut stack);
        }
        if k + 1 < nz {
            push(p + nx * ny, &mut ext, &mut stack);
        }
    }
    ext
}

/// Weld coincident marching-tetrahedra vertices by quantized position, drop
/// degenerate triangles and exact duplicate faces, and report the vertex map.
///
/// `marching_tets` emits a fresh vertex triple per triangle, so its raw output
/// is index-soup; welding restores the shared-edge topology that makes the
/// coat a genuine two-manifold mesh.
fn weld_and_clean(
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
    voxel: f64,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    let scale = 1.0 / (voxel * 1.0e-6).max(1.0e-12);
    let mut map: HashMap<(i64, i64, i64), u32> = HashMap::with_capacity(verts.len());
    let mut out_verts: Vec<[f64; 3]> = Vec::with_capacity(verts.len());
    let mut remap: Vec<u32> = vec![0u32; verts.len()];
    for (old, p) in verts.iter().enumerate() {
        let key = (
            (p[0] * scale).round() as i64,
            (p[1] * scale).round() as i64,
            (p[2] * scale).round() as i64,
        );
        let new = *map.entry(key).or_insert_with(|| {
            let idx = out_verts.len() as u32;
            out_verts.push(*p);
            idx
        });
        remap[old] = new;
    }
    let mut out_tris: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
    let mut seen: HashSet<[u32; 3]> = HashSet::with_capacity(tris.len());
    for t in tris {
        let a = remap[t[0] as usize];
        let b = remap[t[1] as usize];
        let c = remap[t[2] as usize];
        if a == b || b == c || c == a {
            continue;
        }
        let mut key = [a, b, c];
        key.sort_unstable();
        if !seen.insert(key) {
            continue;
        }
        out_tris.push([a, b, c]);
    }
    (out_verts, out_tris)
}

/// Make a closed, edge-manifold triangle soup consistently oriented by
/// propagating orientation across shared edges (breadth-first).  The gradient
/// heuristic in `marching_tets` is ambiguous where the field is flat, which
/// left a few arbitrarily oriented faces on boxy shapes.
fn orient_consistently(tris: &mut [[u32; 3]]) {
    let n = tris.len();
    if n == 0 {
        return;
    }
    let mut edge_faces: HashMap<(u32, u32), Vec<usize>> =
        HashMap::with_capacity(tris.len() * 3);
    for (fi, t) in tris.iter().enumerate() {
        for e in 0..3 {
            let a = t[e];
            let b = t[(e + 1) % 3];
            let k = if a < b { (a, b) } else { (b, a) };
            edge_faces.entry(k).or_default().push(fi);
        }
    }
    let mut visited = vec![false; n];
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for seed in 0..n {
        if visited[seed] {
            continue;
        }
        visited[seed] = true;
        queue.push_back(seed);
        while let Some(fi) = queue.pop_front() {
            let t = tris[fi];
            for e in 0..3 {
                let a = t[e];
                let b = t[(e + 1) % 3];
                let k = if a < b { (a, b) } else { (b, a) };
                let Some(faces) = edge_faces.get(&k) else {
                    continue;
                };
                for &gi in faces {
                    if gi == fi || visited[gi] {
                        continue;
                    }
                    let gt = tris[gi];
                    let mut same = false;
                    for e2 in 0..3 {
                        if gt[e2] == a && gt[(e2 + 1) % 3] == b {
                            same = true;
                            break;
                        }
                    }
                    if same {
                        tris[gi].swap(1, 2);
                    }
                    visited[gi] = true;
                    queue.push_back(gi);
                }
            }
        }
    }
}

/// Full Dressing coat extraction.  See the module docs for the pipeline.
#[allow(clippy::too_many_arguments)]
pub fn dressing_coat(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    defect_pts: &[[f64; 3]],
    params: &DressingParams,
) -> DressingMesh {
    let t_start = std::time::Instant::now();
    let empty = |dims: [usize; 3], origin: [f64; 3], voxel: f64| DressingMesh {
        verts: Vec::new(),
        tris: Vec::new(),
        info: DressingInfo {
            dims,
            origin,
            voxel,
            r_base: 0.0,
            r_max: 0.0,
            sigma: 0.0,
            band: 0.0,
            voxels: voxel_count(dims),
            band_cells: 0,
            sign_cells: 0,
            gwn_cells: 0,
            gwn_exact_cells: 0,
            coarsened: false,
            fallback: false,
            manifold: false,
            seconds: t_start.elapsed().as_secs_f64(),
            band_seconds: 0.0,
            field_seconds: 0.0,
            extract_seconds: 0.0,
        },
    };
    if verts.is_empty() || tris.is_empty() {
        return empty([0, 0, 0], [0.0, 0.0, 0.0], 1.0);
    }

    let mut bmin = [f64::INFINITY; 3];
    let mut bmax = [f64::NEG_INFINITY; 3];
    for v in verts {
        for i in 0..3 {
            bmin[i] = bmin[i].min(v[i]);
            bmax[i] = bmax[i].max(v[i]);
        }
    }

    // Resolve the voxel first so the radius defaults (which depend on it) and
    // the padding are consistent.
    let me = median_edge_length(verts, tris);
    let ext = [
        (bmax[0] - bmin[0]).max(1e-12),
        (bmax[1] - bmin[1]).max(1e-12),
        (bmax[2] - bmin[2]).max(1e-12),
    ];
    let mut voxel = params
        .voxel
        .filter(|v| *v > 0.0)
        .unwrap_or_else(|| ext[0].max(ext[1]).max(ext[2]) / 96.0);
    if !voxel.is_finite() || voxel <= 0.0 {
        voxel = 1.0;
    }
    let r_max0 = resolve_radius(me, voxel, params).1;
    let band = r_max0 + params.band_voxels * voxel;
    let pad = ((band / voxel).ceil() as usize) + params.margin_voxels.ceil() as usize;
    let (origin, voxel, dims, coarsened) =
        plan_dressing_grid(bmin, bmax, Some(voxel), band, pad);
    let n = dims[0] * dims[1] * dims[2];
    if n == 0 {
        return empty(dims, origin, voxel);
    }
    // Re-derive after potential coarsening so r and band track the final voxel.
    let (r_base, r_max, sigma) = resolve_radius(me, voxel, params);
    let band = r_max + params.band_voxels * voxel;
    let band32 = band as f32;
    let r_base32 = r_base as f32;
    let r_max32 = r_max as f32;
    let sigma32 = sigma as f32;

    let bvh = MeshBvh::from_arrays(verts, tris);
    let t_band = std::time::Instant::now();
    let (u, band_mask) = narrow_band_distance(&bvh, origin, voxel, dims, band32);
    let band_cells = band_mask.iter().filter(|&&b| b != 0).count();
    let band_seconds = t_band.elapsed().as_secs_f64();

    // Variable radius r(x): larger near defects.
    let has_defect = !defect_pts.is_empty();
    let dseed = if has_defect {
        defect_distance(dims, origin, voxel, defect_pts)
    } else {
        Vec::new()
    };
    let voxel32 = voxel as f32;
    let r_at = |idx: usize| -> f32 {
        if !has_defect {
            return r_base32;
        }
        let d = dseed[idx].max(0.0).sqrt() * voxel32;
        let z = d / sigma32;
        r_base32 + (r_max32 - r_base32) * (-0.5 * z * z).exp()
    };

    // Dense r for the flood fill's free test.
    let mut r_dense = vec![r_base32; n];
    if has_defect {
        r_dense
            .par_iter_mut()
            .enumerate()
            .for_each(|(idx, rv)| *rv = r_at(idx));
    }
    let ext = flood_exterior(dims, &band_mask, &u, &r_dense, voxel32);

    // Scalar field F = s - r, with the winding sign evaluated only for band
    // cells whose sign is not already forced negative by u < r.
    let mut field = vec![0f32; n];
    let sign_count = AtomicUsize::new(0);
    let exact_count = AtomicUsize::new(0);
    let t_field = std::time::Instant::now();
    let nx = dims[0];
    let ny = dims[1];
    let slice = nx * ny;
    field
        .par_chunks_mut(slice)
        .enumerate()
        .for_each(|(k, frow)| {
            let base = k * slice;
            let z = origin[2] + voxel * k as f64;
            for j in 0..ny {
                let y = origin[1] + voxel * j as f64;
                let row = j * nx;
                for i in 0..nx {
                    let idx = base + row + i;
                    if band_mask[idx] == 0 {
                        frow[row + i] = if ext[idx] != 0 { band32 } else { -band32 };
                        continue;
                    }
                    let uu = u[idx];
                    let r = r_dense[idx];
                    if uu <= r {
                        frow[row + i] = -uu - r;
                    } else {
                        let x = origin[0] + voxel * i as f64;
                        // Cheap loose-tolerance query first; the exact value is
                        // only needed where the sign is genuinely ambiguous
                        // (Generalized winding near 0.5).
                        let wl = bvh.winding_at_tol([x, y, z], PRESCREEN_TOL);
                        let w = if (wl - 0.5).abs() > PRESCREEN_MARGIN {
                            wl
                        } else {
                            exact_count.fetch_add(1, Ordering::Relaxed);
                            bvh.winding_at([x, y, z])
                        };
                        frow[row + i] = if w > 0.5 { -uu - r } else { uu - r };
                        sign_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

    let field_seconds = t_field.elapsed().as_secs_f64();
    let t_extract = std::time::Instant::now();
    let dc = marching_tets_mesh(&field, dims, origin, voxel);
    let (out_verts, mut out_tris) = weld_and_clean(&dc.verts, &dc.tris, voxel);
    orient_consistently(&mut out_tris);
    let manifold = mesh_is_manifold(&out_verts, &out_tris);
    // Consistent outward orientation: the coat encloses {F < 0}, so the signed
    // volume of an outward-oriented, closed mesh is positive.
    let vol: f64 = out_tris
        .iter()
        .map(|t| {
            let a = out_verts[t[0] as usize];
            let b = out_verts[t[1] as usize];
            let c = out_verts[t[2] as usize];
            dot(a, cross(b, c)) / 6.0
        })
        .sum();
    if vol < 0.0 {
        for t in out_tris.iter_mut() {
            t.swap(1, 2);
        }
    }

    let info = DressingInfo {
        dims,
        origin,
        voxel,
        r_base,
        r_max,
        sigma,
        band,
        voxels: voxel_count(dims),
        band_cells,
        sign_cells: sign_count.load(Ordering::Relaxed),
        gwn_cells: sign_count.load(Ordering::Relaxed),
        gwn_exact_cells: exact_count.load(Ordering::Relaxed),
        coarsened,
        fallback: dc.fallback,
        manifold,
        seconds: t_start.elapsed().as_secs_f64(),
        band_seconds,
        field_seconds,
        extract_seconds: t_extract.elapsed().as_secs_f64(),
    };
    DressingMesh {
        verts: out_verts,
        tris: out_tris,
        info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(size: f64) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let s = size / 2.0;
        let v = vec![
            [-s, -s, -s],
            [s, -s, -s],
            [s, s, -s],
            [-s, s, -s],
            [-s, -s, s],
            [s, -s, s],
            [s, s, s],
            [-s, s, s],
        ];
        let f = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ];
        (v, f)
    }

    fn sphere(nlat: usize, nlon: usize) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let mut v = Vec::new();
        for i in 0..=nlat {
            let theta = std::f64::consts::PI * i as f64 / nlat as f64;
            for j in 0..nlon {
                let phi = 2.0 * std::f64::consts::PI * j as f64 / nlon as f64;
                v.push([
                    theta.sin() * phi.cos(),
                    theta.sin() * phi.sin(),
                    theta.cos(),
                ]);
            }
        }
        let idx = |i: usize, j: usize| i * nlon + (j % nlon);
        let mut f = Vec::new();
        for i in 0..nlat {
            for j in 0..nlon {
                let (a, b, c, d) = (idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1));
                f.push([a, b, d]);
                f.push([b, c, d]);
            }
        }
        (v, f)
    }

    fn signed_offsets(m: &DressingMesh, verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Vec<f64> {
        let bvh = MeshBvh::from_arrays(verts, tris);
        m.verts
            .iter()
            .map(|p| {
                let (q, _d, ti) = bvh.closest_point(*p);
                let t = &tris[ti];
                let a = verts[t[0]];
                let b = verts[t[1]];
                let c = verts[t[2]];
                let n = cross(
                    [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                    [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                );
                let nl = dot(n, n).sqrt().max(1e-30);
                dot([p[0] - q[0], p[1] - q[1], p[2] - q[2]], [n[0] / nl, n[1] / nl, n[2] / nl])
            })
            .collect()
    }

    fn report_stats(tag: &str, offs: &[f64], voxel: f64) {
        let mut s = offs.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = s.len();
        let mean: f64 = s.iter().sum::<f64>() / n as f64;
        let pct = |q: f64| s[((n as f64 - 1.0) * q) as usize];
        eprintln!(
            "BIAS {tag} n={n} mean={:.4}v p50={:.4} p90={:.4} p99={:.4} min={:.4} max={:.4}",
            mean / voxel,
            pct(0.5) / voxel,
            pct(0.9) / voxel,
            pct(0.99) / voxel,
            s[0] / voxel,
            s[n - 1] / voxel
        );
    }

    #[test]
    fn zero_radius_bias_sphere_and_cube() {
        let (sv, sf) = sphere(48, 96);
        let p = DressingParams {
            voxel: Some(0.04),
            r_base: Some(0.0),
            r_max: Some(0.0),
            sigma: Some(1.0),
            ..Default::default()
        };
        let sm = dressing_coat(&sv, &sf, &[], &p);
        let so = signed_offsets(&sm, &sv, &sf);
        report_stats("sphere", &so, 0.04);
        let mean = so.iter().sum::<f64>() / so.len() as f64;
        let mut ss = so.clone();
        ss.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(mean.abs() < 0.05 * 0.04, "sphere mean bias {mean}");
        assert!(ss[ss.len() * 99 / 100].abs() < 0.1 * 0.04, "sphere p99");

        let (cv, cf) = cube(10.0);
        let pc = DressingParams {
            voxel: Some(0.1),
            r_base: Some(0.0),
            r_max: Some(0.0),
            sigma: Some(1.0),
            ..Default::default()
        };
        let cm = dressing_coat(&cv, &cf, &[], &pc);
        let co = signed_offsets(&cm, &cv, &cf);
        report_stats("cube", &co, 0.1);
        let mean = co.iter().sum::<f64>() / co.len() as f64;
        assert!(mean.abs() < 0.05 * 0.1, "cube mean bias {mean}");
    }

    #[test]
    fn coat_of_cube_is_manifold() {
        let (v, f) = cube(10.0);
        let p = DressingParams {
            voxel: Some(0.5),
            r_base: Some(1.0),
            r_max: Some(1.0),
            sigma: Some(1.0),
            ..Default::default()
        };
        let m = dressing_coat(&v, &f, &[], &p);
        assert!(!m.tris.is_empty(), "coat should have faces");
        assert!(m.info.manifold, "coat should be two-manifold");
        assert!(m.info.gwn_cells > 0, "some sign cells are expected");
        // The coat body diagonal must not touch the grid boundary.
        assert!(m.info.band_cells > 0);
    }

    #[test]
    fn radius_is_larger_near_defect() {
        let (v, f) = cube(10.0);
        let p = DressingParams {
            voxel: Some(0.5),
            r_base: Some(0.25),
            r_max: Some(2.0),
            sigma: Some(1.0),
            ..Default::default()
        };
        let with = dressing_coat(&v, &f, &[[0.0, 0.0, 5.0]], &p);
        let without = dressing_coat(&v, &f, &[], &p);
        assert!(with.tris.len() > without.tris.len());
    }

    #[test]
    fn median_edge_of_unit_cube() {
        let (v, f) = cube(2.0);
        let me = median_edge_length(&v, &f);
        assert!((me - 2.0).abs() < 1e-9);
    }
}
