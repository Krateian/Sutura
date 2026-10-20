// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Compile the vendored Geogram Delaunay PSM as a static C++20 library and link
// it into sutura_geom. The PSM is the sequential `GEO::Delaunay3d` backend
// (BDEL); it is deterministic and needs no TBB/OpenMP.

fn main() {
    let mut b = cc::Build::new();
    b.cpp(true)
        .std("c++20")
        .opt_level(2)
        .warnings(false)
        .file("third_party/geogram_delaunay_psm/Delaunay_psm.cpp")
        .file("src/splint/geo_psm_shim.cpp")
        .include("third_party/geogram_delaunay_psm");
    // Exact-predicate arithmetic needs the FP environment pinned. These flags
    // are supported by gcc and Apple clang; keep them off if a compiler lacks
    // them rather than silently changing the predicate results.
    for f in ["-frounding-math", "-ffp-contract=off", "-fno-fast-math"] {
        b.flag_if_supported(f);
    }
    b.compile("geogram_delaunay_psm");

    println!("cargo:rerun-if-changed=src/splint/geo_psm_shim.cpp");
    println!("cargo:rerun-if-changed=third_party/geogram_delaunay_psm/Delaunay_psm.cpp");
    println!("cargo:rerun-if-changed=third_party/geogram_delaunay_psm/Delaunay_psm.h");
    println!("cargo:rerun-if-changed=third_party/geogram_delaunay_psm/LICENSE");
}
