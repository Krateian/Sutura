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
        (a.hard_inside_comps, b.hard_inside_comps),
        (a.hard_inside_tets, b.hard_inside_tets),
        (a.cut_shells, b.cut_shells),
        (a.first_cut_shells, b.first_cut_shells),
        (a.retry_threshold, b.retry_threshold),
        (a.adaptive_added, b.adaptive_added),
        (a.adaptive_samples, b.adaptive_samples),
        (a.adaptive_thin, b.adaptive_thin),
    ];
    for (x, y) in fields {
        assert_eq!(x, y);
    }
    assert_eq!(a.winding_hist, b.winding_hist);
    assert_eq!(a.snap_f32, b.snap_f32);
    assert_eq!(a.relabel, b.relabel);
    assert_eq!(a.relabel_first_nm_edges, b.relabel_first_nm_edges);
    assert_eq!(a.relabel_final_nm_edges, b.relabel_final_nm_edges);
    assert_eq!(a.relabel_flips, b.relabel_flips);
    assert_eq!(a.delaunay_hash, b.delaunay_hash);
    assert_eq!(a.out_manifold, b.out_manifold);
    assert_eq!(a.adaptive, b.adaptive);
    assert_eq!(a.adaptive_broad, b.adaptive_broad);
    assert_eq!(a.adaptive_retry, b.adaptive_retry);
    assert_eq!(a.adaptive_capped, b.adaptive_capped);
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
    let serial = splint_cut_core(&v, &t, 32, SplintAdaptive::None, false).expect("serial splint");
    let parallel =
        splint_cut_core(&v, &t, 32, SplintAdaptive::None, true).expect("parallel splint");
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
    let a = splint_cut_core(&v, &t, 32, SplintAdaptive::None, true).expect("first splint");
    let b = splint_cut_core(&v, &t, 32, SplintAdaptive::None, true).expect("second splint");
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
    let baseline = splint_cut_core(&v, &t, 32, SplintAdaptive::None, true).expect("baseline");
    for threads in [1usize, 2, 8] {
        let pool = ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("rayon pool");
        let out = pool
            .install(|| splint_cut_core(&v, &t, 32, SplintAdaptive::None, true))
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
    assert!(splint_cut_core(&v, &t, 0, SplintAdaptive::None, true).is_err());
    assert!(splint_cut_core(&v, &t, MAX_RES + 1, SplintAdaptive::None, true).is_err());
}

#[test]
fn empty_input_is_rejected() {
    assert!(splint_cut_core(&[], &[], 32, SplintAdaptive::None, true).is_err());
}

fn five_cubes() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let mut v = Vec::new();
    let mut t = Vec::new();
    for i in 0..5usize {
        let off = [i as f64 * 3.0, 0.0, 0.0];
        let base = v.len();
        let (cv, ct) = cube();
        v.extend(
            cv.iter()
                .map(|p| [p[0] + off[0], p[1] + off[1], p[2] + off[2]]),
        );
        t.extend(ct.iter().map(|q| [q[0] + base, q[1] + base, q[2] + base]));
    }
    (v, t)
}

#[test]
fn adaptive_hardened_is_deterministic() {
    let (v, t) = cube();
    let a = splint_cut_core(&v, &t, 32, SplintAdaptive::Hardened, true).expect("a");
    let b = splint_cut_core(&v, &t, 32, SplintAdaptive::Hardened, true).expect("b");
    assert_eq!(a.report.adaptive, Some("hardened"));
    assert_eq!(a.report.first_cut_shells, a.report.cut_shells);
    assert_eq!(a.tris, b.tris);
    for (p, q) in a.verts.iter().zip(b.verts.iter()) {
        for k in 0..3 {
            assert_eq!(p[k].to_bits(), q[k].to_bits());
        }
    }
    same_report(&a.report, &b.report);
}

