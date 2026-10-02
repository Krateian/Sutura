//! Generalized winding number and unsigned distance queries over a triangle
//! soup, backed by a triangle BVH.
//!
//! The generalized winding number follows Barill et al. 2018 ("Fast Winding
//! Numbers for Soups with Applications to Poisson Reconstruction"): the exact
//! per-triangle solid angle is summed for near triangles, while a node whose
//! subtended solid angle is below `WINDING_FAR_TOL` contributes the far-field
//! dipole approximation of its aggregated vector area.  Unlike a binary
//! inside/outside test this is robust to holes, self-intersections and nested
//! shells: the field degrades gracefully toward 0 where the surface is open
//! and sums to ~1 (mod 4 pi) around each shell.
//!
//! The unsigned distance uses the same BVH with a branch-and-bound closest
//! point on triangle search.  The signed field needed by the morphology
//! pipeline is `sign(w - 0.5) * unsigned`.

use rayon::prelude::*;

/// Far-field threshold on the estimated subtended solid angle of a BVH node
/// (`sum(|area|) / distance^2`, in steradians).  When a node subtends less than
/// this, the dipole approximation replaces its exact sum.  `1e-2` steradians
/// is ~8e-4 of a full turn, well below the 0.5 winding decision boundary.
pub const WINDING_FAR_TOL: f64 = 1e-2;

const LEAF_SIZE: usize = 8;
const MAX_DEPTH: usize = 64;

/// Maximum ray/triangle hits buffered per ray before the (rare) overflow path
/// switches to un-deduplicated accumulation.  Dense scans can be crossed many
/// times, so this is generous.
const MAX_RAY_HITS: usize = 256;

/// Relative grazing threshold: a ray hit is ignored when the cosine between the
/// ray direction and the (unnormalised) triangle normal is below
/// `GRAZE_EPS * |n|`, i.e. the ray is almost parallel to the triangle plane.
pub const GRAZE_EPS: f64 = 1e-7;

/// Barycentric slack for the ray/triangle acceptance test.
const BARY_EPS: f64 = 1e-9;

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn det(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    dot(a, cross(b, c))
}

