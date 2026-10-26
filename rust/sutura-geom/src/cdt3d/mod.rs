// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDT3D Phase A: PLC completion and validation.
//!
//! This module is opt-in.  It does not change `arrangement_lite_core` or any
//! default Splint output.  It turns a triangle soup into a validated
//! piecewise-linear complex (PLC) suitable for the constrained
//! tetrahedralization phase:
//!
//! 1. weld exact vertices, drop zero-area and duplicate faces;
//! 2. resolve positive-area coplanar overlaps with a deterministic one-ulp
//!    perturbation (reported, not hidden);
//! 3. split every triangle at input vertices lying on its edges or in its
//!    interior (T-junctions and touching points);
//! 4. run the exact arrangement with those conformance points;
//! 5. validate (proper intersections, coplanar overlaps, T-junctions, touch
//!    points, degeneracies) and report boundary loops and non-manifold edges;
//! 6. hand off canonical `f64` vertices/triangles plus a deterministic hash.
//!
//! Implementation note.  This is a clean-room implementation written from the
//! published literature, not a port: no GPL/AGPL/LGPL source is copied or
//! translated.  The constrained Delaunay tetrahedralization follows Shewchuk's
//! work on constrained Delaunay tetrahedralizations and provably good boundary
//! recovery; the acute-vertex segment-recovery rules follow Si (TetGen, a
//! Delaunay-Based Quality Tetrahedral Mesh Generator, ACM TOMS 2015); and the
//! face-cavity retriangulation with a gift-wrapping fallback follows Diazzi,
//! Panozzo, Vaxman and Attene, "Constrained Delaunay Tetrahedrization: A Robust
//! and Practical Approach" (ACM TOG 42(6), 2023; arXiv:2309.09805).  The initial
//! unconstrained Delaunay uses the vendored Geogram Delaunay PSM (BSD-3-Clause),
//! and the exact `orient3d`/`insphere` predicates use the `robust` crate
//! (Shewchuk's adaptive predicates).

pub mod certificate;
pub mod dt;
pub mod face;
pub mod plc;
pub mod segment;
pub mod solid;

pub use certificate::{certify_face_coverage, CoverageReport};
pub use dt::{build_dt, BoundingBox3, Dt, DtOptions, DtReport};
pub use face::{
    detect_missing_faces, recover_faces, FaceMesh, FaceOptions, FaceReport,
};
pub use plc::{build_plc, Plc, PlcOptions, PlcReport};
pub use segment::{recover_segments, SegmentMesh, SegmentOptions, SegmentReport};
pub use solid::{build_solid, SolidMesh, SolidOptions, SolidReport};

#[cfg(test)]
pub(crate) mod testutil {
    use super::plc::{build_plc, Plc, PlcOptions};
    use std::collections::HashMap;

    pub(crate) fn real_sample(name: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let path = format!(
            "{}/../../tests/real-world-samples/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        read_stl_welded(&path)
    }

    pub(crate) fn fixture(name: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        read_stl_welded(&path)
    }

    pub(crate) fn plc_for(kind: &str, name: &str) -> Plc {
        let (v, t) = if kind == "real" {
            real_sample(name)
        } else {
            fixture(name)
        };
        build_plc(&v, &t, &PlcOptions::default()).expect("build PLC")
    }

    pub(crate) fn read_stl_welded(path: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let data = std::fs::read(path).expect("read STL");
        let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
        let mut index: HashMap<[u32; 3], usize> = HashMap::new();
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for f in 0..n {
            let base = 84 + 50 * f + 12;
            let mut tri = [0usize; 3];
            for (k, slot) in tri.iter_mut().enumerate() {
                let o = base + 12 * k;
                let c: [u32; 3] = std::array::from_fn(|j| {
                    u32::from_le_bytes(data[o + 4 * j..o + 4 * j + 4].try_into().unwrap())
                });
                *slot = *index.entry(c).or_insert_with(|| {
                    verts.push(c.map(|b| f32::from_bits(b) as f64));
                    verts.len() - 1
                });
            }
            tris.push(tri);
        }
        (verts, tris)
    }
}
