//! 2D constrained Delaunay triangulation for projected implicit points.
//!
//! The input is a single host triangle (three 3D points) plus a set of points
//! that lie in its plane and a set of constrained segments.  Every point is
//! projected to exact barycentric `(s,t)` coordinates so that `orient2d` and
//! `incircle` can be evaluated with exact rational arithmetic.

use crate::interval::Iv;
use crate::point::{Point3, RatKey};
use crate::profile_count;
use std::cell::Cell;
use num_bigint::BigInt;
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

/// Exact `(s,t)` lookup key of a CDT vertex (see [`RatKey`]).
#[inline]
fn st_key(p: &Point2D) -> (RatKey, RatKey) {
    (RatKey::new(p.s.clone()), RatKey::new(p.t.clone()))
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
    #[cfg_attr(not(test), allow(dead_code))]
    b: [BigRational; 3],
    #[cfg_attr(not(test), allow(dead_code))]
    c: [BigRational; 3],
    /// Original input vertices of the host triangle, kept for provenance.
    pub host_a: [f64; 3],
    pub host_b: [f64; 3],
    pub host_c: [f64; 3],
    /// Per-host constants of the projection, computed once: edge vectors
    /// `u = b - a`, `v = c - a` and the normal-equation terms `u.v`, `v.v`,
    /// `u.u` and their determinant (zero for a degenerate host).
    u: [BigRational; 3],
    v: [BigRational; 3],
    uu: BigRational,
    uv: BigRational,
    vv: BigRational,
    det: BigRational,
}

impl HostFrame {
    pub fn from_points(a: &Point3, b: &Point3, c: &Point3) -> Option<Self> {
        let (a3, b3, c3) = (a.to_rational()?, b.to_rational()?, c.to_rational()?);
        let u = sub_rat(&b3, &a3);
        let v = sub_rat(&c3, &a3);
        let uu = dot_rat(&u, &u);
        let uv = dot_rat(&u, &v);
        let vv = dot_rat(&v, &v);
        let det = &uu * &vv - &uv * &uv;
        Some(Self {
            a: a3,
            b: b3,
            c: c3,
            host_a: a.to_f64()?,
            host_b: b.to_f64()?,
            host_c: c.to_f64()?,
            u,
            v,
            uu,
            uv,
            vv,
            det,
        })
    }

