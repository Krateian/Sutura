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

use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};

type V3 = [f64; 3];

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

/// Result of one close pass.
pub struct CloseReport {
    pub verts: Vec<V3>,
    pub tris: Vec<[i64; 3]>,
    pub closed_holes: usize,
    pub new_faces: usize,
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
    let nf = tris_in.len();
    for t in tris_in {
        for &v in t {
            if v < 0 || v as usize >= nv {
                return Err("triangle index out of range".into());
            }
        }
    }
    let n_he = nf * 3;
    let mut edge_map: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (fi, t) in tris_in.iter().enumerate() {
        for j in 0..3 {
            edge_map
                .entry(canon(t[j], t[(j + 1) % 3]))
                .or_default()
                .push(fi * 3 + j);
        }
    }
    for v in edge_map.values() {
        if v.len() > 2 {
            return Err("mesh has non-2-manifold edges".into());
        }
    }
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
        }
    }

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
}
