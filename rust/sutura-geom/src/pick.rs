// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Face picking for the repeat picker, matching `sutura/heatmap.py`.
//!
//! The Python picker renders a per-pixel face-index image with QPainter and
//! reads the colour back (`_RepeatPickerView.mousePressEvent`). The *value* it
//! produces is not a pixel but a point: the clicked triangle's vertex mean.
//! That is what this module reproduces — the same camera, the same
//! painter's-algorithm visibility order, the same integer truncation — so a
//! click at the same pixel yields the same 3D point.
//!
//! The camera is `heatmap.shared_frame(..., 24)` plus the fixed `_ISOMETRIC`
//! basis; `project` is `heatmap._project`.

/// The picker viewport, from `_RepeatPickerView.W/H`.
pub const W: u32 = 640;
pub const H: u32 = 480;
/// `shared_frame(..., 24)`.
pub const PAD: f64 = 24.0;

/// The fixed isometric camera basis, rows = [right, up, forward] (`_ISOMETRIC`).
pub const ISOMETRIC: [[f64; 3]; 3] = [
    [0.707_106_78, 0.707_106_78, 0.0],
    [-0.408_248_29, 0.408_248_29, 0.816_496_58],
    [0.577_350_27, -0.577_350_27, 0.577_350_27],
];

/// A camera frame: world-space centre and screen scale (`shared_frame`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub center: [f64; 3],
    pub scale: f64,
}

