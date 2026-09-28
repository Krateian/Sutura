//! Morphological operations on a signed-distance grid.
//!
//! Dilation and erosion of the solid `{ sdf < 0 }` are implemented as
//! `sdf - r` and `sdf + r` only for the *occupancy* they toggle.  A closing
//! (`dilate(r)` then `erode(r)`) must not simply add `r` back to the same
//! field -- that would cancel the dilation exactly.  Instead the dilated
//! occupancy is re-distanced with an exact Euclidean distance transform
//! (Felzenszwalb & Huttenlocher 2012, separable lower-envelope-of-parabolas),
//! which yields a true distance to the dilated solid; eroding that field by
//! `r` then recovers the closing.

/// INF sentinel for the distance transform (squared index distance).
const EDT_INF: f32 = 1.0e20;

/// In-place exact 1-D squared-distance transform of `f` (Felzenszwalb &
/// Huttenlocher).  `f[q]` is the seed cost at sample `q`; on return `f[q]` is
/// `min_p ((q-p)^2 + f0[p])`.
pub fn edt_1d(f: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut out = vec![0.0f32; n];
    let mut v = vec![0i32; n];
    let mut z = vec![0.0f32; n + 1];
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;

    for q in 1..n {
        let mut s = intersection(f[q], q as f32, f[v[k] as usize], v[k] as f32);
        while s <= z[k] {
            k -= 1;
            s = intersection(f[q], q as f32, f[v[k] as usize], v[k] as f32);
        }
        k += 1;
        v[k] = q as i32;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }

    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let d = q as f32 - v[k] as f32;
        out[q] = d * d + f[v[k] as usize];
    }
    f.copy_from_slice(&out);
}

#[inline]
fn intersection(fq: f32, q: f32, fv: f32, v: f32) -> f32 {
    if q == v {
        return f32::NEG_INFINITY;
    }
    ((fq + q * q) - (fv + v * v)) / (2.0 * (q - v))
}

/// Exact separable 3-D Euclidean squared-distance transform, C order
/// `[i + nx*(j + ny*k)]`.
pub fn edt_3d(f: &mut [f32], dims: [usize; 3]) {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    if nx == 0 || ny == 0 || nz == 0 {
        return;
    }
    let nxy = nx * ny;

    // X passes (contiguous lines).
    for k in 0..nz {
        for j in 0..ny {
            let base = k * nxy + j * nx;
            edt_1d(&mut f[base..base + nx]);
        }
    }

    // Y passes (strided).
    let mut line = vec![0.0f32; ny];
    for k in 0..nz {
        for i in 0..nx {
            for j in 0..ny {
                line[j] = f[k * nxy + j * nx + i];
            }
            edt_1d(&mut line);
            for j in 0..ny {
                f[k * nxy + j * nx + i] = line[j];
            }
        }
    }

    // Z passes (strided).
    let mut line = vec![0.0f32; nz];
    for j in 0..ny {
        for i in 0..nx {
            for k in 0..nz {
                line[k] = f[k * nxy + j * nx + i];
            }
            edt_1d(&mut line);
            for k in 0..nz {
                f[k * nxy + j * nx + i] = line[k];
            }
        }
    }
}

/// Signed distance (in *index* units) of an occupancy field: negative inside,
/// positive outside, magnitude the distance to the nearest opposite voxel.
pub fn signed_distance_from_occupancy(occ: &[u8], dims: [usize; 3]) -> Vec<f32> {
    let n = occ.len();
    let mut dist_to_inside = vec![EDT_INF; n];
    let mut dist_to_outside = vec![EDT_INF; n];
    for i in 0..n {
        if occ[i] != 0 {
            dist_to_inside[i] = 0.0;
        } else {
            dist_to_outside[i] = 0.0;
        }
    }
    edt_3d(&mut dist_to_inside, dims);
    edt_3d(&mut dist_to_outside, dims);
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        out[i] = if occ[i] != 0 {
            -dist_to_outside[i].sqrt()
        } else {
            dist_to_inside[i].sqrt()
        };
    }
    out
}

/// Occupancy of the solid after dilation by `r`: `sdf - r < 0`.
pub fn dilate_occupancy(sdf: &[f32], r: f32) -> Vec<u8> {
    sdf.iter()
        .map(|&s| if s - r < 0.0 { 1u8 } else { 0u8 })
        .collect()
}

