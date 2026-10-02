//! Ray-stabbing inside/outside voting (Nooruddin & Turk 2003, "spray") over the
//! triangle-soup BVH in [`crate::winding`].
//!
//! For a query point a set of `K` rays is cast along near-uniform directions on
//! a Fibonacci sphere.  Each ray votes inside/outside; the point is inside when
//! the majority vote inside.  The vote is the **oriented net crossing count**
//! `Σ sign(dot(dir, n_face))` (`> 0` inside), not plain crossing parity: parity
//! is wrong for the target cases (overlapping unioned shells give an even
//! crossing count inside the overlap), while the oriented sum matches the
//! generalized winding number's union/cavity semantics and reduces to parity for
//! a single closed shell.
//!
//! Real triangle soups contain inconsistently oriented faces.  A ray along
//! which the oriented sum would be negative (`net < 0`), or zero with an odd
//! number of crossings, cannot arise from a consistently outward-oriented
//! closed surface; such a ray falls back to orientation-independent parity.
//! (`net > 0` with an even crossing count is the legitimate multi-shell overlap
//! and is kept.)  A whole run can also be forced to parity with `parity=true`.
//!
//! A ray that leaves the mesh bounding box without a single hit is an outside
//! vote weighted `escape_weight` (default 2.0, the "strong outside" vote).
//!
//! This module only depends on `winding`; it is deliberately independent of the
//! morphology and dual-contour code so it can be unit-tested in isolation.

use rayon::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::winding::{Aabb, MeshBvh};

/// Default number of stabbing directions (Nooruddin & Turk use 13).
pub const DEFAULT_DIRS: usize = 13;
/// Upper bound on `K`, kept odd so votes never tie.
pub const MAX_DIRS: usize = 26;
/// Lower bound on `K`.
pub const MIN_DIRS: usize = 3;
/// Generalized-winding band `(lo, hi)` outside which the sign is trusted.
pub const AMBIG_LO: f32 = 0.3;
pub const AMBIG_HI: f32 = 0.7;
/// Weight of an escaping ("strong outside") ray in the majority score.
pub const DEFAULT_ESCAPE_WEIGHT: f64 = 2.0;

const JITTER: f64 = 0.05;
const GOLDEN_ANGLE: f64 = 2.399_963_229_728_653;

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn next01(state: &mut u64) -> f64 {
    (splitmix64(state) >> 11) as f64 / (1u64 << 53) as f64
}

/// Deterministic Fibonacci-sphere directions with a small seeded jitter so no
/// ray is aligned with the voxel lattice or a triangle plane.
fn fibonacci_dirs(n: usize, seed: u64) -> Vec<[f64; 3]> {
    let mut state = seed.wrapping_add(0x1234_5678_9ABC_DEF0);
    let mut dirs = Vec::with_capacity(n);
    for i in 0..n {
        let z = 1.0 - (2.0 * i as f64 + 1.0) / n as f64;
        let r = (1.0 - z * z).max(0.0).sqrt();
        let phi = i as f64 * GOLDEN_ANGLE;
        let mut x = r * phi.cos();
        let mut y = r * phi.sin();
        let mut zz = z;
        x += (next01(&mut state) - 0.5) * JITTER;
        y += (next01(&mut state) - 0.5) * JITTER;
        zz += (next01(&mut state) - 0.5) * JITTER;
        let len = (x * x + y * y + zz * zz).sqrt();
        if len > 0.0 {
            dirs.push([x / len, y / len, zz / len]);
        } else {
            dirs.push([0.0, 0.0, 1.0]);
        }
    }
    dirs
}

/// Result of the `K`-ray vote at one point.
#[derive(Clone, Copy, Debug)]
pub struct StabVote {
    pub inside: bool,
    pub inside_votes: u32,
    pub outside_votes: u32,
    pub escape_votes: u32,
    pub suspect: u32,
    pub unresolved: bool,
}

