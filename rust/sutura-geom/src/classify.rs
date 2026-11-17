//! Numeric feature kernels shared by the Rust classifier and Python.
//!
//! The Python v2 classifier calls these through the `python` feature, so the
//! feature values (dihedral bands, RANSAC plane count/area, developability)
//! are produced by one implementation in both languages.

use crate::rng::Pcg64;

const RANSAC_SEED: u64 = 0;
const RANSAC_ITERS: usize = 60;
const RANSAC_POOL: usize = 4000;
const RANSAC_MIN_AREA_FRAC: f64 = 0.01;
const RANSAC_MAX_PLANES: usize = 24;
const RANSAC_ANGLE_COS: f64 = 0.990;
const RANSAC_DIST_FRAC: f64 = 0.02;

/// Python's `round(value, decimals)`.
///
/// Python rounds the *exact binary value* half-to-even; `format!("{:.*}")`
/// applies the same correct rounding, so formatting and parsing back matches
/// it (the previous epsilon snap treated 0.8075 and 0.8075000000000001 alike).
pub fn py_round(value: f64, decimals: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    if decimals < 0 {
        return value;
    }
    let text = format!("{:.*}", decimals as usize, value);
    text.parse::<f64>().unwrap_or(value)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    fn sub(self, other: Self) -> Self {
        Self {
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }
    fn cross(self, other: Self) -> Self {
        Self {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }
    fn dot(self, other: Self) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }
    fn norm(self) -> f64 {
        self.dot(self).sqrt()
    }
    fn scale(self, k: f64) -> Self {
        Self {
            x: self.x * k,
            y: self.y * k,
            z: self.z * k,
        }
    }
    fn add(self, other: Self) -> Self {
        Self {
            x: self.x + other.x,
            y: self.y + other.y,
            z: self.z + other.z,
        }
    }
}

pub fn vertex(verts: &[[f64; 3]], index: usize) -> Vec3 {
    Vec3 {
        x: verts[index][0],
        y: verts[index][1],
        z: verts[index][2],
    }
}

pub struct FaceGeometry {
    normals: Vec<Vec3>,
    centroids: Vec<Vec3>,
    areas: Vec<f64>,
}

pub fn face_geometry(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> FaceGeometry {
    let mut normals = Vec::with_capacity(tris.len());
    let mut centroids = Vec::with_capacity(tris.len());
    let mut areas = Vec::with_capacity(tris.len());
    for tri in tris {
        let a = vertex(verts, tri[0]);
        let b = vertex(verts, tri[1]);
        let c = vertex(verts, tri[2]);
        let cross = b.sub(a).cross(c.sub(a));
        let mag = cross.norm();
        let normal = if mag == 0.0 {
            Vec3::default()
        } else {
            cross.scale(1.0 / mag)
        };
        normals.push(normal);
        centroids.push(a.add(b).add(c).scale(1.0 / 3.0));
        areas.push(mag * 0.5);
    }
    FaceGeometry {
        normals,
        centroids,
        areas,
    }
}

pub fn dihedral_stats(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (f64, f64, f64) {
    let geom = face_geometry(verts, tris);
    let face_count = tris.len();
    if face_count == 0 {
        return (0.0, 0.0, 0.0);
    }
    let mut edges: Vec<((usize, usize), usize)> = Vec::with_capacity(face_count * 3);
    for (face, tri) in tris.iter().enumerate() {
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let (u, v) = (tri[a], tri[b]);
            edges.push((if u < v { (u, v) } else { (v, u) }, face));
        }
    }
    edges.sort_by_key(|(key, _)| *key);
    let mut near90 = 0usize;
    let mut flat = 0usize;
    let mut gentle = 0usize;
    let mut pairs = 0usize;
    let mut i = 0;
    while i < edges.len() {
        let mut j = i + 1;
        while j < edges.len() && edges[j].0 == edges[i].0 {
            j += 1;
        }
        if j - i == 2 {
            let f0 = edges[i].1;
            let f1 = edges[i + 1].1;
            let cos = geom.normals[f0].dot(geom.normals[f1]).clamp(-1.0, 1.0);
            let deg = cos.acos().to_degrees();
            if (60.0..=120.0).contains(&deg) {
                near90 += 1;
            }
            if deg < 1.0 {
                flat += 1;
            }
            if (1.0..15.0).contains(&deg) {
                gentle += 1;
            }
            pairs += 1;
        }
        i = j;
    }
    if pairs == 0 {
        return (0.0, 0.0, 0.0);
    }
    let n = pairs as f64;
    (
        (near90 as f64 / n) * 100.0,
        (flat as f64 / n) * 100.0,
        (gentle as f64 / n) * 100.0,
    )
}


pub fn ransac_plane_features(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (usize, f64) {
    if tris.is_empty() {
        return (0, 0.0);
    }
    let geom = face_geometry(verts, tris);
    let total_area: f64 = geom.areas.iter().sum();
    if total_area <= 0.0 {
        return (0, 0.0);
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for v in verts {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let diag = ((hi[0] - lo[0]).powi(2)
        + (hi[1] - lo[1]).powi(2)
        + (hi[2] - lo[2]).powi(2))
    .sqrt();
    let diag = if diag <= 0.0 { 1.0 } else { diag };
    let dist_tol = RANSAC_DIST_FRAC * diag;

    let n_faces = tris.len();
    let mut rng = Pcg64::new(RANSAC_SEED);
    let pool_size = n_faces.min(RANSAC_POOL);
    let pool: Vec<usize> = rng
        .choice_without_replacement(n_faces as u64, pool_size)
        .into_iter()
        .map(|i| i as usize)
        .collect();
    let mut pool_in = vec![true; pool_size];
    let mut remaining = vec![true; n_faces];
    let mut plane_count = 0usize;
    let mut covered = 0.0f64;

    while plane_count < RANSAC_MAX_PLANES {
        let idx: Vec<usize> = (0..n_faces).filter(|&i| remaining[i]).collect();
        if idx.len() < 3 {
            break;
        }
        let mut best_hits: isize = -1;
        let mut best_plane: Option<(Vec3, Vec3)> = None;
        for _ in 0..RANSAC_ITERS {
            let seed = idx[rng.integers(idx.len() as u64) as usize];
            let pn = geom.normals[seed];
            let pc = geom.centroids[seed];
            let mut hits = 0isize;
            for (pi, &face) in pool.iter().enumerate() {
                if !pool_in[pi] {
                    continue;
                }
                let d = geom.centroids[face].sub(pc).dot(pn).abs();
                let align = geom.normals[face].dot(pn).abs();
                if d <= dist_tol && align >= RANSAC_ANGLE_COS {
                    hits += 1;
                }
            }
            if hits > best_hits {
                best_hits = hits;
                best_plane = Some((pn, pc));
            }
        }
        let Some((pn, pc)) = best_plane else { break };
        let rem: Vec<usize> = (0..n_faces).filter(|&i| remaining[i]).collect();
        let inl: Vec<usize> = rem
            .into_iter()
            .filter(|&face| {
                let d = geom.centroids[face].sub(pc).dot(pn).abs();
                let align = geom.normals[face].dot(pn).abs();
                d <= dist_tol && align >= RANSAC_ANGLE_COS
            })
            .collect();
        if inl.is_empty() {
            break;
        }
        let inl_area: f64 = inl.iter().map(|&i| geom.areas[i]).sum();
        if inl_area / total_area < RANSAC_MIN_AREA_FRAC {
            break;
        }
        plane_count += 1;
        covered += inl_area;
        for &i in &inl {
            remaining[i] = false;
        }
        let inl_set: std::collections::HashSet<usize> = inl.into_iter().collect();
        for (pi, &face) in pool.iter().enumerate() {
            if inl_set.contains(&face) {
                pool_in[pi] = false;
            }
        }
        if remaining.iter().filter(|&&r| r).count() < 3 {
            break;
        }
    }
    (plane_count, py_round(covered.min(total_area) / total_area, 4))
}

/// Area-weighted fraction of normals on a common great circle.
pub fn curvature_developable_fraction(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> f64 {
    if tris.len() < 3 {
        return 0.0;
    }
    let geom = face_geometry(verts, tris);
    let total: f64 = geom.areas.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    let weights: Vec<f64> = geom.areas.iter().map(|a| a / total).collect();
    let mean = Vec3 {
        x: geom
            .normals
            .iter()
            .zip(&weights)
            .map(|(n, w)| n.x * w)
            .sum(),
        y: geom
            .normals
            .iter()
            .zip(&weights)
            .map(|(n, w)| n.y * w)
            .sum(),
        z: geom
            .normals
            .iter()
            .zip(&weights)
            .map(|(n, w)| n.z * w)
            .sum(),
    };
    let mut cov = [[0.0f64; 3]; 3];
    for (n, w) in geom.normals.iter().zip(&weights) {
        let d = [n.x - mean.x, n.y - mean.y, n.z - mean.z];
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += w * d[i] * d[j];
            }
        }
    }
    let axis = smallest_eigenvector(cov);
    let sin_tol = (8.0f64).to_radians().sin();
    let mut covered = 0.0;
    for (n, w) in geom.normals.iter().zip(&weights) {
        let npar = (n.x * axis[0] + n.y * axis[1] + n.z * axis[2]).abs();
        if npar <= sin_tol {
            covered += w;
        }
    }
    py_round(covered.min(1.0), 4)
}

/// Smallest-eigenvalue eigenvector of a symmetric 3x3 matrix.
///
/// Delegates to the shared [`crate::eigh3`] kernel so the classifier and every
/// other caller use one implementation (and one sign convention).
pub fn smallest_eigenvector(a: [[f64; 3]; 3]) -> [f64; 3] {
    crate::eigh3::smallest_eigenvector(a)
}



/// All classifier feature values in one call.
#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub near90: f64,
    pub flat: f64,
    pub gentle: f64,
    pub plane_count: usize,
    pub plane_area: f64,
    pub developable_fraction: f64,
}

pub fn features(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Features {
    let (near90, flat, gentle) = dihedral_stats(verts, tris);
    let (plane_count, plane_area) = ransac_plane_features(verts, tris);
    let developable_fraction = curvature_developable_fraction(verts, tris);
    Features {
        near90,
        flat,
        gentle,
        plane_count,
        plane_area,
        developable_fraction,
    }
}

/// The five opt-in edge-tiebreak signals (`mesh_classifier_v2._edge_extra_features`):
/// `[oppmax_std, don_mean, dihed_mean, rmin_mean, gc_mean]`.
///
/// Mirrors the NumPy implementation: MeshCNN per-edge apex ratio statistics,
/// the Difference-of-Normals 1-ring mean and the Gaussian-curvature angle
/// deficit. Returns zeros for degenerate input.
pub fn edge_extra_features(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> [f64; 5] {
    let v_count = verts.len();
    let f_count = tris.len();
    if f_count == 0 || v_count == 0 {
        return [0.0; 5];
    }
    let geom = face_geometry(verts, tris);
    let v_span = (v_count + 1) as i64;

    // Python builds edges as [t[:,0,1], t[:,1,2], t[:,2,0]] concatenated; the
    // stable sort below keeps that order for the (at most two) faces per key.
    let mut edges: Vec<(i64, usize)> = Vec::with_capacity(f_count * 3);
    for k in 0..3 {
        for (face, tri) in tris.iter().enumerate() {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            edges.push((lo as i64 * v_span + hi as i64, face));
        }
    }
    edges.sort_by_key(|(key, _)| *key);

    let mut dihed_sum = 0.0f64;
    let mut rmin_sum = 0.0f64;
    let mut rmax_values: Vec<f64> = Vec::new();
    let mut pairs = 0usize;
    let mut i = 0usize;
    while i < edges.len() {
        let mut j = i + 1;
        while j < edges.len() && edges[j].0 == edges[i].0 {
            j += 1;
        }
        if j - i == 2 {
            let f1 = edges[i].1;
            let f2 = edges[i + 1].1;
            let ei = (edges[i].0 / v_span) as usize;
            let ej = (edges[i].0 % v_span) as usize;
            let cos = geom.normals[f1]
                .dot(geom.normals[f2])
                .clamp(-1.0, 1.0);
            dihed_sum += std::f64::consts::PI - cos.acos();
            let r1 = apex_ratio(verts, tris, &geom.areas, f1, ei, ej);
            let r2 = apex_ratio(verts, tris, &geom.areas, f2, ei, ej);
            let (lo, hi) = if r1 < r2 { (r1, r2) } else { (r2, r1) };
            rmin_sum += lo;
            rmax_values.push(hi);
            pairs += 1;
        }
        i = j;
    }
    let (dihed_mean, rmin_mean, oppmax_std) = if pairs == 0 {
        (0.0, 0.0, 0.0)
    } else {
        let n = pairs as f64;
        let mean = rmax_values.iter().sum::<f64>() / n;
        let var = rmax_values
            .iter()
            .map(|x| (x - mean) * (x - mean))
            .sum::<f64>()
            / n;
        (dihed_sum / n, rmin_sum / n, var.sqrt())
    };

    // Vertex normals: sum of unit face normals over the incident faces.
    let mut vn = vec![Vec3::default(); v_count];
    let mut vcnt = vec![0.0f64; v_count];
    for (face, tri) in tris.iter().enumerate() {
        for &vi in tri {
            vn[vi] = vn[vi].add(geom.normals[face]);
            vcnt[vi] += 1.0;
        }
    }
    for i in 0..v_count {
        let c = if vcnt[i] == 0.0 { 1.0 } else { vcnt[i] };
        let p = vn[i].scale(1.0 / c);
        let mag = p.norm();
        vn[i] = if mag == 0.0 {
            Vec3::default()
        } else {
            p.scale(1.0 / mag)
        };
    }

    // 1-ring adjacency with the same multiplicities as the Python lists.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); v_count];
    for tri in tris {
        for (p, q) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            adj[p].push(q);
            adj[q].push(p);
        }
    }
    let mut don_sum = 0.0f64;
    for i in 0..v_count {
        if !adj[i].is_empty() {
            let mut mean = Vec3::default();
            for &nb in &adj[i] {
                mean = mean.add(vn[nb]);
            }
            mean = mean.scale(1.0 / adj[i].len() as f64);
            don_sum += vn[i].sub(mean).norm();
        }
    }
    let don_mean = if v_count == 0 {
        0.0
    } else {
        don_sum / v_count as f64
    };

    // Gaussian curvature roughness from the per-vertex angle deficit.
    let mut asum = vec![0.0f64; v_count];
    for tri in tris {
        let (x, y, z) = (tri[0], tri[1], tri[2]);
        for (p, q, r) in [(x, y, z), (y, z, x), (z, x, y)] {
            let e1 = vertex(verts, q).sub(vertex(verts, p));
            let e2 = vertex(verts, r).sub(vertex(verts, p));
            let n1 = e1.norm();
            let n2 = e2.norm();
            if n1 != 0.0 && n2 != 0.0 {
                asum[p] += (e1.dot(e2) / (n1 * n2)).clamp(-1.0, 1.0).acos();
            }
        }
    }
    let mut gc_sum = 0.0f64;
    for value in &asum {
        gc_sum += (2.0 * std::f64::consts::PI - value).abs();
    }
    let gc_mean = if v_count == 0 {
        0.0
    } else {
        gc_sum / v_count as f64
    };

    [oppmax_std, don_mean, dihed_mean, rmin_mean, gc_mean]
}

fn apex_ratio(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    areas: &[f64],
    face: usize,
    i: usize,
    j: usize,
) -> f64 {
    let tri = &tris[face];
    let mut col = 0usize;
    for (idx, &v) in tri.iter().enumerate() {
        if v != i && v != j {
            col = idx;
            break;
        }
    }
    let _ = col;
    let base = vertex(verts, j).sub(vertex(verts, i)).norm();
    let hgt = 2.0 * areas[face] / (base + 1e-12);
    hgt / (base + 1e-12)
}


#[cfg(test)]
mod edge_extra_tests {
    use super::edge_extra_features;

    #[test]
    fn edge_extra_features_is_deterministic_and_finite() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        let tris = vec![[0, 1, 2], [1, 3, 2], [2, 3, 4]];
        let a = edge_extra_features(&verts, &tris);
        let b = edge_extra_features(&verts, &tris);
        assert_eq!(a, b);
        assert!(a.iter().all(|x| x.is_finite()));
        assert_eq!(edge_extra_features(&[], &[]), [0.0; 5]);
    }
}
