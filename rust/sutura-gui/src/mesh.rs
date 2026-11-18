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
    /// Per-vertex defect weight (0 = clean, 1 = touches a defect *vertex*).
    pub defect: Vec<f32>,
    pub defect_faces: usize,
    /// Per-face index into the deviation ramp, when a reference was supplied.
    /// Empty means "no deviation available", and the viewer falls back to the
    /// defect blend — the same precedence `heatmap.draw_frame` uses.
    /// Indexed by **original** face id, which is also the soup's face id.
    pub dev_idx: Vec<u16>,
    /// Per-vertex deviation over the original vertex array, for the headless dump.
    pub deviation: Vec<f32>,
    /// The original indexed topology. The soup drops it, but the heatmap fields
    /// are defined per *original* vertex/face (`heatmap.prepare_render`), so it is
    /// kept here rather than reconstructed.
    pub src_verts: Vec<[f32; 3]>,
    pub src_tris: Vec<[i32; 3]>,
    /// Per-face defect-kind colour from `diagnose_core`'s `face_mask`. Empty
    /// until requested: the diagnosis is much heavier than the surface check the
    /// preview needs, so it is opt-in (the "Defect colours" toggle).
    pub defect_colors: Vec<Option<[u8; 3]>>,
}

/// Per-face defect-kind colours, from the diagnosis `face_mask` bits.
///
/// The palette is Python's `DEFECT_COLORS`, verbatim (`defects.py:262`) -- it is
/// described there as "the SINGLE source of truth", so copying the exact RGB
/// values is the point: two viewers showing different colours for the same defect
/// would be a parity failure. Bit assignments come from `_V2_FACE_BITS`.
///
/// `(0, 0, 0)` means "no defect colour", as in Python; a face with several kinds
/// keeps the most severe, and the order below is that precedence.
pub const DEFECT_COLORS: [(&str, u8, [u8; 3]); 8] = [
    ("non_manifold", 2, [235, 60, 70]),
    ("self_intersection", 16, [200, 60, 200]),
    ("duplicate", 4, [40, 180, 200]),
    ("noise", 32, [140, 140, 140]),
    ("interior", 64, [110, 80, 200]),
    ("thin_wall", 128, [245, 120, 180]),
    ("degenerate", 8, [250, 210, 60]),
    ("orientation", 1, [255, 140, 60]),
];

/// Resolve one face's `face_mask` byte to an RGB colour.
pub fn defect_kind_color(byte: u8) -> Option<[u8; 3]> {
    DEFECT_COLORS
        .iter()
        .find(|(_, bit, _)| byte & bit != 0)
        .map(|(_, _, rgb)| *rgb)
}

impl DisplayMesh {
    /// Compute per-face defect-kind colours, once.
    ///
    /// Optical because `diagnose_core` is a full topological diagnosis -- far
    /// more work than the surface check the preview always does. Idempotent, so
    /// toggling the mode twice does not recompute.
    pub fn ensure_defect_colors(&mut self) -> Result<(), String> {
        if !self.defect_colors.is_empty() || self.src_tris.is_empty() {
            return Ok(());
        }
        let verts: Vec<[f64; 3]> = self
            .src_verts
            .iter()
            .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
            .collect();
        let tris: Vec<[usize; 3]> = self
            .src_tris
            .iter()
            .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
            .collect();
        let report = sutura_geom::diagnosis::diagnose_core(&verts, &tris, "summary");
        if report.face_mask.len() != self.src_tris.len() {
            return Err(format!(
                "diagnosis returned {} face masks for {} faces",
                report.face_mask.len(),
                self.src_tris.len()
            ));
        }
        self.defect_colors = report
            .face_mask
            .iter()
            .map(|&b| defect_kind_color(b))
            .collect();
        Ok(())
    }

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

