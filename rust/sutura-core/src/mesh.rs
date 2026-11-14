use crate::error::{MeshError, MeshResult};

/// A triangle mesh at the core boundary: float32 vertices, int32 indices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub verts: Vec<[f32; 3]>,
    pub tris: Vec<[i32; 3]>,
}

impl Mesh {
    pub fn new(verts: Vec<[f32; 3]>, tris: Vec<[i32; 3]>) -> Self {
        Self { verts, tris }
    }

    pub fn is_empty(&self) -> bool {
        self.verts.is_empty() || self.tris.is_empty()
    }

    pub fn vertex_count(&self) -> usize {
        self.verts.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Axis-aligned bounding box, or `None` for a mesh without vertices.
    pub fn bbox(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut iter = self.verts.iter().copied();
        let first = iter.next()?;
        let mut lo = first;
        let mut hi = first;
        for p in iter {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Some((lo, hi))
    }

    /// Reject triangles whose indices are outside the vertex array.
    pub fn validate(&self) -> MeshResult<()> {
        let nv = self.verts.len();
        for (i, t) in self.tris.iter().enumerate() {
            for &index in t {
                if index < 0 || index as usize >= nv {
                    return Err(MeshError::IndexOutOfRange {
                        triangle: i,
                        index: i64::from(index),
                        vertices: nv,
                    });
                }
            }
        }
        Ok(())
    }
}

/// One `<mesh>` block from a multi-object container (3MF).
#[derive(Clone, Debug, PartialEq)]
pub struct MeshBlock {
    pub model_name: String,
    pub index: usize,
    /// Byte span of the whole `<mesh>...</mesh>` block in the model XML.
    pub span: (usize, usize),
    pub open_tag: String,
    pub close_tag: String,
    pub prefix: String,
    pub mesh: Mesh,
    pub triangle_attrs: Vec<Option<String>>,
}

/// Mesh container formats supported by the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    Stl,
    Obj,
    ThreeMf,
    Ply,
}

impl SourceFormat {
    /// Detect the format from a path extension (ASCII case-insensitive).
    pub fn from_extension(path: &str) -> MeshResult<Self> {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "stl" => Ok(Self::Stl),
            "obj" => Ok(Self::Obj),
            "3mf" => Ok(Self::ThreeMf),
            "ply" => Ok(Self::Ply),
            other => Err(MeshError::UnsupportedExtension(other.to_string())),
        }
    }
}
