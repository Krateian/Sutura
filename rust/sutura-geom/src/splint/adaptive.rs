// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Adaptive proxy sampling for thin walls.
//!
//! Port of the measured spike (`p2/splint-plan` f6f53a5) with the env knobs
//! turned into fixed constants. The default `splint_cut` mode never calls this
//! module, so the non-adaptive output stays byte-identical.

use std::collections::HashMap;
use std::time::Instant;

use crate::winding::MeshBvh;

pub(crate) const THIN_MULT: f64 = 2.0; // local thickness below 2 base cells
pub(crate) const TRIGGER_DOT: f64 = -0.7; // opposite normals must be near-antiparallel
pub(crate) const TRIGGER_ANGLE: f64 = 0.8660; // cos(30 deg): inward hit direction
pub(crate) const ADAPTIVE_CAP: f64 = 1.5; // cap on *added* points vs base+corners
pub(crate) const RETRY_COMPS: usize = 4; // retry when cut_shells exceeds this

pub(crate) struct AdaptiveStats {
    pub(crate) added: usize,
    pub(crate) samples: usize,
    pub(crate) thin: usize,
    pub(crate) capped: bool,
    pub(crate) secs: f64,
    pub(crate) keys: Vec<[i64; 3]>,
}

impl Default for AdaptiveStats {
    fn default() -> Self {
        Self {
            added: 0,
            samples: 0,
            thin: 0,
            capped: false,
            secs: 0.0,
            keys: Vec::new(),
        }
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Append adaptive interior points to `proxy` (which already contains the base
/// shell and the padded corners). `shell_map` maps every input face to its
/// shell id; it is only used to exclude the point's own shell in the thickness
/// probe. Returns the added cell keys for the coordinate-cell jitter.
pub(crate) fn append(
    bvh: &MeshBvh,
    shell_verts: &[[f64; 3]],
    shell_map: &[usize],
    proxy: &mut Vec<[f64; 3]>,
    voxel: f64,
    broad: bool,
) -> AdaptiveStats {
    let t0 = Instant::now();
    let base = proxy.len();
    let qcell = 0.25 * voxel;
    let quant = |p: &[f64; 3]| -> [i64; 3] {
        [
            (p[0] / qcell).floor() as i64,
            (p[1] / qcell).floor() as i64,
            (p[2] / qcell).floor() as i64,
        ]
    };
    let mut occupied: HashMap<[i64; 3], ()> = HashMap::with_capacity(base);
    for p in proxy.iter() {
        occupied.insert(quant(p), ());
    }

    let mut samples = 0usize;
    let mut thin = 0usize;
    let mut cands: Vec<([i64; 3], [u64; 3], [f64; 3])> = Vec::new();
    for &p in shell_verts {
        let (_, _, tri) = bvh.closest_point(p);
        if tri == usize::MAX || tri >= shell_map.len() {
            continue;
        }
        samples += 1;
        let own = shell_map[tri];
        let (qv, t, tj) = bvh.closest_point_excluding_within(p, own, shell_map, THIN_MULT * voxel);
        if tj == usize::MAX {
            continue;
        }
        if !broad {
            // Harden: the opposite hit must be a genuine near-parallel wall,
            // not a coplanar fragment.
            let n_own = bvh.tri_normal(tri);
            let n_opp = bvh.tri_normal(tj);
            let d = sub(qv, p);
            let dhat = if t > 0.0 {
                [d[0] / t, d[1] / t, d[2] / t]
            } else {
                [0.0, 0.0, 0.0]
            };
            let nd = dot(n_own, n_opp);
            let na = -dot(dhat, n_own);
            if nd >= TRIGGER_DOT || na <= TRIGGER_ANGLE {
                continue;
            }
        }
        thin += 1;
        let k = ((4.0 * t / voxel).round() as i64).clamp(1, 4);
        for kk in 1..=k {
            let fr = kk as f64 / (k as f64 + 1.0);
            let pt = [
                p[0] + fr * (qv[0] - p[0]),
                p[1] + fr * (qv[1] - p[1]),
                p[2] + fr * (qv[2] - p[2]),
            ];
            cands.push((
                quant(&pt),
                [pt[0].to_bits(), pt[1].to_bits(), pt[2].to_bits()],
                pt,
            ));
        }
    }

    // Order-independent: sort by (cell, coordinate bits), keep the first
    // survivor per cell against existing points and earlier survivors.
    cands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut survivors: Vec<([i64; 3], [f64; 3])> = Vec::new();
    for (ck, _, pt) in cands {
        if occupied.contains_key(&ck) {
            continue;
        }
        occupied.insert(ck, ());
        survivors.push((ck, pt));
    }
    let cap = (ADAPTIVE_CAP * base as f64).ceil() as usize;
    let mut capped = false;
    if survivors.len() > cap && cap > 0 {
        let stride = (survivors.len() + cap - 1) / cap;
        let mut kept = Vec::with_capacity(cap);
        let mut i = 0;
        while i < survivors.len() && kept.len() < cap {
            kept.push(survivors[i]);
            i += stride;
        }
        survivors = kept;
        capped = true;
    }
    let mut keys = Vec::with_capacity(survivors.len());
    for (ck, pt) in survivors {
        keys.push(ck);
        proxy.push(pt);
    }

    AdaptiveStats {
        added: keys.len(),
        samples,
        thin,
        capped,
        secs: t0.elapsed().as_secs_f64(),
        keys,
    }
}

/// Deterministic coordinate-cell jitter for one adaptive point.
pub(crate) fn jitter(seed_key: [i64; 3], p: &mut [f64; 3], amount: f64) {
    let mut s = mix64(
        (seed_key[0] as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (seed_key[1] as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
            ^ (seed_key[2] as u64).wrapping_mul(0x94D0_49BB_1331_11EB)
            ^ 0x1234_5678_9ABC_DEF0,
    );
    let mut next = || {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        ((mix64(s)) >> 11) as f64 / (1u64 << 53) as f64
    };
    p[0] += (next() - 0.5) * amount;
    p[1] += (next() - 0.5) * amount;
    p[2] += (next() - 0.5) * amount;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_is_coordinate_keyed_not_index_keyed() {
        let mut a = [0.0; 3];
        let mut b = [0.0; 3];
        jitter([1, 2, 3], &mut a, 2e-4);
        jitter([1, 2, 3], &mut b, 2e-4);
        assert_eq!(a, b);
        let mut c = [0.0; 3];
        jitter([4, 5, 6], &mut c, 2e-4);
        assert_ne!(a, c);
    }
}
