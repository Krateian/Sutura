//! Arrangement-lite core: split a self-intersecting triangle soup into one
//! where no two triangles properly intersect.
//!
//! Wiring layer over the Phase B exact classifier (`triangle_intersection`)
//! and the projected 2D constrained triangulation (`cdt2d`).  It does not
//! reimplement any of the predicate math.

use crate::cdt2d::{ConstrainedSegment, HostFrame, SegmentSource, Triangulation};
use crate::point::{f64_to_rat, Point3, RatKey};
use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
use crate::{profile_count, profile_time};
use num_rational::BigRational;
use num_traits::ToPrimitive;
use std::collections::{HashMap, HashSet};

/// Summary of an `arrangement_lite` run, mirroring the report dict in the
/// Phase B design doc §7.
#[derive(Clone, Debug)]
pub struct Report {
    pub input_faces: usize,
    pub output_faces: usize,
    pub si_pairs_detected: usize,
    pub degenerate_cases: HashMap<String, usize>,
    pub converged: bool,
    /// Per-host CDT diagnostics (only populated with the `cdt-diag` feature).
    #[cfg(feature = "cdt-diag")]
    pub host_diagnostics: Vec<crate::cdt2d::DiagState>,
}

impl Report {
    fn new(
        input_faces: usize,
        output_faces: usize,
        si_pairs_detected: usize,
        degenerate_cases: HashMap<String, usize>,
        converged: bool,
    ) -> Self {
        Self {
            input_faces,
            output_faces,
            si_pairs_detected,
            degenerate_cases,
            converged,
            #[cfg(feature = "cdt-diag")]
            host_diagnostics: Vec::new(),
        }
    }
}

struct Aabb {
    min: [f64; 3],
    max: [f64; 3],
}

fn tri_aabb(verts: &[[f64; 3]], t: &[usize; 3]) -> Aabb {
    let a = verts[t[0]];
    let b = verts[t[1]];
    let c = verts[t[2]];
    Aabb {
        min: [
            a[0].min(b[0]).min(c[0]),
            a[1].min(b[1]).min(c[1]),
            a[2].min(b[2]).min(c[2]),
        ],
        max: [
            a[0].max(b[0]).max(c[0]),
            a[1].max(b[1]).max(c[1]),
            a[2].max(b[2]).max(c[2]),
        ],
    }
}

fn aabb_overlap(a: &Aabb, b: &Aabb) -> bool {
    a.min[0] <= b.max[0]
        && b.min[0] <= a.max[0]
        && a.min[1] <= b.max[1]
        && b.min[1] <= a.max[1]
        && a.min[2] <= b.max[2]
        && b.min[2] <= a.max[2]
}

