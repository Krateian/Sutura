// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;
use rayon::ThreadPoolBuilder;

fn cube() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let v = vec![
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    let t = vec![
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [3, 7, 6],
        [3, 6, 2],
        [0, 4, 7],
        [0, 7, 3],
        [1, 2, 6],
        [1, 6, 5],
    ];
    (v, t)
}

fn same_report(a: &SplintReport, b: &SplintReport) {
    let fields = [
        (a.res, b.res),
        (a.input_vertices, b.input_vertices),
        (a.input_faces, b.input_faces),
        (a.proxy_vertices, b.proxy_vertices),
        (a.tets, b.tets),
        (a.internal_faces, b.internal_faces),
        (a.outer_faces, b.outer_faces),
        (a.out_tets, b.out_tets),
        (a.interior_anchors, b.interior_anchors),
        (a.output_vertices, b.output_vertices),
        (a.output_faces, b.output_faces),
        (a.output_parts, b.output_parts),
        (a.dropped_inside_comps, b.dropped_inside_comps),
        (a.filled_outside_comps, b.filled_outside_comps),
    ];
    for (x, y) in fields {
        assert_eq!(x, y);
    }
    assert_eq!(a.delaunay_hash, b.delaunay_hash);
    assert_eq!(a.out_manifold, b.out_manifold);
    // The output arrays are compared bit-for-bit by the caller. These two
    // diagnostic volumes are float sums over a cut component and may differ by
    // a last-bit rounding depending on the parallel scheduling; they do not
    // feed the emitted geometry.
    assert!((a.dropped_inside_vol - b.dropped_inside_vol).abs() <= 1e-12);
    assert!((a.filled_outside_vol - b.filled_outside_vol).abs() <= 1e-12);
}

#[test]
fn serial_and_parallel_outputs_are_byte_identical() {
    let (v, t) = cube();
    let serial = splint_cut_core(&v, &t, 32, false).expect("serial splint");
    let parallel = splint_cut_core(&v, &t, 32, true).expect("parallel splint");
    assert_eq!(serial.tris, parallel.tris);
    assert_eq!(serial.verts.len(), parallel.verts.len());
    for (a, b) in serial.verts.iter().zip(parallel.verts.iter()) {
        for k in 0..3 {
            assert_eq!(a[k].to_bits(), b[k].to_bits());
        }
    }
    same_report(&serial.report, &parallel.report);
}

#[test]
fn repeated_runs_are_byte_identical() {
    let (v, t) = cube();
    let a = splint_cut_core(&v, &t, 32, true).expect("first splint");
    let b = splint_cut_core(&v, &t, 32, true).expect("second splint");
    assert_eq!(a.tris, b.tris);
    assert_eq!(a.verts.len(), b.verts.len());
    for (x, y) in a.verts.iter().zip(b.verts.iter()) {
        for k in 0..3 {
            assert_eq!(x[k].to_bits(), y[k].to_bits());
        }
    }
    println!(
        "vol a={:?} b={:?}",
        a.report.dropped_inside_vol, b.report.dropped_inside_vol
    );
    same_report(&a.report, &b.report);
}

#[test]
fn rayon_thread_counts_are_byte_identical() {
    let (v, t) = cube();
    let baseline = splint_cut_core(&v, &t, 32, true).expect("baseline");
    for threads in [1usize, 2, 8] {
        let pool = ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("rayon pool");
        let out = pool
            .install(|| splint_cut_core(&v, &t, 32, true))
            .expect("splint");
        assert_eq!(baseline.tris, out.tris);
        assert_eq!(baseline.verts.len(), out.verts.len());
        for (a, b) in baseline.verts.iter().zip(out.verts.iter()) {
            for k in 0..3 {
                assert_eq!(a[k].to_bits(), b[k].to_bits());
            }
        }
        same_report(&baseline.report, &out.report);
    }
}

#[test]
fn res_is_validated() {
    let (v, t) = cube();
    assert!(splint_cut_core(&v, &t, 0, true).is_err());
    assert!(splint_cut_core(&v, &t, MAX_RES + 1, true).is_err());
}

#[test]
fn empty_input_is_rejected() {
    assert!(splint_cut_core(&[], &[], 32, true).is_err());
}
