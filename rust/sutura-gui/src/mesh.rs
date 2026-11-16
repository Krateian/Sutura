use std::collections::{HashMap, HashSet};
use std::path::Path;

use sutura_core::load_mesh;

/// Flat-shaded triangle soup ready for the GPU/CPU preview.
#[derive(Clone, Debug)]
pub struct DisplayMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub center: [f32; 3],
    pub radius: f32,
    /// Per-vertex defect weight (0 = clean, 1 = touches a boundary/non-manifold edge).
    pub defect: Vec<f32>,
    pub defect_faces: usize,
}

impl DisplayMesh {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mesh = load_mesh(path).map_err(|e| e.to_string())?;
        if mesh.is_empty() {
            return Err("mesh contains no triangles".to_string());
        }
        let (lo, hi) = mesh
            .bbox()
            .ok_or_else(|| "mesh has no vertices".to_string())?;
        let center = [
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            (lo[2] + hi[2]) * 0.5,
        ];
        let mut radius: f32 = 0.0;
        for v in &mesh.verts {
            let d = [v[0] - center[0], v[1] - center[1], v[2] - center[2]];
            radius = radius.max((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt());
        }
        if !radius.is_finite() || radius <= 1e-9 {
            radius = 1.0;
        }

        let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
        let tris_u32: Vec<[u32; 3]> = mesh
            .tris
            .iter()
            .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
            .collect();
        for tri in &tris_u32 {
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let (u, v) = (tri[a], tri[b]);
                let key = if u < v { (u, v) } else { (v, u) };
                *edges.entry(key).or_insert(0) += 1;
            }
        }
        let bad: HashSet<(u32, u32)> = edges
            .iter()
            .filter(|(_, count)| **count != 2)
            .map(|(edge, _)| *edge)
            .collect();

        let mut positions = Vec::with_capacity(mesh.tris.len() * 3);
        let mut normals = Vec::with_capacity(mesh.tris.len() * 3);
        let mut indices = Vec::with_capacity(mesh.tris.len() * 3);
        let mut defect = Vec::with_capacity(mesh.tris.len() * 3);
        let mut defect_faces = 0usize;
        for (tri_idx, tri) in mesh.tris.iter().enumerate() {
            let a = mesh.verts[tri[0] as usize];
            let b = mesh.verts[tri[1] as usize];
            let c = mesh.verts[tri[2] as usize];
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let normal = if len > 1e-12 {
                [n[0] / len, n[1] / len, n[2] / len]
            } else {
                [0.0, 0.0, 1.0]
            };
            let orig = tris_u32[tri_idx];
            let mut hit = false;
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let (u, v) = (orig[a], orig[b]);
                let key = if u < v { (u, v) } else { (v, u) };
                if bad.contains(&key) {
                    hit = true;
                    break;
                }
            }
            let flag = if hit {
                defect_faces += 1;
                1.0f32
            } else {
                0.0
            };
            let base = positions.len() as u32;
            for p in [a, b, c] {
                positions.push(p);
                normals.push(normal);
                defect.push(flag);
            }
            indices.push(base);
            indices.push(base + 1);
            indices.push(base + 2);
        }

        Ok(Self {
            positions,
            normals,
            indices,
            center,
            radius,
            defect,
            defect_faces,
        })
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn defect_ratio(&self) -> f32 {
        if self.triangle_count() == 0 {
            0.0
        } else {
            self.defect_faces as f32 / self.triangle_count() as f32
        }
    }
}
