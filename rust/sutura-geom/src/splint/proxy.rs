// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! UDF grid + eps-offset dual-contour proxy shell and deterministic jitter.

use std::time::Instant;

use crate::dual_contour::dual_contour;
use crate::winding::MeshBvh;

use super::adaptive::{self, AdaptiveStats};

pub(crate) const JITTER: f64 = 2.0e-4;

pub(crate) struct Proxy {
    pub(crate) verts: Vec<[f64; 3]>,
    pub(crate) voxel: f64,
    pub(crate) secs_udf: f64,
    pub(crate) secs_proxy: f64,
    pub(crate) adaptive: AdaptiveStats,
}

pub(crate) fn build(
    bvh: &MeshBvh,
    res: usize,
    eps: f64,
    shell_map: Option<&[usize]>,
    broad: bool,
) -> Result<Proxy, String> {
    let dims = [res, res, res];
    let voxel = 1.0 / (res as f64 - 1.0);
    let origin = [0.0, 0.0, 0.0];

    let t_udf = Instant::now();
    let udf = bvh.unsigned_grid(origin, voxel, dims);
    let secs_udf = t_udf.elapsed().as_secs_f64();

    let t_proxy = Instant::now();
    let f: Vec<f32> = udf.iter().map(|u| u - eps as f32).collect();
    let shell = dual_contour(&f, dims, origin, voxel);
    if shell.verts.is_empty() {
        return Err("proxy shell extraction produced no vertices".into());
    }

    let mut proxy = shell.verts.clone();
    // Eight padded bbox corners guarantee the hull covers the domain.
    let pad = 0.1;
    for &x in &[-pad, 1.0 + pad] {
        for &y in &[-pad, 1.0 + pad] {
            for &z in &[-pad, 1.0 + pad] {
                proxy.push([x, y, z]);
            }
        }
    }
    let base_len = proxy.len();
    let adaptive = match shell_map {
        Some(map) => adaptive::append(bvh, &shell.verts, map, &mut proxy, voxel, broad),
        None => AdaptiveStats::default(),
    };

    // Base points (shell + corners) keep the historical index-derived jitter,
    // so the default (adaptive off) output is bit-identical. Adaptive points
    // are appended after them and jittered from their coordinate cell; never
    // index-coupled.
    for i in 0..base_len {
        let p = &mut proxy[i];
        let mut s = (i as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x1234_5678_9ABC_DEF0);
        let mut next = || {
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            ((adaptive::mix64(s)) >> 11) as f64 / (1u64 << 53) as f64
        };
        p[0] += (next() - 0.5) * JITTER;
        p[1] += (next() - 0.5) * JITTER;
        p[2] += (next() - 0.5) * JITTER;
    }
    for (j, key) in adaptive.keys.iter().enumerate() {
        adaptive::jitter(*key, &mut proxy[base_len + j], JITTER);
    }
    let secs_proxy = t_proxy.elapsed().as_secs_f64();

    Ok(Proxy {
        verts: proxy,
        voxel,
        secs_udf,
        secs_proxy,
        adaptive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn proxy_is_repeatable_in_one_process() {
        let (v, t) = cube();
        let bvh = MeshBvh::from_arrays(&v, &t);
        let n = crate::splint::normalize::Normalizer::new(&v, 32);
        let a = build(&bvh, 32, n.eps(), None, false).expect("first proxy");
        let b = build(&bvh, 32, n.eps(), None, false).expect("second proxy");
        assert_eq!(a.verts.len(), b.verts.len());
        for (p, q) in a.verts.iter().zip(b.verts.iter()) {
            for k in 0..3 {
                assert_eq!(p[k].to_bits(), q[k].to_bits());
            }
        }
    }

    #[test]
    fn base_points_are_unchanged_by_adaptive_off() {
        let (v, t) = cube();
        let bvh = MeshBvh::from_arrays(&v, &t);
        let n = crate::splint::normalize::Normalizer::new(&v, 32);
        let off = build(&bvh, 32, n.eps(), None, false).expect("off proxy");
        let hardened =
            build(&bvh, 32, n.eps(), Some(&vec![0usize; t.len()]), false).expect("hard proxy");
        assert!(hardened.verts.len() >= off.verts.len());
        for (p, q) in off.verts.iter().zip(hardened.verts.iter()) {
            for k in 0..3 {
                assert_eq!(p[k].to_bits(), q[k].to_bits());
            }
        }
    }
}
