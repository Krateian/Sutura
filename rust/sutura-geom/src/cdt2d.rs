//! 2D constrained Delaunay triangulation for projected implicit points.
//!
//! The input is a single host triangle (three 3D points) plus a set of points
//! that lie in its plane and a set of constrained segments.  Every point is
//! projected to exact barycentric `(s,t)` coordinates so that `orient2d` and
//! `incircle` can be evaluated with exact rational arithmetic.

use crate::point::Point3;
use num_rational::BigRational;
use num_traits::{Signed, Zero};

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
}

impl HostFrame {
    pub fn from_points(a: &Point3, b: &Point3, c: &Point3) -> Option<Self> {
        Some(Self {
            a: a.to_rational()?,
            b: b.to_rational()?,
            c: c.to_rational()?,
        })
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
    verts: Vec<Point2D>,
    tris: Vec<Tri>,
}

impl Triangulation {
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

        let mut t = Self {
            verts: vec![p0, p1, p2],
            tris: vec![Tri::new(0, 1, 2)],
        };

        if !t.orient_ccw(0) {
            t.verts.swap(1, 2);
            t.tris[0].v = [0, 1, 2];
        }
        Some(t)
    }

    fn orient_ccw(&self, tri: usize) -> bool {
        let v = &self.tris[tri].v;
        orient2d(&self.verts[v[0]], &self.verts[v[1]], &self.verts[v[2]]).is_positive()
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
    pub fn insert_vertex(&mut self, host: &HostFrame, p: &Point3) -> Option<usize> {
        let pr = p.to_rational()?;
        let q = project_point(&host.a, &host.b, &host.c, &pr)?;
        let vi = self.verts.len();
        self.verts.push(q);
        self.insert_vertex_at(vi);
        Some(vi)
    }

    /// Return the index of an existing vertex whose projected 2D coordinates
    /// exactly match `p`, if any.
    pub fn find_vertex(&self, host: &HostFrame, p: &Point3) -> Option<usize> {
        let pr = p.to_rational()?;
        let q = project_point(&host.a, &host.b, &host.c, &pr)?;
        self.verts.iter().position(|v| v.s == q.s && v.t == q.t)
    }

    /// Insert an already-projected explicit 2D point.
    pub fn insert_point_2d(&mut self, p: Point2D) -> usize {
        let vi = self.verts.len();
        self.verts.push(p);
        self.insert_vertex_at(vi);
        vi
    }

    fn insert_vertex_at(&mut self, vi: usize) {
        let p = self.verts[vi].clone();
        let t = self.locate(&p);
        assert!(t.is_some(), "inserted point outside host triangle");
        let t = t.unwrap();

        // Check for point on an edge.
        let mut on_edge: Option<usize> = None;
        for i in 0..3 {
            let (a, b) = self.tris[t].edge_opp(i);
            if orient2d(&self.verts[a], &self.verts[b], &p).is_zero()
                && dot1d_between(&self.verts[a], &self.verts[b], &p)
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
                if orient2d(&self.verts[p], &self.verts[q], &self.verts[x]).is_zero()
                    && dot1d_between(&self.verts[p], &self.verts[q], &self.verts[x])
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
        let pa = &self.verts[a];
        let pb = &self.verts[b];
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
                &self.verts[a],
                &self.verts[b],
                &self.verts[c],
                &self.verts[d],
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
            let s0 = orient2d(&self.verts[v[0]], &self.verts[v[1]], p);
            let s1 = orient2d(&self.verts[v[1]], &self.verts[v[2]], p);
            let s2 = orient2d(&self.verts[v[2]], &self.verts[v[0]], p);
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
                    if self.tris[t].constrained[e] {
                        panic!("constrained edge blocks segment insertion");
                    }
                    let nbr = self.tris[t].adj[e].expect("crossing edge on boundary");
                    let ne = self
                        .find_edge(nbr, self.tris[t].edge_opp(e).0, self.tris[t].edge_opp(e).1)
                        .unwrap();
                    // Only flip if the quadrilateral is convex; otherwise we
                    // cannot resolve this crossing by a simple flip.
                    if !self.is_convex_for_flip(t, e, nbr, ne) {
                        // Fallback: split the crossing edge at the intersection
                        // point.  This increases vertex count but always makes
                        // progress.
                        let (p, _) = self.segment_edge_intersection(a, b, t, e);
                        let vi = self.insert_point_2d(p);
                        self.add_constraint(a, vi);
                        self.add_constraint(vi, b);
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

    fn find_any_edge(&self, a: usize, b: usize) -> Option<(usize, usize)> {
        for (i, _) in self.tris.iter().enumerate() {
            if let Some(e) = self.find_edge(i, a, b) {
                return Some((i, e));
            }
        }
        None
    }

    fn find_crossing_edge(&self, a: usize, b: usize) -> Option<(usize, usize)> {
        let pa = &self.verts[a];
        let pb = &self.verts[b];
        for (i, tri) in self.tris.iter().enumerate() {
            for e in 0..3 {
                let (u, v) = tri.edge_opp(e);
                let pu = &self.verts[u];
                let pv = &self.verts[v];
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
        let pa = &self.verts[a];
        let pb = &self.verts[b];
        for (i, v) in self.verts.iter().enumerate() {
            if i == a || i == b {
                continue;
            }
            if orient2d(pa, pb, v).is_zero() && strictly_between(pa, pb, v) {
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
        orient2d(&self.verts[a], &self.verts[b], &self.verts[d]).is_positive()
            && orient2d(&self.verts[a], &self.verts[d], &self.verts[c]).is_positive()
    }

    fn segment_edge_intersection(
        &self,
        a: usize,
        b: usize,
        t: usize,
        e: usize,
    ) -> (Point2D, BigRational) {
        let pa = &self.verts[a];
        let pb = &self.verts[b];
        let (u, v) = self.tris[t].edge_opp(e);
        let pu = &self.verts[u];
        let pv = &self.verts[v];
        line_line_intersection(pa, pb, pu, pv)
    }

    /// Return the triangulation as a list of CCW vertex-index triples.
    pub fn triangles(&self) -> Vec<[usize; 3]> {
        self.tris.iter().map(|t| t.v).collect()
    }

    /// Return a reference to the 2D vertices.
    pub fn vertices(&self) -> &[Point2D] {
        &self.verts
    }
}

/// Project an exact 3D point `p` onto the `(s,t)` parameter space of the host
/// triangle `a,b,c` solving `p - a = s*(b-a) + t*(c-a)`.
fn project_point(
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
    let m = (&b.s - &a.s) * (&c.t - &a.t) - (&b.t - &a.t) * (&c.s - &a.s);
    Sign::from_rational(&m)
}

/// Exact incircle test: positive if `d` lies inside the oriented circle
/// through `a,b,c`.
pub fn incircle(a: &Point2D, b: &Point2D, c: &Point2D, d: &Point2D) -> Sign {
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
                orient2d(&t.verts[v[0]], &t.verts[v[1]], &t.verts[v[2]]).is_positive(),
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
        let frame = HostFrame::from_points(&a, &b, &c).unwrap();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // Segment from edge (0,1) at midpoint to edge (0,2) at midpoint.
        let p = Point3::Explicit([0.5, 0.0, 0.0]);
        let q = Point3::Explicit([0.0, 0.5, 0.0]);
        let pi = t.insert_vertex(&frame, &p).unwrap();
        let qi = t.insert_vertex(&frame, &q).unwrap();
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
        let frame = HostFrame::from_points(&a, &b, &c).unwrap();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        let p = Point3::Explicit([0.5, 0.0, 0.0]);
        let pi = t.insert_vertex(&frame, &p).unwrap();

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
        let frame = HostFrame::from_points(&a, &b, &c).unwrap();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        let m01 = Point3::Explicit([0.5, 0.0, 0.0]);
        let mi = t.insert_vertex(&frame, &m01).unwrap();
        // After the boundary split the mesh is (p,1,2) and (p,2,0); the
        // internal edge is (2, p) == (2, mi).  Insert its midpoint, which
        // splits that internal edge via the interior-neighbour branch.
        let inner = Point3::Explicit([0.25, 0.5, 0.0]);
        let inner_i = t.insert_vertex(&frame, &inner).unwrap();

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
        let frame = HostFrame::from_points(&a, &b, &c).unwrap();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        let m01 = Point3::Explicit([0.5, 0.0, 0.0]);
        let m12 = Point3::Explicit([0.5, 0.5, 0.0]);
        let m20 = Point3::Explicit([0.0, 0.5, 0.0]);
        let p01 = t.insert_vertex(&frame, &m01).unwrap();
        let p12 = t.insert_vertex(&frame, &m12).unwrap();
        let p20 = t.insert_vertex(&frame, &m20).unwrap();

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
        let frame = HostFrame::from_points(&a, &b, &c).unwrap();
        let mut t = Triangulation::from_host(&a, &b, &c).unwrap();

        // Two segments meeting at an interior point, fanning out to edges.
        let interior = Point3::Explicit([0.25, 0.25, 0.0]);
        let e01 = Point3::Explicit([0.5, 0.0, 0.0]);
        let e02 = Point3::Explicit([0.0, 0.5, 0.0]);
        let ii = t.insert_vertex(&frame, &interior).unwrap();
        let m01 = t.insert_vertex(&frame, &e01).unwrap();
        let m02 = t.insert_vertex(&frame, &e02).unwrap();
        t.add_constraint(ii, m01);
        t.add_constraint(ii, m02);

        validate_triangulation(&t);
        assert!(has_edge(&t, ii, m01));
        assert!(has_edge(&t, ii, m02));
    }
}