#[inline]
fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Axis-aligned bounding box.
#[derive(Clone, Copy, Debug)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    pub fn empty() -> Self {
        Aabb {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    pub fn from_point(p: [f64; 3]) -> Self {
        Aabb { min: p, max: p }
    }

    pub fn is_empty(&self) -> bool {
        self.min[0] > self.max[0]
    }

    pub fn expand_point(&mut self, p: [f64; 3]) {
        for i in 0..3 {
            if p[i] < self.min[i] {
                self.min[i] = p[i];
            }
            if p[i] > self.max[i] {
                self.max[i] = p[i];
            }
        }
    }

    pub fn union(&mut self, o: &Aabb) {
        if o.is_empty() {
            return;
        }
        for i in 0..3 {
            if o.min[i] < self.min[i] {
                self.min[i] = o.min[i];
            }
            if o.max[i] > self.max[i] {
                self.max[i] = o.max[i];
            }
        }
    }

    pub fn center(&self) -> [f64; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    /// Squared distance from a point to the box (0 when inside).
    pub fn dist2(&self, p: [f64; 3]) -> f64 {
        let mut s = 0.0;
        for i in 0..3 {
            let mut d = 0.0;
            if p[i] < self.min[i] {
                d = self.min[i] - p[i];
            } else if p[i] > self.max[i] {
                d = p[i] - self.max[i];
            }
            s += d * d;
        }
        s
    }

    pub fn diagonal(&self) -> f64 {
        norm(sub(self.max, self.min))
    }

    /// True when the ray `o + t*d` intersects the box for some `t` in
    /// `[0, t_max]`.  `inv_d` is `1/d` precomputed once per ray; an infinite
    /// component means the ray is axis-parallel to that slab and is accepted
    /// only when `o` already lies inside it.  This is the classic slab test and
    /// is exact for axis-parallel rays.
    pub fn ray_hits(&self, o: [f64; 3], inv_d: [f64; 3], t_max: f64) -> bool {
        let mut t0 = 0.0f64;
        let mut t1 = t_max;
        for i in 0..3 {
            if !inv_d[i].is_finite() {
                if o[i] < self.min[i] || o[i] > self.max[i] {
                    return false;
                }
                continue;
            }
            let mut near = (self.min[i] - o[i]) * inv_d[i];
            let mut far = (self.max[i] - o[i]) * inv_d[i];
            if near > far {
                std::mem::swap(&mut near, &mut far);
            }
            if near > t0 {
                t0 = near;
            }
            if far < t1 {
                t1 = far;
            }
            if t0 > t1 {
                return false;
            }
        }
        true
    }

    pub fn longest_axis(&self) -> usize {
        let e = sub(self.max, self.min);
        if e[0] >= e[1] && e[0] >= e[2] {
            0
        } else if e[1] >= e[2] {
            1
        } else {
            2
        }
    }
}

/// A triangle with explicit vertex positions (cache-friendly for traversal).
#[derive(Clone, Copy)]
pub struct Tri {
    pub a: [f64; 3],
    pub b: [f64; 3],
    pub c: [f64; 3],
}

impl Tri {
    pub fn new(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Self {
        Tri { a, b, c }
    }

    #[inline]
    pub fn area_vector(&self) -> [f64; 3] {
        scale(cross(sub(self.b, self.a), sub(self.c, self.a)), 0.5)
    }

    #[inline]
    pub fn area(&self) -> f64 {
        norm(cross(sub(self.b, self.a), sub(self.c, self.a))) * 0.5
    }

    #[inline]
    fn solid_angle(&self, p: [f64; 3]) -> f64 {
        let a = sub(self.a, p);
        let b = sub(self.b, p);
        let c = sub(self.c, p);
        let la = norm(a);
        let lb = norm(b);
        let lc = norm(c);
        if la == 0.0 || lb == 0.0 || lc == 0.0 {
            return 0.0;
        }
        let num = det(a, b, c);
        let den = la * lb * lc + dot(a, b) * lc + dot(b, c) * la + dot(c, a) * lb;
        2.0 * num.atan2(den)
    }
}

struct Node {
    bbox: Aabb,
    left: i32,
    right: i32,
    start: u32,
    count: u32,
    /// Area-weighted aggregate centroid (dipole position).
    center: [f64; 3],
    /// Sum of oriented triangle area vectors (dipole moment).
    vec_area: [f64; 3],
    /// Sum of unsigned triangle areas (far-field magnitude bound).
    area_sum: f64,
}

/// Triangle-soup bounding volume hierarchy for winding-number and distance
/// queries.
pub struct MeshBvh {
    nodes: Vec<Node>,
    order: Vec<u32>,
    tris: Vec<Tri>,
}

/// Summary of one ray cast against the soup.
///
/// `net` is the oriented crossing sum `Σ sign(dot(dir, n_face))`: `+1` for a
/// crossing whose outward face normal points along the ray, `-1` against.
/// For a consistently outward-oriented closed surface this equals the number of
/// shells containing the ray origin (and is therefore `>= 0`), while `crossings`
/// is the raw (deduplicated) count used for the orientation-independent parity
/// fallback.
#[derive(Clone, Copy, Debug)]
pub struct RayQuery {
    pub net: i32,
    pub crossings: u32,
    pub nearest_t: f64,
    pub suspect: bool,
}

/// Outcome of a single ray/triangle test.
enum TriHit {
    Miss,
    /// Ray lies (almost) in the triangle plane; ignored but flagged.
    Graze,
    Hit { t: f64, sign: i8 },
}

/// Fixed-capacity buffer of ray hits along one ray, with an overflow path.
struct HitBuf {
    t: [f64; MAX_RAY_HITS],
    s: [i8; MAX_RAY_HITS],
    n: usize,
    overflow_net: i32,
    overflow_count: u32,
    suspect: bool,
}

impl HitBuf {
    fn new() -> Self {
        HitBuf {
            t: [0.0; MAX_RAY_HITS],
            s: [0; MAX_RAY_HITS],
            n: 0,
            overflow_net: 0,
            overflow_count: 0,
            suspect: false,
        }
    }

    #[inline]
    fn push(&mut self, t: f64, sign: i8) {
        if self.n < MAX_RAY_HITS {
            self.t[self.n] = t;
            self.s[self.n] = sign;
            self.n += 1;
        } else {
            self.overflow_net += sign as i32;
            self.overflow_count += 1;
            self.suspect = true;
        }
    }

    #[inline]
    fn graze(&mut self) {
        self.suspect = true;
    }
}

/// Möller–Trumbore ray/triangle intersection with an explicit grazing test.
///
/// `d` must be normalised.  The returned `sign` is `sign(dot(d, n))` with
/// `n = cross(b - a, c - a)`, the quantity the oriented crossing sum needs.
fn ray_tri_hit(o: [f64; 3], d: [f64; 3], tri: &Tri, t_max: f64) -> TriHit {
    let e1 = sub(tri.b, tri.a);
    let e2 = sub(tri.c, tri.a);
    let n = cross(e1, e2);
    let nl = norm(n);
    if nl == 0.0 {
        return TriHit::Miss;
    }
    let cosang = dot(d, n);
    if cosang.abs() < GRAZE_EPS * nl {
        return TriHit::Graze;
    }
    let pvec = cross(d, e2);
    let det = dot(e1, pvec);
    if det == 0.0 {
        return TriHit::Graze;
    }
    let inv_det = 1.0 / det;
    let tvec = sub(o, tri.a);
    let u = dot(tvec, pvec) * inv_det;
    if u < -BARY_EPS || u > 1.0 + BARY_EPS {
        return TriHit::Miss;
    }
    let qvec = cross(tvec, e1);
    let v = dot(d, qvec) * inv_det;
    if v < -BARY_EPS || u + v > 1.0 + BARY_EPS {
        return TriHit::Miss;
    }
    let t = dot(e2, qvec) * inv_det;
    if t < 0.0 || t > t_max {
        return TriHit::Miss;
    }
    TriHit::Hit {
        t,
        sign: if cosang > 0.0 { 1 } else { -1 },
    }
}

impl MeshBvh {
    pub fn from_arrays(verts: &[[f64; 3]], tris_idx: &[[usize; 3]]) -> Self {
        let mut tris = Vec::with_capacity(tris_idx.len());
        for t in tris_idx {
            tris.push(Tri::new(verts[t[0]], verts[t[1]], verts[t[2]]));
        }
        let centroids: Vec<[f64; 3]> = tris
            .iter()
            .map(|t| {
                [
                    (t.a[0] + t.b[0] + t.c[0]) / 3.0,
                    (t.a[1] + t.b[1] + t.c[1]) / 3.0,
                    (t.a[2] + t.b[2] + t.c[2]) / 3.0,
                ]
            })
            .collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes: Vec<Node> = Vec::new();
        if !tris.is_empty() {
            build_rec(&tris, &centroids, &mut order, 0, tris.len(), 0, &mut nodes);
        }
        MeshBvh { nodes, order, tris }
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Bounding box of the whole soup (`empty` for an empty mesh).
    pub fn bounds(&self) -> Aabb {
        self.nodes
            .first()
            .map(|n| n.bbox)
            .unwrap_or_else(Aabb::empty)
    }

    /// Generalized winding number at `p`; ~1 inside a closed shell, ~0 outside.
    pub fn winding_at(&self, p: [f64; 3]) -> f64 {
        if self.nodes.is_empty() {
            return 0.0;
        }
        self.winding_node(0, p) / (4.0 * std::f64::consts::PI)
    }

    fn winding_node(&self, idx: usize, p: [f64; 3]) -> f64 {
        let n = &self.nodes[idx];
        if n.left < 0 {
            let mut s = 0.0;
            for i in n.start..n.start + n.count {
                s += self.tris[self.order[i as usize] as usize].solid_angle(p);
            }
            return s;
        }
        let d2 = n.bbox.dist2(p);
        if d2 > 0.0 && n.area_sum / d2 < WINDING_FAR_TOL {
            let r = sub(n.center, p);
            let r2 = dot(r, r);
            if r2 <= 0.0 {
                return 0.0;
            }
            return dot(n.vec_area, r) / (r2 * r2.sqrt());
        }
        self.winding_node(n.left as usize, p) + self.winding_node(n.right as usize, p)
    }

    /// Unsigned distance from `p` to the nearest triangle.
    pub fn unsigned_distance_at(&self, p: [f64; 3]) -> f64 {
        if self.nodes.is_empty() {
            return f64::INFINITY;
        }
        let mut best = f64::INFINITY;
        self.dist_node(0, p, &mut best);
        best.sqrt()
    }

    /// Closest point on the triangle soup: `(point, distance, triangle index)`,
    /// or `(p, inf, usize::MAX)` for an empty mesh.
    pub fn closest_point(&self, p: [f64; 3]) -> ([f64; 3], f64, usize) {
        if self.nodes.is_empty() {
            return (p, f64::INFINITY, usize::MAX);
        }
        let mut best = (p, f64::INFINITY, usize::MAX);
        self.closest_node(0, p, &mut best);
        (best.0, best.1.sqrt(), best.2)
    }

    fn closest_node(&self, idx: usize, p: [f64; 3], best: &mut ([f64; 3], f64, usize)) {
        let n = &self.nodes[idx];
        if n.bbox.dist2(p) >= best.1 {
            return;
        }
        if n.left < 0 {
            for i in n.start..n.start + n.count {
                let tri = self.order[i as usize] as usize;
                let (q, d2) = point_tri_closest(p, &self.tris[tri]);
                if d2 < best.1 {
                    best.0 = q;
                    best.1 = d2;
                    best.2 = tri;
                }
            }
            return;
        }
        let dl = self.nodes[n.left as usize].bbox.dist2(p);
        let dr = self.nodes[n.right as usize].bbox.dist2(p);
        if dl <= dr {
            self.closest_node(n.left as usize, p, best);
            self.closest_node(n.right as usize, p, best);
        } else {
            self.closest_node(n.right as usize, p, best);
            self.closest_node(n.left as usize, p, best);
        }
    }

    fn dist_node(&self, idx: usize, p: [f64; 3], best: &mut f64) {
        let n = &self.nodes[idx];
        if n.bbox.dist2(p) >= *best {
            return;
        }
        if n.left < 0 {
            for i in n.start..n.start + n.count {
                let t = &self.tris[self.order[i as usize] as usize];
                let d2 = point_tri_dist2(p, t);
                if d2 < *best {
                    *best = d2;
                }
            }
            return;
        }
        let dl = self.nodes[n.left as usize].bbox.dist2(p);
        let dr = self.nodes[n.right as usize].bbox.dist2(p);
        if dl <= dr {
            self.dist_node(n.left as usize, p, best);
            self.dist_node(n.right as usize, p, best);
        } else {
            self.dist_node(n.right as usize, p, best);
            self.dist_node(n.left as usize, p, best);
        }
    }

    /// Winding number on a regular grid, C order `[i + nx*(j + ny*k)]`.
    pub fn winding_grid(&self, origin: [f64; 3], voxel: f64, dims: [usize; 3]) -> Vec<f32> {
        let slice = dims[0] * dims[1];
        let mut out = vec![0.0f32; slice * dims[2]];
        out.par_chunks_mut(slice)
            .enumerate()
            .for_each(|(k, chunk)| {
                let z = origin[2] + voxel * k as f64;
                for j in 0..dims[1] {
                    let y = origin[1] + voxel * j as f64;
                    let row = j * dims[0];
                    for i in 0..dims[0] {
                        let x = origin[0] + voxel * i as f64;
                        chunk[row + i] = self.winding_at([x, y, z]) as f32;
                    }
                }
            });
        out
    }

    /// Unsigned distance on a regular grid, C order `[i + nx*(j + ny*k)]`.
    pub fn unsigned_grid(&self, origin: [f64; 3], voxel: f64, dims: [usize; 3]) -> Vec<f32> {
        let slice = dims[0] * dims[1];
        let mut out = vec![0.0f32; slice * dims[2]];
        out.par_chunks_mut(slice)
            .enumerate()
            .for_each(|(k, chunk)| {
                let z = origin[2] + voxel * k as f64;
                for j in 0..dims[1] {
                    let y = origin[1] + voxel * j as f64;
                    let row = j * dims[0];
                    for i in 0..dims[0] {
                        let x = origin[0] + voxel * i as f64;
                        chunk[row + i] = self.unsigned_distance_at([x, y, z]) as f32;
                    }
                }
            });
        out
    }

    /// Winding, unsigned and signed distance grids in one pass.
    ///
    /// `signed[i] = if winding[i] > 0.5 { -unsigned[i] } else { unsigned[i] }`.
    pub fn sdf_grid(
        &self,
        origin: [f64; 3],
        voxel: f64,
        dims: [usize; 3],
    ) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let slice = dims[0] * dims[1];
        let n = slice * dims[2];
        let mut winding = vec![0.0f32; n];
        let mut unsigned = vec![0.0f32; n];
        let mut signed = vec![0.0f32; n];
        winding
            .par_chunks_mut(slice)
            .zip(unsigned.par_chunks_mut(slice))
            .zip(signed.par_chunks_mut(slice))
            .enumerate()
            .for_each(|(k, ((wch, uch), sch))| {
                let z = origin[2] + voxel * k as f64;
                for j in 0..dims[1] {
                    let y = origin[1] + voxel * j as f64;
                    let row = j * dims[0];
                    for i in 0..dims[0] {
                        let x = origin[0] + voxel * i as f64;
                        let p = [x, y, z];
                        let w = self.winding_at(p) as f32;
                        let u = self.unsigned_distance_at(p) as f32;
                        wch[row + i] = w;
                        uch[row + i] = u;
                        sch[row + i] = if w > 0.5 { -u } else { u };
                    }
                }
            });
        (winding, unsigned, signed)
    }

    /// Cast a ray against the soup, returning the oriented and raw crossing
    /// counts.  `d` must be normalised; `t_max` bounds the ray (use at least
    /// the bbox diagonal).  Hits closer together than `merge_eps` along the ray
    /// are treated as one (shared-edge duplicates from adjacent triangles);
    /// when their signs disagree the ray is flagged `suspect`.
    ///
    /// Unlike `winding_at`, this never uses the far-field dipole shortcut: a
    /// ray needs the exact set of real crossings.
    pub fn intersect_ray(
        &self,
        o: [f64; 3],
        d: [f64; 3],
        t_max: f64,
        merge_eps: f64,
    ) -> RayQuery {
        let mut q = RayQuery {
            net: 0,
            crossings: 0,
            nearest_t: f64::INFINITY,
            suspect: false,
        };
        if self.nodes.is_empty() {
            return q;
        }
        let inv_d = [1.0 / d[0], 1.0 / d[1], 1.0 / d[2]];
        let mut hb = HitBuf::new();
        self.ray_node(0, o, d, &inv_d, t_max, &mut hb);

        // Insertion sort by `t` (hit counts are small).
        for i in 1..hb.n {
            let t = hb.t[i];
            let s = hb.s[i];
            let mut j = i;
            while j > 0 && hb.t[j - 1] > t {
                hb.t[j] = hb.t[j - 1];
                hb.s[j] = hb.s[j - 1];
                j -= 1;
            }
            hb.t[j] = t;
            hb.s[j] = s;
        }

        let mut last_t = f64::NEG_INFINITY;
        let mut last_sign: i8 = 0;
        for i in 0..hb.n {
            if hb.t[i] - last_t <= merge_eps {
                if hb.s[i] != last_sign {
                    q.suspect = true;
                }
                continue;
            }
            q.crossings += 1;
            q.net += hb.s[i] as i32;
            if hb.t[i] < q.nearest_t {
                q.nearest_t = hb.t[i];
            }
            last_t = hb.t[i];
            last_sign = hb.s[i];
        }
        q.net += hb.overflow_net;
        q.crossings += hb.overflow_count;
        if hb.suspect {
            q.suspect = true;
        }
        q
    }

    fn ray_node(
        &self,
        idx: usize,
        o: [f64; 3],
        d: [f64; 3],
        inv_d: &[f64; 3],
        t_max: f64,
        hb: &mut HitBuf,
    ) {
        let n = &self.nodes[idx];
        if !n.bbox.ray_hits(o, *inv_d, t_max) {
            return;
        }
        if n.left < 0 {
            for i in n.start..n.start + n.count {
                let tri = &self.tris[self.order[i as usize] as usize];
                match ray_tri_hit(o, d, tri, t_max) {
                    TriHit::Hit { t, sign } => hb.push(t, sign),
                    TriHit::Graze => hb.graze(),
                    TriHit::Miss => {}
                }
            }
            return;
        }
        self.ray_node(n.left as usize, o, d, inv_d, t_max, hb);
        self.ray_node(n.right as usize, o, d, inv_d, t_max, hb);
    }
}

fn build_rec(
    tris: &[Tri],
    centroids: &[[f64; 3]],
    order: &mut [u32],
    start: usize,
    end: usize,
    depth: usize,
    nodes: &mut Vec<Node>,
) -> i32 {
    let idx = nodes.len() as i32;
    nodes.push(Node {
        bbox: Aabb::empty(),
        left: -1,
        right: -1,
        start: start as u32,
        count: (end - start) as u32,
        center: [0.0; 3],
        vec_area: [0.0; 3],
        area_sum: 0.0,
    });
    let mut bbox = Aabb::empty();
    let mut vec_area = [0.0f64; 3];
    let mut area_sum = 0.0f64;
    let mut csum = [0.0f64; 3];
    for &o in &order[start..end] {
        let t = &tris[o as usize];
        bbox.expand_point(t.a);
        bbox.expand_point(t.b);
        bbox.expand_point(t.c);
        let av = t.area_vector();
        let a = t.area();
        vec_area = add(vec_area, av);
        area_sum += a;
        csum = add(csum, scale(centroids[o as usize], a));
    }
    let center = if area_sum > 0.0 {
        scale(csum, 1.0 / area_sum)
    } else {
        bbox.center()
    };
    nodes[idx as usize].bbox = bbox;
    nodes[idx as usize].center = center;
    nodes[idx as usize].vec_area = vec_area;
    nodes[idx as usize].area_sum = area_sum;

    if end - start <= LEAF_SIZE || depth >= MAX_DEPTH {
        return idx;
    }
    let axis = bbox.longest_axis();
    let mid = start + (end - start) / 2;
    order[start..end].select_nth_unstable_by(mid - start, |&a, &b| {
        centroids[a as usize][axis]
            .partial_cmp(&centroids[b as usize][axis])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let l = build_rec(tris, centroids, order, start, mid, depth + 1, nodes);
    let r = build_rec(tris, centroids, order, mid, end, depth + 1, nodes);
    nodes[idx as usize].left = l;
    nodes[idx as usize].right = r;
    nodes[idx as usize].start = 0;
    nodes[idx as usize].count = 0;
    idx
}

/// Squared distance from `p` to triangle `t` (Ericson, Real-Time Collision
/// Detection, closest-point-on-triangle).
fn point_tri_dist2(p: [f64; 3], t: &Tri) -> f64 {
    point_tri_closest(p, t).1
}

/// Closest point on triangle `t` to `p`: `(point, squared distance)`.
fn point_tri_closest(p: [f64; 3], t: &Tri) -> ([f64; 3], f64) {
    let ab = sub(t.b, t.a);
    let ac = sub(t.c, t.a);
    let ap = sub(p, t.a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (t.a, dot(ap, ap));
    }
    let bp = sub(p, t.b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (t.b, dot(bp, bp));
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        let q = add(t.a, scale(ab, v));
        let r = sub(p, q);
        return (q, dot(r, r));
    }
    let cp = sub(p, t.c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (t.c, dot(cp, cp));
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        let q = add(t.a, scale(ac, w));
        let r = sub(p, q);
        return (q, dot(r, r));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let q = add(t.b, scale(sub(t.c, t.b), w));
        let r = sub(p, q);
        return (q, dot(r, r));
    }
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    let q = add(t.a, add(scale(ab, v), scale(ac, w)));
    let r = sub(p, q);
    (q, dot(r, r))
}

/// Number of grid voxels covered by `dims`, with saturating multiply so the
/// caller can compare it against a budget without overflow.
pub fn voxel_count(dims: [usize; 3]) -> u64 {
    (dims[0] as u64)
        .saturating_mul(dims[1] as u64)
        .saturating_mul(dims[2] as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn winding_inside_cube_is_one() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        let w = bvh.winding_at([0.5, 0.5, 0.5]);
        assert!((w - 1.0).abs() < 1e-2, "inside winding {w}");
    }

    #[test]
    fn winding_outside_cube_is_zero() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        let w = bvh.winding_at([3.0, 0.5, 0.5]);
        assert!(w.abs() < 1e-2, "outside winding {w}");
    }

    #[test]
    fn unsigned_distance_cube() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        // On the surface.
        assert!(bvh.unsigned_distance_at([0.5, 0.5, 0.0]) < 1e-9);
        // Outside along x by 0.5.
        let d = bvh.unsigned_distance_at([1.5, 0.5, 0.5]);
        assert!((d - 0.5).abs() < 1e-6, "distance {d}");
        // Inside: distance to the nearest face is 0.5.
        let d = bvh.unsigned_distance_at([0.5, 0.5, 0.5]);
        assert!((d - 0.5).abs() < 1e-6, "inside distance {d}");
    }

    #[test]
    fn ray_bounds_and_escape() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        let b = bvh.bounds();
        assert!((b.min[0] - 0.0).abs() < 1e-12 && (b.max[2] - 1.0).abs() < 1e-12);
        // A ray leaving the cube without hitting its surface escapes.
        let q = bvh.intersect_ray([0.5, 0.5, 0.5], [0.0, 0.0, 1.0], 10.0, 0.0);
        assert_eq!(q.crossings, 1, "one exit crossing");
        // A ray that misses the cube entirely.
        let q = bvh.intersect_ray([5.0, 5.0, 5.0], [0.0, 0.0, 1.0], 10.0, 0.0);
        assert_eq!(q.crossings, 0);
        assert!(q.nearest_t.is_infinite());
    }

    #[test]
    fn ray_net_inside_and_outside_cube() {
        let (v, f) = cube();
        let bvh = MeshBvh::from_arrays(&v, &f);
        // Interior: outgoing rays net +1.
        for d in [
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.577, 0.577, 0.577],
        ] {
            let q = bvh.intersect_ray([0.5, 0.5, 0.5], d, 10.0, 0.0);
            assert_eq!(q.net, 1, "dir {d:?} net {}", q.net);
            assert_eq!(q.crossings, 1);
        }
        // Exterior with the ray passing right through: entry + exit cancel.
        let q = bvh.intersect_ray([-1.0, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0, 0.0);
        assert_eq!(q.net, 0, "through-ray net {}", q.net);
        assert_eq!(q.crossings, 2);
    }

    #[test]
    fn ray_through_shared_edge_counts_once() {
        // Two triangles sharing the diagonal (0,0,0)-(1,1,0) of a unit square.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let tris = vec![[0, 1, 2], [0, 2, 3]];
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        // Ray straight through the shared-edge midpoint.
        let q = bvh.intersect_ray([0.5, 0.5, -1.0], [0.0, 0.0, 1.0], 10.0, 1e-9);
        assert_eq!(q.crossings, 1, "shared edge double-counted: {:?}", q);
    }

    #[test]
    fn ray_grazing_is_flagged_not_counted() {
        // A single z=0 triangle; a ray lying in its plane must be flagged as a
        // graze and contribute no crossing.
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0, 1, 2]];
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        let q = bvh.intersect_ray([0.2, 0.2, 0.0], [1.0, 0.0, 0.0], 10.0, 0.0);
        assert!(q.suspect, "grazing ray not flagged: {q:?}");
        assert_eq!(q.crossings, 0);
    }

    #[test]
    fn ray_orientation_inconsistency_visible_as_negative_net() {
        // A single triangle with a ray crossing it from the back: net -1.
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let tris = vec![[0, 1, 2]];
        let bvh = MeshBvh::from_arrays(&verts, &tris);
        // Triangle normal is +z; ray travelling -z hits its back -> sign -1.
        let q = bvh.intersect_ray([0.25, 0.25, 1.0], [0.0, 0.0, -1.0], 10.0, 0.0);
        assert_eq!(q.net, -1);
        assert_eq!(q.crossings, 1);
    }
}
