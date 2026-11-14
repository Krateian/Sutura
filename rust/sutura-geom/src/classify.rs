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

/// Python's round-half-even at `decimals`.
pub fn py_round(value: f64, decimals: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = 10f64.powi(decimals);
    let scaled = value * factor;
    let floor = scaled.floor();
    let frac = scaled - floor;
    let rounded = if (frac - 0.5).abs() < 1e-9 {
        if (floor as i64) % 2 == 0 { floor } else { floor + 1.0 }
    } else {
        scaled.round()
    };
    rounded / factor
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

/// Smallest-eigenvalue eigenvector of a symmetric 3x3 matrix (Jacobi).
pub fn smallest_eigenvector(mut a: [[f64; 3]; 3]) -> [f64; 3] {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..32 {
        // largest off-diagonal
        let (mut p, mut q, mut max) = (0usize, 1usize, a[0][1].abs());
        for (i, j) in [(0, 2), (1, 2)] {
            if a[i][j].abs() > max {
                max = a[i][j].abs();
                p = i;
                q = j;
            }
        }
        if max < 1e-15 {
            break;
        }
        let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let akp = a[k][p];
            let akq = a[k][q];
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let apk = a[p][k];
            let aqk = a[q][k];
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        for k in 0..3 {
            let vkp = v[k][p];
            let vkq = v[k][q];
            v[k][p] = c * vkp - s * vkq;
            v[k][q] = s * vkp + c * vkq;
        }
    }
    let mut best = 0usize;
    for i in 1..3 {
        if a[i][i] < a[best][best] {
            best = i;
        }
    }
    [v[0][best], v[1][best], v[2][best]]
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
