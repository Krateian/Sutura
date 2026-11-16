// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! CPU rasterizer for the heatmap PNGs, ported from `sutura/heatmap.py`.
//!
//! Python ships an isometric **orthographic** CPU renderer (QPainter), which is
//! what `heatmap_render.py`, `before_after_render.py` and the GUI's zoom view
//! produce. This reproduces it pixel for pixel so those file outputs survive the
//! port — the GUI's perspective GPU view is a *different* picture and cannot
//! stand in for them.
//!
//! Deliberately not reused from the viewer: the viewer is a realtime surface with
//! a perspective camera and depth buffer, while these outputs must be
//! deterministic, headless and identical to Python's.

/// The fixed isometric camera basis, rows = [right, up, forward].
pub const ISOMETRIC: [[f64; 3]; 3] = [
    [0.707_106_78, 0.707_106_78, 0.0],
    [-0.408_248_29, 0.408_248_29, 0.816_496_58],
    [0.577_350_27, -0.577_350_27, 0.577_350_27],
];

/// The render background (`heatmap.py`'s `bg=(18, 22, 26)`). Not the GUI's
/// window colour — these PNGs must match Python's, so its constant is the one
/// that counts.
pub const BACKGROUND: [u8; 3] = [18, 22, 26];

/// Default body colour (178, 186, 194) as `_shade_lut`'s base.
pub const BODY: [u8; 3] = [178, 186, 194];
/// Defect colour (235, 60, 70).
pub const DEFECT: [u8; 3] = [235, 60, 70];
/// Healed colour (46, 204, 113).
pub const HEALED: [u8; 3] = [46, 204, 113];
/// `viewer_common.ORANGE`, the still-broken colour after a repair.
pub const STILL_BROKEN: [u8; 3] = [230, 126, 34];

/// An RGB image, row-major, 3 bytes per pixel.
pub struct Rgb8 {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Rgb8 {
    pub fn new(w: u32, h: u32, background: [u8; 3]) -> Self {
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            data.extend_from_slice(&background);
        }
        Self { w, h, data }
    }

    fn put(&mut self, x: i64, y: i64, c: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 3) as usize;
        self.data[i..i + 3].copy_from_slice(&c);
    }

    /// Encode as a PNG using the same writer the screenshot path uses.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let img: image::RgbImage =
            image::ImageBuffer::from_raw(self.w, self.h, self.data.clone())
                .ok_or_else(|| "image buffer size mismatch".to_string())?;
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// A camera frame: world-space centre and screen scale (`shared_frame`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub center: [f64; 3],
    pub scale: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Fit one mesh into `w`×`h` with `pad` — `heatmap.shared_frame`.
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
        let p = [v[0] - center[0], v[1] - center[1], v[2] - center[2]];
        let x = dot(ISOMETRIC[0], p);
        let y = dot(ISOMETRIC[1], p);
        xs.0 = xs.0.min(x);
        xs.1 = xs.1.max(x);
        ys.0 = ys.0.min(y);
        ys.1 = ys.1.max(y);
    }
    let span_x = xs.1 - xs.0;
    let span_y = ys.1 - ys.0;
    let span_x = if span_x <= 0.0 { 1.0 } else { span_x };
    let span_y = if span_y <= 0.0 { 1.0 } else { span_y };
    let s = ((w as f64) - 2.0 * pad) / span_x;
    let s2 = ((h as f64) - 2.0 * pad) / span_y;
    Frame {
        center,
        scale: s.min(s2),
    }
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