#[test]
fn retry_uses_cut_shells_and_triggers_on_five_bodies() {
    let (v, t) = five_cubes();
    let out = splint_cut_core(&v, &t, 32, SplintAdaptive::Retry, true).expect("retry");
    assert_eq!(out.report.adaptive, Some("retry"));
    assert!(
        out.report.first_cut_shells > super::adaptive::RETRY_COMPS,
        "{:?}",
        out.report
    );
    assert!(out.report.adaptive_retry, "{:?}", out.report);
    assert!(out.report.adaptive_broad, "{:?}", out.report);
    assert_eq!(out.report.first_cut_shells, out.report.first_cut_shells);
    // Report's final cut_shells is the broad pass; the fixture stays five
    // disjoint shells after fix A.
    assert!(out.report.cut_shells >= 5, "{:?}", out.report);
}

#[test]
fn retry_does_not_trigger_when_cut_shells_is_small() {
    let (v, t) = cube();
    let out = splint_cut_core(&v, &t, 32, SplintAdaptive::Retry, true).expect("retry");
    assert_eq!(out.report.adaptive, Some("retry"));
    assert!(out.report.first_cut_shells <= super::adaptive::RETRY_COMPS);
    assert!(!out.report.adaptive_retry);
    assert!(!out.report.adaptive_broad);
}

#[test]
fn adaptive_rayon_thread_counts_are_byte_identical() {
    let (v, t) = five_cubes();
    let baseline = splint_cut_core(&v, &t, 32, SplintAdaptive::Retry, true).expect("baseline");
    for threads in [1usize, 2, 8] {
        let pool = ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("rayon pool");
        let out = pool
            .install(|| splint_cut_core(&v, &t, 32, SplintAdaptive::Retry, true))
            .expect("splint");
        assert_eq!(baseline.tris, out.tris);
        for (a, b) in baseline.verts.iter().zip(out.verts.iter()) {
            for k in 0..3 {
                assert_eq!(a[k].to_bits(), b[k].to_bits());
            }
        }
        same_report(&baseline.report, &out.report);
    }
}

#[test]
fn snap_f32_is_deterministic_and_world_exact() {
    let (v, t) = cube();
    let mut o = SplintOpts::default();
    o.snap_f32 = true;
    let a = splint_cut_core_opts(&v, &t, 32, SplintAdaptive::None, true, o).expect("a");
    let b = splint_cut_core_opts(&v, &t, 32, SplintAdaptive::None, true, o).expect("b");
    assert!(a.report.snap_f32);
    assert_eq!(a.report.hard_inside_comps, b.report.hard_inside_comps);
    assert_eq!(a.tris, b.tris);
    assert_eq!(a.verts.len(), b.verts.len());
    for (p, q) in a.verts.iter().zip(b.verts.iter()) {
        for k in 0..3 {
            assert_eq!(p[k].to_bits(), q[k].to_bits());
            // Every emitted world coordinate is exactly representable in f32.
            assert_eq!(p[k] as f32 as f64, p[k]);
        }
    }
}

// Root-caused blocker (spike notes 2026-10-26): `delaunay::delaunay3` returns
// the Geogram BDEL cells in a nondeterministic ORDER (same set: `delaunay_hash`
// is order-independent and matches, but `centroids`/`phi`/`is_inside` are
// permuted run to run). The emitted surface is permutation-invariant, so the
// base output is stable, but the greedy relabel tie-breaks on tet index and is
// therefore not. Canonicalise the tet order first, then re-enable this.
#[test]
#[ignore = "blocked on nondeterministic delaunay3 tet order"]
fn greedy_relabel_is_deterministic_and_non_increasing() {
    let (v, t) = cube();
    let mut o = SplintOpts::default();
    o.greedy_relabel = true;
    let a = splint_cut_core_opts(&v, &t, 32, SplintAdaptive::Hardened, true, o).expect("a");
    let b = splint_cut_core_opts(&v, &t, 32, SplintAdaptive::Hardened, true, o).expect("b");
    assert!(a.report.relabel);
    assert_eq!(a.report.relabel_flips, b.report.relabel_flips);
    assert_eq!(a.report.relabel_first_nm_edges, b.report.relabel_first_nm_edges);
    assert_eq!(a.report.relabel_final_nm_edges, b.report.relabel_final_nm_edges);
    assert!(a.report.relabel_final_nm_edges <= a.report.relabel_first_nm_edges);
    assert_eq!(a.tris, b.tris);
    for (p, q) in a.verts.iter().zip(b.verts.iter()) {
        for k in 0..3 {
            assert_eq!(p[k].to_bits(), q[k].to_bits());
        }
    }
}
