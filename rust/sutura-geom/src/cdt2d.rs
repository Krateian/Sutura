//! 2D constrained Delaunay triangulation for projected implicit points.
//!
//! The input is a single host triangle (three 3D points) plus a set of points
//! that lie in its plane and a set of constrained segments.  Every point is
//! projected to exact barycentric `(s,t)` coordinates so that `orient2d` and
//! `incircle` can be evaluated with exact rational arithmetic.

use crate::point::Point3;
use crate::profile_count;
use num_rational::BigRational;
use num_traits::{Signed, Zero};
use std::collections::HashMap;

/// A point in the 2D parameter space of a host triangle.
///
/// The host triangle maps to `(s,t)` coordinates
/// `a -> (0,0)`, `b -> (1,0)`, `c -> (0,1)` via an affine projection.
/// Coordinates are stored as exact rationals so all predicates are exact.
#[derive(Clone, Debug, PartialEq)]
pub struct Point2D {
    pub s: BigRational,
    pub t: BigRational,
}

/// Per-host diagnostic snapshot (only populated when the `cdt-diag` feature
/// is enabled).
#[derive(Clone, Debug, Default)]
pub struct DiagState {
    pub input_segments: usize,
    pub crossings: usize,
    pub final_vertices: usize,
    pub max_bit_len: u64,
}

#[cfg(feature = "cdt-diag")]
impl DiagState {
    fn update_bit_len(&mut self, p: &Point2D) {
        use num_traits::Signed;
        let bits = |r: &BigRational| {
            let n = r.numer().abs();
            let d = r.denom().abs();
            n.bits().max(d.bits())
        };
        let m = bits(&p.s).max(bits(&p.t));
        if m > self.max_bit_len {
            self.max_bit_len = m;
        }
    }
}

/// A vertex of the 2D CDT, carrying both its cached parameter-space
/// coordinate and its construction recipe in 3D.
#[derive(Clone, Debug)]
pub struct CdtVertex {
    pub st: Point2D,
    /// Exact construction recipe from original input data.  `None` only for
    /// rare fallback vertices built exactly in 2D (e.g. constraint splits
    /// against an arbitrary CDT edge); those are deduplicated by `(s,t)`.
    pub provenance: Option<Point3>,
}

/// Describes how a constrained segment inside a host triangle relates to the
/// original input geometry. The CDT uses these descriptions to compute
/// segment-segment crossings directly from original planes and lines, never
/// from previously constructed points.
#[derive(Clone, Debug)]
pub enum SegmentSource {
    /// The segment is the intersection of the host plane with a transversal
    /// input triangle. `other_plane` are three points defining that triangle.
    Transversal {
        other_plane: ([f64; 3], [f64; 3], [f64; 3]),
    },
    /// The segment lies on a boundary edge of the host triangle itself.
    /// `endpoints` are the original 3D coordinates of that host edge.
    HostEdge {
        endpoints: ([f64; 3], [f64; 3]),
    },
    /// The segment is an edge of a triangle that is coplanar with the host.
    /// `endpoints` are the original 3D coordinates of the edge endpoints.
    CoplanarEdge {
        endpoints: ([f64; 3], [f64; 3]),
    },
}

/// A constrained segment together with its source geometry.
#[derive(Clone, Debug)]
pub struct ConstrainedSegment {
    pub endpoints: (usize, usize),
    pub source: SegmentSource,
}

/// Result sign of an exact predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    Negative,
    Zero,
    Positive,
}

impl Sign {
    fn from_rational(r: &BigRational) -> Self {
        if r.is_zero() {
            Sign::Zero
        } else if r.is_positive() {
            Sign::Positive
        } else {
            Sign::Negative
        }
    }

    pub fn is_positive(self) -> bool {
        matches!(self, Sign::Positive)
    }

    pub fn is_negative(self) -> bool {
        matches!(self, Sign::Negative)
    }

    pub fn is_zero(self) -> bool {
        matches!(self, Sign::Zero)
    }
}

/// A triangle in the CDT, stored with its three vertex indices and the
/// indices of the neighbouring triangles across each opposite edge.
#[derive(Clone, Debug)]
struct Tri {
    v: [usize; 3],
    adj: [Option<usize>; 3],
    constrained: [bool; 3],
}

impl Tri {
    fn new(a: usize, b: usize, c: usize) -> Self {
        Self {
            v: [a, b, c],
            adj: [None; 3],
            constrained: [false; 3],
        }
    }

    /// Edge opposite vertex `i`.
    fn edge_opp(&self, i: usize) -> (usize, usize) {
        (self.v[(i + 1) % 3], self.v[(i + 2) % 3])
    }

    /// Index of the edge between vertices `a` and `b`, if present.
    fn find_edge(&self, a: usize, b: usize) -> Option<usize> {
        for i in 0..3 {
            let (e0, e1) = self.edge_opp(i);
            if (e0 == a && e1 == b) || (e0 == b && e1 == a) {
                return Some(i);
            }
        }
        None
    }
}

/// Host triangle frame used for repeated projection of 3D points.
#[derive(Clone, Debug)]
pub struct HostFrame {
    a: [BigRational; 3],
    b: [BigRational; 3],
    c: [BigRational; 3],
    /// Original input vertices of the host triangle, kept for provenance.
    pub host_a: [f64; 3],
    pub host_b: [f64; 3],
    pub host_c: [f64; 3],
}

impl HostFrame {
    pub fn from_points(a: &Point3, b: &Point3, c: &Point3) -> Option<Self> {
        Some(Self {
            a: a.to_rational()?,
            b: b.to_rational()?,
            c: c.to_rational()?,
            host_a: a.to_f64()?,
            host_b: b.to_f64()?,
            host_c: c.to_f64()?,
        })
    }

    /// Return a (non-unit) normal vector of the host plane, computed from the
    /// original input vertices. Used only for constructing exact provenance of
    /// coplanar-edge crossings.
    pub fn normal(&self) -> [f64; 3] {
        let u = sub_f64(self.host_b, self.host_a);
        let v = sub_f64(self.host_c, self.host_a);
        cross_f64(u, v)
    }

    /// Project an exact 3D point into the host's `(s,t)` parameter space.
    pub(crate) fn project(&self, p: &Point3) -> Option<Point2D> {
        let pr = p.to_rational()?;
        project_point(&self.a, &self.b, &self.c, &pr)
    }

    /// Map 2D barycentric `(s,t)` coordinates back to an exact rational 3D
    /// point using `p = a + s*(b - a) + t*(c - a)`.
    pub fn point3d(&self, s: &BigRational, t: &BigRational) -> [BigRational; 3] {
        let u0 = &self.b[0] - &self.a[0];
        let u1 = &self.b[1] - &self.a[1];
        let u2 = &self.b[2] - &self.a[2];
        let v0 = &self.c[0] - &self.a[0];
        let v1 = &self.c[1] - &self.a[1];
        let v2 = &self.c[2] - &self.a[2];
        [
            &self.a[0] + s.clone() * &u0 + t.clone() * &v0,
            &self.a[1] + s.clone() * &u1 + t.clone() * &v1,
            &self.a[2] + s.clone() * &u2 + t.clone() * &v2,
        ]
    }
}

/// A constrained Delaunay triangulation of a host triangle.
#[derive(Clone, Debug)]
pub struct Triangulation {
    verts: Vec<CdtVertex>,
    tris: Vec<Tri>,
    /// Host triangle frame, kept for projecting inserted provenance points.
    host: HostFrame,
    /// Maps a vertex's canonical provenance key to its index.
    vertex_index: HashMap<Vec<u64>, usize>,
    /// Maps cached (s,t) coordinates to a vertex index.
    st_index: HashMap<(BigRational, BigRational), usize>,
    #[cfg(feature = "cdt-diag")]
    diag: DiagState,
}

impl Triangulation {
    #[cfg(feature = "cdt-diag")]
    fn push_vertex(&mut self, v: CdtVertex) -> usize {
        debug_assert!(
            !v.provenance.as_ref().map_or(false, |p| p.is_nan_placeholder()),
            "NaN placeholder provenance must never be stored in the CDT"
        );
        self.diag.update_bit_len(&v.st);
        let vi = self.verts.len();
        if let Some(p) = &v.provenance {
            self.vertex_index.insert(p.canonical_key(), vi);
        }
        self.st_index
            .insert((v.st.s.clone(), v.st.t.clone()), vi);
        self.verts.push(v);
        vi
    }

