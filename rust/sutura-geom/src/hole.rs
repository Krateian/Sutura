//! hole — native port of the VCG / PyMeshLab `meshing_close_holes` filter
//! (Faz 4A of the PyMeshLab-removal roadmap).
//!
//! The filter fills the boundary loops of a triangle mesh with a
//! minimum-weight ear-clipping triangulation, in the spirit of
//! `vcg::tri::Hole` / `MinimumWeightEar` / `SelfIntersectionEar`.  The port is
//! clean-room: the VCG sources were used as an algorithmic reference only
//! (PolyForm Noncommercial forbids copying GPL code), so the outputs are
//! quality-compatible, not byte-identical.
//!
//! User-visible contract of the filter:
//! * the original vertices are preserved; `refinehole=False` adds no vertex;
//! * the new triangles are appended after the input faces;
//! * a loop is closed only when its edge count is `<= max_hole_size`;
//! * with `self_intersection=true` a candidate ear is rejected when it would
//!   intersect the existing surface (adjacency-ring test).
//!
//! `refinehole=True` (the Liepa subdivision) is deliberately **not** ported in
//! this stage; the Python layer keeps those call sites on PyMeshLab.

use numpy::ndarray::Array2;
use numpy::{AllowTypeChange, PyArray2, PyArrayLike2};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::predicates2d::{orient2d_sign, Point2};
use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};

pub(crate) type V3 = [f64; 3];

#[inline]
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

