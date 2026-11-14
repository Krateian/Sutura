//! Mesh file I/O.
//!
//! Phase 1 covers STL, OBJ, 3MF and PLY.

pub mod obj;
pub mod ply;
pub mod stl;
pub mod three_mf;

use std::path::Path;

use crate::error::{MeshError, MeshResult};
use crate::mesh::{Mesh, SourceFormat};

pub use obj::{obj_has_material_refs, read_obj, scan_obj_bad_coordinates, write_obj};
pub use ply::{read_ply, write_ply};
pub use stl::{
    load_stl, read_stl, scan_stl_bad_coordinates, write_stl_binary, write_stl_binary_bytes,
};
pub use three_mf::{
    check_units, match_triangle_attributes, parse_3mf_mesh_blocks, read_3mf_units,
    rewrite_3mf_meshes, write_3mf_single, TriangleAttrStats, UnitWarning,
};

/// Read STL/OBJ/PLY, keeping the format's native vertex semantics.
pub fn read_mesh(path: impl AsRef<Path>) -> MeshResult<Mesh> {
    let path = path.as_ref();
    match format_of(path)? {
        SourceFormat::Stl => read_stl(path),
        SourceFormat::Obj => read_obj(path),
        SourceFormat::Ply => read_ply(path),
        SourceFormat::ThreeMf => Err(MeshError::Malformed3mf(
            "use parse_3mf_mesh_blocks for 3MF input".to_string(),
        )),
    }
}

/// Read a single mesh; STL is welded exactly like the Python load_mesh path.
pub fn load_mesh(path: impl AsRef<Path>) -> MeshResult<Mesh> {
    let path = path.as_ref();
    match format_of(path)? {
        SourceFormat::Stl => load_stl(path),
        SourceFormat::Obj => read_obj(path),
        SourceFormat::Ply => read_ply(path),
        SourceFormat::ThreeMf => {
            let blocks = parse_3mf_mesh_blocks(path)?;
            blocks
                .into_iter()
                .next()
                .map(|b| b.mesh)
                .ok_or_else(|| MeshError::Malformed3mf("no mesh geometry found".to_string()))
        }
    }
}

/// Write one mesh, dispatching by extension.
pub fn save_mesh(path: impl AsRef<Path>, mesh: &Mesh) -> MeshResult<()> {
    let path = path.as_ref();
    match format_of(path)? {
        SourceFormat::Stl => write_stl_binary(path, mesh),
        SourceFormat::Obj => write_obj(path, mesh),
        SourceFormat::Ply => write_ply(path, mesh),
        SourceFormat::ThreeMf => write_3mf_single(path, mesh),
    }
}

/// Return a description of NaN/Inf coordinates, mirroring the CLI pre-scan.
pub fn scan_bad_coordinates(path: impl AsRef<Path>) -> MeshResult<Option<String>> {
    let path = path.as_ref();
    match format_of(path)? {
        SourceFormat::ThreeMf => Ok(None),
        SourceFormat::Obj => scan_obj_bad_coordinates(path),
        _ => scan_stl_bad_coordinates(path),
    }
}

fn format_of(path: &Path) -> MeshResult<SourceFormat> {
    let path = path
        .to_str()
        .ok_or_else(|| MeshError::UnsupportedExtension(String::new()))?;
    SourceFormat::from_extension(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sutura-mesh-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn scan_dispatches_by_extension() {
        let obj = tmp("scan").with_extension("obj");
        std::fs::write(&obj, b"v nan 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        assert_eq!(
            scan_bad_coordinates(&obj).unwrap().as_deref(),
            Some("NaN or infinite coordinates")
        );
        std::fs::remove_file(obj).unwrap();

        let missing_3mf = tmp("missing").with_extension("3mf");
        assert_eq!(scan_bad_coordinates(&missing_3mf).unwrap(), None);

        let unknown = tmp("x").with_extension("unknown");
        assert!(matches!(
            scan_bad_coordinates(&unknown),
            Err(MeshError::UnsupportedExtension(_))
        ));
    }

    #[test]
    fn save_and_load_dispatch_by_extension() {
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        );
        for ext in ["stl", "obj", "ply", "3mf"] {
            let path = tmp("dispatch").with_extension(ext);
            save_mesh(&path, &m).unwrap();
            let got = load_mesh(&path).unwrap();
            assert_eq!(got.verts, m.verts, "{ext}");
            assert_eq!(got.tris, m.tris, "{ext}");
            std::fs::remove_file(path).unwrap();
        }
    }
}