/// Closing of the solid by `r`: dilate, re-distance with a true EDT of the
/// dilated occupancy, then erode by `r`.  `voxel` converts the index-unit EDT
/// into physical units.  Returns a physical-ish SDF on the same grid.
pub fn close_sdf(sdf: &[f32], dims: [usize; 3], r: f32, voxel: f32) -> Vec<f32> {
    let occ = dilate_occupancy(sdf, r);
    let sd = signed_distance_from_occupancy(&occ, dims);
    sd.iter().map(|&s| s * voxel + r).collect()
}

/// Fill enclosed cavities: any empty region not connected to the grid
/// boundary is treated as solid, and a fresh signed distance is computed.
/// Used by the `fill_cavities=True` option to keep only the outermost shell.
pub fn fill_cavities(sdf: &[f32], dims: [usize; 3], voxel: f32) -> Vec<f32> {
    let (nx, ny, nz) = (dims[0], dims[1], dims[2]);
    let n = nx * ny * nz;
    let mut occ = vec![0u8; n];
    for i in 0..n.min(sdf.len()) {
        occ[i] = if sdf[i] < 0.0 { 1 } else { 0 };
    }
    // Flood-fill the empty voxels reachable from the grid boundary.
    let mut reachable = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let nxy = nx * ny;
    let push = |id: usize, occ: &[u8], reachable: &mut [bool], stack: &mut Vec<usize>| {
        if occ[id] == 0 && !reachable[id] {
            reachable[id] = true;
            stack.push(id);
        }
    };
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                if i == 0 || j == 0 || k == 0 || i == nx - 1 || j == ny - 1 || k == nz - 1 {
                    push(i + nx * j + nxy * k, &occ, &mut reachable, &mut stack);
                }
            }
        }
    }
    while let Some(id) = stack.pop() {
        let i = id % nx;
        let j = (id / nx) % ny;
        let k = id / nxy;
        if i > 0 {
            push(id - 1, &occ, &mut reachable, &mut stack);
        }
        if i + 1 < nx {
            push(id + 1, &occ, &mut reachable, &mut stack);
        }
        if j > 0 {
            push(id - nx, &occ, &mut reachable, &mut stack);
        }
        if j + 1 < ny {
            push(id + nx, &occ, &mut reachable, &mut stack);
        }
        if k > 0 {
            push(id - nxy, &occ, &mut reachable, &mut stack);
        }
        if k + 1 < nz {
            push(id + nxy, &occ, &mut reachable, &mut stack);
        }
    }
    for i in 0..n {
        if occ[i] == 0 && !reachable[i] {
            occ[i] = 1;
        }
    }
    let sd = signed_distance_from_occupancy(&occ, dims);
    sd.iter().map(|&s| s * voxel).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edt_1d_simple() {
        let mut f = vec![EDT_INF, 0.0, EDT_INF, EDT_INF];
        edt_1d(&mut f);
        assert_eq!(f, vec![1.0, 0.0, 1.0, 4.0]);
    }

    #[test]
    fn edt_1d_two_seeds() {
        let mut f = vec![0.0, EDT_INF, EDT_INF, EDT_INF, EDT_INF, 0.0];
        edt_1d(&mut f);
        // Nearest seed boundaries at the midpoint; distances are index^2.
        assert_eq!(f, vec![0.0, 1.0, 4.0, 4.0, 1.0, 0.0]);
    }

    #[test]
    fn edt_3d_signed_cube() {
        // 5^3 grid, solid = the central voxel column [2,2,2].
        let dims = [5, 5, 5];
        let mut occ = vec![0u8; 125];
        occ[2 + 5 * 2 + 25 * 2] = 1;
        let sd = signed_distance_from_occupancy(&occ, dims);
        // Centre is inside (negative), distance to nearest outside voxel = 1.
        assert!(sd[2 + 5 * 2 + 25 * 2] < 0.0);
        // A neighbour just outside -> +1.
        let d = sd[2 + 5 * 2 + 25 * 3];
        assert!((d - 1.0).abs() < 1e-5, "outside distance {d}");
    }

    #[test]
    fn closing_fills_a_gap() {
        // 5x1x1 grid: solid at indices 1 and 3, an empty gap at index 2, with
        // outside voxels at both ends.  Closing by r=1 must fill the gap.
        let dims = [5, 1, 1];
        let sdf = vec![2.0f32, -0.5, 0.5, -0.5, 2.0];
        let closed = close_sdf(&sdf, dims, 1.0, 1.0);
        assert!(closed[2] < 0.0, "gap not closed: {:?}", closed);
        assert!(closed[0] > 0.0, "outside changed: {:?}", closed);
        assert!(closed[4] > 0.0, "outside changed: {:?}", closed);
    }
}