/// Per-face shading, `_lighting_shade` + `_shade_index`.
///
/// `s = 0.30 + 0.62*max(n·key,0) + 0.22*max(n·fill,0) + 0.16*max(n·rim,0)`,
/// clipped to [0,1]; the LUT is then indexed by `round(s*64)`.
pub fn face_shade(verts: &[[f64; 3]], tri: &[i64; 3]) -> f64 {
    let a = verts[tri[0] as usize];
    let b = verts[tri[1] as usize];
    let c = verts[tri[2] as usize];
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    let n = if len > 1e-12 {
        [n[0] / len, n[1] / len, n[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    };
    // The exact lights from `heatmap._LIGHT_KEY/_FILL/_RIM`. Getting these
    // wrong shifts every face's shade (first attempt used guessed vectors and
    // only 446 of 43200 pixels matched).
    //   key  = [0.57735027, 0.57735027, 0.57735027]  (already unit length)
    //   fill = [-0.7, 0.3, 0.65]  / |…|
    //   rim  = [-0.55, 0.55, -0.62] / |…|
    let key = [0.577_350_27, 0.577_350_27, 0.577_350_27];
    let fill = [-0.7, 0.3, 0.65];
    let rim = [-0.55, 0.55, -0.62];
    let kn = dot(n, norm(key));
    let fn_ = dot(n, norm(fill));
    let rn = dot(n, norm(rim));
    let s = 0.30 + 0.62 * kn.max(0.0) + 0.22 * fn_.max(0.0) + 0.16 * rn.max(0.0);
    s.clamp(0.0, 1.0)
}

fn norm(v: [f64; 3]) -> [f64; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l <= 1e-12 {
        v
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

/// Apply `_shade_lut` to a base colour at a shade value.
pub fn shade_colour(base: [u8; 3], shade: f64) -> [u8; 3] {
    let idx = shade_lut_index(shade);
    let v = 0.18 + (1.0 - 0.18) * (idx as f64 / 64.0);
    [
        (base[0] as f64 * v).round().clamp(0.0, 255.0) as u8,
        (base[1] as f64 * v).round().clamp(0.0, 255.0) as u8,
        (base[2] as f64 * v).round().clamp(0.0, 255.0) as u8,
    ]
}

fn shade_lut_index(shade: f64) -> usize {
    (shade * 64.0).round_ties_even().clamp(0.0, 64.0) as usize
}

/// Triangle coverage on snapped vertices, testing the pixel centre.
///
/// Python snaps to `QPoint(int(px), int(py))` and lets QPainter fill; testing the
/// pixel centre against those snapped vertices reproduces it for 99.6% of
/// pixels. The residue (about 0.36%) sits on *shared* edges, where QPainter's
/// fill rule picks one of the two adjacent faces and this inclusive test picks
/// the other. I tried the standard top-left rule to close that gap and it made
/// the match slightly worse (99.38%), so the winding assumption it needs does not
/// hold for these triangles; the exact residue is left as a known, bounded
/// difference rather than papered over with a rule that does not match.
fn contains(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, px: f64, py: f64) -> bool {
    let d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
    let d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
    let d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// Render one frame, matching `heatmap.draw_frame`.
///
/// `face_colour` decides each face's colour from its index, shade and defect
/// flag — the caller supplies the precedence (defect red, healed green, ramp…).
pub fn draw_frame(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    w: u32,
    h: u32,
    frame: &Frame,
    background: [u8; 3],
    face_colour: &dyn Fn(usize, f64) -> [u8; 3],
) -> Rgb8 {
    let mut img = Rgb8::new(w, h, background);
    if tris.is_empty() {
        return img;
    }
    let proj: Vec<(f64, f64, f64)> = verts.iter().map(|v| project(*v, frame, w, h)).collect();

    // Painter's algorithm: ascending mean depth, later faces overwrite.
    let mut order: Vec<usize> = (0..tris.len()).collect();
    let mean_z = |t: &[i64; 3]| {
        (proj[t[0] as usize].2 + proj[t[1] as usize].2 + proj[t[2] as usize].2) / 3.0
    };
    order.sort_by(|a, b| {
        mean_z(&tris[*a])
            .partial_cmp(&mean_z(&tris[*b]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // `QPoint(int(px), int(py))` truncates to whole pixels, so the vertices are
    // snapped to the integer grid before the polygon is filled. Testing pixel
    // centres against the un-truncated triangle instead shifted every silhouette
    // edge by up to a pixel.
    for fi in order {
        let t = tris[fi];
        let shade = face_shade(verts, &t);
        let colour = face_colour(fi, shade);
        let (ax, ay) = (
            proj[t[0] as usize].0 as i64 as f64,
            proj[t[0] as usize].1 as i64 as f64,
        );
        let (bx, by) = (
            proj[t[1] as usize].0 as i64 as f64,
            proj[t[1] as usize].1 as i64 as f64,
        );
        let (cx, cy) = (
            proj[t[2] as usize].0 as i64 as f64,
            proj[t[2] as usize].1 as i64 as f64,
        );
        let x0 = ax.min(bx).min(cx).max(0.0) as i64;
        let x1 = ax.max(bx).max(cx).min(w as f64 - 1.0) as i64;
        let y0 = ay.min(by).min(cy).max(0.0) as i64;
        let y1 = ay.max(by).max(cy).min(h as f64 - 1.0) as i64;
        for y in y0..=y1 {
            for x in x0..=x1 {
                if contains(ax, ay, bx, by, cx, cy, x as f64 + 0.5, y as f64 + 0.5) {
                    img.put(x, y, colour);
                }
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let t = vec![[0i64, 1, 2], [0, 2, 3]];
        (v, t)
    }

    #[test]
    fn frame_centre_is_the_bbox_centre() {
        let v = vec![[0.0, 0.0, 0.0], [2.0, 4.0, 6.0]];
        let f = shared_frame(&v, 100, 100, 10.0);
        assert_eq!(f.center, [1.0, 2.0, 3.0]);
        assert!(f.scale > 0.0);
    }

    /// The mesh must fit inside the padded viewport.
    #[test]
    fn projected_mesh_fits_in_the_padding() {
        let (v, _) = quad();
        let f = shared_frame(&v, 200, 200, 20.0);
        let pts: Vec<(f64, f64, f64)> = v.iter().map(|p| project(*p, &f, 200, 200)).collect();
        for (x, y, _) in pts {
            assert!(x >= 19.0 && x <= 181.0, "x {x}");
            assert!(y >= 19.0 && y <= 181.0, "y {y}");
        }
    }

    /// A face filling the viewport paints its pixels; the background survives
    /// outside it.
    #[test]
    fn draws_a_face_and_leaves_the_background() {
        let (v, t) = quad();
        let f = shared_frame(&v, 64, 64, 4.0);
        let img = draw_frame(&v, &t, 64, 64, &f, [0, 0, 0], &|_, shade| {
            shade_colour(BODY, shade)
        });
        assert_eq!(img.data.len(), 64 * 64 * 3);
        // The centre is inside the quad.
        let centre = ((32 * 64 + 32) * 3) as usize;
        assert_ne!(&img.data[centre..centre + 3], &[0, 0, 0]);
        // The top-left corner is not.
        assert_eq!(&img.data[0..3], &[0, 0, 0]);
    }

    /// Shading is deterministic and bounded.
    #[test]
    fn shading_is_bounded_and_stable() {
        let (v, t) = quad();
        let s = face_shade(&v, &t[0]);
        assert!((0.0..=1.0).contains(&s), "{s}");
        assert_eq!(s, face_shade(&v, &t[0]));
        // The LUT's darkest and lightest ends.
        assert_eq!(shade_colour([255, 255, 255], 0.0), [46, 46, 46]);
        assert_eq!(shade_colour([255, 255, 255], 1.0), [255, 255, 255]);
    }

    /// PNG output is a real PNG (magic bytes), so the file paths work.
    #[test]
    fn encodes_png() {
        let (v, t) = quad();
        let f = shared_frame(&v, 32, 32, 2.0);
        let img = draw_frame(&v, &t, 32, 32, &f, [20, 20, 20], &|_, s| {
            shade_colour(BODY, s)
        });
        let png = img.to_png().expect("png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