        // Defect set, exactly as Python's render path builds it: union every hole
        // rim and non-manifold region **vertex**, then flag a face when any of its
        // corners is in that set (`heatmap.prepare_render`). This is NOT "the face
        // has a bad edge" — the two definitions agree on boundary loops but differ
        // for isolated non-manifold vertices, and the vertex rule is the one the
        // heatmap draws.
        let report = sutura_core::report::detect_defects(&mesh.verts, &mesh.tris, true);
        let mut defect_vertices: Vec<i64> = Vec::new();
        for h in &report.holes {
            if let Some(v) = &h.verts_idx {
                defect_vertices.extend(v);
            }
        }
        for nm in &report.non_manifold {
            if let Some(v) = &nm.verts_idx {
                defect_vertices.extend(v);
            }
        }
        defect_vertices.sort_unstable();
        defect_vertices.dedup();
        let face_is_defect = sutura_geom::heatmap::defect_faces(&defect_vertices, &mesh.tris);

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
            let flag = if face_is_defect.get(tri_idx).copied().unwrap_or(false) {
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
            dev_idx: Vec::new(),
            deviation: Vec::new(),
            src_verts: mesh.verts.clone(),
            src_tris: mesh.tris.clone(),
            defect_colors: Vec::new(),
        })
    }

    /// Compute the deviation ramp against a reference (original) mesh.
    ///
    /// Mirrors `viewer_data_render.main`: per-vertex distance from **this**
    /// (repaired) mesh to the reference surface, then the face means reduced
    /// through the 1%/99% window. The baseline view has no reference, so it keeps
    /// the defect-only display — as Python does.
    ///
    /// Both fields are indexed by the *original* topology, which is what
    /// `heatmap.prepare_render` works with; the soup is only a display artefact.
    pub fn with_deviation(&mut self, reference_path: &Path) -> Result<(), String> {
        let reference = load_mesh(reference_path).map_err(|e| e.to_string())?;
        self.with_deviation_against(&reference)
    }

    /// `with_deviation` against an already-loaded reference mesh.
    ///
    /// The GUI holds the original in memory, so this avoids re-reading and
    /// re-parsing the input file on every Before/After switch.
    pub fn with_deviation_against(&mut self, reference: &sutura_core::Mesh) -> Result<(), String> {
        let ref_v: Vec<[f64; 3]> = reference
            .verts
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let ref_t: Vec<[usize; 3]> = reference
            .tris
            .iter()
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect();
        let dst_v: Vec<[f64; 3]> = self
            .src_verts
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();

        let dist = sutura_geom::heatmap::deviation(&dst_v, &ref_v, &ref_t);
        self.dev_idx = sutura_geom::heatmap::deviation_quantile_index(&dist, &self.src_tris);
        self.deviation = dist.iter().map(|d| *d as f32).collect();
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect mask must follow Python's **vertex** rule, and the deviation
    /// fields must be indexable by original face id.
    #[test]
    fn defect_uses_vertex_rule_and_deviation_is_populated() {
        let dir = std::env::temp_dir().join(format!("sutura-mesh-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // A quad with one triangle removed: a boundary loop makes every rim
        // vertex a defect vertex, so the remaining triangle is flagged.
        let a = dir.join("open.stl");
        let verts = [[0.0f32,0.0,0.0],[1.0,0.0,0.0],[1.0,1.0,0.0],[0.0,1.0,0.0]];
        let tris = [[0i32,1,2],[0,2,3]];
        sutura_core::save_mesh(&a, &sutura_core::Mesh::new(verts.to_vec(), tris.to_vec())).unwrap();
        let m = DisplayMesh::load(&a).expect("load");
        assert_eq!(m.defect.len(), m.positions.len());
        assert!(m.defect_faces > 0, "an open mesh has defect faces");
        // The weight must be exactly 1.0 on defect faces — a fractional or tiny
        // value would render as an almost-invisible tint in the shader.
        let nz: Vec<f32> = m.defect.iter().copied().filter(|d| *d > 0.0).collect();
        assert!(!nz.is_empty(), "some vertices are defective");
        for v in &nz {
            assert_eq!(*v, 1.0, "defect weight must be exactly 1.0, got {v}");
        }
        assert_eq!(nz.len(), m.defect_faces * 3, "3 soup vertices per defect face");
        assert_eq!(m.src_tris.len(), m.triangle_count());
        // No reference supplied: no ramp, and the viewer falls back to defect.
        assert!(m.dev_idx.is_empty());
        assert!(m.deviation.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