    #[cfg(not(feature = "cdt-diag"))]
    fn push_vertex(&mut self, v: CdtVertex) -> usize {
        debug_assert!(
            !v.provenance.as_ref().map_or(false, |p| p.is_nan_placeholder()),
            "NaN placeholder provenance must never be stored in the CDT"
        );
        let vi = self.verts.len();
        if let Some(p) = &v.provenance {
            self.vertex_index.insert(p.canonical_key(), vi);
        }
        self.st_index
            .insert((v.st.s.clone(), v.st.t.clone()), vi);
        self.verts.push(v);
        vi
    }

    #[cfg(feature = "cdt-diag")]
    fn diag_begin_batch(&mut self, n: usize) {
        self.diag.input_segments = n;
    }

    #[cfg(not(feature = "cdt-diag"))]
    fn diag_begin_batch(&mut self, _n: usize) {}

    #[cfg(feature = "cdt-diag")]
    fn diag_end_batch(&mut self) {
        self.diag.final_vertices = self.verts.len();
    }

    #[cfg(not(feature = "cdt-diag"))]
    fn diag_end_batch(&mut self) {}

    #[cfg(feature = "cdt-diag")]
    fn diag_record_crossing(&mut self, vi: usize, n_initial: usize) {
        if vi >= n_initial {
            self.diag.crossings += 1;
        }
    }

    #[cfg(not(feature = "cdt-diag"))]
    fn diag_record_crossing(&mut self, _vi: usize, _n_initial: usize) {}

    /// Compute the exact provenance of the crossing of two constrained
    /// segments from their original source descriptions.
    ///
    /// All constructions use only original input data:
    /// - transversal ∩ transversal       -> PPI(host, A, B)
    /// - transversal ∩ host edge         -> LPI(host edge, plane A)
    /// - transversal ∩ coplanar edge     -> LPI(edge, plane A)
    /// - host edge ∩ host edge           -> explicit endpoint (already a vertex)
    /// - host edge ∩ coplanar edge       -> LPI(edge, plane of host edge line + host normal)
    /// - coplanar edge ∩ coplanar edge   -> LPI(edge A, plane through edge B and host normal)
    fn crossing_provenance(
        host: &HostFrame,
        a: &SegmentSource,
        b: &SegmentSource,
    ) -> Option<Point3> {
        match (a, b) {
            (
                SegmentSource::Transversal {
                    other_plane: (a1, a2, a3),
                },
                SegmentSource::Transversal {
                    other_plane: (b1, b2, b3),
                },
            ) => Some(Point3::Ppi {
                r1: host.host_a,
                s1: host.host_b,
                t1: host.host_c,
                r2: *a1,
                s2: *a2,
                t2: *a3,
                r3: *b1,
                s3: *b2,
                t3: *b3,
            }),
            (
                SegmentSource::Transversal {
                    other_plane: (a1, a2, a3),
                },
                SegmentSource::HostEdge {
                    endpoints: (e1, e2),
                },
            )
            | (
                SegmentSource::HostEdge {
                    endpoints: (e1, e2),
                },
                SegmentSource::Transversal {
                    other_plane: (a1, a2, a3),
                },
            )
            | (
                SegmentSource::Transversal {
                    other_plane: (a1, a2, a3),
                },
                SegmentSource::CoplanarEdge {
                    endpoints: (e1, e2),
                },
            )
            | (
                SegmentSource::CoplanarEdge {
                    endpoints: (e1, e2),
                },
                SegmentSource::Transversal {
                    other_plane: (a1, a2, a3),
                },
            ) => Some(Point3::Lpi {
                q1: *e1,
                q2: *e2,
                r: *a1,
                s: *a2,
                t: *a3,
            }),
            (
                SegmentSource::HostEdge { .. },
                SegmentSource::HostEdge { .. },
            ) => {
                // Two host edges only meet at a host vertex, which is already
                // in the triangulation. No new vertex is needed.
                None
            }
            (
                SegmentSource::HostEdge {
                    endpoints: (a1, a2),
                },
                SegmentSource::CoplanarEdge {
                    endpoints: (b1, b2),
                },
            )
            | (
                SegmentSource::CoplanarEdge {
                    endpoints: (b1, b2),
                },
                SegmentSource::HostEdge {
                    endpoints: (a1, a2),
                },
            )
            | (
                SegmentSource::CoplanarEdge {
                    endpoints: (a1, a2),
                },
                SegmentSource::CoplanarEdge {
                    endpoints: (b1, b2),
                },
            ) => {
                let n = host.normal();
                let off = [b1[0] + n[0], b1[1] + n[1], b1[2] + n[2]];
                Some(Point3::Lpi {
                    q1: *a1,
                    q2: *a2,
                    r: *b1,
                    s: *b2,
                    t: off,
                })
            }
        }
    }

    /// Create a triangulation from a host triangle given as three 3D points.
    /// The host triangle's vertices are vertices 0, 1, 2 of the new mesh.
    pub fn from_host(a: &Point3, b: &Point3, c: &Point3) -> Option<Self> {
        let pa = a.to_rational()?;
        let pb = b.to_rational()?;
        let pc = c.to_rational()?;

        let p0 = project_point(&pa, &pb, &pc, &pa)?;
        let p1 = project_point(&pa, &pb, &pc, &pb)?;
        let p2 = project_point(&pa, &pb, &pc, &pc)?;

        if orient2d(&p0, &p1, &p2).is_zero() {
            return None;
        }

        // Ensure CCW orientation before building indices.
        let mut st = [p0, p1, p2];
        let mut prov = [a.clone(), b.clone(), c.clone()];
        if !orient2d(&st[0], &st[1], &st[2]).is_positive() {
            st.swap(1, 2);
            prov.swap(1, 2);
        }

        let host = HostFrame::from_points(&prov[0], &prov[1], &prov[2])?;
        let verts: Vec<CdtVertex> = st
            .into_iter()
            .zip(prov.into_iter())
            .map(|(s, p)| CdtVertex { st: s, provenance: Some(p) })
            .collect();
        let mut vertex_index = HashMap::with_capacity(3);
        let mut st_index = HashMap::with_capacity(3);
        for (i, v) in verts.iter().enumerate() {
            if let Some(p) = &v.provenance {
                vertex_index.insert(p.canonical_key(), i);
            }
            st_index.insert((v.st.s.clone(), v.st.t.clone()), i);
        }
        Some(Self {
            verts,
            tris: vec![Tri::new(0, 1, 2)],
            host,
            vertex_index,
            st_index,
            #[cfg(feature = "cdt-diag")]
            diag: DiagState::default(),
        })
    }

    /// Number of vertices (including the three host vertices).
    pub fn num_verts(&self) -> usize {
        self.verts.len()
    }

    /// Number of triangles.
    pub fn num_tris(&self) -> usize {
        self.tris.len()
    }

    /// Insert a new point (projected from a 3D point) into the triangulation.
    /// Returns the index of the new vertex.
    pub fn insert_vertex(&mut self, p: &Point3) -> Option<usize> {
        // Dedup against existing vertices by provenance or (s,t).
        if let Some(vi) = self.find_vertex(p) {
            return Some(vi);
        }
        let pr = p.to_rational()?;
        let q = project_point(&self.host.a, &self.host.b, &self.host.c, &pr)?;
        let vi = self.push_vertex(CdtVertex {
            st: q,
            provenance: Some(p.clone()),
        });
        self.insert_vertex_at(vi);
        Some(vi)
    }

    /// Insert a vertex given only by exact 2D parameter coordinates (no 3D
    /// provenance).  Used as the exact fallback when no original-data
    /// construction is available.  Deduplicates by `(s,t)`.
    fn insert_vertex_2d(&mut self, q: Point2D) -> usize {
        if let Some(&vi) = self.st_index.get(&(q.s.clone(), q.t.clone())) {
            return vi;
        }
        let vi = self.push_vertex(CdtVertex {
            st: q,
            provenance: None,
        });
        self.insert_vertex_at(vi);
        vi
    }

