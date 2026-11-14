//! Mesh type classification: the classic engine and the v2 experimental head.
//!
//! Bit-parity port of `sutura/mesh_classifier.py` and
//! `sutura/mesh_classifier_v2.py`. The v2 head uses RANSAC plane features
//! drawn through [`crate::rng::Pcg64`], which mirrors NumPy's `default_rng`
//! exactly (see `rng.rs`).
//!
//! The classic engine remains the fallback: with no baked head, v2 returns the
//! classic decision verbatim (metrics still carry the RANSAC/curvature
//! features).

use serde_json::{json, Value};

use crate::rng::Pcg64;

const MECH_NEAR90: f64 = 35.0;
const W_NEAR90: f64 = 8.0;
const MECH_FLAT: f64 = 50.0;
const W_FLAT: f64 = 12.0;
const ORG_FLAT: f64 = 45.0;
const W_ORG_FLAT: f64 = 6.0;
const ORG_GENTLE: f64 = 15.0;
const W_GENTLE: f64 = 5.0;
const MECH_FLOOR: f64 = 0.7;
const ORG_FLOOR: f64 = 0.5;

const RANSAC_SEED: u64 = 0;
const RANSAC_ITERS: usize = 60;
const RANSAC_POOL: usize = 4000;
const RANSAC_MIN_AREA_FRAC: f64 = 0.01;
const RANSAC_MAX_PLANES: usize = 24;
const RANSAC_ANGLE_COS: f64 = 0.990;
const RANSAC_DIST_FRAC: f64 = 0.02;

const HEAD_MECH_FLOOR: f64 = 0.50;
const HEAD_MARGIN: f64 = 0.15;
const CLASSIC_STRONG: f64 = 0.70;
const V2_WEIGHTS: [f64; 7] = [2.3823, 2.9866, -0.9694, -0.5230, 0.5083, 2.2289, 1.0432];
const V2_MEAN: [f64; 6] = [20.9532, 30.5532, 35.5085, 4.9296, 0.2917, 0.5122];
const V2_STD: [f64; 6] = [24.1622, 17.6785, 30.5004, 6.4313, 0.3129, 0.2819];

/// Python's `round(value, decimals)` (round-half-even on the decimal digits).
pub fn py_round(value: f64, decimals: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = 10f64.powi(decimals);
    let scaled = value * factor;
    let floor = scaled.floor();
    let frac = scaled - floor;
    let rounded = if (frac - 0.5).abs() < 1e-9 {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    };
    rounded / factor
}

fn sigmoid(x: f64) -> f64 {
    let x = x.clamp(-50.0, 50.0);
    1.0 / (1.0 + (-x).exp())
}

#[derive(Clone, Copy, Debug, Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
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

fn vertex(verts: &[[f64; 3]], index: usize) -> Vec3 {
    Vec3 {
        x: verts[index][0],
        y: verts[index][1],
        z: verts[index][2],
    }
}

struct FaceGeometry {
    normals: Vec<Vec3>,
    centroids: Vec<Vec3>,
    areas: Vec<f64>,
}

