//! Mesh file I/O.
//!
//! Phase 1 starts with STL; OBJ, 3MF and PLY are added in later slices.

pub mod stl;

pub use stl::{
    load_stl, read_stl, scan_stl_bad_coordinates, write_stl_binary, write_stl_binary_bytes,
};