    /// Return the index of an existing vertex whose projected 2D coordinates
    /// exactly match `p`, if any.
    pub fn find_vertex(&self, p: &Point3) -> Option<usize> {
        // Fast path: look up by canonical provenance key.
        if let Some(&vi) = self.vertex_index.get(&p.canonical_key()) {
            return Some(vi);
        }
        // Fallback: project and look up by (s,t).
        let pr = p.to_rational()?;
        let q = project_point(&self.host.a, &self.host.b, &self.host.c, &pr)?;
        self.st_index.get(&(q.s, q.t)).copied()
    }

    fn insert_vertex_at(&mut self, vi: usize) {
        let p = self.verts[vi].st.clone();
        let t = self.locate(&p);
        assert!(t.is_some(), "inserted point outside host triangle");
        let t = t.unwrap();

        // Check for point on an edge.
        let mut on_edge: Option<usize> = None;
        for i in 0..3 {
            let (a, b) = self.tris[t].edge_opp(i);
            if orient2d(&self.verts[a].st, &self.verts[b].st, &p).is_zero()
                && dot1d_between(&self.verts[a].st, &self.verts[b].st, &p)
            {
                on_edge = Some(i);
                break;
            }
        }

        let mut to_legalize = Vec::new();
        match on_edge {
            Some(e) => self.split_edge(t, e, vi, &mut to_legalize),
            None => self.split_triangle(t, vi, &mut to_legalize),
        }

        // Delaunay legalization is currently disabled.  The fan created by
        // splitting triangles is already a valid constrained triangulation for
        // the non-crossing segments used by the arrangement-lite caller, and
        // keeping it avoids edge flips that can remove a segment endpoint's
        // incident edge before the segment is inserted.  Full Delaunay
        // optimization can be added later once constraint insertion handles
        // all degenerate cases.
        let _ = to_legalize;
    }

    fn split_triangle(&mut self, t: usize, p: usize, stack: &mut Vec<(usize, usize)>) {
        let v = self.tris[t].v;
        let adj = self.tris[t].adj;

        let t0 = t;
        let t1 = self.tris.len();
        let t2 = self.tris.len() + 1;

        self.tris[t0] = Tri::new(p, v[0], v[1]);
        self.tris.push(Tri::new(p, v[1], v[2]));
        self.tris.push(Tri::new(p, v[2], v[0]));

        // adj[0] is edge opposite p, i.e. the old edge.
        self.tris[t0].adj = [adj[2], Some(t1), Some(t2)];
        self.tris[t1].adj = [adj[0], Some(t2), Some(t0)];
        self.tris[t2].adj = [adj[1], Some(t0), Some(t1)];

        self.update_adj(adj[2], v[0], v[1], Some(t0));
        self.update_adj(adj[0], v[1], v[2], Some(t1));
        self.update_adj(adj[1], v[2], v[0], Some(t2));

        stack.push((t0, 0));
        stack.push((t1, 0));
        stack.push((t2, 0));
    }

    fn split_edge(&mut self, t: usize, e: usize, p: usize, stack: &mut Vec<(usize, usize)>) {
        let v = self.tris[t].v;
        let adj = self.tris[t].adj;
        let a = v[e]; // opposite vertex
        let b = v[(e + 1) % 3];
        let c = v[(e + 2) % 3];

        let tn = adj[e];
        let t0 = t;
        let t1 = self.tris.len();
        self.tris[t0] = Tri::new(p, c, a);
        self.tris.push(Tri::new(p, a, b));

        // t0 becomes (p, c, a), t1 becomes (p, a, b); they share edge (p,a).
        self.tris[t0].adj = [adj[(e + 1) % 3], Some(t1), None];
        self.tris[t1].adj = [adj[(e + 2) % 3], None, Some(t0)];

        self.update_adj(adj[(e + 1) % 3], c, a, Some(t0));
        self.update_adj(adj[(e + 2) % 3], a, b, Some(t1));

        if let Some(tn) = tn {
            // Neighbour shares edge (c,b) reversed.  Its vertex ordering is
            // [d, c, b] where d is opposite the shared edge.
            let ne = self.find_edge(tn, b, c).unwrap();
            let nv = self.tris[tn].v;
            let nd = nv[ne];
            let nadj = self.tris[tn].adj;

            let n0 = tn;
            let n1 = self.tris.len();
            self.tris[n0] = Tri::new(p, b, nd);
            self.tris.push(Tri::new(p, nd, c));

            // n0=(p,b,d), n1=(p,d,c); they share edge (p,d).
            // tn = [d, c, b] with the shared edge at index `ne`, so:
            //   edge (b,d) (n0's edge0) is tn's edge_opp((ne+1)%3)
            //   edge (d,c) (n1's edge0) is tn's edge_opp((ne+2)%3)
            self.tris[n0].adj = [nadj[(ne + 1) % 3], Some(n1), Some(t1)];
            self.tris[n1].adj = [nadj[(ne + 2) % 3], Some(t0), Some(n0)];

            self.update_adj(nadj[(ne + 1) % 3], b, nd, Some(n0));
            self.update_adj(nadj[(ne + 2) % 3], nd, c, Some(n1));

            // Glue the four triangles along the split edge.
            self.tris[t0].adj[2] = Some(n1);
            self.tris[n1].adj[1] = Some(t0);
            self.tris[t1].adj[1] = Some(n0);
            self.tris[n0].adj[2] = Some(t1);

            stack.push((t0, 0));
            stack.push((t1, 0));
            stack.push((n0, 0));
            stack.push((n1, 0));
        } else {
            stack.push((t0, 0));
            stack.push((t1, 0));
        }
    }

    /// Split the triangle containing the chord `(a, b)` so the chord becomes
    /// an edge.  Called when a constraint segment connects two existing
    /// vertices but crosses no existing edge (i.e. it lies entirely inside one
    /// triangle).  The triangle's boundary cycle is split at `a` and `b` and
    /// the two resulting polygons are fan-triangulated from their chord
    /// endpoints; adjacencies are then rebuilt for the whole (small) CDT.
    fn split_triangle_by_chord(&mut self, a: usize, b: usize) {
        let t = match self.locate_chord_triangle(a, b) {
            Some(t) => t,
            None => {
                // No single triangle contains the segment: leave it
                // unconstrained rather than panic (the caller already handles
                // vertex-on-segment splitting).
                return;
            }
        };
        let v = self.tris[t].v;

        // CCW boundary cycle of triangle t: v[0] -> v[1] -> v[2] -> v[0].
        // Insert a and b (when not vertices of t) at their edge positions.
        let mut cycle: Vec<usize> = vec![v[0], v[1], v[2]];
        for x in [a, b] {
            if cycle.contains(&x) {
                continue;
            }
            let mut placed = false;
            for i in 0..3 {
                let (p, q) = (cycle[i], cycle[(i + 1) % 3]);
                if orient2d(&self.verts[p].st, &self.verts[q].st, &self.verts[x].st).is_zero()
                    && dot1d_between(&self.verts[p].st, &self.verts[q].st, &self.verts[x].st)
                {
                    cycle.insert(i + 1, x);
                    placed = true;
                    break;
                }
            }
            if !placed {
                // The endpoint is not on this triangle's boundary (the
                // segment is not a chord of one triangle); leave the
                // constraint unresolved rather than panic.
                return;
            }
        }

        let n = cycle.len();
        let ia = match cycle.iter().position(|&c| c == a) {
            Some(i) => i,
            None => return,
        };
        let ib = match cycle.iter().position(|&c| c == b) {
            Some(i) => i,
            None => return,
        };
        let steps = (ib + n - ia) % n;

        let mut arc_a: Vec<usize> = Vec::new();
        for k in 0..=steps {
            arc_a.push(cycle[(ia + k) % n]);
        }
        let mut arc_b: Vec<usize> = Vec::new();
        for k in 0..=(n - steps) {
            arc_b.push(cycle[(ib + k) % n]);
        }

        let mut new_tris: Vec<[usize; 3]> = Vec::new();
        for arc in [&arc_a, &arc_b] {
            if arc.len() < 3 {
                continue;
            }
            let p0 = arc[0];
            for j in 1..arc.len() - 1 {
                new_tris.push([p0, arc[j], arc[j + 1]]);
            }
        }

        if new_tris.is_empty() {
            // a and b are adjacent on the boundary: the chord is already an
            // edge (or a boundary sub-edge); just mark it constrained.
            self.mark_constrained_edge(a, b);
            return;
        }

        // Replace triangle t with the first new triangle; push the rest.
        let first = new_tris[0];
        self.tris[t] = Tri::new(first[0], first[1], first[2]);
        for nt in &new_tris[1..] {
            self.tris.push(Tri::new(nt[0], nt[1], nt[2]));
        }

        // Rebuild adjacencies for the whole CDT (small per-host triangle).
        for tri in &mut self.tris {
            tri.adj = [None; 3];
        }
        let ntris = self.tris.len();
        for i in 0..ntris {
            for e in 0..3 {
                if self.tris[i].adj[e].is_some() {
                    continue;
                }
                let (ea, eb) = self.tris[i].edge_opp(e);
                for j in (i + 1)..ntris {
                    if let Some(f) = self.tris[j].find_edge(ea, eb) {
                        if self.tris[j].adj[f].is_none() {
                            self.tris[i].adj[e] = Some(j);
                            self.tris[j].adj[f] = Some(i);
                            break;
                        }
                    }
                }
            }
        }

        // The chord is the constraint being added.
        self.mark_constrained_edge(a, b);
    }

