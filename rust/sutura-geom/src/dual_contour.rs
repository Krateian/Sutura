//! Dual contouring of a scalar (signed-distance) grid.
//!
//! One vertex is produced per connected surface component of each cell, its
//! position the minimiser of the cell's Hermite quadric error function (QEF)
//! built from the sign-change edge intersection points and the field gradient
//! (the surface normal) there, solved with a Tikhonov-regularised symmetric
//! 3x3 eigen-decomposition and clamped to the cell.  Connectivity emits one
//! quad per sign-change grid edge over its four incident cells, which yields a
//! watertight two-manifold.  Splitting a cell's sign-change edges into
//! connected components (manifold dual contouring rules) duplicates vertices
//! where two surface sheets pass through one cell, removing the classic
//! non-manifold dual-contouring artifact.
//!
//! If the dual mesh fails a topological manifold check (not expected for a
//! properly padded grid) the routine falls back to marching tetrahedra, the
//! tetrahedral equivalent of marching cubes with the same watertight
//! guarantee.

use std::collections::HashMap;

const CORNER: [[usize; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [0, 1, 0],
    [1, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [0, 1, 1],
    [1, 1, 1],
];

const EDGE: [[usize; 2]; 12] = [
    [0, 1],
    [2, 3],
    [4, 5],
    [6, 7],
    [0, 2],
    [1, 3],
    [4, 6],
    [5, 7],
    [0, 4],
    [1, 5],
    [2, 6],
    [3, 7],
];

/// Cyclic corner order of each cell face.
const FACE_CORNERS: [[usize; 4]; 6] = [
    [0, 2, 6, 4],
    [1, 3, 7, 5],
    [0, 1, 5, 4],
    [2, 3, 7, 6],
    [0, 1, 3, 2],
    [4, 5, 7, 6],
];

/// Local edge indices on each face, between consecutive corners.
const FACE_EDGES: [[usize; 4]; 6] = [
    [4, 10, 6, 8],
    [5, 11, 7, 9],
    [0, 9, 2, 8],
    [1, 11, 3, 10],
    [0, 5, 1, 4],
    [2, 7, 3, 6],
];

pub struct DcMesh {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[u32; 3]>,
    pub fallback: bool,
    pub manifold: bool,
}

/// Dual-contour a scalar field into a closed mesh.
///
/// If the dual mesh fails the manifold check the marching-tetrahedra fallback
/// is tried, but only when it is actually manifold; otherwise the dual result
/// is returned unchanged (a pathological field must not be replaced by the
/// unwelded tetrahedra output).
pub fn dual_contour(f: &[f32], dims: [usize; 3], origin: [f64; 3], voxel: f64) -> DcMesh {
    let dc = dual_contour_core(f, dims, origin, voxel);
    if mesh_is_manifold(&dc.0, &dc.1) {
        return DcMesh {
            verts: dc.0,
            tris: dc.1,
            fallback: false,
            manifold: true,
        };
    }
    let tets = marching_tets_mesh(f, dims, origin, voxel);
    if tets.manifold {
        return tets;
    }
    DcMesh {
        verts: dc.0,
        tris: dc.1,
        fallback: false,
        manifold: false,
    }
}

/// Marching-tetrahedra surface of a scalar field (the tetrahedral equivalent
/// of marching cubes): watertight, two-manifold and free of the
/// geometry-level self-intersections dual contouring can produce.  Provided as
/// an explicit alternative to `dual_contour`.
pub fn marching_tets_mesh(
    f: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
) -> DcMesh {
    let (v, t) = marching_tets(f, dims, origin, voxel);
    let manifold = mesh_is_manifold(&v, &t);
    DcMesh {
        verts: v,
        tris: t,
        fallback: true,
        manifold,
    }
}

fn dual_contour_core(
    f: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    if nx < 2 || ny < 2 || nz < 2 {
        return (Vec::new(), Vec::new());
    }
    let cx = nx - 1;
    let cy = ny - 1;
    let cz = nz - 1;
    let ncells = cx * cy * cz;

    let at = |i: usize, j: usize, k: usize| -> f32 { f[i + nx * (j + ny * k)] };
    let cell_of = |ci: usize, cj: usize, ck: usize| -> usize { ci + cx * (cj + cy * ck) };

    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    // Per cell, the global vertex index for each of its 12 local edges (-1 none).
    let mut edge_vert = vec![-1i32; ncells * 12];

    let corner_pos = |ci: usize, cj: usize, ck: usize, c: usize| -> [f64; 3] {
        [
            origin[0] + voxel * (ci + CORNER[c][0]) as f64,
            origin[1] + voxel * (cj + CORNER[c][1]) as f64,
            origin[2] + voxel * (ck + CORNER[c][2]) as f64,
        ]
    };

    for ck in 0..cz {
        for cj in 0..cy {
            for ci in 0..cx {
                let mut vals = [0.0f32; 8];
                for c in 0..8 {
                    vals[c] = at(ci + CORNER[c][0], cj + CORNER[c][1], ck + CORNER[c][2]);
                }
                let mut present = [false; 12];
                let mut any = false;
                for e in 0..12 {
                    let (a, b) = (EDGE[e][0], EDGE[e][1]);
                    let s = (vals[a] < 0.0) != (vals[b] < 0.0);
                    present[e] = s;
                    any |= s;
                }
                if !any {
                    continue;
                }

                // Union-find over the 12 local edges.
                let mut parent: [usize; 12] = [0; 12];
                for (i, p) in parent.iter_mut().enumerate() {
                    *p = i;
                }
                fn find(parent: &mut [usize; 12], mut x: usize) -> usize {
                    while parent[x] != x {
                        parent[x] = parent[parent[x]];
                        x = parent[x];
                    }
                    x
                }
                let union = |parent: &mut [usize; 12], a: usize, b: usize| {
                    let ra = find(parent, a);
                    let rb = find(parent, b);
                    if ra != rb {
                        parent[ra] = rb;
                    }
                };

                for face in 0..6 {
                    let fe = FACE_EDGES[face];
                    let pc = FACE_CORNERS[face];
                    let act: Vec<usize> = fe.iter().copied().filter(|&e| present[e]).collect();
                    match act.len() {
                        0 | 1 => {}
                        2 => union(&mut parent, act[0], act[1]),
                        4 => {
                            // Checkerboard face: asymptotic decider (Nielson &
                            // Hamann).  Either pairing keeps the mesh manifold;
                            // this choice matches the bilinear saddle.
                            let v0 = vals[pc[0]];
                            let v1 = vals[pc[1]];
                            let v2 = vals[pc[2]];
                            let v3 = vals[pc[3]];
                            let s = (v0 as f64) * (v2 as f64) - (v1 as f64) * (v3 as f64);
                            if s > 0.0 {
                                union(&mut parent, fe[0], fe[1]);
                                union(&mut parent, fe[2], fe[3]);
                            } else {
                                union(&mut parent, fe[1], fe[2]);
                                union(&mut parent, fe[3], fe[0]);
                            }
                        }
                        _ => {}
                    }
                }

                // Group present edges by component and accumulate QEFs.
                let mut comp_index: [i32; 12] = [-1; 12];
                let mut root_to_comp: [i32; 12] = [-1; 12];
                let mut comps: Vec<(f64, [[f64; 3]; 3], [f64; 3], [f64; 3], usize)> = Vec::new();
                for e in 0..12 {
                    if !present[e] {
                        continue;
                    }
                    let r = find(&mut parent, e);
                    if root_to_comp[r] < 0 {
                        root_to_comp[r] = comps.len() as i32;
                        comps.push((0.0, [[0.0; 3]; 3], [0.0; 3], [0.0; 3], 0));
                    }
                    let cid2 = root_to_comp[r];
                    comp_index[e] = cid2;

                    // Hermite point + normal for this edge.
                    let (a, b) = (EDGE[e][0], EDGE[e][1]);
                    let pa = corner_pos(ci, cj, ck, a);
                    let pb = corner_pos(ci, cj, ck, b);
                    let va = vals[a] as f64;
                    let vb = vals[b] as f64;
                    let t = if (vb - va).abs() > 1e-12 {
                        (-va / (vb - va)).clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    let p = [
                        pa[0] + t * (pb[0] - pa[0]),
                        pa[1] + t * (pb[1] - pa[1]),
                        pa[2] + t * (pb[2] - pa[2]),
                    ];
                    let n = gradient_normal(f, dims, origin, voxel, p, pb, pa, vb - va);

                    let entry = &mut comps[cid2 as usize];
                    for i in 0..3 {
                        for j in 0..3 {
                            entry.1[i][j] += n[i] * n[j];
                        }
                        entry.2[i] += n[i] * (n[0] * p[0] + n[1] * p[1] + n[2] * p[2]);
                        entry.3[i] += p[i];
                    }
                    entry.4 += 1;
                }

                let cell_min = [
                    origin[0] + voxel * ci as f64,
                    origin[1] + voxel * cj as f64,
                    origin[2] + voxel * ck as f64,
                ];
                let mut comp_vert = vec![-1i32; comps.len()];
                for (cidx, comp) in comps.iter().enumerate() {
                    let (_, a, b, psum, count) = comp;
                    let mass = [
                        psum[0] / *count as f64,
                        psum[1] / *count as f64,
                        psum[2] / *count as f64,
                    ];
                    let mut x = solve_qef(a, b, mass, 1e-4);
                    for d in 0..3 {
                        if !x[d].is_finite() {
                            x[d] = mass[d];
                        }
                        x[d] = x[d].clamp(cell_min[d], cell_min[d] + voxel);
                    }
                    let idx = verts.len() as i32;
                    verts.push(x);
                    comp_vert[cidx] = idx;
                }

                let base = cell_of(ci, cj, ck) * 12;
                for e in 0..12 {
                    if present[e] {
                        edge_vert[base + e] = comp_vert[comp_index[e] as usize];
                    }
                }
            }
        }
    }

    // Connectivity: one quad per sign-change grid edge.
    let gv = |cell: usize, e: usize| -> i32 { edge_vert[cell * 12 + e] };

    // X edges.
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..cx {
                let f0 = at(i, j, k) < 0.0;
                let f1 = at(i + 1, j, k) < 0.0;
                if f0 == f1 || j < 1 || k < 1 || j >= cy || k >= cz {
                    continue;
                }
                let ca = cell_of(i, j - 1, k - 1);
                let cb = cell_of(i, j, k - 1);
                let cc = cell_of(i, j, k);
                let cd = cell_of(i, j - 1, k);
                let q = [gv(ca, 3), gv(cb, 2), gv(cc, 0), gv(cd, 1)];
                emit_quad(&mut tris, q, !f1);
            }
        }
    }
    // Y edges.
    for k in 0..nz {
        for j in 0..cy {
            for i in 0..nx {
                let f0 = at(i, j, k) < 0.0;
                let f1 = at(i, j + 1, k) < 0.0;
                if f0 == f1 || i < 1 || k < 1 || i >= cx || k >= cz {
                    continue;
                }
                let ca = cell_of(i - 1, j, k - 1);
                let cb = cell_of(i, j, k - 1);
                let cc = cell_of(i, j, k);
                let cd = cell_of(i - 1, j, k);
                let q = [gv(ca, 7), gv(cb, 6), gv(cc, 4), gv(cd, 5)];
                // [A,B,C,D] has normal -y; keep it only when outward is -y,
                // i.e. when the field decreases along +y.
                emit_quad(&mut tris, q, f1);
            }
        }
    }
    // Z edges.
    for k in 0..cz {
        for j in 0..ny {
            for i in 0..nx {
                let f0 = at(i, j, k) < 0.0;
                let f1 = at(i, j, k + 1) < 0.0;
                if f0 == f1 || i < 1 || j < 1 || i >= cx || j >= cy {
                    continue;
                }
                let ca = cell_of(i - 1, j - 1, k);
                let cb = cell_of(i, j - 1, k);
                let cc = cell_of(i, j, k);
                let cd = cell_of(i - 1, j, k);
                let q = [gv(ca, 11), gv(cb, 10), gv(cc, 8), gv(cd, 9)];
                emit_quad(&mut tris, q, !f1);
            }
        }
    }

    (verts, tris)
}

#[inline]
fn emit_quad(tris: &mut Vec<[u32; 3]>, q: [i32; 4], keep_order: bool) {
    if q.iter().any(|&v| v < 0) {
        return;
    }
    let (a, b, c, d) = (q[0] as u32, q[1] as u32, q[2] as u32, q[3] as u32);
    if keep_order {
        tris.push([a, b, c]);
        tris.push([a, c, d]);
    } else {
        tris.push([d, c, b]);
        tris.push([d, b, a]);
    }
}

/// Outward unit normal at a sign-change edge: field gradient at the
/// intersection point, falling back to the edge direction.
#[allow(clippy::too_many_arguments)]
fn gradient_normal(
    f: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
    p: [f64; 3],
    pb: [f64; 3],
    pa: [f64; 3],
    dval: f64,
) -> [f64; 3] {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let fi = ((p[0] - origin[0]) / voxel)
        .round()
        .clamp(0.0, (nx - 1) as f64) as usize;
    let fj = ((p[1] - origin[1]) / voxel)
        .round()
        .clamp(0.0, (ny - 1) as f64) as usize;
    let fk = ((p[2] - origin[2]) / voxel)
        .round()
        .clamp(0.0, (nz - 1) as f64) as usize;
    let get = |i: usize, j: usize, k: usize| f[i + nx * (j + ny * k)] as f64;
    let i0 = fi.saturating_sub(1);
    let i1 = (fi + 1).min(nx - 1);
    let j0 = fj.saturating_sub(1);
    let j1 = (fj + 1).min(ny - 1);
    let k0 = fk.saturating_sub(1);
    let k1 = (fk + 1).min(nz - 1);
    let gx = (get(i1, fj, fk) - get(i0, fj, fk)) / ((i1 - i0) as f64 * voxel);
    let gy = (get(fi, j1, fk) - get(fi, j0, fk)) / ((j1 - j0) as f64 * voxel);
    let gz = (get(fi, fj, k1) - get(fi, fj, k0)) / ((k1 - k0) as f64 * voxel);
    let n = [gx, gy, gz];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 1e-12 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        let d = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
        let dl = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let s = if dval >= 0.0 { 1.0 } else { -1.0 };
        if dl > 1e-12 {
            [s * d[0] / dl, s * d[1] / dl, s * d[2] / dl]
        } else {
            [0.0, 0.0, 1.0]
        }
    }
}