/// Reusable direction set + ray parameters for one mesh.
pub struct Stabber {
    dirs: Vec<[f64; 3]>,
    max_t: f64,
    merge_eps: f64,
    parity: bool,
    escape_weight: f64,
}

impl Stabber {
    /// `bounds` is the mesh bounding box; `n_dirs` is clamped to
    /// `[MIN_DIRS, MAX_DIRS]`.  `parity=true` forces the orientation-independent
    /// parity vote for every ray (the fallback mode).
    pub fn new(
        bounds: &Aabb,
        n_dirs: usize,
        seed: u64,
        parity: bool,
        escape_weight: f64,
    ) -> Self {
        let diag = if bounds.is_empty() {
            1.0
        } else {
            bounds.diagonal().max(1e-12)
        };
        let n = n_dirs.clamp(MIN_DIRS, MAX_DIRS);
        Stabber {
            dirs: fibonacci_dirs(n, seed),
            max_t: 2.0 * diag,
            merge_eps: 1e-6 * diag,
            parity,
            escape_weight: if escape_weight.is_finite() && escape_weight >= 1.0 {
                escape_weight
            } else {
                DEFAULT_ESCAPE_WEIGHT
            },
        }
    }

    pub fn n_dirs(&self) -> usize {
        self.dirs.len()
    }

    pub fn parity(&self) -> bool {
        self.parity
    }

    /// Majority inside/outside vote at `p`.
    pub fn vote(&self, bvh: &MeshBvh, p: [f64; 3]) -> StabVote {
        let mut inside_score = 0.0f64;
        let mut outside_score = 0.0f64;
        let mut inside_votes = 0u32;
        let mut outside_votes = 0u32;
        let mut escape_votes = 0u32;
        let mut suspect = 0u32;
        for d in &self.dirs {
            let q = bvh.intersect_ray(p, *d, self.max_t, self.merge_eps);
            if q.suspect {
                suspect += 1;
            }
            if q.crossings == 0 {
                escape_votes += 1;
                outside_score += self.escape_weight;
                continue;
            }
            let odd = q.crossings % 2 == 1;
            let ray_inside = if self.parity {
                odd
            } else if q.net < 0 || (q.net == 0 && odd) {
                // Orientation-inconsistent along this ray -> parity fallback.
                odd
            } else {
                q.net > 0
            };
            if ray_inside {
                inside_votes += 1;
                inside_score += 1.0;
            } else {
                outside_votes += 1;
                outside_score += 1.0;
            }
        }
        StabVote {
            inside: inside_score > outside_score,
            inside_votes,
            outside_votes,
            escape_votes,
            suspect,
            unresolved: (suspect as usize) * 2 > self.dirs.len(),
        }
    }
}

/// Statistics from a grid disambiguation pass.
#[derive(Clone, Copy, Debug)]
pub struct RayStabStats {
    pub dirs: usize,
    pub parity: bool,
    pub cells_ambiguous: u64,
    pub cells_flipped: u64,
    pub cells_unresolved: u64,
    pub rays_cast: u64,
    pub seconds: f64,
}