#[inline]
fn normalize(a: V3) -> V3 {
    let n = norm(a);
    if n > 0.0 && n.is_finite() {
        [a[0] / n, a[1] / n, a[2] / n]
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[inline]
fn dist2(a: V3, b: V3) -> f64 {
    let d = sub(a, b);
    dot(d, d)
}

#[inline]
fn canon(a: i64, b: i64) -> (i64, i64) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

#[inline]
fn face_normal(a: V3, b: V3, c: V3) -> V3 {
    normalize(cross(sub(b, a), sub(c, a)))
}

/// Angle between two (normalised) vectors, clamped to `[0, pi]`.
fn angle_between(u: V3, v: V3) -> f64 {
    dot(u, v).clamp(-1.0, 1.0).acos()
}

/// Ear-priority record over the boundary linked list.
#[derive(Clone, Copy)]
struct Ear {
    /// Boundary node whose `tail` is the vertex this ear removes.
    node: usize,
    /// Boundary node preceding `node`.
    prev: usize,
    /// The ear vertex (`node.tail`).
    ear_v: i64,
    weight: f64,
    concave: bool,
    seq: u64,
}

impl PartialEq for Ear {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ear {}

impl Ord for Ear {
    fn cmp(&self, other: &Self) -> Ordering {
        // Non-concave ears are preferred over concave ones.
        match (self.concave, other.concave) {
            (false, true) => return Ordering::Greater,
            (true, false) => return Ordering::Less,
            _ => {}
        }
        match self.weight.partial_cmp(&other.weight) {
            Some(Ordering::Greater) => return Ordering::Greater,
            Some(Ordering::Less) => return Ordering::Less,
            _ => {}
        }
        // Deterministic tie-break: smaller ear vertex first, then older seq.
        other.ear_v.cmp(&self.ear_v).then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Ear {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy)]
struct Node {
    tail: i64,
    head: i64,
    /// Normal of the face currently adjacent across this boundary edge.
    owner: V3,
    prev: usize,
    next: usize,
    alive: bool,
}

/// Proper interior triangle/triangle crossing (a real segment), the same
/// criterion the rest of the crate uses for self-intersections
/// (`tamponade::local_si_subset`, `self_intersections_subset_core`).  Mere
/// vertex/edge contact and coplanar contact are **not** crossings: treating
/// those as one would reject almost every ear on a dense mesh, because the
/// adjacency ring always touches the candidate.  Faces that share a vertex
/// with the candidate are therefore handled by their proper crossing alone.
fn proper_tri_tri_intersect(a: [V3; 3], b: [V3; 3]) -> bool {
    for p in a.iter().chain(b.iter()) {
        if !p.iter().all(|x| x.is_finite()) {
            return true; // conservative on non-finite input
        }
    }
    let ta = Triangle::explicit(a[0], a[1], a[2]);
    let tb = Triangle::explicit(b[0], b[1], b[2]);
    matches!(
        intersect_triangles(&ta, &tb),
        TriangleIntersection::ProperSegment { .. }
    )
}

/// A boundary loop is capped in its own best-fit plane when it is planar
/// within this relative tolerance (max out-of-plane distance / loop extent).
/// Measured on the open shells of `thingi10k_1038439` (the motivating case):
/// the fold boundary loops read `~3.5e-6`, three orders of magnitude below.
const PLANAR_TOL: f64 = 1e-4;

/// Loops longer than this fall back to the ear-clipper: the O(k^2) planar
/// validation is not worth it for very large loops.
const PLANAR_MAX_LOOP: usize = 1024;

/// Number of start vertices tried by the in-plane planar cap.  Greedy
/// ear-clipping is start-order sensitive: a cap that crosses the body from one
/// start can be traced without any crossing from another, so the search retries
/// deterministically from successive start vertices (up to this many, or the
/// loop length) and keeps the first triangulation the crossing check accepts.
/// Measured on `thingi10k_1038439`'s two open 85-gons (starts 0 and 3), far
/// inside this bound.
const PLANAR_MAX_STARTS: usize = 128;

/// Best-fit plane of a loop: `(centroid, unit normal, extent, max out-of-plane
/// distance)`.  The normal is the Newell normal (translation invariant); the
/// extent is the loop's bounding-box diagonal.  `None` for a degenerate loop.
fn loop_plane(pts: &[V3]) -> Option<(V3, V3, f64, f64)> {
    let k = pts.len();
    if k < 3 {
        return None;
    }
    let mut c = [0.0f64; 3];
    for p in pts {
        c[0] += p[0];
        c[1] += p[1];
        c[2] += p[2];
    }
    c[0] /= k as f64;
    c[1] /= k as f64;
    c[2] /= k as f64;
    let mut n = [0.0f64; 3];
    for i in 0..k {
        let a = pts[i];
        let b = pts[(i + 1) % k];
        n[0] += a[1] * b[2] - a[2] * b[1];
        n[1] += a[2] * b[0] - a[0] * b[2];
        n[2] += a[0] * b[1] - a[1] * b[0];
    }
    let nl = norm(n);
    if !(nl > 0.0) || !nl.is_finite() {
        return None;
    }
    let n = [n[0] / nl, n[1] / nl, n[2] / nl];
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for p in pts {
        for d in 0..3 {
            if p[d] < lo[d] {
                lo[d] = p[d];
            }
            if p[d] > hi[d] {
                hi[d] = p[d];
            }
        }
    }
    let extent = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt();
    if !(extent > 0.0) || !extent.is_finite() {
        return None;
    }
    let mut maxd = 0.0f64;
    for p in pts {
        let d = dot(sub(*p, c), n).abs();
        if d > maxd {
            maxd = d;
        }
    }
    Some((c, n, extent, maxd))
}

/// Right-handed orthonormal 2D basis `(u, v)` of the plane with normal `n`
/// (`u x v == n`).
fn plane_basis(n: V3) -> (V3, V3) {
    let ax = if n[0].abs() <= n[1].abs() && n[0].abs() <= n[2].abs() {
        [1.0, 0.0, 0.0]
    } else if n[1].abs() <= n[2].abs() {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let u = normalize(cross(n, ax));
    let v = cross(n, u);
    (u, v)
}

/// Signed area of a 2D polygon (positive == counter-clockwise).
fn signed_area_2d(uv: &[[f64; 2]]) -> f64 {
    let k = uv.len();
    let mut s = 0.0;
    for i in 0..k {
        let a = uv[i];
        let b = uv[(i + 1) % k];
        s += a[0] * b[1] - b[0] * a[1];
    }
    0.5 * s
}

/// Exact 2D segment orientation predicate (`robust` adaptive arithmetic), used
/// for the planar-cap triangulation so near-degenerate seam geometry cannot
/// stall the ear-clipper.
fn ori2(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    orient2d_sign(
        &Point2::Explicit(a),
        &Point2::Explicit(b),
        &Point2::Explicit(c),
    )
}

/// Do two 2D segments cross (proper interior intersection or a collinear
/// touch/overlap)?  Endpoint-only touching where the contact is a shared
/// polygon vertex is filtered by the caller; here any contact counts.
fn seg_proper_intersect(p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], p4: [f64; 2]) -> bool {
    let d1 = ori2(p3, p4, p1);
    let d2 = ori2(p3, p4, p2);
    let d3 = ori2(p1, p2, p3);
    let d4 = ori2(p1, p2, p4);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    let on = |a: [f64; 2], b: [f64; 2], c: [f64; 2], d: f64| {
        d == 0.0
            && c[0] >= a[0].min(b[0])
            && c[0] <= a[0].max(b[0])
            && c[1] >= a[1].min(b[1])
            && c[1] <= a[1].max(b[1])
    };
    on(p3, p4, p1, d1) || on(p3, p4, p2, d2) || on(p1, p2, p3, d3) || on(p1, p2, p4, d4)
}

/// Is the projected polygon simple (no non-adjacent edge crossings)?
fn polygon_simple(uv: &[[f64; 2]]) -> bool {
    let k = uv.len();
    for i in 0..k {
        let a1 = uv[i];
        let a2 = uv[(i + 1) % k];
        for j in (i + 1)..k {
            if j == i + 1 || (i == 0 && j == k - 1) {
                continue; // adjacent edges share a vertex
            }
            if seg_proper_intersect(a1, a2, uv[j], uv[(j + 1) % k]) {
                return false;
            }
        }
    }
    true
}

/// Greedy ear-clipping of a CCW simple polygon (indices into `uv`), returned as
/// counter-clockwise index triples.  `None` when the loop stalls (numerically
/// degenerate input).  Greedy clipping is start-order sensitive, so the caller
/// retries from several start vertices and keeps the first cap that survives its
/// crossing check.
fn earcut_ccw(uv: &[[f64; 2]], order: &mut Vec<usize>) -> Option<Vec<(usize, usize, usize)>> {
    let n = order.len();
    if n < 3 {
        return None;
    }
    let mut out: Vec<(usize, usize, usize)> = Vec::with_capacity(n.saturating_sub(2));
    let guard_cap = n * n + 16;
    let mut guard = 0usize;
    while order.len() > 3 {
        let m = order.len();
        let mut found = false;
        for j in 0..m {
            let i0 = order[(j + m - 1) % m];
            let i1 = order[j];
            let i2 = order[(j + 1) % m];
            let a = uv[i0];
            let b = uv[i1];
            let c = uv[i2];
            if ori2(a, b, c) <= 0.0 {
                continue; // reflex or degenerate corner
            }
            let mut inside = false;
            for &v in order.iter() {
                if v == i0 || v == i1 || v == i2 {
                    continue;
                }
                let p = uv[v];
                let d1 = ori2(a, b, p);
                let d2 = ori2(b, c, p);
                let d3 = ori2(c, a, p);
                let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
                let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
                if !(neg && pos) {
                    inside = true;
                    break;
                }
            }
            if inside {
                continue;
            }
            out.push((i0, i1, i2));
            order.remove(j);
            found = true;
            break;
        }
        guard += 1;
        if !found || guard > guard_cap {
            return None;
        }
    }
    out.push((order[0], order[1], order[2]));
    if out.len() != n - 2 {
        return None;
    }
    Some(out)
}

/// True when original face `f` makes cap/ear triangle `t` invalid: a duplicate
/// face (all three vertices shared), a face sharing a non-cap edge (a chord),
/// or a face that properly crosses `t` (the same proper-crossing definition the
/// self-intersection metric uses).  `border` holds the edges of `t` that are
/// legitimate continuations of the surface.
fn face_blocks(
    verts: &[V3],
    tris_in: &[[i64; 3]],
    border: &HashSet<(i64, i64)>,
    f: u32,
    t: [i64; 3],
) -> bool {
    let ft = tris_in[f as usize];
    let mut shared = 0usize;
    for &x in &t {
        if ft.contains(&x) {
            shared += 1;
        }
    }
    match shared {
        3 => true,
        2 => {
            let mut e = [0i64; 2];
            let mut k = 0;
            for &x in &t {
                if ft.contains(&x) {
                    e[k] = x;
                    k += 1;
                }
            }
            !border.contains(&canon(e[0], e[1]))
        }
        _ => {
            let tv = [
                verts[ft[0] as usize],
                verts[ft[1] as usize],
                verts[ft[2] as usize],
            ];
            let ta = Triangle::explicit(verts[t[0] as usize], verts[t[1] as usize], verts[t[2] as usize]);
            let tb = Triangle::explicit(tv[0], tv[1], tv[2]);
            matches!(
                intersect_triangles(&ta, &tb),
                TriangleIntersection::ProperSegment { .. }
            )
        }
    }
}

/// Reject an in-plane cap that properly crosses the surrounding surface.  The
/// cap's own edges (including diagonals) are legitimate, so a face sharing two
/// cap vertices is allowed only when that edge belongs to the cap.  Only a
/// `ProperSegment` crossing is rejected -- the same definition the
/// self-intersection metric uses (shared-vertex and coplanar contacts are not
/// counted).  Every candidate face incident to any loop vertex is tested against
/// every cap triangle, not just the faces incident to that triangle's own
/// vertices, because a body face crossing one cap triangle need not be incident
/// to that triangle.
fn cap_ok(verts: &[V3], tris_in: &[[i64; 3]], cand_faces: &[u32], cap: &[[i64; 3]]) -> bool {
    let mut border: HashSet<(i64, i64)> = HashSet::new();
    for t in cap {
        border.insert(canon(t[0], t[1]));
        border.insert(canon(t[1], t[2]));
        border.insert(canon(t[2], t[0]));
    }
    for t in cap {
        for &f in cand_faces {
            if face_blocks(verts, tris_in, &border, f, *t) {
                return false;
            }
        }
    }
    true
}

/// Cap a planar + simple boundary loop in its own plane.
///
/// Returns the cap triangles with the boundary-cancelling winding (each loop
/// half-edge appears once, reversed), or `None` when the loop is not planar,
/// not simple (a repeated vertex, i.e. a pinch), too long, or when the in-plane
/// cap would properly cross the surrounding surface -- the caller then keeps the
/// ear-clipper.  This guarantees a flat, non-self-intersecting cap for a
/// genuinely planar boundary such as a scan's fold seam, which the weight-based
/// ear-clipper can otherwise slice through the body.
pub(crate) fn planar_cap(
    verts: &[V3],
    tris_in: &[[i64; 3]],
    _vfaces: &[Vec<u32>],
    loop_v: &[i64],
) -> Option<Vec<[i64; 3]>> {
    let k = loop_v.len();
    if k < 3 || k > PLANAR_MAX_LOOP {
        return None;
    }
    {
        let mut seen: HashSet<i64> = HashSet::with_capacity(k);
        for &v in loop_v {
            if !seen.insert(v) {
                return None; // pinched loop is not simple
            }
        }
    }
    let pts: Vec<V3> = loop_v.iter().map(|&v| verts[v as usize]).collect();
    let (c, n, extent, maxd) = loop_plane(&pts)?;
    if !(maxd <= PLANAR_TOL * extent) {
        return None;
    }
    let (u, vv) = plane_basis(n);
    let uv: Vec<[f64; 2]> = pts
        .iter()
        .map(|p| {
            let d = sub(*p, c);
            [dot(d, u), dot(d, vv)]
        })
        .collect();
    if !polygon_simple(&uv) {
        return None;
    }
    let area = signed_area_2d(&uv);
    if area == 0.0 || !area.is_finite() {
        return None;
    }
    // Work in a CCW copy; track whether the working order is the loop or its
    // reverse so the final winding cancels the boundary half-edges.
    let reversed = area < 0.0;
    let mut base: Vec<usize> = (0..k).collect();
    if reversed {
        base.reverse();
    }
    // Any face whose bbox overlaps the loop's bbox is a candidate the cap could
    // cross.  A body face that properly crosses a cap triangle need not be
    // incident to any loop vertex (and may share no vertex with the cap triangle
    // it pierces), so the candidate set is bbox-filtered over ALL faces, not just
    // those incident to loop vertices.
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &pts {
        for d in 0..3 {
            lo[d] = lo[d].min(p[d]);
            hi[d] = hi[d].max(p[d]);
        }
    }
    let cand: Vec<u32> = tris_in
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let a = verts[t[0] as usize];
            let b = verts[t[1] as usize];
            let c = verts[t[2] as usize];
            (0..3).all(|d| {
                a[d].max(b[d]).max(c[d]) >= lo[d] && a[d].min(b[d]).min(c[d]) <= hi[d]
            })
        })
        .map(|(i, _)| i as u32)
        .collect();
    // Greedy ear-clipping is start-order sensitive: the same polygon capped from
    // a different start vertex can trace a different diagonal set, one that does
    // not slice the folded body.  Retry deterministically from successive starts
    // and keep the first cap the crossing check accepts.
    let starts = k.min(PLANAR_MAX_STARTS);
    for s in 0..starts {
        let mut order: Vec<usize> = base[s..].iter().chain(&base[..s]).copied().collect();
        let tris2 = match earcut_ccw(&uv, &mut order) {
            Some(t) => t,
            None => continue,
        };
        let mut cap: Vec<[i64; 3]> = Vec::with_capacity(tris2.len());
        for (a, b, cc) in tris2 {
            let (va, vb, vc) = (loop_v[a], loop_v[b], loop_v[cc]);
            cap.push(if reversed { [va, vb, vc] } else { [va, vc, vb] });
        }
        if cap_ok(verts, tris_in, &cand, &cap) {
            return Some(cap);
        }
    }
    None
}
/// Result of one close pass.
pub struct CloseReport {
    pub verts: Vec<V3>,
    pub tris: Vec<[i64; 3]>,
    pub closed_holes: usize,
    pub new_faces: usize,
}

/// Boundary half-edge topology shared by the hole closer and diagnosis.
///
/// `loops` holds half-edge indices (`face * 3 + edge`) in boundary order.
/// `incomplete` is set when a walk could not close (an edge with more than
/// two uses, or a repeated half-edge), which the diagnosis loops channel
/// reports as an estimate.
pub(crate) struct BoundaryTopology {
    pub twin: Vec<Option<usize>>,
    pub edges: HashSet<(i64, i64)>,
    pub loops: Vec<Vec<usize>>,
    pub boundary_edges: usize,
    pub incomplete: bool,
}

/// Walk the boundary half-edge loops of `tris_in`.
///
/// With `reject_non_manifold` the function fails on any edge used more than
/// twice, which is the `close_holes_core` contract; diagnosis passes `false`
/// so it can still describe the loops it finds and flag the result as
/// incomplete.
pub(crate) fn boundary_topology(
    tris_in: &[[i64; 3]],
    reject_non_manifold: bool,
) -> Result<BoundaryTopology, String> {
    let n_he = tris_in.len() * 3;
    let mut edge_map: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, t) in tris_in.iter().enumerate() {
        for j in 0..3 {
            edge_map
                .entry(canon(t[j], t[(j + 1) % 3]))
                .or_default()
                .push(fi * 3 + j);
        }
    }
    if reject_non_manifold {
        for v in edge_map.values() {
            if v.len() > 2 {
                return Err("mesh has non-2-manifold edges".into());
            }
        }
    }
    let boundary_edges = edge_map.values().filter(|v| v.len() == 1).count();
    let mut twin: Vec<Option<usize>> = vec![None; n_he];
    for v in edge_map.values() {
        if v.len() == 2 {
            twin[v[0]] = Some(v[1]);
            twin[v[1]] = Some(v[0]);
        }
    }
    let edges: HashSet<(i64, i64)> = edge_map.keys().cloned().collect();

    // Walk from a boundary half-edge to the next boundary half-edge of the
    // same loop: step to the next edge of the face, cross its twin while the
    // edge is interior, and repeat.
    let next_border = |he: usize| -> Option<usize> {
        let f = he / 3;
        let j = he % 3;
        let mut e = f * 3 + (j + 1) % 3;
        let mut guard = 0usize;
        loop {
            match twin[e] {
                None => return Some(e),
                Some(tw) => {
                    e = (tw / 3) * 3 + (tw % 3 + 1) % 3;
                }
            }
            guard += 1;
            if guard > n_he + 3 {
                return None;
            }
        }
    };

    let mut visited = vec![false; n_he];
    let mut loops: Vec<Vec<usize>> = Vec::new();
    let mut incomplete = false;
    for he in 0..n_he {
        if twin[he].is_some() || visited[he] {
            continue;
        }
        let start = he;
        let mut cur = start;
        let mut seq: Vec<usize> = Vec::new();
        let mut ok = true;
        loop {
            visited[cur] = true;
            seq.push(cur);
            match next_border(cur) {
                Some(nx) => cur = nx,
                None => {
                    ok = false;
                    break;
                }
            }
            if cur == start {
                break;
            }
            if visited[cur] {
                ok = false;
                break;
            }
        }
        if ok && seq.len() >= 3 {
            loops.push(seq);
        } else if !ok {
            incomplete = true;
        }
    }
    Ok(BoundaryTopology {
        twin,
        edges,
        loops,
        boundary_edges,
        incomplete,
    })
}

/// Build the vertex -> incident-face incidence for the original faces.
fn vertex_faces(nv: usize, tris: &[[i64; 3]]) -> Vec<Vec<u32>> {
    let mut out = vec![Vec::new(); nv];
    for (fi, t) in tris.iter().enumerate() {
        for &v in t {
            if v >= 0 && (v as usize) < nv {
                out[v as usize].push(fi as u32);
            }
        }
    }
    out
}

/// Area-weighted vertex normals of the original faces.
fn vertex_normals(verts: &[V3], tris: &[[i64; 3]], nv: usize) -> Vec<V3> {
    let mut acc = vec![[0.0f64; 3]; nv];
    for t in tris {
        let (i0, i1, i2) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let n = cross(sub(verts[i1], verts[i0]), sub(verts[i2], verts[i0]));
        for &i in &[i0, i1, i2] {
            acc[i][0] += n[0];
            acc[i][1] += n[1];
            acc[i][2] += n[2];
        }
    }
    acc.iter().map(|a| normalize(*a)).collect()
}

/// Ear-clipping state for a single boundary loop.
struct Closer<'a> {
    verts: &'a [V3],
    vnorm: &'a [V3],
    orig: &'a [[i64; 3]],
    vfaces: &'a [Vec<u32>],
    edges: HashSet<(i64, i64)>,
    nodes: Vec<Node>,
    alive_count: usize,
    heap: BinaryHeap<Ear>,
    seq: u64,
    added: Vec<[i64; 3]>,
    ring_added: Vec<[i64; 3]>,
    closed_this_hole: bool,
    si: bool,
    /// When set, the ear's self-intersection test is skipped.  VCG's
    /// `selfintersection` flag is best-effort: it prefers a non-crossing ear
    /// but never leaves a closable loop open (measured: PyMeshLab with
    /// `selfintersection=True` still emits crossings but closes every loop).
    /// `close_holes_core` runs the loop once with the guard on and, only when
    /// that stalls, once more with the guard relaxed.
    relax_si: bool,
}