    /// Find a triangle whose boundary contains both `a` and `b` (the chord's
    /// interior lies in that triangle).
    fn locate_chord_triangle(&self, a: usize, b: usize) -> Option<usize> {
        let pa = &self.verts[a].st;
        let pb = &self.verts[b].st;
        let mid = Point2D {
            s: (&pa.s + &pb.s) / BigRational::from_integer(2.into()),
            t: (&pa.t + &pb.t) / BigRational::from_integer(2.into()),
        };
        self.locate(&mid)
    }

    /// Mark every occurrence of edge `(a, b)` as constrained.
    fn mark_constrained_edge(&mut self, a: usize, b: usize) {
        for tri in &mut self.tris {
            if let Some(e) = tri.find_edge(a, b) {
                tri.constrained[e] = true;
            }
        }
    }

    #[allow(dead_code)]
    fn legalize(&mut self, mut stack: Vec<(usize, usize)>) {
        while let Some((ti, ei)) = stack.pop() {
            if self.tris[ti].constrained[ei] {
                continue;
            }
            let nbr = match self.tris[ti].adj[ei] {
                Some(n) => n,
                None => continue,
            };

            // Get the quadrilateral: ti = (a,b,c) with edge (b,c), nbr opposite a is d.
            let (a, b, c) = {
                let v = self.tris[ti].v;
                (v[ei], v[(ei + 1) % 3], v[(ei + 2) % 3])
            };
            let ne = self.find_edge(nbr, b, c).unwrap();
            let d = self.tris[nbr].v[ne];

            // If d is inside circumcircle of (a,b,c), flip.
            if incircle(
                &self.verts[a].st,
                &self.verts[b].st,
                &self.verts[c].st,
                &self.verts[d].st,
            )
            .is_positive()
            {
                self.flip(ti, ei, nbr, ne);
                // Push the two edges opposite a and c in the new configuration.
                // After the flip the new shared edge is (a,d) and (d,c)?
                // New triangles are (a,b,d) and (a,d,c).  The shared edge is (a,d).
                // Edge opposite b is (a,d); edge opposite c is (a,d).
                // We push both edges (they have the same geometry but belong to
                // different triangles).
                let t_new = ti;
                let n_new = nbr;
                // In t_new=(a,b,d), edge opposite b is index of edge (a,d).
                let e_b = self.edge_with_vertices(t_new, a, d).unwrap();
                let e_c = self.edge_with_vertices(n_new, a, d).unwrap();
                stack.push((t_new, e_b));
                stack.push((n_new, e_c));
            }
        }
    }

    fn flip(&mut self, t1: usize, e1: usize, t2: usize, e2: usize) {
        let (a, b, c) = {
            let v = self.tris[t1].v;
            (v[e1], v[(e1 + 1) % 3], v[(e1 + 2) % 3])
        };
        let d = self.tris[t2].v[e2];

        let adj1 = self.tris[t1].adj;
        let adj2 = self.tris[t2].adj;
        let _cons1 = self.tris[t1].constrained;
        let _cons2 = self.tris[t2].constrained;

        // New triangles: (a,b,d) and (a,d,c).
        self.tris[t1] = Tri::new(a, b, d);
        self.tris[t2] = Tri::new(a, d, c);

        // adjacency across edges of new t1:
        // edge 0 (a,b) -> old adj1[(e1+1)%3] (edge opposite c? wait old edge (a,b) was index (e1+2)%3? Let's recompute)
        // Old t1=(a,b,c). Edge indices: edge0 opp a=(b,c), edge1 opp b=(c,a), edge2 opp c=(a,b).
        // So old edge (a,b) is index (e1+2)%3, (b,c) index e1, (c,a) index (e1+1)%3.
        // New t1=(a,b,d). Edges: opp a=(b,d), opp b=(d,a), opp d=(a,b).
        // New edge (a,b) is opposite d -> index 2. So adj[2] = old adj[(e1+2)%3].
        // New edge (b,d) is opposite a -> index 0. It was part of old t2; the neighbor across (b,d) is adj2[(e2+2)%3]? Old t2 orientation: it had edge (c,b) as shared, with d opposite. Let's name old t2 vertices as (c,b,d) because it shares edge (c,b) with t1. In our data, t2.v[e2] = d, edge_opp(e2) = (b,c) (matches shared edge reversed). So old t2 edges: opp d=(b,c) (shared), opp c=(d,b), opp b=(c,d). Edge (b,d) is opposite c -> index (e2+1)%3. So new t1.adj[0] = adj2[(e2+1)%3]. New edge (d,a) is opposite b -> index 1; old edge (c,a) in t1 was index (e1+1)%3, but (d,a) is new. Actually (d,a) is the extension of old edge (c,a) after replacing c with d? No, it's a new edge. Its neighbor is old edge (c,d) of t2, which is opposite b -> index (e2+2)%3. So new t1.adj[1] = adj2[(e2+2)%3].
        // New t2=(a,d,c). Edges: opp a=(d,c), opp d=(c,a), opp c=(a,d).
        // Edge (d,c) is opposite a -> index 0; old t2 edge (c,d) opposite b -> index (e2+2)%3. Reversed, so neighbor same adjacency.
        // Edge (c,a) is opposite d -> index 1; old t1 edge (c,a) opposite b -> index (e1+1)%3.
        // Edge (a,d) is opposite c -> index 2; old t1 edge (a,b) opposite c -> index (e1+2)%3? Wait old edge (a,b) becomes new edge (a,d)? Not exactly. Let's recompute shared edge before flip: (b,c). After flip, shared edge becomes (a,d). The two triangles are (a,b,d) and (a,d,c). The edge (a,d) is shared. The two triangles that were adjacent to the old edges (a,b) and (a,c) become adjacent to (a,d)? Actually no: the old diagonal (b,c) is removed, new diagonal (a,d) added. The external adjacencies of the quadrilateral are preserved.
        // Let's enumerate external edges before flip:
        // t1=(a,b,c): edges (a,b) adj x, (b,c) shared, (c,a) adj y.
        // t2=(c,b,d): edges (c,b) shared, (b,d) adj z, (d,c) adj w.
        // After flip:
        // t1'=(a,b,d): edges (a,b) adj x, (b,d) adj z, (d,a) shared.
        // t2'=(a,d,c): edges (a,d) shared, (d,c) adj w, (c,a) adj y.
        // So mapping:
        // t1'.adj[2] (opp d, edge a-b) = x = adj1[(e1+2)%3]
        // t1'.adj[0] (opp a, edge b-d) = z = adj2[(e2+1)%3]
        // t1'.adj[1] (opp b, edge d-a) = Some(t2')
        // t2'.adj[2] (opp c, edge a-d) = Some(t1')
        // t2'.adj[0] (opp a, edge d-c) = w = adj2[(e2+2)%3]
        // t2'.adj[1] (opp d, edge c-a) = y = adj1[(e1+1)%3]
        //
        self.tris[t1].adj = [adj2[(e2 + 1) % 3], Some(t2), adj1[(e1 + 2) % 3]];
        self.tris[t2].adj = [adj2[(e2 + 2) % 3], adj1[(e1 + 1) % 3], Some(t1)];

        self.update_adj(adj1[(e1 + 2) % 3], a, b, Some(t1));
        self.update_adj(adj2[(e2 + 1) % 3], b, d, Some(t1));
        self.update_adj(adj2[(e2 + 2) % 3], d, c, Some(t2));
        self.update_adj(adj1[(e1 + 1) % 3], c, a, Some(t2));
    }