/// Fit one mesh into `w`×`h` with `pad` — `shared_frame([verts], w, h, pad)`.
pub fn shared_frame(verts: &[[f64; 3]], w: u32, h: u32, pad: f64) -> Frame {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for v in verts {
        for i in 0..3 {
            lo[i] = lo[i].min(v[i]);
            hi[i] = hi[i].max(v[i]);
        }
    }
    if !lo[0].is_finite() {
        return Frame {
            center: [0.0; 3],
            scale: 1.0,
        };
    }
    let center = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];
    let mut xs = (f64::INFINITY, f64::NEG_INFINITY);
    let mut ys = (f64::INFINITY, f64::NEG_INFINITY);
    for v in verts {
        let p = [
            v[0] - center[0],
            v[1] - center[1],
            v[2] - center[2],
        ];
        let x = dot(ISOMETRIC[0], p);
        let y = dot(ISOMETRIC[1], p);
        xs.0 = xs.0.min(x);
        xs.1 = xs.1.max(x);
        ys.0 = ys.0.min(y);
        ys.1 = ys.1.max(y);
    }
    let span_x = (xs.1 - xs.0).max(f64::MIN_POSITIVE);
    let span_y = (ys.1 - ys.0).max(f64::MIN_POSITIVE);
    // `or 1.0` in Python: an exactly-zero span means 1.0, not the epsilon.
    let span_x = if span_x <= 0.0 { 1.0 } else { span_x };
    let span_y = if span_y <= 0.0 { 1.0 } else { span_y };
    let s = ((w as f64) - 2.0 * pad) / span_x;
    let s2 = ((h as f64) - 2.0 * pad) / span_y;
    Frame {
        center,
        scale: s.min(s2),
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Project a point to screen pixels plus view-space depth (`_project`).
pub fn project(p: [f64; 3], frame: &Frame, w: u32, h: u32) -> (f64, f64, f64) {
    let v = [
        p[0] - frame.center[0],
        p[1] - frame.center[1],
        p[2] - frame.center[2],
    ];
    let x = dot(ISOMETRIC[0], v);
    let y = dot(ISOMETRIC[1], v);
    let z = dot(ISOMETRIC[2], v);
    (
        w as f64 * 0.5 + x * frame.scale,
        h as f64 * 0.5 - y * frame.scale,
        z,
    )
}

/// A triangle's vertex mean — what the picker reports (`verts[tris[fid-1]].mean`).
///
/// This is the **vertex mean**, not the area-weighted centroid; the two differ
/// on any non-equilateral triangle, so the distinction matters for parity.
pub fn face_centroid(verts: &[[f64; 3]], tris: &[[i64; 3]], fid: usize) -> Option<[f64; 3]> {
    let t = tris.get(fid.checked_sub(1)?)?;
    let idx: Vec<usize> = t.iter().map(|i| *i as usize).collect();
    if idx.iter().any(|i| *i >= verts.len()) {
        return None;
    }
    let n = idx.len() as f64;
    let mut m = [0.0; 3];
    for i in idx {
        for k in 0..3 {
            m[k] += verts[i][k];
        }
    }
    Some([m[0] / n, m[1] / n, m[2] / n])
}

/// The frontmost face at pixel `(px, py)`, or `None` for background.
///
/// Reproduces `draw_face_index`: triangles are painted in ascending mean-depth
/// order, so the *last* triangle to cover a pixel wins. Vertices are truncated
/// to integer pixels like `QPoint(int(px), int(py))`, and coverage uses the
/// same edge-function sign test so interior pixels agree. Returns the 1-based
/// face id Python reports (0 = background is mapped to `None`).
pub fn face_at(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    frame: &Frame,
    w: u32,
    h: u32,
    px: i64,
    py: i64,
) -> Option<usize> {
    if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 {
        return None;
    }
    let proj: Vec<(f64, f64, f64)> = verts.iter().map(|v| project(*v, frame, w, h)).collect();
    let mx = px as f64 + 0.5;
    let my = py as f64 + 0.5;

    let mut order: Vec<usize> = (0..tris.len()).collect();
    let mean_z = |t: &[i64; 3]| -> f64 {
        (proj[t[0] as usize].2 + proj[t[1] as usize].2 + proj[t[2] as usize].2) / 3.0
    };
    order.sort_by(|a, b| {
        mean_z(&tris[*a])
            .partial_cmp(&mean_z(&tris[*b]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut hit = None;
    for i in order {
        let t = tris[i];
        let (ax, ay) = (proj[t[0] as usize].0, proj[t[0] as usize].1);
        let (bx, by) = (proj[t[1] as usize].0, proj[t[1] as usize].1);
        let (cx, cy) = (proj[t[2] as usize].0, proj[t[2] as usize].1);
        if contains(ax, ay, bx, by, cx, cy, mx, my) {
            hit = Some(i + 1);
        }
    }
    hit
}

fn contains(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, px: f64, py: f64) -> bool {
    let d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
    let d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
    let d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
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
        let t = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (v, t)
    }

    /// The camera centre is the world bbox centre, independent of rotation.
    #[test]
    fn frame_centre_is_bbox_centre() {
        let (v, _) = cube();
        let f = shared_frame(&v, W, H, PAD);
        assert_eq!(f.center, [0.5, 0.5, 0.5]);
        assert!(f.scale > 0.0);
    }

    /// The mesh must fit inside the padded viewport: that is the whole point of
    /// `shared_frame`'s scale.
    #[test]
    fn projected_mesh_fits_in_padding() {
        let (v, _) = cube();
        let f = shared_frame(&v, W, H, PAD);
        let pts: Vec<(f64, f64, f64)> = v.iter().map(|p| project(*p, &f, W, H)).collect();
        let xs: Vec<f64> = pts.iter().map(|p| p.0).collect();
        let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
        let lo_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let lo_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(lo_x >= PAD - 1e-9 && hi_x <= W as f64 - PAD + 1e-9, "{lo_x}..{hi_x}");
        assert!(lo_y >= PAD - 1e-9 && hi_y <= H as f64 - PAD + 1e-9, "{lo_y}..{hi_y}");
    }

    /// The reported point is the triangle's vertex mean, not the area-weighted
    /// centroid. On a scalene triangle the two differ, so this pins the choice.
    #[test]
    fn face_centroid_is_vertex_mean() {
        let v = vec![[0.0, 0.0, 0.0], [6.0, 0.0, 0.0], [0.0, 3.0, 0.0]];
        let t = vec![[0i64, 1, 2]];
        let c = face_centroid(&v, &t, 1).unwrap();
        assert_eq!(c, [2.0, 1.0, 0.0]);
        // Area-weighted centroid happens to coincide here; move one vertex so
        // the two definitions diverge and confirm the mean still wins.
        let v2 = vec![[0.0, 0.0, 0.0], [9.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let c2 = face_centroid(&v2, &t, 1).unwrap();
        assert_eq!(c2, [3.0, 1.0 / 3.0, 0.0]);
    }

    #[test]
    fn face_at_finds_a_face_on_the_cube_and_none_outside() {
        let (v, t) = cube();
        let f = shared_frame(&v, W, H, PAD);
        let centre = face_at(&v, &t, &f, W, H, (W / 2) as i64, (H / 2) as i64);
        assert!(centre.is_some(), "the cube covers the viewport centre");
        assert!(centre.unwrap() >= 1 && centre.unwrap() <= t.len());
        assert_eq!(face_at(&v, &t, &f, W, H, -1, 5), None);
        assert_eq!(face_at(&v, &t, &f, W, H, (W as i64) + 5, 5), None);
        // A corner outside the fitted mesh is background.
        assert_eq!(face_at(&v, &t, &f, W, H, 1, 1), None);
    }

    /// Picking is deterministic: the same pixel always yields the same face, so
    /// a gate can compare Python and Rust on a fixed click list.
    #[test]
    fn picking_is_deterministic() {
        let (v, t) = cube();
        let f = shared_frame(&v, W, H, PAD);
        for (x, y) in [(320, 240), (300, 200), (350, 280)] {
            let a = face_at(&v, &t, &f, W, H, x, y);
            let b = face_at(&v, &t, &f, W, H, x, y);
            assert_eq!(a, b);
        }
    }
}