/// Overwrite the sign of the signed grid for generalized-winding-ambiguous
/// cells (`lo < w < hi`) using the ray vote.  Cells outside the band and
/// unresolved votes keep their existing sign, so a "no opinion" result falls
/// back to the winding number.  Parallel over z-slices like the other grid
/// passes.
#[allow(clippy::too_many_arguments)]
pub fn disambiguate_sdf(
    bvh: &MeshBvh,
    winding: &[f32],
    unsigned: &[f32],
    signed: &mut [f32],
    origin: [f64; 3],
    voxel: f64,
    dims: [usize; 3],
    stabber: &Stabber,
    lo: f32,
    hi: f32,
) -> RayStabStats {
    let t_start = Instant::now();
    let slice = dims[0] * dims[1];
    let ambiguous = AtomicU64::new(0);
    let flipped = AtomicU64::new(0);
    let unresolved = AtomicU64::new(0);
    signed
        .par_chunks_mut(slice)
        .enumerate()
        .for_each(|(k, sch)| {
            let z = origin[2] + voxel * k as f64;
            let base = k * slice;
            let wch = &winding[base..base + slice];
            let uch = &unsigned[base..base + slice];
            for j in 0..dims[1] {
                let y = origin[1] + voxel * j as f64;
                let row = j * dims[0];
                for i in 0..dims[0] {
                    let w = wch[row + i];
                    if !(w > lo && w < hi) {
                        continue;
                    }
                    ambiguous.fetch_add(1, Ordering::Relaxed);
                    let x = origin[0] + voxel * i as f64;
                    let v = stabber.vote(bvh, [x, y, z]);
                    if v.unresolved {
                        unresolved.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let s = uch[row + i];
                    let was_inside = s < 0.0;
                    if was_inside != v.inside {
                        flipped.fetch_add(1, Ordering::Relaxed);
                    }
                    sch[row + i] = if v.inside { -s } else { s };
                }
            }
        });
    let amb = ambiguous.load(Ordering::Relaxed);
    RayStabStats {
        dirs: stabber.n_dirs(),
        parity: stabber.parity,
        cells_ambiguous: amb,
        cells_flipped: flipped.load(Ordering::Relaxed),
        cells_unresolved: unresolved.load(Ordering::Relaxed),
        rays_cast: amb.saturating_mul(stabber.n_dirs() as u64),
        seconds: t_start.elapsed().as_secs_f64(),
    }
}

/// Full-grid inside mask (`1` inside) and signed vote score
/// (`inside_votes - outside_votes`) for analysis and tests.  C order
/// `[i + nx*(j + ny*k)]`.
pub struct GridVote {
    pub inside: Vec<u8>,
    pub score: Vec<i32>,
}

pub fn raystab_grid(
    bvh: &MeshBvh,
    origin: [f64; 3],
    voxel: f64,
    dims: [usize; 3],
    stabber: &Stabber,
) -> GridVote {
    let slice = dims[0] * dims[1];
    let n = slice * dims[2];
    let mut inside = vec![0u8; n];
    let mut score = vec![0i32; n];
    inside
        .par_chunks_mut(slice)
        .zip(score.par_chunks_mut(slice))
        .enumerate()
        .for_each(|(k, (ich, sch))| {
            let z = origin[2] + voxel * k as f64;
            for j in 0..dims[1] {
                let y = origin[1] + voxel * j as f64;
                let row = j * dims[0];
                for i in 0..dims[0] {
                    let x = origin[0] + voxel * i as f64;
                    let v = stabber.vote(bvh, [x, y, z]);
                    ich[row + i] = if v.inside { 1 } else { 0 };
                    sch[row + i] =
                        v.inside_votes as i32 - (v.outside_votes + v.escape_votes) as i32;
                }
            }
        });
    GridVote { inside, score }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::winding::MeshBvh;

    fn cube() -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let f = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (v, f)
    }

    fn stabber(bvh: &MeshBvh, parity: bool) -> Stabber {
        Stabber::new(&bvh.bounds(), DEFAULT_DIRS, 0, parity, DEFAULT_ESCAPE_WEIGHT)
    }

    #[test]
    fn vote_inside_and_outside_cube() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        let s = stabber(&bvh, false);
        let inside = s.vote(&bvh, [0.5, 0.5, 0.5]);
        assert!(inside.inside, "centre voted outside: {inside:?}");
        assert_eq!(inside.inside_votes, DEFAULT_DIRS as u32);
        assert_eq!(inside.escape_votes, 0);
        let outside = s.vote(&bvh, [3.0, 0.5, 0.5]);
        assert!(!outside.inside, "far point voted inside: {outside:?}");
    }

    #[test]
    fn escape_is_a_strong_outside_vote() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        let s = stabber(&bvh, false);
        let v = s.vote(&bvh, [10.0, 10.0, 10.0]);
        assert!(!v.inside);
        assert_eq!(v.escape_votes, DEFAULT_DIRS as u32);
    }

    #[test]
    fn overlapping_cubes_overlap_is_inside() {
        // Two axis-aligned unit cubes whose intersection is [0.75,1]^3.
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
            [0.5, 0.5, 0.5],
            [1.5, 0.5, 0.5],
            [1.5, 1.5, 0.5],
            [0.5, 1.5, 0.5],
            [0.5, 0.5, 1.5],
            [1.5, 0.5, 1.5],
            [1.5, 1.5, 1.5],
            [0.5, 1.5, 1.5],
        ];
        let f = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [3, 0, 4],
            [3, 4, 7],
            [8, 10, 9],
            [8, 11, 10],
            [12, 13, 14],
            [12, 14, 15],
            [8, 9, 13],
            [8, 13, 12],
            [10, 11, 15],
            [10, 15, 14],
            [9, 10, 14],
            [9, 14, 13],
            [11, 8, 12],
            [11, 12, 15],
        ];
        let bvh = MeshBvh::from_arrays(&v, &f);
        let s = stabber(&bvh, false);
        // Point in the overlap: parity-based winding reads 0.5; the oriented
        // ray sum must still vote inside.
        assert!(s.vote(&bvh, [0.9, 0.9, 0.9]).inside, "overlap voted outside");
        // Point inside only the first cube (not the overlap).
        assert!(s.vote(&bvh, [0.2, 0.2, 0.2]).inside);
        // Far outside.
        assert!(!s.vote(&bvh, [5.0, 5.0, 5.0]).inside);
    }

    #[test]
    fn parity_mode_is_orientation_independent() {
        // A cube with one face's winding flipped: the oriented vote can be
        // corrupted, parity must still classify the interior correctly.
        let (v, mut f) = cube();
        f[0] = [0, 1, 2]; // flip the first face
        let bvh = MeshBvh::from_arrays(&v, &f);
        let oriented = stabber(&bvh, false).vote(&bvh, [0.5, 0.5, 0.5]);
        let parity = stabber(&bvh, true).vote(&bvh, [0.5, 0.5, 0.5]);
        assert!(oriented.inside, "oriented vote: {oriented:?}");
        assert!(parity.inside, "parity vote: {parity:?}");
    }

    #[test]
    fn disambiguate_only_touches_the_band() {
        // 3x1x1 grid; the middle cell is winding-ambiguous and gets flipped.
        let verts = vec![
            [0.5, -1.0, -1.0],
            [0.5, 1.0, -1.0],
            [0.5, -1.0, 1.0],
        ];
        let tris = vec![[0, 1, 2]];
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        let dims = [3, 1, 1];
        let origin = [0.0, 0.0, 0.0];
        let voxel = 0.5;
        let w = vec![0.0f32, 0.5, 1.0];
        let u = vec![1.0f32, 2.0, 3.0];
        let mut s = vec![1.0f32, 2.0, -3.0];
        let st = stabber(&bvh, false);
        let stats =
            disambiguate_sdf(&bvh, &w, &u, &mut s, origin, voxel, dims, &st, AMBIG_LO, AMBIG_HI);
        assert_eq!(stats.cells_ambiguous, 1);
        // Cells outside the band are byte-identical.
        assert_eq!(s[0], 1.0);
        assert_eq!(s[2], -3.0);
        // The ambiguous cell was rewritten using its unsigned magnitude.
        assert!(s[1].abs() == 2.0);
    }

    #[test]
    fn grid_vote_matches_point_votes() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        let s = stabber(&bvh, false);
        // 4^3 grid starting half a voxel outside the cube: cell (0,0,0) is at
        // (-0.5,-0.5,-0.5) (clearly outside) and cell (2,2,2) at (0.5,0.5,0.5).
        let dims = [4, 4, 4];
        let grid = raystab_grid(&bvh, [-0.5, -0.5, -0.5], 0.5, dims, &s);
        let c = 2 + 4 * 2 + 16 * 2;
        assert_eq!(grid.inside[c], 1);
        assert!(grid.score[c] > 0);
        assert_eq!(grid.inside[0], 0);
    }
}