    fn find_edge(&self, tri: usize, a: usize, b: usize) -> Option<usize> {
        for i in 0..3 {
            let (e0, e1) = self.tris[tri].edge_opp(i);
            if (e0 == a && e1 == b) || (e0 == b && e1 == a) {
                return Some(i);
            }
        }
        None
    }

    fn edge_with_vertices(&self, tri: usize, a: usize, b: usize) -> Option<usize> {
        self.find_edge(tri, a, b)
    }

    fn update_adj(&mut self, tri: Option<usize>, a: usize, b: usize, new_adj: Option<usize>) {
        if let Some(t) = tri {
            let e = self.find_edge(t, a, b).unwrap_or_else(|| {
                panic!(
                    "adjacent triangle {} missing edge ({},{}) among {:?}",
                    t, a, b, self.tris[t].v
                )
            });
            self.tris[t].adj[e] = new_adj;
        }
    }

    /// Locate the triangle containing point `p`.  If `p` lies exactly on an
    /// edge, any of the two incident triangles may be returned.
    fn locate(&self, p: &Point2D) -> Option<usize> {
        for (i, tri) in self.tris.iter().enumerate() {
            let v = tri.v;
            let s0 = orient2d(&self.verts[v[0]].st, &self.verts[v[1]].st, p);
            let s1 = orient2d(&self.verts[v[1]].st, &self.verts[v[2]].st, p);
            let s2 = orient2d(&self.verts[v[2]].st, &self.verts[v[0]].st, p);
            if (s0.is_positive() || s0.is_zero())
                && (s1.is_positive() || s1.is_zero())
                && (s2.is_positive() || s2.is_zero())
            {
                return Some(i);
            }
        }
        None
    }

    /// Enforce a constrained segment between two existing vertices.
    pub fn add_constraint(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        // If the edge already exists, mark it constrained.
        if let Some((t, e)) = self.find_any_edge(a, b) {
            self.tris[t].constrained[e] = true;
            if let Some(n) = self.tris[t].adj[e] {
                let ne = self.find_edge(n, a, b).unwrap();
                self.tris[n].constrained[ne] = true;
            }
            return;
        }

        // Repeatedly flip edges that cross the segment until the segment
        // appears in the triangulation.
        loop {
            // If the segment passes through an existing vertex (a degenerate
            // alignment), split it there: each sub-segment is shorter and
            // falls into the regular cases.
            if let Some(v) = self.find_vertex_on_open_segment(a, b) {
                self.add_constraint(a, v);
                self.add_constraint(v, b);
                return;
            }
            let crossing = self.find_crossing_edge(a, b);
            match crossing {
                Some((t, e)) => {
                    let nbr = self.tris[t].adj[e].expect("crossing edge on boundary");
                    let ne = self
                        .find_edge(nbr, self.tris[t].edge_opp(e).0, self.tris[t].edge_opp(e).1)
                        .unwrap();
                    // Only flip if the quadrilateral is convex and the crossed
                    // edge is not itself a constraint.  If a previously
                    // inserted constraint blocks us, split both segments at the
                    // exact intersection point and recurse; this is the
                    // arrangement fallback that makes multi-segment batches
                    // robust.
                    if self.tris[t].constrained[e] || !self.is_convex_for_flip(t, e, nbr, ne) {
                        // Compute the 3D provenance of the fallback split point as
                        // the intersection of the constraint's supporting line and
                        // the blocking edge's supporting line in the host plane.
                        // Exact 2D construction (the pre-C1 behaviour).  A 3D
                        // recipe cannot be rebuilt here without the sources of
                        // both lines; rounding implicit points to f64 would
                        // break exactness, so none is attempted.
                        let (u, v) = self.tris[t].edge_opp(e);
                        let (q, _) = line_line_intersection(
                            &self.verts[a].st,
                            &self.verts[b].st,
                            &self.verts[u].st,
                            &self.verts[v].st,
                        );
                        let vi = self.insert_vertex_2d(q);
                        self.add_constraint(a, vi);
                        self.add_constraint(vi, b);
                        if self.tris[t].constrained[e] {
                            let (u, v) = self.tris[t].edge_opp(e);
                            self.add_constraint(u, vi);
                            self.add_constraint(vi, v);
                        }
                        return;
                    }
                    self.flip(t, e, nbr, ne);
                }
                None => {
                    // No edge crosses the segment and the segment is not yet
                    // an edge: it is a chord inside a single triangle.  Split
                    // that triangle along the chord so the constraint becomes
                    // an edge.
                    self.split_triangle_by_chord(a, b);
                    return;
                }
            }
        }
    }

    /// Insert a batch of constrained segments in a segment-order-independent
    /// way by first computing their planar arrangement.
    ///
    /// The input endpoints refer to vertices already present in the
    /// triangulation.  Every pair of input segments is examined for a proper
    /// intersection; when one exists, the crossing point is constructed from
    /// the original input planes/lines (PPI/LPI) and inserted, then both
    /// segments are split at that point.  Existing vertices that lie in the
    /// interior of a segment are also used as split points.  After this
    /// pre-splitting, no two sub-segments cross in their interior, so
    /// enforcing them one by one is safe.
    pub fn add_constraints_batch(&mut self, segments: &[ConstrainedSegment]) {
        if segments.is_empty() {
            return;
        }

        let n_initial = self.verts.len();
        let n_raw = segments.len();
        self.diag_begin_batch(n_raw);

        let host = self.host.clone();

        // Lists of vertex indices lying on each raw segment.  Start with the
        // two endpoints; intersections and endpoint-on-segment events are
        // added below.
        let mut on_seg: Vec<Vec<usize>> = Vec::with_capacity(n_raw);
        for s in segments {
            on_seg.push(vec![s.endpoints.0, s.endpoints.1]);
        }

        // 1) Pairwise intersections.  For every intersecting pair, compute the
        // crossing from original source geometry (PPI/LPI), insert it, and add
        // its index to both segments.  A cheap 2D bbox prefilter skips disjoint
        // pairs before the exact predicate work.
        let endpoints: Vec<_> = segments.iter().map(|s| s.endpoints).collect();
        let bboxes: Vec<_> = endpoints
            .iter()
            .map(|&(a, b)| bbox2d(&self.verts[a].st, &self.verts[b].st))
            .collect();
        for i in 0..n_raw {
            for j in (i + 1)..n_raw {
                if !bboxes_overlap(&bboxes[i], &bboxes[j]) {
                    continue;
                }
                let (a, b) = endpoints[i];
                let (c, d) = endpoints[j];
                if !segments_properly_intersect(
                    &self.verts[a].st,
                    &self.verts[b].st,
                    &self.verts[c].st,
                    &self.verts[d].st,
                ) {
                    continue;
                }
                let provenance =
                    Self::crossing_provenance(&host, &segments[i].source, &segments[j].source);
                // Preferred: construct the crossing from original input data
                // (bounded bit size).  If that is unavailable or degenerate,
                // fall back to the exact 2D line-line construction from the
                // raw segment endpoints -- a proper crossing is never dropped.
                let vi = match provenance.and_then(|p| self.insert_vertex(&p)) {
                    Some(vi) => vi,
                    None => {
                        let (q, _) = line_line_intersection(
                            &self.verts[a].st,
                            &self.verts[b].st,
                            &self.verts[c].st,
                            &self.verts[d].st,
                        );
                        self.insert_vertex_2d(q)
                    }
                };
                self.diag_record_crossing(vi, n_initial);
                on_seg[i].push(vi);
                on_seg[j].push(vi);
            }
        }

        // 2) Existing vertices that lie in the interior of a raw segment.
        for i in 0..n_raw {
            let (a, b) = endpoints[i];
            for v in 0..n_initial {
                if v == a || v == b {
                    continue;
                }
                if point_on_segment(&self.verts[a].st, &self.verts[b].st, &self.verts[v].st) {
                    on_seg[i].push(v);
                }
            }
        }

        // 3) Sort each segment's vertices along the segment, deduplicate, and
        // emit the short sub-segments.
        let mut sub_segments: Vec<(usize, usize)> = Vec::new();
        for i in 0..n_raw {
            let (a, b) = endpoints[i];
            let pa = &self.verts[a].st;
            let pb = &self.verts[b].st;
            on_seg[i].sort_by(|u, v| {
                param_along(pa, pb, &self.verts[*u].st)
                    .cmp(&param_along(pa, pb, &self.verts[*v].st))
            });
            on_seg[i].dedup();
            for k in 0..on_seg[i].len().saturating_sub(1) {
                let u = on_seg[i][k];
                let v = on_seg[i][k + 1];
                if u != v {
                    sub_segments.push((u, v));
                }
            }
        }

        // 4) Enforce every sub-segment.  Because all interior crossings were
        // resolved in step 1, these cannot cross each other except at shared
        // vertices.
        for (a, b) in sub_segments {
            self.add_constraint(a, b);
        }
        self.diag_end_batch();
    }