impl<'a> Closer<'a> {
    fn ear_of(&self, node_idx: usize) -> Option<Ear> {
        let node = *self.nodes.get(node_idx)?;
        if !node.alive {
            return None;
        }
        let prev = *self.nodes.get(node.prev)?;
        if !prev.alive {
            return None;
        }
        let ev = node.tail;
        let pt = prev.tail;
        let nh = node.head;
        let pe = self.verts[ev as usize];
        let pp = self.verts[pt as usize];
        let pn = self.verts[nh as usize];
        let a = norm(cross(sub(pp, pe), sub(pn, pe)));
        let b = dist2(pe, pp).max(dist2(pe, pn)).max(dist2(pp, pn));
        let aspect = if a == 0.0 || b == 0.0 { 0.0 } else { a / b };
        let n = face_normal(pe, pp, pn);
        let dih = angle_between(n, prev.owner).max(angle_between(n, node.owner));
        let mut weight = aspect - (dih / std::f64::consts::PI) * 0.1;
        if !weight.is_finite() {
            weight = -1.0e30;
        }
        let mut ang = angle_between(sub(pp, pe), sub(pn, pe));
        if dot(n, self.vnorm[ev as usize]) < 0.0 {
            ang = 2.0 * std::f64::consts::PI - ang;
        }
        Some(Ear {
            node: node_idx,
            prev: node.prev,
            ear_v: ev,
            weight,
            concave: ang > std::f64::consts::PI,
            seq: 0,
        })
    }