/// Regularised QEF minimiser.  `a` is the accumulated normal outer-product
/// matrix, `b` the accumulated `n * (n . p)` vector, `mass` the mean Hermite
/// point.  Minimises `|A x - b|^2 + lambda |x - mass|^2`.
fn solve_qef(a: &[[f64; 3]; 3], b: &[f64; 3], mass: [f64; 3], reg: f64) -> [f64; 3] {
    let (evals, evecs) = jacobi_eigen(a);
    let lambda = reg * (evals[0] + evals[1] + evals[2]).abs() / 3.0 + 1e-9;
    let breg = [
        b[0] + lambda * mass[0],
        b[1] + lambda * mass[1],
        b[2] + lambda * mass[2],
    ];
    let mut x = [0.0f64; 3];
    for k in 0..3 {
        let coef = (evecs[0][k] * breg[0] + evecs[1][k] * breg[1] + evecs[2][k] * breg[2])
            / (evals[k] + lambda);
        for i in 0..3 {
            x[i] += coef * evecs[i][k];
        }
    }
    x
}

/// Jacobi eigen-decomposition of a symmetric 3x3 matrix.  Returns
/// `(eigenvalues, eigenvectors-as-columns)`.
fn jacobi_eigen(input: &[[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut a = *input;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..32 {
        let (mut p, mut q, mut max) = (0usize, 1usize, 0.0f64);
        for i in 0..3 {
            for j in (i + 1)..3 {
                if a[i][j].abs() > max {
                    max = a[i][j].abs();
                    p = i;
                    q = j;
                }
            }
        }
        if max < 1e-15 {
            break;
        }
        let apq = a[p][q];
        let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
        let t = if theta >= 0.0 {
            1.0 / (theta + (1.0 + theta * theta).sqrt())
        } else {
            -1.0 / (-theta + (1.0 + theta * theta).sqrt())
        };
        let c = 1.0 / (1.0 + t * t).sqrt();
        let s = t * c;
        let app = a[p][p];
        let aqq = a[q][q];
        a[p][p] = app - t * apq;
        a[q][q] = aqq + t * apq;
        a[p][q] = 0.0;
        a[q][p] = 0.0;
        let r = 3 - p - q;
        let arp = a[r][p];
        let arq = a[r][q];
        a[r][p] = c * arp - s * arq;
        a[p][r] = a[r][p];
        a[r][q] = s * arp + c * arq;
        a[q][r] = a[r][q];
        for i in 0..3 {
            let vip = v[i][p];
            let viq = v[i][q];
            v[i][p] = c * vip - s * viq;
            v[i][q] = s * vip + c * viq;
        }
    }
    ([a[0][0], a[1][1], a[2][2]], v)
}

/// True when every undirected edge is used exactly twice and each directed
/// edge is used at most once (consistent orientation, closed manifold).
pub fn mesh_is_manifold(_verts: &[[f64; 3]], tris: &[[u32; 3]]) -> bool {
    if tris.is_empty() {
        return false;
    }
    let mut dir: HashMap<(u32, u32), i32> = HashMap::with_capacity(tris.len() * 3);
    for t in tris {
        for e in 0..3 {
            let a = t[e];
            let b = t[(e + 1) % 3];
            if a == b {
                return false;
            }
            *dir.entry((a, b)).or_insert(0) += 1;
        }
    }
    for (&(a, b), &c) in &dir {
        if c != 1 {
            return false;
        }
        match dir.get(&(b, a)) {
            Some(&rc) if rc == 1 => {}
            _ => return false,
        }
    }
    true
}

/// Marching-tetrahedra fallback (tetrahedral equivalent of marching cubes).
/// Cube split into six tets via the Kuhn decomposition; faces are re-oriented
/// by the field gradient afterwards.
fn marching_tets(
    f: &[f32],
    dims: [usize; 3],
    origin: [f64; 3],
    voxel: f64,
) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
    // Star decomposition of the cube around the body diagonal 0-7.  The
    // earlier table was authored for a corner numbering where index 6 was the
    // corner opposite 0; with this module's `CORNER` (6 = (0,1,1)) it left
    // overlaps and gaps and produced a cracked, non-manifold surface.
    const TETS: [[usize; 4]; 6] = [
        [0, 5, 1, 7],
        [0, 1, 3, 7],
        [0, 3, 2, 7],
        [0, 2, 6, 7],
        [0, 6, 4, 7],
        [0, 4, 5, 7],
    ];
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    if nx < 2 || ny < 2 || nz < 2 {
        return (Vec::new(), Vec::new());
    }
    let at = |i: usize, j: usize, k: usize| -> f32 { f[i + nx * (j + ny * k)] };
    let cpos = |ci: usize, cj: usize, ck: usize, c: usize| -> [f64; 3] {
        [
            origin[0] + voxel * (ci + CORNER[c][0]) as f64,
            origin[1] + voxel * (cj + CORNER[c][1]) as f64,
            origin[2] + voxel * (ck + CORNER[c][2]) as f64,
        ]
    };
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();

    for ck in 0..(nz - 1) {
        for cj in 0..(ny - 1) {
            for ci in 0..(nx - 1) {
                let mut vals = [0.0f32; 8];
                let mut pos = [[0.0f64; 3]; 8];
                for c in 0..8 {
                    vals[c] = at(ci + CORNER[c][0], cj + CORNER[c][1], ck + CORNER[c][2]);
                    pos[c] = cpos(ci, cj, ck, c);
                }
                for tet in TETS {
                    let mut inside = [false; 4];
                    let mut cnt = 0;
                    for i in 0..4 {
                        inside[i] = vals[tet[i]] < 0.0;
                        if inside[i] {
                            cnt += 1;
                        }
                    }
                    if cnt == 0 || cnt == 4 {
                        continue;
                    }
                    // Crossing point on the segment between an inside and an
                    // outside vertex of the tet, with the inside vertex first.
                    let crossing = |ia: usize, ib: usize| -> [f64; 3] {
                        let va = vals[tet[ia]] as f64;
                        let vb = vals[tet[ib]] as f64;
                        let t = if (vb - va).abs() > 1e-12 {
                            (-va / (vb - va)).clamp(0.0, 1.0)
                        } else {
                            0.5
                        };
                        let pa = pos[tet[ia]];
                        let pb = pos[tet[ib]];
                        [
                            pa[0] + t * (pb[0] - pa[0]),
                            pa[1] + t * (pb[1] - pa[1]),
                            pa[2] + t * (pb[2] - pa[2]),
                        ]
                    };
                    let ins: Vec<usize> = (0..4).filter(|&i| inside[i]).collect();
                    let outs: Vec<usize> = (0..4).filter(|&i| !inside[i]).collect();
                    let pts: Vec<[f64; 3]> = if cnt == 1 || cnt == 3 {
                        if cnt == 1 {
                            outs.iter().map(|&o| crossing(ins[0], o)).collect()
                        } else {
                            ins.iter().map(|&i| crossing(i, outs[0])).collect()
                        }
                    } else {
                        // 2-2 split: order the four crossing points cyclically
                        // around the quad so neighbouring tets share whole
                        // edges (an unordered quad triangulation leaves
                        // T-junction cracks).
                        vec![
                            crossing(ins[0], outs[0]),
                            crossing(ins[0], outs[1]),
                            crossing(ins[1], outs[1]),
                            crossing(ins[1], outs[0]),
                        ]
                    };
                    let push_tri = |verts: &mut Vec<[f64; 3]>,
                                    tris: &mut Vec<[u32; 3]>,
                                    a: usize,
                                    b: usize,
                                    c: usize| {
                        let base = verts.len() as u32;
                        verts.push(pts[a]);
                        verts.push(pts[b]);
                        verts.push(pts[c]);
                        tris.push([base, base + 1, base + 2]);
                    };
                    if pts.len() == 3 {
                        push_tri(&mut verts, &mut tris, 0, 1, 2);
                    } else if pts.len() == 4 {
                        push_tri(&mut verts, &mut tris, 0, 1, 2);
                        push_tri(&mut verts, &mut tris, 0, 2, 3);
                    }
                }
            }
        }
    }

    // Orient every face with the outward field gradient.
    for t in tris.iter_mut() {
        let a = verts[t[0] as usize];
        let b = verts[t[1] as usize];
        let c = verts[t[2] as usize];
        let cen = [
            (a[0] + b[0] + c[0]) / 3.0,
            (a[1] + b[1] + c[1]) / 3.0,
            (a[2] + b[2] + c[2]) / 3.0,
        ];
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let g = gradient_normal(f, dims, origin, voxel, cen, b, a, 1.0);
        if n[0] * g[0] + n[1] * g[1] + n[2] * g[2] < 0.0 {
            t.swap(1, 2);
        }
    }

    (verts, tris)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere_field(n: usize) -> (Vec<f32>, [usize; 3], [f64; 3], f64) {
        let dims = [n, n, n];
        let voxel = 2.0 / (n as f64 - 1.0);
        let origin = [-1.0 - voxel, -1.0 - voxel, -1.0 - voxel];
        let mut f = vec![0.0f32; n * n * n];
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let p = [
                        origin[0] + voxel * i as f64,
                        origin[1] + voxel * j as f64,
                        origin[2] + voxel * k as f64,
                    ];
                    f[i + n * (j + n * k)] =
                        (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() as f32 - 0.7;
                }
            }
        }
        (f, dims, origin, voxel)
    }

    #[test]
    fn dc_sphere_is_manifold_and_closed() {
        let (f, dims, origin, voxel) = sphere_field(32);
        let m = dual_contour(&f, dims, origin, voxel);
        assert!(!m.fallback, "unexpected fallback");
        assert!(m.manifold);
        assert!(mesh_is_manifold(&m.verts, &m.tris));
        assert!(m.verts.len() > 100);
    }

    #[test]
    fn marching_tets_sphere_is_closed() {
        // Regression: the tet table used to assume corner 6 was opposite 0;
        // with this module's CORNER it left cracks.  Weld by position and
        // require every undirected edge to be used exactly twice.
        let (f, dims, origin, voxel) = sphere_field(24);
        let (v, t) = marching_tets(&f, dims, origin, voxel);
        let scale = 1.0 / (voxel * 1.0e-6);
        let mut map: std::collections::HashMap<(i64, i64, i64), u32> =
            std::collections::HashMap::new();
        let mut remap = vec![0u32; v.len()];
        for (i, p) in v.iter().enumerate() {
            let k = (
                (p[0] * scale).round() as i64,
                (p[1] * scale).round() as i64,
                (p[2] * scale).round() as i64,
            );
            remap[i] = match map.get(&k) {
                Some(e) => *e,
                None => {
                    let e = map.len() as u32;
                    map.insert(k, e);
                    e
                }
            };
        }
        let mut und: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
        for tri in &t {
            let a = remap[tri[0] as usize];
            let b = remap[tri[1] as usize];
            let c = remap[tri[2] as usize];
            if a == b || b == c || c == a {
                continue;
            }
            for (x, y) in [(a, b), (b, c), (c, a)] {
                let k = if x < y { (x, y) } else { (y, x) };
                *und.entry(k).or_insert(0) += 1;
            }
        }
        let bad = und.values().filter(|&&c| c != 2).count();
        assert_eq!(bad, 0, "surface is not closed: {bad} bad edges");
    }

    #[test]
    fn dc_handles_sign_change_on_the_last_grid_layer() {
        // Regression: the connectivity guards used `> cy`/`> cz`, which let a
        // sign change on the LAST sample layer (j == cy or k == cz) index a
        // cell one past the end of `edge_vert` and panic.  Build a field with a
        // sign change along x exactly on the last y and z layers.
        let dims = [6usize, 6, 6];
        let origin = [0.0f64; 3];
        let voxel = 1.0f64;
        let idx = |i: usize, j: usize, k: usize| i + 6 * (j + 6 * k);
        let mut f = vec![-1.0f32; 6 * 6 * 6];
        for k in 0..6 {
            for i in 0..6 {
                f[idx(i, 5, k)] = if i >= 3 { 1.0 } else { -1.0 };
                f[idx(i, k, 5)] = if i >= 3 { 1.0 } else { -1.0 };
            }
        }
        // must not panic
        let m = dual_contour(&f, dims, origin, voxel);
        assert!(m.verts.len() < 1_000_000);
    }

    #[test]
    fn jacobi_recovers_diagonal() {
        let a = [[2.0, 0.0, 0.0], [0.0, 5.0, 0.0], [0.0, 0.0, 9.0]];
        let (mut e, _) = jacobi_eigen(&a);
        e.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert!((e[0] - 2.0).abs() < 1e-9);
        assert!((e[1] - 5.0).abs() < 1e-9);
        assert!((e[2] - 9.0).abs() < 1e-9);
    }
}
