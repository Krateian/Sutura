// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Normalise the input bbox into `[eps, 1-eps]^3` for the fixed grid.

pub(crate) struct Normalizer {
    pub(crate) bmin: [f64; 3],
    pub(crate) scale: f64,
    pub(crate) eps: f64,
}

impl Normalizer {
    pub(crate) fn new(verts: &[[f64; 3]], res: usize) -> Self {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in verts {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        let eps = 1.0 / res as f64;
        let extent = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        let max_ext = extent[0].max(extent[1]).max(extent[2]).max(1e-30);
        let scale = (1.0 - 2.0 * eps) / max_ext;
        Self {
            bmin: lo,
            scale,
            eps,
        }
    }

    pub(crate) fn eps(&self) -> f64 {
        self.eps
    }

    pub(crate) fn scale(&self) -> f64 {
        self.scale
    }

    pub(crate) fn norm(&self, p: [f64; 3]) -> [f64; 3] {
        [
            self.eps + (p[0] - self.bmin[0]) * self.scale,
            self.eps + (p[1] - self.bmin[1]) * self.scale,
            self.eps + (p[2] - self.bmin[2]) * self.scale,
        ]
    }

    pub(crate) fn de_norm(&self, q: [f64; 3]) -> [f64; 3] {
        [
            self.bmin[0] + (q[0] - self.eps) / self.scale,
            self.bmin[1] + (q[1] - self.eps) / self.scale,
            self.bmin[2] + (q[2] - self.eps) / self.scale,
        ]
    }
}