    fn push_ear(&mut self, node_idx: usize) {
        if let Some(mut e) = self.ear_of(node_idx) {
            self.seq += 1;
            e.seq = self.seq;
            self.heap.push(e);
        }
    }

    fn fresh(&self, e: &Ear) -> bool {
        let n = self.nodes[e.node];
        let p = self.nodes[e.prev];
        n.alive && p.alive && n.prev == e.prev && p.next == e.node && n.tail == e.ear_v
    }

    /// Faces around the current boundary vertices plus the ears already added
    /// for this hole (the VCG `AdjacencyRing`).
    fn ring(&self) -> Vec<[i64; 3]> {
        let mut out = self.ring_added.clone();
        let mut seen: HashSet<u32> = HashSet::new();
        for i in 0..self.nodes.len() {
            if !self.nodes[i].alive {
                continue;
            }
            let v = self.nodes[i].tail as usize;
            if let Some(fs) = self.vfaces.get(v) {
                for &fi in fs {
                    if seen.insert(fi) {
                        out.push(self.orig[fi as usize]);
                    }
                }
            }
        }
        out
    }

    /// Reject the candidate triangle `cv` when it intersects a ring face.
    /// `border` lists the candidate's edges that are legitimate hole edges
    /// (sharing such an edge with a face is allowed).
    fn ring_ok(&self, cv: [i64; 3], border: &[(i64, i64)]) -> bool {
        let cand = [self.verts[cv[0] as usize], self.verts[cv[1] as usize], self.verts[cv[2] as usize]];
        for f in self.ring() {
            let mut shared: Vec<i64> = Vec::new();
            for &x in &cv {
                if f.contains(&x) {
                    shared.push(x);
                }
            }
            match shared.len() {
                3 => return false,
                2 => {
                    let e = canon(shared[0], shared[1]);
                    if border.iter().any(|&b| b == e) {
                        continue;
                    }
                    return false;
                }
                _ => {
                    let tv = [
                        self.verts[f[0] as usize],
                        self.verts[f[1] as usize],
                        self.verts[f[2] as usize],
                    ];
                    if proper_tri_tri_intersect(cand, tv) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn is_ear(&self, e: &Ear) -> bool {
        let n = self.nodes[e.node];
        let p = self.nodes[e.prev];
        let ev = n.tail;
        let pt = p.tail;
        let nh = n.head;
        if self.edges.contains(&canon(pt, nh)) {
            return false;
        }
        let pe = self.verts[ev as usize];
        let pp = self.verts[pt as usize];
        let pn = self.verts[nh as usize];
        if norm(face_normal(pe, pp, pn)) == 0.0 {
            return false;
        }
        if !self.si || self.relax_si {
            return true;
        }
        self.ring_ok([ev, pt, nh], &[canon(pt, ev), canon(ev, nh)])
    }

    /// Run the ear-closing loop to completion (or until no ear is available).
    fn run_close(&mut self) -> bool {
        if self.alive_count == 3 {
            return self.emit_final();
        }
        while self.alive_count > 3 {
            let mut closed = false;
            while let Some(e) = self.heap.pop() {
                if !self.fresh(&e) {
                    continue;
                }
                if !self.is_ear(&e) {
                    continue;
                }
                self.close_ear(&e);
                closed = true;
                break;
            }
            if !closed {
                return false;
            }
        }
        self.emit_final()
    }

    fn close_ear(&mut self, e: &Ear) {
        let p_idx = e.prev;
        let n_idx = e.node;
        let node = self.nodes[n_idx];
        let prev = self.nodes[p_idx];
        let ev = node.tail;
        let pt = prev.tail;
        let nh = node.head;
        let q = prev.prev;
        let r = node.next;
        let nf = [ev, pt, nh];
        let owner = face_normal(
            self.verts[ev as usize],
            self.verts[pt as usize],
            self.verts[nh as usize],
        );
        self.added.push(nf);
        self.ring_added.push(nf);
        self.edges.insert(canon(nf[0], nf[1]));
        self.edges.insert(canon(nf[1], nf[2]));
        self.edges.insert(canon(nf[2], nf[0]));
        self.nodes[p_idx].alive = false;
        self.nodes[n_idx].alive = false;
        self.alive_count -= 2;
        let m = self.nodes.len();
        self.nodes.push(Node {
            tail: pt,
            head: nh,
            owner,
            prev: q,
            next: r,
            alive: true,
        });
        self.alive_count += 1;
        self.nodes[q].next = m;
        self.nodes[r].prev = m;
        self.closed_this_hole = true;
        self.push_ear(m);
        self.push_ear(r);
    }

    /// The last three boundary nodes form the final triangle.
    fn emit_final(&mut self) -> bool {
        let a = match (0..self.nodes.len()).find(|&i| self.nodes[i].alive) {
            Some(x) => x,
            None => return false,
        };
        let b = self.nodes[a].next;
        let c = self.nodes[b].next;
        let ev = self.nodes[a].tail;
        let pt = self.nodes[c].tail;
        let nh = self.nodes[a].head;
        let nf = [ev, pt, nh];
        let nrm = face_normal(
            self.verts[ev as usize],
            self.verts[pt as usize],
            self.verts[nh as usize],
        );
        if norm(nrm) == 0.0 {
            return false;
        }
        if self.si
            && !self.relax_si
            && !self.ring_ok(
                nf,
                &[canon(ev, pt), canon(pt, nh), canon(nh, ev)],
            )
        {
            return false;
        }
        self.added.push(nf);
        self.ring_added.push(nf);
        self.closed_this_hole = true;
        true
    }
}

/// Close every boundary loop with at most `max_hole_size` edges.
///
/// Returns the unchanged vertices, the input triangles followed by the new
/// triangles, and the number of loops closed / faces added.
pub fn close_holes_core(
    verts: &[V3],
    tris_in: &[[i64; 3]],
    max_hole_size: i64,
    self_intersection: bool,
) -> Result<CloseReport, String> {
    let nv = verts.len();
    for t in tris_in {
        for &v in t {
            if v < 0 || v as usize >= nv {
                return Err("triangle index out of range".into());
            }
        }
    }
    let axis = boundary_topology(tris_in, true)?;
    let edges = axis.edges;
    let loops = axis.loops;

    let vf = vertex_faces(nv, tris_in);
    let vnrm = vertex_normals(verts, tris_in, nv);

    let mut out_tris: Vec<[i64; 3]> = tris_in.to_vec();
    let mut closed_holes = 0usize;
    let mut new_faces = 0usize;

    for seq in loops {
        let k = seq.len();
        if k < 3 || k as i64 > max_hole_size {
            continue;
        }
        let loop_v: Vec<i64> = seq
            .iter()
            .map(|&he| tris_in[he / 3][he % 3])
            .collect();
        // A genuinely planar, simple loop is capped flat in its own plane: the
        // weight-based ear-clipper can otherwise pick ears that slice through
        // the body (measured on thingi10k_1038439's fold seam), and a flat cap
        // is both self-intersection-free and shape-faithful.
        if let Some(cap) = planar_cap(verts, tris_in, &vf, &loop_v) {
            closed_holes += 1;
            new_faces += cap.len();
            out_tris.extend(cap);
            continue;
        }
        let mut nodes: Vec<Node> = Vec::with_capacity(k);
        for (i, &he) in seq.iter().enumerate() {
            let f = he / 3;
            let tail = tris_in[f][he % 3];
            let head = tris_in[f][(he % 3 + 1) % 3];
            let ft = tris_in[f];
            let owner = face_normal(
                verts[ft[0] as usize],
                verts[ft[1] as usize],
                verts[ft[2] as usize],
            );
            nodes.push(Node {
                tail,
                head,
                owner,
                prev: (i + k - 1) % k,
                next: (i + 1) % k,
                alive: true,
            });
        }
        let mut closer = Closer {
            verts,
            vnorm: &vnrm,
            orig: tris_in,
            vfaces: &vf,
            edges: edges.clone(),
            nodes,
            alive_count: k,
            heap: BinaryHeap::new(),
            seq: 0,
            added: Vec::new(),
            ring_added: Vec::new(),
            closed_this_hole: false,
            si: self_intersection,
            relax_si: false,
        };
        for i in 0..k {
            closer.push_ear(i);
        }
        let mut finished = closer.run_close();
        if !finished && self_intersection {
            // Best-effort fallback: VCG never leaves a size-eligible loop
            // open because of the self-intersection guard alone.
            closer.relax_si = true;
            closer.heap.clear();
            for i in 0..closer.nodes.len() {
                if closer.nodes[i].alive {
                    closer.push_ear(i);
                }
            }
            finished = closer.run_close();
        }
        if finished {
            closed_holes += 1;
            new_faces += closer.added.len();
            out_tris.extend(closer.added.iter().copied());
        }
    }

    Ok(CloseReport {
        verts: verts.to_vec(),
        tris: out_tris,
        closed_holes,
        new_faces,
    })
}

fn read_f64_rows(arr: &PyArrayLike2<'_, f64, AllowTypeChange>) -> PyResult<Vec<V3>> {
    let v = arr.as_array();
    if v.ncols() != 3 {
        return Err(PyValueError::new_err("verts must be Nx3"));
    }
    let n = v.nrows();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push([v[[i, 0]], v[[i, 1]], v[[i, 2]]]);
    }
    Ok(out)
}

fn read_i64_rows(arr: &PyArrayLike2<'_, i64, AllowTypeChange>) -> PyResult<Vec<[i64; 3]>> {
    let t = arr.as_array();
    if t.ncols() != 3 {
        return Err(PyValueError::new_err("tris must be Mx3"));
    }
    let n = t.nrows();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push([t[[i, 0]], t[[i, 1]], t[[i, 2]]]);
    }
    Ok(out)
}

fn rows_to_array2_f64<'py>(py: Python<'py>, rows: &[V3]) -> Bound<'py, PyArray2<f64>> {
    let mut a = Array2::<f64>::zeros((rows.len(), 3));
    for (r, row) in rows.iter().enumerate() {
        a[[r, 0]] = row[0];
        a[[r, 1]] = row[1];
        a[[r, 2]] = row[2];
    }
    PyArray2::from_owned_array(py, a)
}