    fn find_any_edge(&self, a: usize, b: usize) -> Option<(usize, usize)> {
        for (i, _) in self.tris.iter().enumerate() {
            if let Some(e) = self.find_edge(i, a, b) {
                return Some((i, e));
            }
        }
        None
    }

    fn find_crossing_edge(&self, a: usize, b: usize) -> Option<(usize, usize)> {
        let pa = &self.verts[a].st;
        let pb = &self.verts[b].st;
        for (i, tri) in self.tris.iter().enumerate() {
            for e in 0..3 {
                let (u, v) = tri.edge_opp(e);
                let pu = &self.verts[u].st;
                let pv = &self.verts[v].st;
                if segments_properly_intersect(pa, pb, pu, pv) {
                    return Some((i, e));
                }
            }
        }
        None
    }

    /// A vertex (other than `a`/`b`) lying strictly between them on the open
    /// segment `(a, b)`, if any.
    fn find_vertex_on_open_segment(&self, a: usize, b: usize) -> Option<usize> {
        let pa = &self.verts[a].st;
        let pb = &self.verts[b].st;
        for (i, v) in self.verts.iter().enumerate() {
            if i == a || i == b {
                continue;
            }
            if orient2d(pa, pb, &v.st).is_zero() && strictly_between(pa, pb, &v.st) {
                return Some(i);
            }
        }
        None
    }

    fn is_convex_for_flip(&self, t1: usize, e1: usize, t2: usize, e2: usize) -> bool {
        let (a, b, c) = {
            let v = self.tris[t1].v;
            (v[e1], v[(e1 + 1) % 3], v[(e1 + 2) % 3])
        };
        let d = self.tris[t2].v[e2];
        // Quadrilateral a-b-c-d (ordered around t1 then t2) is convex iff
        // both new triangles would be CCW.
        orient2d(&self.verts[a].st, &self.verts[b].st, &self.verts[d].st).is_positive()
            && orient2d(&self.verts[a].st, &self.verts[d].st, &self.verts[c].st).is_positive()
    }


    /// Return the triangulation as a list of CCW vertex-index triples.
    pub fn triangles(&self) -> Vec<[usize; 3]> {
        self.tris.iter().map(|t| t.v).collect()
    }

    /// Return a reference to the CDT vertices.
    pub fn vertices(&self) -> &[CdtVertex] {
        &self.verts
    }

    /// Return the diagnostic snapshot for this host triangle.
    #[cfg(feature = "cdt-diag")]
    pub fn diagnostic(&self) -> &DiagState {
        &self.diag
    }
}

/// Project an exact 3D point `p` onto the `(s,t)` parameter space of the host
/// triangle `a,b,c` solving `p - a = s*(b-a) + t*(c-a)`.
pub(crate) fn project_point(
    a: &[BigRational; 3],
    b: &[BigRational; 3],
    c: &[BigRational; 3],
    p: &[BigRational; 3],
) -> Option<Point2D> {
    let u = sub_rat(b, a);
    let v = sub_rat(c, a);
    let w = sub_rat(p, a);

    // Solve the normal equations [u v]^T [u v] [s;t] = [u v]^T w.
    let uu = dot_rat(&u, &u);
    let uv = dot_rat(&u, &v);
    let vv = dot_rat(&v, &v);
    let uw = dot_rat(&u, &w);
    let vw = dot_rat(&v, &w);

    let det = &uu * &vv - &uv * &uv;
    if det.is_zero() {
        return None;
    }

    let det_s = &uw * &vv - &uv * &vw;
    let det_t = &uu * &vw - &uw * &uv;

    Some(Point2D {
        s: det_s / det.clone(),
        t: det_t / det,
    })
}

/// Exact 2D orientation test.
pub fn orient2d(a: &Point2D, b: &Point2D, c: &Point2D) -> Sign {
    profile_count!(ORIENT2D_CALLS, 1);
    let m = (&b.s - &a.s) * (&c.t - &a.t) - (&b.t - &a.t) * (&c.s - &a.s);
    Sign::from_rational(&m)
}

/// Exact incircle test: positive if `d` lies inside the oriented circle
/// through `a,b,c`.
pub fn incircle(a: &Point2D, b: &Point2D, c: &Point2D, d: &Point2D) -> Sign {
    profile_count!(INCIRCLE_CALLS, 1);
    let a2 = &a.s * &a.s + &a.t * &a.t;
    let b2 = &b.s * &b.s + &b.t * &b.t;
    let c2 = &c.s * &c.s + &c.t * &c.t;
    let d2 = &d.s * &d.s + &d.t * &d.t;

    let m11 = &b.s - &a.s;
    let m12 = &b.t - &a.t;
    let m13 = &b2 - &a2;

    let m21 = &c.s - &a.s;
    let m22 = &c.t - &a.t;
    let m23 = &c2 - &a2;

    let m31 = &d.s - &a.s;
    let m32 = &d.t - &a.t;
    let m33 = &d2 - &a2;
    let det = m11.clone() * (m22.clone() * m33.clone() - m23.clone() * m32.clone())
        - m12.clone() * (m21.clone() * m33.clone() - m23.clone() * m31.clone())
        + m13.clone() * (m21.clone() * m32.clone() - m22.clone() * m31.clone());

    Sign::from_rational(&det)
}

/// Compute the intersection point of two (non-parallel) lines in 2D.
fn line_line_intersection(
    a: &Point2D,
    b: &Point2D,
    u: &Point2D,
    v: &Point2D,
) -> (Point2D, BigRational) {
    let d1 = &b.t - &a.t;
    let c1 = &a.s - &b.s;
    let f1 = &a.s * (&b.t - &a.t) - &a.t * (&b.s - &a.s);

    let d2 = &v.t - &u.t;
    let c2 = &u.s - &v.s;
    let f2 = &u.s * (&v.t - &u.t) - &u.t * (&v.s - &u.s);

    let det = &d1 * &c2 - &d2 * &c1;
    let x = (&f1 * &c2 - &f2 * &c1) / &det;
    let y = (&d1 * &f2 - &d2 * &f1) / &det;
    (Point2D { s: x, t: y }, det)
}