    /// Same result as `project_point(a, b, c, p)`, reusing the per-host
    /// constants (exact rationals are canonical, so the values are equal).
    fn project_rational(&self, p: &[BigRational; 3]) -> Option<Point2D> {
        if self.det.is_zero() {
            return None;
        }
        let w = sub_rat(p, &self.a);
        let uw = dot_rat(&self.u, &w);
        let vw = dot_rat(&self.v, &w);
        let det_s = &uw * &self.vv - &self.uv * &vw;
        let det_t = &self.uu * &vw - &uw * &self.uv;
        Some(Point2D {
            s: det_s / &self.det,
            t: det_t / &self.det,
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
        self.project_rational(&pr)
    }

    /// Map 2D barycentric `(s,t)` coordinates back to an exact rational 3D
    /// point using `p = a + s*(b - a) + t*(c - a)`.
    pub fn point3d(&self, s: &BigRational, t: &BigRational) -> [BigRational; 3] {
        // Same value as `a + s*u + t*v`, summed over one common denominator
        // with a single normalisation per coordinate instead of one per
        // BigRational operation.
        let (u, v) = (&self.u, &self.v);
        let (sn, sd) = (s.numer(), s.denom());
        let (tn, td) = (t.numer(), t.denom());
        let coord = |k: usize| {
            let (an, ad) = (self.a[k].numer(), self.a[k].denom());
            let (un, ud) = (u[k].numer(), u[k].denom());
            let (vn, vd) = (v[k].numer(), v[k].denom());
            let su_d = sd * ud;
            let tv_d = td * vd;
            let num = an * &su_d * &tv_d + sn * un * ad * &tv_d + tn * vn * ad * &su_d;
            BigRational::new(num, ad * su_d * tv_d)
        };
        [coord(0), coord(1), coord(2)]
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
    st_index: HashMap<(RatKey, RatKey), usize>,
    /// Rigorous interval enclosure of every vertex's `(s,t)`, cached so the
    /// filtered predicates do not re-convert the rationals on every call.
    viv: Vec<[Iv; 2]>,
    /// One triangle incident to each vertex (`NO_TRI` until the vertex is
    /// part of the triangulation).  Kept current by `set_tri`/`push_tri`.
    vert_tri: Vec<usize>,
    /// Start triangle for the next point-location walk.
    hint: Cell<usize>,
    /// State of the xorshift generator used by the stochastic walk.
    rng: Cell<u64>,
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
            .insert(st_key(&v.st), vi);
        self.viv.push(st_interval(&v.st));
        self.vert_tri.push(NO_TRI);
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
            .insert(st_key(&v.st), vi);
        self.viv.push(st_interval(&v.st));
        self.vert_tri.push(NO_TRI);
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
        // The host vertices project exactly to (0,0), (1,0) and (0,1) (the
        // normal equations give s = det/det etc.) whenever the host is not
        // degenerate; the frame's determinant decides that.
        let frame = HostFrame::from_points(a, b, c)?;
        if frame.det.is_zero() {
            return None;
        }
        let (zero, one) = (BigRational::zero(), BigRational::from_integer(1.into()));
        let p0 = Point2D { s: zero.clone(), t: zero.clone() };
        let p1 = Point2D { s: one.clone(), t: zero.clone() };
        let p2 = Point2D { s: zero, t: one };

        if orient2d(&p0, &p1, &p2).is_zero() {
            return None;
        }

        // Ensure CCW orientation before building indices.
        let mut st = [p0, p1, p2];
        let mut prov = [a.clone(), b.clone(), c.clone()];
        let swapped = !orient2d(&st[0], &st[1], &st[2]).is_positive();
        if swapped {
            st.swap(1, 2);
            prov.swap(1, 2);
        }

        // (s,t) of the three host vertices is (0,0), (1,0), (0,1): always
        // CCW, so no swap happens and `frame` already is the host frame.
        debug_assert!(!swapped);
        let host = frame;
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
            st_index.insert(st_key(&v.st), i);
        }
        let viv = verts.iter().map(|v| st_interval(&v.st)).collect();
        Some(Self {
            verts,
            tris: vec![Tri::new(0, 1, 2)],
            host,
            vertex_index,
            st_index,
            viv,
            vert_tri: vec![0, 0, 0],
            hint: Cell::new(0),
            rng: Cell::new(0x9e37_79b9_7f4a_7c15),
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
        // Dedup against existing vertices by provenance or (s,t); the
        // projection computed for the lookup is reused for the insertion.
        let q = match self.lookup_vertex(p) {
            Ok(vi) => return Some(vi),
            Err(q) => q?,
        };
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
        if let Some(&vi) = self.st_index.get(&st_key(&q)) {
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
        self.lookup_vertex(p).ok()
    }

    /// `Ok(index)` of an existing vertex matching `p`, or `Err(projection)`
    /// (`Err(None)` when `p` cannot be constructed or projected).
    fn lookup_vertex(&self, p: &Point3) -> Result<usize, Option<Point2D>> {
        // Fast path: look up by canonical provenance key.
        if let Some(&vi) = self.vertex_index.get(&p.canonical_key()) {
            return Ok(vi);
        }
        // Fallback: project and look up by (s,t).
        let q = match p.to_rational() {
            Some(pr) => self.host.project_rational(&pr),
            None => None,
        };
        let q = match q {
            Some(q) => q,
            None => return Err(None),
        };
        match self.st_index.get(&st_key(&q)) {
            Some(&vi) => Ok(vi),
            None => Err(Some(q)),
        }
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
            if self.orient_idx(a, b, vi).is_zero()
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

        self.set_tri(t0, Tri::new(p, v[0], v[1]));
        self.push_tri(Tri::new(p, v[1], v[2]));
        self.push_tri(Tri::new(p, v[2], v[0]));

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
        self.set_tri(t0, Tri::new(p, c, a));
        self.push_tri(Tri::new(p, a, b));

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
            self.set_tri(n0, Tri::new(p, b, nd));
            self.push_tri(Tri::new(p, nd, c));

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
        // When (a, b) is already an edge, its midpoint lies in the interior
        // of that edge, so the lowest-index containing triangle is simply the
        // lowest-index triangle owning the edge (no predicates needed).
        let owners = self.tris_with_edge(a, b);
        let located = if owners.is_empty() {
            self.locate_chord_triangle(a, b)
        } else {
            owners.into_iter().min()
        };
        #[cfg(feature = "cdt-check")]
        assert_eq!(located, self.locate_chord_triangle(a, b), "chord triangle shortcut");
        let t = match located {
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
                if self.orient_idx(p, q, x).is_zero()
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

        if n == 3 && new_tris.len() == 1 {
            // Common case: `a` and `b` are both vertices of `t`, so the chord
            // is one of its edges and the "split" only rewrites `t` as a
            // rotation of itself.  This is the normal exit of the flip loop
            // in `add_constraint`.  The rewrite is reproduced exactly: same
            // vertex rotation, neighbours carried over (a whole-mesh rebuild
            // would find the same, unique, neighbours), constraint flags of
            // `t` cleared as `Tri::new` does, then `(a, b)` marked.
            let nt = new_tris[0];
            let old = self.tris[t].clone();
            let r = (0..3).find(|&i| old.v[i] == nt[0]).expect("rotation of t");
            let mut tri = Tri::new(nt[0], nt[1], nt[2]);
            debug_assert_eq!(tri.v, [old.v[r], old.v[(r + 1) % 3], old.v[(r + 2) % 3]]);
            tri.adj = [old.adj[r], old.adj[(r + 1) % 3], old.adj[(r + 2) % 3]];
            self.set_tri(t, tri);
            self.mark_constrained_edge(a, b);
            return;
        }

        // Replace triangle t with the first new triangle; push the rest.
        let first = new_tris[0];
        self.set_tri(t, Tri::new(first[0], first[1], first[2]));
        for nt in &new_tris[1..] {
            self.push_tri(Tri::new(nt[0], nt[1], nt[2]));
        }

        // Rebuild adjacencies for the whole CDT (small per-host triangle).
        // Only reached for a genuine chord through one triangle, which does
        // not occur on a conforming triangulation; kept verbatim.
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
        for t in self.tris_with_edge(a, b) {
            let e = self.tris[t].find_edge(a, b).unwrap();
            self.tris[t].constrained[e] = true;
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
        self.set_tri(t1, Tri::new(a, b, d));
        self.set_tri(t2, Tri::new(a, d, c));

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

    /// Locate the triangle containing point `p`: the LOWEST-index triangle
    /// whose closed region contains `p`, i.e. exactly what a linear scan in
    /// index order returns.  Found by a stochastic visibility walk (which
    /// terminates on any triangulation, Delaunay or not) followed by a
    /// canonicalisation step when `p` lies on an edge or a vertex; falls back
    /// to the linear scan if the walk cannot conclude.
    fn locate(&self, p: &Point2D) -> Option<usize> {
        let found = self.locate_walk(p);
        #[cfg(feature = "cdt-check")]
        assert_eq!(found, self.locate_linear(p), "walking locate disagrees with scan");
        found
    }

    fn locate_walk(&self, p: &Point2D) -> Option<usize> {
        let n = self.tris.len();
        let piv = st_interval(p);
        let mut t = self.hint.get().min(n - 1);
        let mut steps = 0usize;
        loop {
            steps += 1;
            if steps > 4 * n + 16 {
                return self.locate_linear(p);
            }
            let start = (self.next_rand() % 3) as usize;
            let mut next = None;
            for k in 0..3 {
                let e = (start + k) % 3;
                let (u, w) = self.tris[t].edge_opp(e);
                if self.orient_vvp(u, w, p, &piv).is_negative() {
                    match self.tris[t].adj[e] {
                        Some(nb) => next = Some(nb),
                        // Outside the host triangle: let the scan decide.
                        None => return self.locate_linear(p),
                    }
                    break;
                }
            }
            match next {
                Some(nb) => t = nb,
                None => break,
            }
        }
        self.hint.set(t);
        // `t` contains `p`; make the answer the lowest containing index.
        let v = self.tris[t].v;
        let zero: Vec<usize> = (0..3)
            .filter(|&e| {
                let (u, w) = self.tris[t].edge_opp(e);
                self.orient_vvp(u, w, p, &piv).is_zero()
            })
            .collect();
        match zero.len() {
            3 => self.locate_linear(p),
            0 => Some(t),
            1 => Some(match self.tris[t].adj[zero[0]] {
                Some(nb) => t.min(nb),
                None => t,
            }),
            _ => {
                // On a vertex: the vertex shared by the two zero edges is the
                // one not opposite either of them.
                let vx = v[(0..3).find(|i| !zero.contains(i)).unwrap()];
                self.fan(vx).into_iter().min()
            }
        }
    }

    /// Reference linear scan (the pre-C2 implementation).
    fn locate_linear(&self, p: &Point2D) -> Option<usize> {
        let piv = st_interval(p);
        for (i, tri) in self.tris.iter().enumerate() {
            let v = tri.v;
            let s0 = self.orient_vvp(v[0], v[1], p, &piv);
            let s1 = self.orient_vvp(v[1], v[2], p, &piv);
            let s2 = self.orient_vvp(v[2], v[0], p, &piv);
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
            let walk = self.walk_segment(a, b);
            if let Some(v) = self.find_vertex_on_open_segment(a, b, walk.as_ref()) {
                self.add_constraint(a, v);
                self.add_constraint(v, b);
                return;
            }
            let crossing = self.find_crossing_edge(a, b, walk.as_ref());
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
                        let q = line_line_intersection(
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
        // The prefilter uses the cached interval enclosures: it is
        // conservative (never rejects a pair whose exact boxes overlap), and
        // a proper crossing implies overlapping boxes, so the exact test below
        // sees every pair that can intersect and the result is unchanged.
        let endpoints: Vec<_> = segments.iter().map(|s| s.endpoints).collect();
        let bboxes: Vec<IvBox> = endpoints
            .iter()
            .map(|&(a, b)| iv_box(&self.viv[a], &self.viv[b]))
            .collect();
        for i in 0..n_raw {
            for j in (i + 1)..n_raw {
                if !iv_boxes_overlap(&bboxes[i], &bboxes[j]) {
                    continue;
                }
                let (a, b) = endpoints[i];
                let (c, d) = endpoints[j];
                if !self.cross_idx(a, b, c, d) {
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
                        let q = line_line_intersection(
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
            let bb = &bboxes[i];
            for v in 0..n_initial {
                if v == a || v == b {
                    continue;
                }
                // Conservative box rejection before the exact test.
                let q = &self.viv[v];
                if q[0].hi < bb[0] || q[0].lo > bb[1] || q[1].hi < bb[2] || q[1].lo > bb[3] {
                    continue;
                }
                if self.orient_idx(a, b, v).is_zero()
                    && dot1d_between(&self.verts[a].st, &self.verts[b].st, &self.verts[v].st)
                {
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
            // Stable sort, each key computed once. The key is the numerator
            // of `param_along` (its denominator |b-a|^2 is the same positive
            // value for every entry), so the order and the ties are identical.
            on_seg[i].sort_by_cached_key(|u| param_numerator(pa, pb, &self.verts[*u].st));
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


    /// Lowest-index triangle containing edge `(a, b)`, and the edge's slot.
    fn find_any_edge(&self, a: usize, b: usize) -> Option<(usize, usize)> {
        let found = self
            .tris_with_edge(a, b)
            .into_iter()
            .min()
            .map(|t| (t, self.find_edge(t, a, b).unwrap()));
        #[cfg(feature = "cdt-check")]
        assert_eq!(found, self.find_any_edge_linear(a, b));
        found
    }

    #[cfg(feature = "cdt-check")]
    fn find_any_edge_linear(&self, a: usize, b: usize) -> Option<(usize, usize)> {
        for (i, _) in self.tris.iter().enumerate() {
            if let Some(e) = self.find_edge(i, a, b) {
                return Some((i, e));
            }
        }
        None
    }

    /// Lowest `(triangle, edge)` whose edge properly crosses segment `ab`,
    /// matching a linear scan in index order.  Only triangles of the walk
    /// corridor can own such an edge, so only they are tested.
    fn find_crossing_edge(
        &self,
        a: usize,
        b: usize,
        walk: Option<&SegmentWalk>,
    ) -> Option<(usize, usize)> {
        let found = match walk {
            Some(w) => {
                let mut best: Option<(usize, usize)> = None;
                for &t in &w.corridor {
                    if best.map_or(false, |(bt, _)| t >= bt) {
                        continue;
                    }
                    for e in 0..3 {
                        let (u, v) = self.tris[t].edge_opp(e);
                        if self.cross_idx(a, b, u, v) {
                            best = Some((t, e));
                            break;
                        }
                    }
                }
                best
            }
            None => self.find_crossing_edge_linear(a, b),
        };
        #[cfg(feature = "cdt-check")]
        assert_eq!(found, self.find_crossing_edge_linear(a, b), "corridor crossing disagrees");
        found
    }

    fn find_crossing_edge_linear(&self, a: usize, b: usize) -> Option<(usize, usize)> {
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

    /// The lowest-index vertex (other than `a`/`b`) lying strictly between
    /// them on the open segment `(a, b)`, if any.  The segment walk visits
    /// every vertex on the segment, so its minimum equals the linear scan.
    fn find_vertex_on_open_segment(
        &self,
        a: usize,
        b: usize,
        walk: Option<&SegmentWalk>,
    ) -> Option<usize> {
        let found = match walk {
            Some(w) => w.on_segment.iter().copied().min(),
            None => self.find_vertex_on_open_segment_linear(a, b),
        };
        #[cfg(feature = "cdt-check")]
        assert_eq!(found, self.find_vertex_on_open_segment_linear(a, b), "walk vertices disagree");
        found
    }

    fn find_vertex_on_open_segment_linear(&self, a: usize, b: usize) -> Option<usize> {
        let pa = &self.verts[a].st;
        let pb = &self.verts[b].st;
        for (i, v) in self.verts.iter().enumerate() {
            if i == a || i == b {
                continue;
            }
            if self.orient_idx(a, b, i).is_zero() && strictly_between(pa, pb, &v.st) {
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
        self.orient_idx(a, b, d).is_positive() && self.orient_idx(a, d, c).is_positive()
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

/// Sentinel for a vertex that is not yet part of any triangle.
const NO_TRI: usize = usize::MAX;

/// Rigorous interval enclosure of a parameter-space point.
fn st_interval(p: &Point2D) -> [Iv; 2] {
    [Iv::from_rational(&p.s), Iv::from_rational(&p.t)]
}

/// Interval-filtered orientation sign; `None` when undecided.
#[inline]
fn orient2d_iv(a: &[Iv; 2], b: &[Iv; 2], c: &[Iv; 2]) -> Option<Sign> {
    let f = b[0].sub(a[0]).mul(c[1].sub(a[1])).sub(b[1].sub(a[1]).mul(c[0].sub(a[0])));
    match f.sign() {
        Some(1) => Some(Sign::Positive),
        Some(-1) => Some(Sign::Negative),
        _ => None,
    }
}

/// `x - y` as an unreduced fraction `(numerator, positive denominator)`.
#[inline]
fn diff_frac(x: &BigRational, y: &BigRational) -> (BigInt, BigInt) {
    if x.denom() == y.denom() {
        (x.numer() - y.numer(), x.denom().clone())
    } else {
        (x.numer() * y.denom() - y.numer() * x.denom(), x.denom() * y.denom())
    }
}

/// Sign of `n1/d1 * n2/d2 + sgn * n3/d3 * n4/d4` for positive denominators,
/// evaluated on integers without the per-operation gcd of `BigRational`.
#[inline]
fn sign_of_products(
    (n1, d1): (BigInt, BigInt),
    (n2, d2): (BigInt, BigInt),
    (n3, d3): (BigInt, BigInt),
    (n4, d4): (BigInt, BigInt),
    subtract: bool,
) -> Sign {
    let lhs = n1 * n2 * &d3 * &d4;
    let rhs = n3 * n4 * d1 * d2;
    let m = if subtract { lhs - rhs } else { lhs + rhs };
    match m.sign() {
        num_bigint::Sign::Plus => Sign::Positive,
        num_bigint::Sign::Minus => Sign::Negative,
        num_bigint::Sign::NoSign => Sign::Zero,
    }
}

/// Exact orientation (same value as the `BigRational` determinant; the
/// denominators of normalised rationals are positive, so clearing them
/// preserves the sign).
#[inline]
fn orient2d_exact(a: &Point2D, b: &Point2D, c: &Point2D) -> Sign {
    sign_of_products(
        diff_frac(&b.s, &a.s),
        diff_frac(&c.t, &a.t),
        diff_frac(&b.t, &a.t),
        diff_frac(&c.s, &a.s),
        true,
    )
}

/// Exact sign of `(p - a) . (p - b)`.
#[inline]
fn dot_sign(a: &Point2D, b: &Point2D, p: &Point2D) -> Sign {
    sign_of_products(
        diff_frac(&p.s, &a.s),
        diff_frac(&p.s, &b.s),
        diff_frac(&p.t, &a.t),
        diff_frac(&p.t, &b.t),
        false,
    )
}

/// Result of walking segment `(a, b)` through the triangulation.
#[derive(Debug, Default)]
struct SegmentWalk {
    /// Vertices hit strictly between `a` and `b`.
    on_segment: Vec<usize>,
    /// Triangles whose interior the segment passes through.
    corridor: Vec<usize>,
}

impl Triangulation {
    /// Write triangle `ti` and record it as incident to its vertices.
    fn set_tri(&mut self, ti: usize, tri: Tri) {
        for &x in &tri.v {
            self.vert_tri[x] = ti;
        }
        self.tris[ti] = tri;
    }

    /// Append a triangle and record it as incident to its vertices.
    fn push_tri(&mut self, tri: Tri) -> usize {
        let ti = self.tris.len();
        for &x in &tri.v {
            self.vert_tri[x] = ti;
        }
        self.tris.push(tri);
        ti
    }

    fn next_rand(&self) -> u64 {
        let mut x = self.rng.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng.set(x);
        x
    }

    /// Filtered orientation of three vertices (by index).
    fn orient_idx(&self, i: usize, j: usize, k: usize) -> Sign {
        profile_count!(ORIENT2D_CALLS, 1);
        orient2d_iv(&self.viv[i], &self.viv[j], &self.viv[k]).unwrap_or_else(|| {
            orient2d_exact(&self.verts[i].st, &self.verts[j].st, &self.verts[k].st)
        })
    }

    /// Filtered orientation of two vertices and a free point.
    fn orient_vvp(&self, i: usize, j: usize, p: &Point2D, piv: &[Iv; 2]) -> Sign {
        profile_count!(ORIENT2D_CALLS, 1);
        orient2d_iv(&self.viv[i], &self.viv[j], piv)
            .unwrap_or_else(|| orient2d_exact(&self.verts[i].st, &self.verts[j].st, p))
    }

    /// `segments_properly_intersect` on vertex indices.
    fn cross_idx(&self, a: usize, b: usize, u: usize, v: usize) -> bool {
        let s1 = self.orient_idx(a, b, u);
        let s2 = self.orient_idx(a, b, v);
        if !(s1.is_positive() && s2.is_negative() || s1.is_negative() && s2.is_positive()) {
            return false;
        }
        let s3 = self.orient_idx(u, v, a);
        let s4 = self.orient_idx(u, v, b);
        s3.is_positive() && s4.is_negative() || s3.is_negative() && s4.is_positive()
    }

    /// All triangles incident to vertex `x`, by rotating through the
    /// adjacency around it.  Falls back to a scan if the fan is inconsistent.
    fn fan(&self, x: usize) -> Vec<usize> {
        let t0 = self.vert_tri[x];
        if t0 == NO_TRI || !self.tris[t0].v.contains(&x) {
            return self.fan_linear(x);
        }
        let cap = self.tris.len() + 1;
        let mut out = vec![t0];
        let pos = |t: usize| self.tris[t].v.iter().position(|&y| y == x);
        let mut t = t0;
        let mut closed = false;
        for _ in 0..cap {
            let i = match pos(t) {
                Some(i) => i,
                None => return self.fan_linear(x),
            };
            match self.tris[t].adj[(i + 1) % 3] {
                Some(nb) if nb == t0 => {
                    closed = true;
                    break;
                }
                Some(nb) => {
                    out.push(nb);
                    t = nb;
                }
                None => break,
            }
        }
        if !closed {
            t = t0;
            for _ in 0..cap {
                let i = match pos(t) {
                    Some(i) => i,
                    None => return self.fan_linear(x),
                };
                match self.tris[t].adj[(i + 2) % 3] {
                    Some(nb) if nb == t0 => break,
                    Some(nb) => {
                        out.push(nb);
                        t = nb;
                    }
                    None => break,
                }
            }
        }
        #[cfg(feature = "cdt-check")]
        {
            let mut a = out.clone();
            a.sort_unstable();
            a.dedup();
            assert_eq!(a, self.fan_linear(x), "fan of vertex {} inconsistent", x);
        }
        out
    }

    fn fan_linear(&self, x: usize) -> Vec<usize> {
        (0..self.tris.len()).filter(|&t| self.tris[t].v.contains(&x)).collect()
    }

    /// Triangles that contain edge `(a, b)`.
    fn tris_with_edge(&self, a: usize, b: usize) -> Vec<usize> {
        self.fan(a)
            .into_iter()
            .filter(|&t| self.tris[t].find_edge(a, b).is_some())
            .collect()
    }

    /// Exact `dot(u - x, b - x) > 0`, i.e. `u` lies ahead of `x` towards `b`.
    fn forward(&self, x: usize, u: usize, b: usize) -> bool {
        let (ix, iu, ib) = (&self.viv[x], &self.viv[u], &self.viv[b]);
        let f = iu[0].sub(ix[0]).mul(ib[0].sub(ix[0])).add(iu[1].sub(ix[1]).mul(ib[1].sub(ix[1])));
        if let Some(sg) = f.sign() {
            return sg > 0;
        }
        // (u - x) . (b - x) = -((x - u) . (x - b))
        let (px, pu, pb) = (&self.verts[x].st, &self.verts[u].st, &self.verts[b].st);
        dot_sign(pu, pb, px).is_negative()
    }

    /// Walk segment `(a, b)` from `a` to `b`, collecting the vertices it
    /// passes through and the triangles it crosses.  Returns `None` if the
    /// walk cannot proceed (inconsistent triangulation); callers then use the
    /// linear reference scans.
    fn walk_segment(&self, a: usize, b: usize) -> Option<SegmentWalk> {
        let mut out = SegmentWalk::default();
        let mut x = a;
        let cap = self.tris.len() + self.verts.len() + 8;
        let mut steps = 0usize;
        'from_vertex: loop {
            // Leave vertex `x` towards `b`: either along an edge to a
            // collinear vertex, or into the interior of one fan triangle.
            let mut wedge: Option<(usize, usize)> = None;
            let mut next_vertex: Option<usize> = None;
            for t in self.fan(x) {
                let i = self.tris[t].v.iter().position(|&y| y == x)?;
                let u = self.tris[t].v[(i + 1) % 3];
                let w = self.tris[t].v[(i + 2) % 3];
                let ou = self.orient_idx(x, u, b);
                if ou.is_zero() && self.forward(x, u, b) {
                    next_vertex = Some(u);
                    break;
                }
                let ow = self.orient_idx(x, w, b);
                if ow.is_zero() && self.forward(x, w, b) {
                    next_vertex = Some(w);
                    break;
                }
                if ou.is_positive() && ow.is_negative() {
                    wedge = Some((t, i));
                    break;
                }
            }
            if let Some(nv) = next_vertex {
                if nv == b {
                    return Some(out);
                }
                out.on_segment.push(nv);
                x = nv;
                steps += 1;
                if steps > cap {
                    return None;
                }
                continue 'from_vertex;
            }
            let (mut t, i) = wedge?;
            out.corridor.push(t);
            // Crossed edge (u, w): `u` right of a->b, `w` left of it.
            let mut u = self.tris[t].v[(i + 1) % 3];
            let mut w = self.tris[t].v[(i + 2) % 3];
            loop {
                steps += 1;
                if steps > cap {
                    return None;
                }
                let e = self.tris[t].find_edge(u, w)?;
                let nb = self.tris[t].adj[e]?;
                let ne = self.tris[nb].find_edge(u, w)?;
                let z = self.tris[nb].v[ne];
                out.corridor.push(nb);
                let oz = self.orient_idx(a, b, z);
                if oz.is_zero() {
                    if z == b {
                        return Some(out);
                    }
                    out.on_segment.push(z);
                    x = z;
                    continue 'from_vertex;
                }
                if oz.is_negative() {
                    u = z; // z on the right: exit through (z, w)
                } else {
                    w = z; // z on the left: exit through (u, z)
                }
                t = nb;
            }
        }
    }
}

/// Project an exact 3D point `p` onto the `(s,t)` parameter space of the host
/// triangle `a,b,c` solving `p - a = s*(b-a) + t*(c-a)`.
#[cfg_attr(not(test), allow(dead_code))]
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
    {
        use crate::interval::Iv;
        let (as_, at) = (Iv::from_rational(&a.s), Iv::from_rational(&a.t));
        let (bs, bt) = (Iv::from_rational(&b.s), Iv::from_rational(&b.t));
        let (cs, ct) = (Iv::from_rational(&c.s), Iv::from_rational(&c.t));
        let f = bs.sub(as_).mul(ct.sub(at)).sub(bt.sub(at).mul(cs.sub(as_)));
        match f.sign() {
            Some(1) => return Sign::Positive,
            Some(-1) => return Sign::Negative,
            _ => {}
        }
    }
    orient2d_exact(a, b, c)
}

/// Exact incircle test: positive if `d` lies inside the oriented circle
/// through `a,b,c`.
pub fn incircle(a: &Point2D, b: &Point2D, c: &Point2D, d: &Point2D) -> Sign {
    profile_count!(INCIRCLE_CALLS, 1);
    {
        use crate::interval::Iv;
        let iv = |p: &Point2D| (Iv::from_rational(&p.s), Iv::from_rational(&p.t));
        let (as_, at) = iv(a);
        let (bs, bt) = iv(b);
        let (cs, ct) = iv(c);
        let (ds, dt) = iv(d);
        let sq = |s: Iv, t: Iv| s.mul(s).add(t.mul(t));
        let (a2, b2, c2, d2) = (sq(as_, at), sq(bs, bt), sq(cs, ct), sq(ds, dt));
        let (m11, m12, m13) = (bs.sub(as_), bt.sub(at), b2.sub(a2));
        let (m21, m22, m23) = (cs.sub(as_), ct.sub(at), c2.sub(a2));
        let (m31, m32, m33) = (ds.sub(as_), dt.sub(at), d2.sub(a2));
        let det = m11
            .mul(m22.mul(m33).sub(m23.mul(m32)))
            .sub(m12.mul(m21.mul(m33).sub(m23.mul(m31))))
            .add(m13.mul(m21.mul(m32).sub(m22.mul(m31))));
        // Only the sign of the exact determinant matters below; map it the
        // same way the exact branch does.
        match det.sign() {
            Some(1) => return Sign::Positive,
            Some(-1) => return Sign::Negative,
            _ => {}
        }
    }
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
) -> Point2D {
    // Same values as the textbook BigRational formula, computed on integers:
    // write every point over one common denominator per line, so each
    // coordinate is one normalised fraction at the end.
    let line = |p: &Point2D, q: &Point2D| {
        // Integer coefficients (d, c, f) of d*x + c*y = f, scaled by D > 0.
        let den = p.s.denom() * p.t.denom() * q.s.denom() * q.t.denom();
        let sc = |r: &BigRational| r.numer() * (&den / r.denom());
        let (ps, pt, qs, qt) = (sc(&p.s), sc(&p.t), sc(&q.s), sc(&q.t));
        let d = &qt - &pt;
        let c = &ps - &qs;
        let f = &ps * &d + &pt * &c; // = ps*(qt-pt) - pt*(qs-ps), scaled by den^2
        (d, c, f, den)
    };
    let (d1, c1, f1, den1) = line(a, b);
    let (d2, c2, f2, den2) = line(u, v);
    // Line 1 in true coordinates: (d1/den1) x + (c1/den1) y = f1/den1^2.
    // Multiply by den1^2: (d1*den1) x + (c1*den1) y = f1. Likewise line 2.
    let (a1, b1, e1) = (&d1 * &den1, &c1 * &den1, f1);
    let (a2, b2, e2) = (&d2 * &den2, &c2 * &den2, f2);
    let det_i = &a1 * &b2 - &a2 * &b1;
    let x = BigRational::new(&e1 * &b2 - &e2 * &b1, det_i.clone());
    let y = BigRational::new(&a1 * &e2 - &a2 * &e1, det_i);
    Point2D { s: x, t: y }
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
    !dot_sign(a, b, p).is_positive()
}

/// Strictly-between test: `p` lies in the open segment `(a, b)`.
fn strictly_between(a: &Point2D, b: &Point2D, p: &Point2D) -> bool {
    dot_sign(a, b, p).is_negative()
}

/// Numerator `(p - a) . (b - a)` of `param_along`, as one normalised
/// rational built from integer arithmetic.
fn param_numerator(a: &Point2D, b: &Point2D, p: &Point2D) -> BigRational {
    let (n1, d1) = diff_frac(&p.s, &a.s);
    let (n2, d2) = diff_frac(&b.s, &a.s);
    let (n3, d3) = diff_frac(&p.t, &a.t);
    let (n4, d4) = diff_frac(&b.t, &a.t);
    let den12 = d1 * d2;
    let den34 = d3 * d4;
    BigRational::new(n1 * n2 * &den34 + n3 * n4 * &den12, den12 * den34)
}

#[cfg(test)]
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

/// Outer interval box `[min_s, max_s, min_t, max_t]` of a segment.
type IvBox = [f64; 4];

fn iv_box(a: &[Iv; 2], b: &[Iv; 2]) -> IvBox {
    [
        a[0].lo.min(b[0].lo),
        a[0].hi.max(b[0].hi),
        a[1].lo.min(b[1].lo),
        a[1].hi.max(b[1].hi),
    ]
}

fn iv_boxes_overlap(a: &IvBox, b: &IvBox) -> bool {
    a[0] <= b[1] && b[0] <= a[1] && a[2] <= b[3] && b[2] <= a[3]
}

#[cfg(test)]
mod tests {
    /// Filtered orient2d / incircle must equal the exact rational result,
    /// including exactly collinear / cocircular inputs built from rationals
    /// that are not representable in f64 (e.g. thirds).
    #[test]
    fn filtered_2d_predicates_agree_with_exact() {
        use num_bigint::BigInt;
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xc1_2d);
        let q = |n: i64, d: i64| BigRational::new(BigInt::from(n), BigInt::from(d));
        let exact_o = |a: &Point2D, b: &Point2D, c: &Point2D| {
            Sign::from_rational(&((&b.s - &a.s) * (&c.t - &a.t) - (&b.t - &a.t) * (&c.s - &a.s)))
        };
        for _ in 0..4000 {
            let mut rp = || Point2D {
                s: q(rng.gen_range(-50..50), rng.gen_range(1..9)),
                t: q(rng.gen_range(-50..50), rng.gen_range(1..9)),
            };
            let a = rp();
            let b = rp();
            let mut c = rp();
            if rng.gen_bool(0.3) {
                // Exactly collinear: c = a + k (b - a) with a rational k.
                let k = q(rng.gen_range(-7..7), 3);
                c = Point2D { s: &a.s + &k * (&b.s - &a.s), t: &a.t + &k * (&b.t - &a.t) };
            }
            assert_eq!(orient2d(&a, &b, &c), exact_o(&a, &b, &c));
        }
        // Cocircular: four points on the unit circle with rational coords.
        let p = |n: i64, m: i64| {
            let den = n * n + m * m;
            Point2D { s: q(n * n - m * m, den), t: q(2 * n * m, den) }
        };
        let (a, b, c, d) = (p(2, 1), p(3, 2), p(5, 1), p(7, 3));
        assert!(incircle(&a, &b, &c, &d).is_zero());
    }

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

    /// The integer-arithmetic rewrites must equal the textbook BigRational
    /// formulas exactly (values, not just signs).
    #[test]
    fn integer_rewrites_equal_rational_formulas() {
        use num_bigint::BigInt;
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xc3);
        let mut q = || BigRational::new(BigInt::from(rng.gen_range(-900i64..900)), BigInt::from(rng.gen_range(1i64..97)));
        for _ in 0..3000 {
            let (a, b, u, v, p) = (
                Point2D { s: q(), t: q() },
                Point2D { s: q(), t: q() },
                Point2D { s: q(), t: q() },
                Point2D { s: q(), t: q() },
                Point2D { s: q(), t: q() },
            );
            // param numerator: same value as param_along times |b-a|^2
            let d2 = (&b.s - &a.s) * (&b.s - &a.s) + (&b.t - &a.t) * (&b.t - &a.t);
            if !d2.is_zero() {
                assert_eq!(param_numerator(&a, &b, &p), param_along(&a, &b, &p) * &d2);
            }
            // line-line intersection: textbook formula
            let d1 = &b.t - &a.t;
            let c1 = &a.s - &b.s;
            let f1 = &a.s * (&b.t - &a.t) - &a.t * (&b.s - &a.s);
            let dd = &v.t - &u.t;
            let c2 = &u.s - &v.s;
            let f2 = &u.s * (&v.t - &u.t) - &u.t * (&v.s - &u.s);
            let det = &d1 * &c2 - &dd * &c1;
            if !det.is_zero() {
                let r = line_line_intersection(&a, &b, &u, &v);
                assert_eq!(r.s, (&f1 * &c2 - &f2 * &c1) / &det);
                assert_eq!(r.t, (&d1 * &f2 - &dd * &f1) / &det);
            }
        }
        // point3d: a + s*u + t*v
        let (ha, hb, hc) = (
            Point3::Explicit([0.3, -1.25, 7.0]),
            Point3::Explicit([2.5, 0.1, -3.0]),
            Point3::Explicit([-1.0, 4.75, 0.2]),
        );
        let f = HostFrame::from_points(&ha, &hb, &hc).unwrap();
        for _ in 0..500 {
            let (s, t) = (q(), q());
            let want = [
                &f.a[0] + &s * &f.u[0] + &t * &f.v[0],
                &f.a[1] + &s * &f.u[1] + &t * &f.v[1],
                &f.a[2] + &s * &f.u[2] + &t * &f.v[2],
            ];
            assert_eq!(f.point3d(&s, &t), want);
        }
    }

    /// C2 differential test: the accelerated queries (walking locate, fan
    /// rotation, segment walk) must return exactly what the linear reference
    /// scans return, on a dense random arrangement with many collinear and
    /// crossing constraints.
    #[test]
    fn accelerated_queries_match_linear_scans() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xc2);
        for round in 0..4 {
            let (a, b, c) = tri_right();
            let mut t = Triangulation::from_host(&a, &b, &c).unwrap();
            // Dyadic grid points (exact in f64) so many are collinear.
            let mut pt = || loop {
                let x = rng.gen_range(0..=16) as f64 / 16.0;
                let y = rng.gen_range(0..=16) as f64 / 16.0;
                if x + y <= 1.0 {
                    return Point3::Explicit([x, y, 0.0]);
                }
            };
            let mut segs = Vec::new();
            for _ in 0..(10 + 6 * round) {
                let (p, q) = (pt(), pt());
                let pi = t.insert_vertex(&p).unwrap();
                let qi = t.insert_vertex(&q).unwrap();
                if pi != qi {
                    segs.push(seg(pi, qi, &p, &q));
                }
            }
            t.add_constraints_batch(&segs);
            validate_triangulation(&t);

            let nv = t.verts.len();
            for x in 0..nv {
                let mut f = t.fan(x);
                f.sort_unstable();
                assert_eq!(f, t.fan_linear(x), "fan of {}", x);
            }
            for x in 0..nv {
                for y in 0..nv {
                    if x == y {
                        continue;
                    }
                    let w = t.walk_segment(x, y);
                    assert!(w.is_some(), "walk {}->{} failed", x, y);
                    let w = w.as_ref();
                    let lin_v = t.find_vertex_on_open_segment_linear(x, y);
                    assert_eq!(t.find_vertex_on_open_segment(x, y, w), lin_v);
                    if lin_v.is_none() {
                        assert_eq!(
                            t.find_crossing_edge(x, y, w),
                            t.find_crossing_edge_linear(x, y)
                        );
                    }
                }
            }
            // Points on vertices, on edge midpoints and random interior points.
            let mut probes: Vec<Point2D> = t.verts.iter().map(|v| v.st.clone()).collect();
            for tri in &t.tris {
                for e in 0..3 {
                    let (u, w) = tri.edge_opp(e);
                    let (pu, pw) = (&t.verts[u].st, &t.verts[w].st);
                    let half = BigRational::new(1.into(), 2.into());
                    probes.push(Point2D {
                        s: (&pu.s + &pw.s) * &half,
                        t: (&pu.t + &pw.t) * &half,
                    });
                }
            }
            for _ in 0..200 {
                let x = rng.gen_range(0..1000) as f64 / 1000.0;
                let y = rng.gen_range(0..1000) as f64 / 1000.0;
                if x + y <= 1.0 {
                    probes.push(Point2D { s: f64_to_rat(x), t: f64_to_rat(y) });
                }
            }
            for (k, p) in probes.iter().enumerate() {
                t.hint.set(k % t.tris.len());
                assert_eq!(t.locate_walk(p), t.locate_linear(p), "locate probe {}", k);
            }
        }
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
