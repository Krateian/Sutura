//! Mesh file I/O.
//!
//! Phase 1 starts with STL, OBJ and 3MF; PLY follows in the next slice.

pub mod obj;
pub mod stl;
pub mod three_mf;

pub use obj::{obj_has_material_refs, read_obj, write_obj};
pub use stl::{
    load_stl, read_stl, scan_stl_bad_coordinates, write_stl_binary, write_stl_binary_bytes,
};
pub use three_mf::{
    check_units, match_triangle_attributes, parse_3mf_mesh_blocks, read_3mf_units,
    rewrite_3mf_meshes, write_3mf_single, TriangleAttrStats, UnitWarning,
};