fn face_geometry(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> FaceGeometry {
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

fn dihedral_stats(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (f64, f64, f64) {
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

fn class_scores(near90: f64, flat: f64, gentle: f64) -> (f64, f64) {
    let mech_near = sigmoid((near90 - MECH_NEAR90) / W_NEAR90);
    let mech_flat = sigmoid((flat - MECH_FLAT) / W_FLAT);
    let mechanical = mech_near.max(mech_flat);
    let org_near = 1.0 - mech_near;
    let org_flat = 1.0 - sigmoid((flat - ORG_FLAT) / W_ORG_FLAT);
    let org_gentle = sigmoid((gentle - ORG_GENTLE) / W_GENTLE);
    (mechanical, organic_min(org_near, org_flat, org_gentle))
}

fn organic_min(a: f64, b: f64, c: f64) -> f64 {
    a.min(b).min(c)
}

/// `{'type', 'confidence', 'metrics'}` from the classic engine.
pub fn classify_classic(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Value {
    let (near90, flat, gentle) = dihedral_stats(verts, tris);
    let (mechanical, organic) = class_scores(near90, flat, gentle);
    let mut metrics = serde_json::Map::new();
    metrics.insert("near90".into(), json!(py_round(near90, 2)));
    metrics.insert("flat".into(), json!(py_round(flat, 2)));
    metrics.insert("gentle".into(), json!(py_round(gentle, 2)));
    metrics.insert(
        "mechanical_score".into(),
        json!(py_round(mechanical, 3)),
    );
    metrics.insert("organic_score".into(), json!(py_round(organic, 3)));
    if mechanical >= MECH_FLOOR {
        return json!({"type": "mechanical", "confidence": py_round(mechanical, 3),
                      "metrics": Value::Object(metrics)});
    }
    if organic >= ORG_FLOOR {
        return json!({"type": "organic", "confidence": py_round(organic, 3),
                      "metrics": Value::Object(metrics)});
    }
    metrics.insert(
        "leaning".into(),
        json!(if mechanical >= organic {
            "mechanical"
        } else {
            "organic"
        }),
    );
    json!({"type": "unknown", "confidence": py_round(mechanical.max(organic), 3),
           "metrics": Value::Object(metrics)})
}

/// RANSAC plane features: `(plane_count, covered_area_fraction)`.
fn ransac_plane_features(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> (usize, f64) {
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
fn curvature_developable_fraction(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> f64 {
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
fn smallest_eigenvector(mut a: [[f64; 3]; 3]) -> [f64; 3] {
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

fn head_p_mechanical(features: [f64; 6]) -> f64 {
    let xs: Vec<f64> = features
        .iter()
        .enumerate()
        .map(|(i, x)| (x - V2_MEAN[i]) / V2_STD[i])
        .collect();
    let z = V2_WEIGHTS[0]
        + xs.iter()
            .enumerate()
            .map(|(i, x)| V2_WEIGHTS[i + 1] * x)
            .sum::<f64>();
    sigmoid(z)
}

fn classic_verdict(mechanical: f64, organic: f64) -> (&'static str, f64) {
    if mechanical >= MECH_FLOOR {
        ("mechanical", mechanical)
    } else if organic >= ORG_FLOOR {
        ("organic", organic)
    } else {
        ("unknown", mechanical.max(organic))
    }
}

/// `classify_mesh(..., engine='experimental')` with the baked head.
pub fn classify_v2(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Value {
    let (near90, flat, gentle) = dihedral_stats(verts, tris);
    let (plane_count, plane_area) = ransac_plane_features(verts, tris);
    let developable = curvature_developable_fraction(verts, tris);
    let (mechanical, organic) = class_scores(near90, flat, gentle);

    let mut metrics = serde_json::Map::new();
    metrics.insert("near90".into(), json!(py_round(near90, 2)));
    metrics.insert("flat".into(), json!(py_round(flat, 2)));
    metrics.insert("gentle".into(), json!(py_round(gentle, 2)));
    metrics.insert(
        "mechanical_score".into(),
        json!(py_round(mechanical, 3)),
    );
    metrics.insert("organic_score".into(), json!(py_round(organic, 3)));
    metrics.insert("plane_count".into(), json!(plane_count));
    metrics.insert("plane_area".into(), json!(plane_area));
    metrics.insert("developable_fraction".into(), json!(developable));

    let p_mech = head_p_mechanical([
        near90,
        flat,
        gentle,
        plane_count as f64,
        plane_area,
        developable,
    ]);
    if !(0.0..=1.0).contains(&p_mech) {
        return classify_classic(verts, tris);
    }
    let mut head_type = if p_mech >= HEAD_MECH_FLOOR {
        "mechanical"
    } else {
        "organic"
    };
    let mut head_conf = ((p_mech - 0.5).abs() * 2.0).clamp(0.0, 1.0);
    if head_conf < HEAD_MARGIN {
        let (classic_type, classic_score) = classic_verdict(mechanical, organic);
        if classic_type == head_type && classic_score >= CLASSIC_STRONG {
            head_conf = classic_score;
        }
    }
    if head_type.is_empty() {
        head_type = "unknown";
    }
    json!({"type": head_type, "confidence": py_round(head_conf, 3),
           "metrics": Value::Object(metrics)})
}

/// Public switch: `engine` is `classic` or anything else (experimental).
pub fn classify_mesh(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    engine: &str,
) -> Value {
    if engine == "classic" {
        classify_classic(verts, tris)
    } else {
        classify_v2(verts, tris)
    }
}