/// Enumerate candidate `(i, j)` pairs whose AABBs overlap using a uniform-grid
/// spatial hash.  Mirror of `autorefine.broad_phase_candidates`.
pub fn broad_phase_candidates(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Vec<(usize, usize)> {
    let m = tris.len();
    if m < 2 {
        return Vec::new();
    }
    let aabbs: Vec<Aabb> = tris.iter().map(|t| tri_aabb(verts, t)).collect();

    let mut diags: Vec<f64> = aabbs
        .iter()
        .map(|b| {
            let dx = b.max[0] - b.min[0];
            let dy = b.max[1] - b.min[1];
            let dz = b.max[2] - b.min[2];
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .collect();
    diags.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let median = diags[m / 2];
    let mut cell = median * 1.5;
    if !(cell > 0.0) {
        cell = 1.0;
    }
    if cell < 1e-9 {
        cell = 1e-9;
    }

    let mut entries: Vec<((i64, i64, i64), usize)> = Vec::new();
    for (i, b) in aabbs.iter().enumerate() {
        let c0 = [
            (b.min[0] / cell).floor() as i64,
            (b.min[1] / cell).floor() as i64,
            (b.min[2] / cell).floor() as i64,
        ];
        let c1 = [
            (b.max[0] / cell).floor() as i64,
            (b.max[1] / cell).floor() as i64,
            (b.max[2] / cell).floor() as i64,
        ];
        for x in c0[0]..=c1[0] {
            for y in c0[1]..=c1[1] {
                for z in c0[2]..=c1[2] {
                    entries.push(((x, y, z), i));
                }
            }
        }
    }
    entries.sort_by_key(|e| e.0);

    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    while start < entries.len() {
        let key = entries[start].0;
        let mut end = start + 1;
        while end < entries.len() && entries[end].0 == key {
            end += 1;
        }
        let grp: Vec<usize> = entries[start..end].iter().map(|e| e.1).collect();
        for x in 0..grp.len() {
            for y in (x + 1)..grp.len() {
                let (i, j) = if grp[x] < grp[y] {
                    (grp[x], grp[y])
                } else {
                    (grp[y], grp[x])
                };
                if seen.contains(&(i, j)) {
                    continue;
                }
                seen.insert((i, j));
                if aabb_overlap(&aabbs[i], &aabbs[j]) {
                    out.push((i, j));
                }
            }
        }
        start = end;
    }
    out
}

fn rat3(p: [f64; 3]) -> [BigRational; 3] {
    [f64_to_rat(p[0]), f64_to_rat(p[1]), f64_to_rat(p[2])]
}

/// A constrained segment lying inside a host triangle, together with the
/// description of which original input geometry produced it.
#[derive(Clone, Debug)]
struct HostSegment {
    p: Point3,
    q: Point3,
    source: SegmentSource,
}

/// Determine the source description for a touch segment between host triangle
/// `host_idx` and the other triangle `other_idx`. If the segment lies on a host
/// edge, it is classified as `HostEdge`; otherwise it is the intersection of
/// the host plane with the other triangle's plane.
fn touch_segment_source(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    host_idx: usize,
    other_idx: usize,
    p: &Point3,
    q: &Point3,
    frames: &mut HashMap<usize, HostFrame>,
) -> SegmentSource {
    let host = &tris[host_idx];
    // The exact frame depends only on the host triangle: build it once.
    let frame = frames.entry(host_idx).or_insert_with(|| {
        HostFrame::from_points(
            &Point3::Explicit(verts[host[0]]),
            &Point3::Explicit(verts[host[1]]),
            &Point3::Explicit(verts[host[2]]),
        )
        .expect("host triangle must be non-degenerate")
    });

    fn host_edge_index(st: &crate::cdt2d::Point2D) -> Option<(usize, usize)> {
        use num_traits::{One, Zero};
        let zero = BigRational::zero();
        let one = BigRational::one();
        // Frame parametrisation: p = a + s*(b - a) + t*(c - a), so
        // t = 0 is edge a-b, s = 0 is edge a-c and s + t = 1 is edge b-c.
        // A host vertex lies on two edges and is reported as None; the
        // caller then falls back to the (always valid) transversal source.
        let on_ab = st.t == zero;
        let on_ac = st.s == zero;
        let on_bc = &st.s + &st.t == one;
        match (on_ab, on_ac, on_bc) {
            (true, false, false) => Some((0, 1)),
            (false, true, false) => Some((0, 2)),
            (false, false, true) => Some((1, 2)),
            _ => None,
        }
    }

    let st_p = frame.project(p);
    let st_q = frame.project(q);

    match (st_p.as_ref().and_then(host_edge_index), st_q.as_ref().and_then(host_edge_index)) {
        (Some(edge), Some(edge2)) if edge == edge2 => SegmentSource::HostEdge {
            endpoints: (verts[host[edge.0]], verts[host[edge.1]]),
        },
        _ => SegmentSource::Transversal {
            other_plane: (
                verts[tris[other_idx][0]],
                verts[tris[other_idx][1]],
                verts[tris[other_idx][2]],
            ),
        },
    }
}

fn weld_point(
    pool: &mut Vec<[BigRational; 3]>,
    map: &mut HashMap<[RatKey; 3], usize>,
    p: [BigRational; 3],
) -> usize {
    profile_time!(WELD_AND_OUTPUT, {
        let key = p.map(RatKey::new);
        let result = if let Some(&i) = map.get(&key) {
            i
        } else {
            let i = pool.len();
            pool.push([key[0].0.clone(), key[1].0.clone(), key[2].0.clone()]);
            map.insert(key, i);
            i
        };
        result
    })
}

/// Order-independent key of the input edge between vertices `a` and `b`,
/// by coordinates so that duplicated input vertices still share the key.
fn edge_key(verts: &[[f64; 3]], a: usize, b: usize) -> [u64; 6] {
    let pa = verts[a].map(f64::to_bits);
    let pb = verts[b].map(f64::to_bits);
    let (lo, hi) = if pa <= pb { (pa, pb) } else { (pb, pa) };
    [lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]]
}

/// For every input edge, the distinct segment endpoints lying strictly
/// inside it (exact test in the host frame: `t = 0`, `s = 0` or
/// `s + t = 1`, excluding the corners).
fn collect_edge_points(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
    segments: &[Vec<HostSegment>],
) -> HashMap<[u64; 6], Vec<Point3>> {
    use num_traits::{One, Zero};
    let mut out: HashMap<[u64; 6], Vec<Point3>> = HashMap::new();
    let mut seen: HashSet<([u64; 6], [RatKey; 3])> = HashSet::new();
    let zero = BigRational::zero();
    let one = BigRational::one();
    for (ti, segs) in segments.iter().enumerate() {
        if segs.is_empty() {
            continue;
        }
        let t = tris[ti];
        let frame = match HostFrame::from_points(
            &Point3::Explicit(verts[t[0]]),
            &Point3::Explicit(verts[t[1]]),
            &Point3::Explicit(verts[t[2]]),
        ) {
            Some(f) => f,
            None => continue,
        };
        for seg in segs {
            for p in [&seg.p, &seg.q] {
                let st = match frame.project(p) {
                    Some(st) => st,
                    None => continue,
                };
                let on_ab = st.t == zero;
                let on_ac = st.s == zero;
                let on_bc = &st.s + &st.t == one;
                let edge = match (on_ab, on_ac, on_bc) {
                    (true, false, false) => (t[0], t[1]),
                    (false, true, false) => (t[0], t[2]),
                    (false, false, true) => (t[1], t[2]),
                    _ => continue, // interior point or a corner
                };
                let key = edge_key(verts, edge.0, edge.1);
                let exact = match p.to_rational() {
                    Some(r) => r.map(RatKey::new),
                    None => continue,
                };
                if seen.insert((key, exact)) {
                    out.entry(key).or_default().push(p.clone());
                }
            }
        }
    }
    out
}

/// Split a self-intersecting triangle soup along its proper-intersection
/// segments.
///
/// Returns the welded output vertices (as exact rational 3D points), the
/// output triangle index triples, and a report.  The caller converts the
/// rational coordinates to `f64` for the Python binding.
pub fn arrangement_lite_core(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> Result<(Vec<[BigRational; 3]>, Vec<[usize; 3]>, Report), String> {
    let m = tris.len();
    let mut degenerate: HashMap<String, usize> = HashMap::new();
    if m == 0 {
        return Ok((
            Vec::new(),
            Vec::new(),
            Report::new(0, 0, 0, degenerate, true),
        ));
    }

    let candidates = profile_time!(BROAD_PHASE, broad_phase_candidates(verts, tris));
    profile_count!(BROAD_PHASE, candidates.len() as u64);

    let mut segments: Vec<Vec<HostSegment>> = vec![Vec::new(); m];
    let mut si_pairs = 0usize;
    let mut frames: HashMap<usize, HostFrame> = HashMap::new();

    for (i, j) in candidates {
        let isect = profile_time!(CLASSIFY_PAIRS, {
            let t1 = Triangle::explicit(verts[tris[i][0]], verts[tris[i][1]], verts[tris[i][2]]);
            let t2 = Triangle::explicit(verts[tris[j][0]], verts[tris[j][1]], verts[tris[j][2]]);
            intersect_triangles(&t1, &t2)
        });
        profile_count!(CLASSIFY_PAIRS, 1);
        match isect {
            TriangleIntersection::ProperSegment { p, q } => {
                si_pairs += 1;
                profile_count!(PROPER_SI_PAIRS, 1);
                segments[i].push(HostSegment {
                    p: p.clone(),
                    q: q.clone(),
                    source: SegmentSource::Transversal {
                        other_plane: (verts[tris[j][0]], verts[tris[j][1]], verts[tris[j][2]]),
                    },
                });
                segments[j].push(HostSegment {
                    p,
                    q,
                    source: SegmentSource::Transversal {
                        other_plane: (verts[tris[i][0]], verts[tris[i][1]], verts[tris[i][2]]),
                    },
                });
            }
            TriangleIntersection::TouchSegment { p, q } => {
                // Boundary contact; splitting keeps the shared boundary exact.
                profile_count!(TOUCH_SEGMENTS, 1);
                // A touch segment lies on the boundary of one of the triangles.
                // Classify by checking which triangle contributes the supporting
                // line; if both are coplanar, treat as coplanar edges.
                let src_i = touch_segment_source(&verts, &tris, i, j, &p, &q, &mut frames);
                let src_j = touch_segment_source(&verts, &tris, j, i, &p, &q, &mut frames);
                segments[i].push(HostSegment {
                    p: p.clone(),
                    q: q.clone(),
                    source: src_i,
                });
                segments[j].push(HostSegment { p, q, source: src_j });
            }
            TriangleIntersection::TouchPoint(_) => {
                profile_count!(TOUCH_POINTS, 1);
                *degenerate.entry("touch_point".to_string()).or_insert(0) += 1;
            }
            TriangleIntersection::CoplanarOverlap => {
                profile_count!(COPLANAR_OVERLAPS, 1);
                *degenerate
                    .entry("coplanar_overlap".to_string())
                    .or_insert(0) += 1;
                // NOTE: emitting the overlapping edges as CoplanarEdge
                // constraints is deliberately deferred. Clipping them to the
                // host boundary is required for a conforming result, and
                // partial overlaps would otherwise create inconsistent
                // constraint sets between the two host triangles.
            }
            TriangleIntersection::Disjoint => {}
        }
    }

    // Optional (experimental) edge conformity: a segment endpoint that lies
    // in the interior of a host's edge is also a vertex of every other input
    // triangle sharing that edge; without it the host is split there and
    // its edge neighbour is not, which leaves a T-junction in the output.
    let edge_points = if crate::cdt2d::experimental::enabled(
        crate::cdt2d::experimental::PROPAGATE_EDGE_POINTS,
    ) {
        collect_edge_points(verts, tris, &segments)
    } else {
        HashMap::new()
    };

    let mut pool: Vec<[BigRational; 3]> = Vec::new();
    let mut weld: HashMap<[RatKey; 3], usize> = HashMap::new();
    let mut out_tris: Vec<[usize; 3]> = Vec::new();
    #[cfg(feature = "cdt-diag")]
    let mut host_diagnostics: Vec<crate::cdt2d::DiagState> = Vec::new();

    for (ti, tri) in tris.iter().enumerate() {
        let a = verts[tri[0]];
        let b = verts[tri[1]];
        let c = verts[tri[2]];

        let mut extra: Vec<&Point3> = Vec::new();
        if !edge_points.is_empty() {
            for k in 0..3 {
                if let Some(pts) = edge_points.get(&edge_key(verts, tri[k], tri[(k + 1) % 3])) {
                    extra.extend(pts.iter());
                }
            }
        }

        if !extra.is_empty()
            && segments[ti].is_empty()
            && (HostFrame::from_points(
                &Point3::Explicit(a),
                &Point3::Explicit(b),
                &Point3::Explicit(c),
            )
            .is_none()
                || Triangulation::from_host(
                    &Point3::Explicit(a),
                    &Point3::Explicit(b),
                    &Point3::Explicit(c),
                )
                .is_none())
        {
            // A degenerate (zero-area) triangle with only propagated edge
            // points cannot be triangulated; keep it unchanged, as before.
            extra.clear();
        }

        if segments[ti].is_empty() && extra.is_empty() {
            let ia = weld_point(&mut pool, &mut weld, rat3(a));
            let ib = weld_point(&mut pool, &mut weld, rat3(b));
            let ic = weld_point(&mut pool, &mut weld, rat3(c));
            out_tris.push([ia, ib, ic]);
            continue;
        }

        profile_count!(CDT_PER_HOST, 1);
        profile_time!(CDT_PER_HOST, {
            let host = HostFrame::from_points(
                &Point3::Explicit(a),
                &Point3::Explicit(b),
                &Point3::Explicit(c),
            )
            .ok_or("degenerate host triangle")?;
            let mut cdt = Triangulation::from_host(
                &Point3::Explicit(a),
                &Point3::Explicit(b),
                &Point3::Explicit(c),
            )
            .ok_or("degenerate host triangle")?;

            // Insert all segment endpoints first, then resolve the planar
            // arrangement of the segments inside this host triangle.  The batch
            // approach computes every segment-segment intersection up front from
            // the original plane/line descriptions (PPI/LPI), adds the
            // intersection vertices, and only then enforces the split
            // sub-segments.
            let mut constraint_segments: Vec<ConstrainedSegment> =
                Vec::with_capacity(segments[ti].len());
            for seg in &segments[ti] {
                // insert_vertex deduplicates against existing vertices.
                let pi = cdt
                    .insert_vertex(&seg.p)
                    .ok_or("unprojectable intersection point")?;
                let qi = cdt
                    .insert_vertex(&seg.q)
                    .ok_or("unprojectable intersection point")?;
                constraint_segments.push(ConstrainedSegment {
                    endpoints: (pi, qi),
                    source: seg.source.clone(),
                });
            }
            for p in &extra {
                // insert_vertex deduplicates the host's own endpoints.
                cdt.insert_vertex(p).ok_or("unprojectable edge point")?;
            }
            cdt.add_constraints_batch(&constraint_segments);

            #[cfg(feature = "cdt-diag")]
            {
                host_diagnostics.push(cdt.diagnostic().clone());
            }

            let sub = cdt.triangles();
            let v2 = cdt.vertices();
            // Map each CDT vertex back to 3D and weld it once; triangles then
            // reuse the pooled index (same welded result, fewer conversions).
            let mut welded: Vec<Option<usize>> = vec![None; v2.len()];
            let mut weld_vertex = |k: usize, pool: &mut Vec<[BigRational; 3]>,
                                   weld: &mut HashMap<[RatKey; 3], usize>| {
                *welded[k].get_or_insert_with(|| {
                    weld_point(pool, weld, host.point3d(&v2[k].st.s, &v2[k].st.t))
                })
            };
            for [u, w, z] in sub {
                let iu = weld_vertex(u, &mut pool, &mut weld);
                let iw = weld_vertex(w, &mut pool, &mut weld);
                let iz = weld_vertex(z, &mut pool, &mut weld);
                out_tris.push([iu, iw, iz]);
            }
            Ok::<(), String>(())
        })?;
    }

    profile_count!(WELD_AND_OUTPUT, out_tris.len() as u64);

    #[cfg_attr(not(feature = "cdt-diag"), allow(unused_mut))]
    let mut report = Report::new(m, out_tris.len(), si_pairs, degenerate, true);

    #[cfg(feature = "cdt-diag")]
    {
        report.host_diagnostics = host_diagnostics;
    }

    Ok((pool, out_tris, report))
}

/// Convert an exact rational 3D point to `f64` coordinates.
pub fn rat3_to_f64(p: &[BigRational; 3]) -> [f64; 3] {
    [
        p[0].to_f64().unwrap_or(0.0),
        p[1].to_f64().unwrap_or(0.0),
        p[2].to_f64().unwrap_or(0.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two triangles crossing in a proper segment must split into a mesh with
    /// no proper-intersection pairs left.
    #[test]
    fn two_crossing_triangles_split() {
        // T1 in the XY plane, T2 vertical, piercing T1 along a segment.
        let verts: Vec<[f64; 3]> = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
        ];
        let tris: Vec<[usize; 3]> = vec![[0, 1, 2], [3, 4, 5]];

        let (out_v, out_t, report) = arrangement_lite_core(&verts, &tris).unwrap();
        assert_eq!(report.input_faces, 2);
        assert!(report.si_pairs_detected >= 1);
        assert!(report.output_faces > 2);

        // Re-run the broad phase + classifier on the output: no proper
        // intersections may remain.
        let fout: Vec<[f64; 3]> = out_v.iter().map(rat3_to_f64).collect();
        let mut si_after = 0;
        for (i, j) in broad_phase_candidates(&fout, &out_t) {
            let t1 = Triangle::explicit(fout[out_t[i][0]], fout[out_t[i][1]], fout[out_t[i][2]]);
            let t2 = Triangle::explicit(fout[out_t[j][0]], fout[out_t[j][1]], fout[out_t[j][2]]);
            if matches!(
                intersect_triangles(&t1, &t2),
                TriangleIntersection::ProperSegment { .. }
            ) {
                si_after += 1;
            }
        }
        assert_eq!(si_after, 0);
    }

    /// Binary STL reader with exact vertex welding (test helper).
    fn read_stl_welded(path: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let data = std::fs::read(path).expect("read STL");
        let n = u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize;
        let mut index: HashMap<[u32; 3], usize> = HashMap::new();
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        for f in 0..n {
            let base = 84 + 50 * f + 12;
            let mut tri = [0usize; 3];
            for (k, slot) in tri.iter_mut().enumerate() {
                let o = base + 12 * k;
                let c: [u32; 3] = std::array::from_fn(|j| {
                    u32::from_le_bytes(data[o + 4 * j..o + 4 * j + 4].try_into().unwrap())
                });
                *slot = *index.entry(c).or_insert_with(|| {
                    verts.push(c.map(|b| f32::from_bits(b) as f64));
                    verts.len() - 1
                });
            }
            tris.push(tri);
        }
        (verts, tris)
    }

    /// Undirected edges used by an odd number of triangles.
    fn odd_use_edges(tris: &[[usize; 3]]) -> usize {
        let mut count: HashMap<(usize, usize), usize> = HashMap::new();
        for t in tris {
            for k in 0..3 {
                let (u, v) = (t[k], t[(k + 1) % 3]);
                *count.entry((u.min(v), u.max(v))).or_insert(0) += 1;
            }
        }
        count.values().filter(|&&c| c % 2 == 1).count()
    }

    /// thingi10k_100045 is closed (no odd-use edge).  The reference split
    /// leaves T-junctions where a segment endpoint lies inside a host edge;
    /// propagating those endpoints to the edge neighbours removes all of
    /// them, and the default behaviour stays the reference one.
    #[test]
    fn propagated_edge_points_remove_t_junctions() {
        use crate::cdt2d::experimental;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/real-world-samples/thingi10k_100045.stl"
        );
        let (verts, tris) = read_stl_welded(path);
        assert_eq!(odd_use_edges(&tris), 0, "input must be closed");

        experimental::set_options(0);
        let (_, reference, _) = arrangement_lite_core(&verts, &tris).unwrap();
        assert!(odd_use_edges(&reference) > 0, "reference output has T-junctions");

        experimental::set_options(experimental::PROPAGATE_EDGE_POINTS);
        let (_, propagated, _) = arrangement_lite_core(&verts, &tris).unwrap();
        experimental::set_options(0);
        assert_eq!(odd_use_edges(&propagated), 0, "T-junctions left after propagation");
    }

}
