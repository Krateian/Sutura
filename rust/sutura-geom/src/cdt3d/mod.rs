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
//! The algorithm follows the published exact-arrangement and CDT-validity
//! literature; no GPL/AGPL/LGPL source is copied.  See
//! `.claude/cdt3d-design.md` for the licensing and algorithmic rationale.

pub mod plc;

pub use plc::{build_plc, Plc, PlcOptions, PlcReport};