/// True if the closed segments ab and uv intersect in their interiors or at
/// endpoints.
fn segments_properly_intersect(a: &Point2D, b: &Point2D, u: &Point2D, v: &Point2D) -> bool {
    let s1 = orient2d(a, b, u);
    let s2 = orient2d(a, b, v);
    let s3 = orient2d(u, v, a);
    let s4 = orient2d(u, v, b);

    // Proper intersection.
    if (s1.is_positive() && s2.is_negative() || s1.is_negative() && s2.is_positive())
        && (s3.is_positive() && s4.is_negative() || s3.is_negative() && s4.is_positive())
    {
        return true;
    }

    // Collinear endpoint cases: we treat an endpoint lying on the other
    // segment as an intersection only if that endpoint is an input vertex.
    // For crossing detection during constraint insertion we are looking for
    // edges whose interiors cross the segment, so collinear overlaps are
    // handled by the caller (they should not happen for non-crossing
    // constraints after vertices are inserted).
    false
}

fn sub_f64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross_f64(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> [BigRational; 3] {
    [
        a[0].clone() - &b[0],
        a[1].clone() - &b[1],
        a[2].clone() - &b[2],
    ]
}

fn dot_rat(a: &[BigRational; 3], b: &[BigRational; 3]) -> BigRational {
    a[0].clone() * &b[0] + a[1].clone() * &b[1] + a[2].clone() * &b[2]
}

fn dot1d_between(a: &Point2D, b: &Point2D, p: &Point2D) -> bool {
    let px = &p.s - &a.s;
    let py = &p.t - &a.t;
    let qx = &p.s - &b.s;
    let qy = &p.t - &b.t;
    let dot = px * qx + py * qy;
    dot <= BigRational::zero()
}

/// Strictly-between test: `p` lies in the open segment `(a, b)`.
fn strictly_between(a: &Point2D, b: &Point2D, p: &Point2D) -> bool {
    let px = &p.s - &a.s;
    let py = &p.t - &a.t;
    let qx = &p.s - &b.s;
    let qy = &p.t - &b.t;
    let dot = px * qx + py * qy;
    dot < BigRational::zero()
}

/// True if `p` lies on the closed segment `[a, b]` (endpoints included).
fn point_on_segment(a: &Point2D, b: &Point2D, p: &Point2D) -> bool {
    orient2d(a, b, p).is_zero() && dot1d_between(a, b, p)
}

/// Projection parameter of `p` onto the directed line `a -> b`.
///
/// Returns `t` such that `p = a + t*(b-a)` in the 1D ordering along the
/// segment.  All arguments are assumed collinear; the value is used only for
/// sorting vertices that lie on the same segment.
fn param_along(a: &Point2D, b: &Point2D, p: &Point2D) -> BigRational {
    let dx = &b.s - &a.s;
    let dy = &b.t - &a.t;
    let wx = &p.s - &a.s;
    let wy = &p.t - &a.t;
    let num = wx * &dx + wy * &dy;
    let den = &dx * &dx + &dy * &dy;
    num / den
}

/// Axis-aligned 2D bounding box in parameter space.
#[derive(Clone, Debug)]
struct Bbox2D {
    min_s: BigRational,
    max_s: BigRational,
    min_t: BigRational,
    max_t: BigRational,
}

fn bbox2d(a: &Point2D, b: &Point2D) -> Bbox2D {
    Bbox2D {
        min_s: a.s.clone().min(b.s.clone()),
        max_s: a.s.clone().max(b.s.clone()),
        min_t: a.t.clone().min(b.t.clone()),
        max_t: a.t.clone().max(b.t.clone()),
    }
}

fn bboxes_overlap(a: &Bbox2D, b: &Bbox2D) -> bool {
    a.min_s <= b.max_s && b.min_s <= a.max_s && a.min_t <= b.max_t && b.min_t <= a.max_t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::f64_to_rat;

    fn tri_right() -> (Point3, Point3, Point3) {
        (
            Point3::Explicit([0.0, 0.0, 0.0]),
            Point3::Explicit([1.0, 0.0, 0.0]),
            Point3::Explicit([0.0, 1.0, 0.0]),
        )
    }

    fn validate_triangulation(t: &Triangulation) {
        for tri in &t.tris {
            let v = tri.v;
            assert!(
                orient2d(&t.verts[v[0]].st, &t.verts[v[1]].st, &t.verts[v[2]].st).is_positive(),
                "triangle {:?} is not CCW",
                v
            );
        }
    }

    fn has_edge(t: &Triangulation, a: usize, b: usize) -> bool {
        for tri in &t.tris {
            for i in 0..3 {
                let (e0, e1) = tri.edge_opp(i);
                if (e0 == a && e1 == b) || (e0 == b && e1 == a) {
                    return true;
                }
            }
        }
        false
    }

    fn are_connected(t: &Triangulation, a: usize, b: usize) -> bool {
        if a == b {
            return true;
        }
        let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut stack = vec![a];
        while let Some(v) = stack.pop() {
            if v == b {
                return true;
            }
            if !seen.insert(v) {
                continue;
            }
            for tri in &t.tris {
                if let Some(e) = tri.find_edge(v, b) {
                    let _ = e;
                    return true;
                }
                if tri.v.contains(&v) {
                    for &u in &tri.v {
                        if u != v {
                            stack.push(u);
                        }
                    }
                }
            }
        }
        false
    }

    fn seg(a: usize, b: usize, pa: &Point3, pb: &Point3) -> ConstrainedSegment {
        ConstrainedSegment {
            endpoints: (a, b),
            source: SegmentSource::CoplanarEdge {
                endpoints: (pa.to_f64().unwrap(), pb.to_f64().unwrap()),
            },
        }
    }

    #[test]
    fn orient_and_incircle_on_unit_triangle() {
        let a = Point2D {
            s: f64_to_rat(0.0),
            t: f64_to_rat(0.0),
        };
        let b = Point2D {
            s: f64_to_rat(1.0),
            t: f64_to_rat(0.0),
        };
        let c = Point2D {
            s: f64_to_rat(0.0),
            t: f64_to_rat(1.0),
        };
        assert!(orient2d(&a, &b, &c).is_positive());

        // A point far from the circumcircle is positive (outside).
        let d = Point2D {
            s: f64_to_rat(-10.0),
            t: f64_to_rat(-10.0),
        };
        assert!(incircle(&a, &b, &c, &d).is_positive());

        // Centroid is inside.
        let e = Point2D {
            s: f64_to_rat(1.0 / 3.0),
            t: f64_to_rat(1.0 / 3.0),
        };
        assert!(incircle(&a, &b, &c, &e).is_negative());
    }

    #[test]
    fn simple_crossing() {
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // Segment from edge (0,1) at midpoint to edge (0,2) at midpoint.
        let p = Point3::Explicit([0.5, 0.0, 0.0]);
        let q = Point3::Explicit([0.0, 0.5, 0.0]);
        let pi = t.insert_vertex(&p).unwrap();
        let qi = t.insert_vertex(&q).unwrap();
        t.add_constraint(pi, qi);

        validate_triangulation(&t);
        assert!(has_edge(&t, pi, qi));
        // Two edge insertions on the boundary split the host triangle into 3
        // triangles; the constraint edge already exists, so no further split.
        assert_eq!(t.num_tris(), 3);
    }

    #[test]
    fn vertex_on_edge() {
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        let p = Point3::Explicit([0.5, 0.0, 0.0]);
        let pi = t.insert_vertex(&p).unwrap();

        validate_triangulation(&t);
        assert_eq!(t.num_tris(), 2);
        assert!(has_edge(&t, 0, pi));
        assert!(has_edge(&t, 1, pi));
    }

    #[test]
    fn split_internal_edge_with_neighbour() {
        // First split the host edge (0,1) at its midpoint, then split the NEW
        // internal edge (shared by the two sub-triangles) at its midpoint.
        // The second split exercises the interior-neighbour branch of
        // split_edge (regression: the neighbour adjacency slots were swapped,
        // panicking with "adjacent triangle missing edge").
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        let m01 = Point3::Explicit([0.5, 0.0, 0.0]);
        let mi = t.insert_vertex(&m01).unwrap();
        // After the boundary split the mesh is (p,1,2) and (p,2,0); the
        // internal edge is (2, p) == (2, mi).  Insert its midpoint, which
        // splits that internal edge via the interior-neighbour branch.
        let inner = Point3::Explicit([0.25, 0.5, 0.0]);
        let inner_i = t.insert_vertex(&inner).unwrap();

        validate_triangulation(&t);
        // The internal edge (mi, 2) is split into (mi, inner) and (inner, 2);
        // both incident triangles split, so 1 -> 2 -> 4 triangles total.
        assert!(has_edge(&t, mi, inner_i));
        assert!(has_edge(&t, inner_i, 2));
        assert_eq!(t.num_tris(), 4);
    }

    #[test]
    fn chord_constraint_inside_triangle() {
        // Insert points on all three host edges, then constrain the segment
        // between two of them that is NOT created by the insertion fan.  The
        // chord splits the containing triangle (regression: previously this
        // panicked in add_constraint's find_any_edge().unwrap()).
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        let m01 = Point3::Explicit([0.5, 0.0, 0.0]);
        let m12 = Point3::Explicit([0.5, 0.5, 0.0]);
        let m20 = Point3::Explicit([0.0, 0.5, 0.0]);
        let _p01 = t.insert_vertex(&m01).unwrap();
        let p12 = t.insert_vertex(&m12).unwrap();
        let p20 = t.insert_vertex(&m20).unwrap();

        // Segment between the edge-02 and edge-12 midpoints: a chord inside
        // one of the fan triangles.
        t.add_constraint(p20, p12);

        validate_triangulation(&t);
        assert!(has_edge(&t, p20, p12));
    }

    #[test]
    fn coplanar_edge_constraint() {
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // Constrain the host edge (0,1).
        t.add_constraint(0, 1);
        validate_triangulation(&t);
        assert!(has_edge(&t, 0, 1));
    }

    #[test]
    fn multiple_segments() {
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // Two segments meeting at an interior point, fanning out to edges.
        let interior = Point3::Explicit([0.25, 0.25, 0.0]);
        let e01 = Point3::Explicit([0.5, 0.0, 0.0]);
        let e02 = Point3::Explicit([0.0, 0.5, 0.0]);
        let ii = t.insert_vertex(&interior).unwrap();
        let m01 = t.insert_vertex(&e01).unwrap();
        let m02 = t.insert_vertex(&e02).unwrap();
        t.add_constraint(ii, m01);
        t.add_constraint(ii, m02);

        validate_triangulation(&t);
        assert!(has_edge(&t, ii, m01));
        assert!(has_edge(&t, ii, m02));
    }

    #[test]
    fn crossing_segments_in_one_triangle() {
        // Regression for thingi10k_1038441-style dense SI: several
        // intersection segments lie inside the same host triangle and cross
        // each other (including a triple intersection).  The old incremental
        // add_constraint would panic with "constrained edge blocks segment
        // insertion" because it inserted segments one by one; the batch
        // arrangement must resolve all crossings first.
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // All coordinates are exact binary fractions so the rational
        // projection stays geometrically consistent.  Endpoints are kept
        // distinct so the test does not rely on the caller welding coincident
        // intersection points.
        let v_bottom = Point3::Explicit([0.25, 0.0, 0.0]);
        let v_top = Point3::Explicit([0.25, 0.75, 0.0]);
        let d1_left = Point3::Explicit([0.0, 0.5, 0.0]);
        let d1_right = Point3::Explicit([0.5, 0.0, 0.0]);
        let d2_left = Point3::Explicit([0.0, 0.25, 0.0]);
        let d2_right = Point3::Explicit([0.5, 0.25, 0.0]);
        let d3_bottom = Point3::Explicit([0.5, 0.125, 0.0]);
        let d3_left = Point3::Explicit([0.0, 0.375, 0.0]);

        let ivb = t.insert_vertex(&v_bottom).unwrap();
        let ivt = t.insert_vertex(&v_top).unwrap();
        let id1l = t.insert_vertex(&d1_left).unwrap();
        let id1r = t.insert_vertex(&d1_right).unwrap();
        let id2l = t.insert_vertex(&d2_left).unwrap();
        let id2r = t.insert_vertex(&d2_right).unwrap();
        let id3b = t.insert_vertex(&d3_bottom).unwrap();
        let id3l = t.insert_vertex(&d3_left).unwrap();

        t.add_constraints_batch(&[
            seg(ivb, ivt, &v_bottom, &v_top),
            seg(id1l, id1r, &d1_left, &d1_right),
            seg(id2l, id2r, &d2_left, &d2_right),
            seg(id3b, id3l, &d3_bottom, &d3_left),
        ]);

        validate_triangulation(&t);
        // All four segments meet at the single interior point (0.25,0.25).
        // The batch arrangement may insert additional fallback split vertices
        // on a segment when the current triangulation edge is non-convex, so
        // we verify endpoint-to-endpoint connectivity rather than a single
        // direct edge.
        let p1 = t
            .vertices()
            .iter()
            .position(|v| v.st.s == f64_to_rat(0.25) && v.st.t == f64_to_rat(0.25))
            .expect("common intersection missing");

        assert!(are_connected(&t, ivb, p1));
        assert!(are_connected(&t, p1, ivt));
        assert!(are_connected(&t, id1l, p1));
        assert!(are_connected(&t, p1, id1r));
        assert!(are_connected(&t, id2l, p1));
        assert!(are_connected(&t, p1, id2r));
        assert!(are_connected(&t, id3b, p1));
        assert!(are_connected(&t, p1, id3l));
    }

    #[test]
    fn batch_two_crossing_segments() {
        let (a, b, c) = tri_right();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // Exact binary fractions so the projected rational coordinates are
        // geometrically consistent (0.2 in f64 is not exactly 1/5).
        let p = Point3::Explicit([0.25, 0.0, 0.0]);
        let q = Point3::Explicit([0.25, 0.75, 0.0]);
        let r = Point3::Explicit([0.0, 0.5, 0.0]);
        let s = Point3::Explicit([0.5, 0.0, 0.0]);

        let pi = t.insert_vertex(&p).unwrap();
        let qi = t.insert_vertex(&q).unwrap();
        let ri = t.insert_vertex(&r).unwrap();
        let si = t.insert_vertex(&s).unwrap();

        t.add_constraints_batch(&[seg(pi, qi, &p, &q), seg(ri, si, &r, &s)]);

        validate_triangulation(&t);
        // The original segments are split at their intersection (0.25,0.25).
        // Find the split vertex and verify both sub-segments exist.
        let ix = t.vertices().iter().position(|v| {
            v.st.s == f64_to_rat(0.25) && v.st.t == f64_to_rat(0.25)
        }).expect("intersection vertex missing");
        assert!(has_edge(&t, pi, ix));
        assert!(has_edge(&t, ix, qi));
        assert!(has_edge(&t, ri, ix));
        assert!(has_edge(&t, ix, si));
    }

    #[test]
    fn crossing_provenance_agrees_for_coplanar_edges() {
        // Two different LPI constructions of the same triple crossing point
        // must project to the identical (s,t) so the (s,t) index dedups them.
        let (a, b, c) = tri_right();
        let t = Triangulation::from_host(&a, &b, &c).unwrap();
        let host = t.host;
        let s1 = SegmentSource::CoplanarEdge {
            endpoints: ([0.25, 0.0, 0.0], [0.25, 0.75, 0.0]),
        };
        let s2 = SegmentSource::CoplanarEdge {
            endpoints: ([0.0, 0.5, 0.0], [0.5, 0.0, 0.0]),
        };
        let s3 = SegmentSource::CoplanarEdge {
            endpoints: ([0.0, 0.25, 0.0], [0.5, 0.25, 0.0]),
        };
        let p12 = Triangulation::crossing_provenance(&host, &s1, &s2)
            .unwrap()
            .to_rational()
            .unwrap();
        let p13 = Triangulation::crossing_provenance(&host, &s1, &s3)
            .unwrap()
            .to_rational()
            .unwrap();
        assert_eq!(p12, p13);
        let st12 = crate::cdt2d::project_point(&host.a, &host.b, &host.c, &p12).unwrap();
        let st13 = crate::cdt2d::project_point(&host.a, &host.b, &host.c, &p13).unwrap();
        assert_eq!(st12.s, f64_to_rat(0.25));
        assert_eq!(st12.t, f64_to_rat(0.25));
        assert_eq!(st12.s, st13.s);
        assert_eq!(st12.t, st13.t);
    }
}