fn rows_to_array2_i64<'py>(py: Python<'py>, rows: &[[i64; 3]]) -> Bound<'py, PyArray2<i64>> {
    let mut a = Array2::<i64>::zeros((rows.len(), 3));
    for (r, row) in rows.iter().enumerate() {
        a[[r, 0]] = row[0];
        a[[r, 1]] = row[1];
        a[[r, 2]] = row[2];
    }
    PyArray2::from_owned_array(py, a)
}

/// Native `meshing_close_holes` (Faz 4A: `refinehole=False`).
///
/// `verts`/`tris` are the input mesh; `max_hole_size` is the maximum boundary
/// loop length to close; `self_intersection` rejects ears that would cross the
/// surface.  `new_faces_selected` is accepted for PyMeshLab signature parity
/// but has no geometric effect.  Returns `(verts, tris, closed_holes,
/// new_faces)`.
#[pyfunction]
#[pyo3(signature = (verts, tris, max_hole_size=1000, self_intersection=true, new_faces_selected=false))]
pub fn close_holes<'py>(
    py: Python<'py>,
    verts: PyArrayLike2<'py, f64, AllowTypeChange>,
    tris: PyArrayLike2<'py, i64, AllowTypeChange>,
    max_hole_size: i64,
    self_intersection: bool,
    new_faces_selected: bool,
) -> PyResult<(Bound<'py, PyArray2<f64>>, Bound<'py, PyArray2<i64>>, i64, i64)> {
    let _ = new_faces_selected;
    let vrows = read_f64_rows(&verts)?;
    let trows = read_i64_rows(&tris)?;
    let rep = close_holes_core(&vrows, &trows, max_hole_size, self_intersection)
        .map_err(PyValueError::new_err)?;
    Ok((
        rows_to_array2_f64(py, &rep.verts),
        rows_to_array2_i64(py, &rep.tris),
        rep.closed_holes as i64,
        rep.new_faces as i64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tetra_open() -> (Vec<V3>, Vec<[i64; 3]>) {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        // Outward-wound tetrahedron with the (0,2,1) base face removed, so the
        // single triangular hole must be filled by exactly that face again.
        let tris = vec![
            [0, 1, 3],
            [1, 2, 3],
            [2, 0, 3],
        ];
        (verts, tris)
    }

    #[test]
    fn closes_triangular_hole() {
        let (v, t) = tetra_open();
        let rep = close_holes_core(&v, &t, 1000, false).unwrap();
        assert_eq!(rep.closed_holes, 1);
        assert_eq!(rep.new_faces, 1);
        assert_eq!(rep.tris.len(), 4);
        assert_eq!(rep.verts, v);
        let nf = rep.tris[3];
        assert_eq!(nf, [0, 2, 1]);
    }

    #[test]
    fn closes_pentagonal_hole() {
        // Outward-wound unit cube with the top face (4 edges / 2 triangles)
        // removed -> one quad hole, filled by two triangles.
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let tris = vec![
            [0, 3, 2],
            [0, 2, 1],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [0, 4, 7],
            [0, 7, 3],
        ];
        let rep = close_holes_core(&verts, &tris, 1000, false).unwrap();
        assert_eq!(rep.closed_holes, 1);
        assert_eq!(rep.new_faces, 2);
    }

    #[test]
    fn size_filter_skips_large_hole() {
        let (v, t) = tetra_open();
        let rep = close_holes_core(&v, &t, 2, false).unwrap();
        assert_eq!(rep.closed_holes, 0);
        assert_eq!(rep.tris.len(), 3);
    }

    #[test]
    fn clean_mesh_untouched() {
        let (v, t) = tetra_open();
        let mut closed = t.clone();
        closed.push([0, 2, 1]);
        let rep = close_holes_core(&v, &closed, 1000, false).unwrap();
        assert_eq!(rep.closed_holes, 0);
        assert_eq!(rep.new_faces, 0);
        assert_eq!(rep.tris, closed);
    }

    #[test]
    fn rejects_non_manifold_edges() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        // Edge (0,1) used by three faces -> non-manifold.
        let tris = vec![[0, 1, 2], [0, 1, 3], [1, 0, 2]];
        assert!(close_holes_core(&verts, &tris, 1000, false).is_err());
    }

    #[test]
    fn deterministic_output() {
        let (v, t) = tetra_open();
        let a = close_holes_core(&v, &t, 1000, true).unwrap();
        for _ in 0..5 {
            let b = close_holes_core(&v, &t, 1000, true).unwrap();
            assert_eq!(a.tris, b.tris);
        }
    }

    #[test]
    fn planar_cap_triangulates_nonconvex_loop() {
        // L-shaped (non-convex) planar hexagon at z=0, area 5.
        let verts: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [3.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 3.0, 0.0],
            [0.0, 3.0, 0.0],
        ];
        let tris: Vec<[i64; 3]> = vec![];
        let vfaces = vertex_faces(verts.len(), &tris);
        let loop_v: Vec<i64> = (0..6).collect();
        let cap = planar_cap(&verts, &tris, &vfaces, &loop_v).expect("planar cap");
        assert_eq!(cap.len(), 4, "an n-gon needs n-2 triangles");
        let mut area = 0.0;
        for t in &cap {
            let p = [
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            ];
            area += 0.5 * norm(cross(sub(p[1], p[0]), sub(p[2], p[0])));
        }
        assert!((area - 5.0).abs() < 1e-9, "cap area {area} != polygon area");
        // Boundary-cancelling winding: every loop edge appears once, reversed.
        let mut dir: HashMap<(i64, i64), i32> = HashMap::new();
        for t in &cap {
            for j in 0..3 {
                *dir.entry((t[j], t[(j + 1) % 3])).or_insert(0) += 1;
            }
        }
        for i in 0..6 {
            let f = loop_v[i];
            let g = loop_v[(i + 1) % 6];
            assert_eq!(
                dir.get(&(g, f)).copied().unwrap_or(0),
                1,
                "reversed loop edge {g}->{f} missing"
            );
            assert_eq!(
                dir.get(&(f, g)).copied().unwrap_or(0),
                0,
                "forward loop edge {f}->{g} present"
            );
        }
    }

    #[test]
    fn planar_cap_rejects_pinched_loop() {
        let verts: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let tris: Vec<[i64; 3]> = vec![];
        let vfaces = vertex_faces(verts.len(), &tris);
        // Vertex 1 appears twice -> a pinch, not a simple polygon.
        let loop_v = vec![0i64, 1, 2, 1, 3];
        assert!(planar_cap(&verts, &tris, &vfaces, &loop_v).is_none());
    }

    #[test]
    fn planar_cap_rejects_nonplanar_loop() {
        let verts: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.5],
            [0.0, 1.0, 0.0],
        ];
        let tris: Vec<[i64; 3]> = vec![];
        let vfaces = vertex_faces(verts.len(), &tris);
        let loop_v = vec![0i64, 1, 2, 3];
        assert!(planar_cap(&verts, &tris, &vfaces, &loop_v).is_none());
    }

    #[test]
    fn planar_cap_rejects_crossing_body() {
        // Planar rectangle loop in z=0 with a body face passing through it.
        let verts: Vec<V3> = vec![
            [0.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [3.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [2.0, 0.5, 1.0],
            [2.0, 0.5, -1.0],
            [2.6, 0.5, 1.0],
        ];
        // Face [4,5,6] is not incident to any loop vertex; its 4-5 edge pierces
        // the plane at (2.0,0.5,0), which lies inside the rectangle whichever
        // diagonal the cap uses, so every triangulation has a proper crossing.
        let tris: Vec<[i64; 3]> = vec![[4, 5, 6]];
        let vfaces = vertex_faces(verts.len(), &tris);
        let loop_v = vec![0i64, 1, 2, 3];
        assert!(planar_cap(&verts, &tris, &vfaces, &loop_v).is_none());
    }
}
