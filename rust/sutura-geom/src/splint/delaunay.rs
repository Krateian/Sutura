// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Safe wrapper around the vendored Geogram standalone Delaunay PSM.

use std::os::raw::c_longlong;
use std::sync::Mutex;

/// Geogram keeps process-global state; serialize the sequential Delaunay so
/// concurrent Rust tests (or future callers) cannot perturb each other.
static GEO_LOCK: Mutex<()> = Mutex::new(());

extern "C" {
    fn geo_psm_delaunay3(
        pts: *const f64,
        n: c_longlong,
        tets: *mut i32,
        neigh: *mut i32,
        max_tets: c_longlong,
        out_n: *mut c_longlong,
        out_nb_vert: *mut c_longlong,
    ) -> i32;
}

/// Unconstrained Delaunay of `proxy` via the sequential Geogram `BDEL`
/// engine. Returns the finite tetrahedra (input vertex indices).
pub(crate) fn delaunay3(proxy: &[[f64; 3]]) -> Result<Vec<[usize; 4]>, String> {
    let n = proxy.len();
    if n < 4 {
        return Err("Delaunay needs at least 4 points".into());
    }
    if n > i32::MAX as usize {
        return Err("too many proxy points for the Geogram index type".into());
    }
    let _guard = GEO_LOCK.lock().expect("Geogram Delaunay lock poisoned");
    let mut flat: Vec<f64> = Vec::with_capacity(n * 3);
    for p in proxy {
        flat.push(p[0]);
        flat.push(p[1]);
        flat.push(p[2]);
    }

    let mut cap: i64 = (n as i64 * 7).max(16);
    loop {
        let mut tets = vec![0i32; cap as usize * 4];
        let mut neigh = vec![0i32; cap as usize * 4];
        let mut out_n: i64 = 0;
        let mut out_nb_vert: i64 = 0;
        let rc = unsafe {
            geo_psm_delaunay3(
                flat.as_ptr(),
                n as i64,
                tets.as_mut_ptr(),
                neigh.as_mut_ptr(),
                cap,
                &mut out_n,
                &mut out_nb_vert,
            )
        };
        match rc {
            0 => {
                // The engine may leave some duplicate proxy points unused;
                // the returned cell indices still refer to the input array.
                let _ = out_nb_vert;
                let mut out = Vec::with_capacity(out_n as usize);
                for c in 0..out_n as usize {
                    out.push([
                        tets[c * 4] as usize,
                        tets[c * 4 + 1] as usize,
                        tets[c * 4 + 2] as usize,
                        tets[c * 4 + 3] as usize,
                    ]);
                }
                return Ok(out);
            }
            2 => {
                // Buffer too small: out_n holds the required cell count.
                cap = (out_n * 3 / 2).max(cap * 2);
            }
            other => return Err(format!("Geogram Delaunay returned rc={other}")),
        }
    }
}

/// Canonical, order-independent form of a tetrahedron set: each tet's vertex
/// ids sorted ascending, then the whole tet list sorted ascending. This is the
/// `tets_hash` comparator, factored out so it can also be applied to the live
/// tet array immediately after `delaunay3` (the Geogram engine emits the same
/// cells in a nondeterministic ORDER, which would otherwise leak into every
/// index-ordered downstream pass).
pub(crate) fn canonicalize_tets(tets: &mut [[usize; 4]]) {
    for t in tets.iter_mut() {
        t.sort_unstable();
    }
    tets.sort_unstable();
}

/// Order-independent digest of a tetrahedron set (canonical per-tet vertex
/// order + sorted tet list), for determinism and reference comparison.
pub(crate) fn tets_hash(tets: &[[usize; 4]]) -> u64 {
    let mut canon: Vec<[usize; 4]> = tets.to_vec();
    canonicalize_tets(&mut canon);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for t in &canon {
        for &v in t {
            h ^= v as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geogram_delaunay_is_repeatable_in_one_process() {
        let mut pts = Vec::new();
        for i in 0..6 {
            for j in 0..6 {
                for k in 0..6 {
                    let f = |n: usize| (n as f64) * 0.173 + 0.011;
                    pts.push([f(i), f(j), f(k)]);
                }
            }
        }
        let a = delaunay3(&pts).expect("first delaunay");
        let b = delaunay3(&pts).expect("second delaunay");
        assert_eq!(a.len(), b.len());
        assert_eq!(tets_hash(&a), tets_hash(&b));
    }

    #[test]
    fn canonicalize_tets_is_a_function_of_the_cell_set() {
        let mut a: Vec<[usize; 4]> = vec![[3, 1, 2, 0], [5, 4, 7, 6]];
        let mut b: Vec<[usize; 4]> = vec![[6, 7, 5, 4], [0, 2, 3, 1]];
        canonicalize_tets(&mut a);
        canonicalize_tets(&mut b);
        assert_eq!(a, b);
        assert_eq!(a, vec![[0, 1, 2, 3], [4, 5, 6, 7]]);
    }
}
