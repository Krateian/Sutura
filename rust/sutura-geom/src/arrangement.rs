//! Arrangement-lite core: split a self-intersecting triangle soup into one
//! where no two triangles properly intersect.
//!
//! Wiring layer over the Phase B exact classifier (`triangle_intersection`)
//! and the projected 2D constrained triangulation (`cdt2d`).  It does not
//! reimplement any of the predicate math.

use crate::cdt2d::{HostFrame, Triangulation};
use crate::point::{f64_to_rat, Point3};
use crate::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
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
fn broad_phase_candidates(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> Vec<(usize, usize)> {
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

fn weld_point(
    pool: &mut Vec<[BigRational; 3]>,
    map: &mut HashMap<[BigRational; 3], usize>,
    p: [BigRational; 3],
) -> usize {
    if let Some(&i) = map.get(&p) {
        return i;
    }
    let i = pool.len();
    map.insert(p.clone(), i);
    pool.push(p);
    i
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
            Report {
                input_faces: 0,
                output_faces: 0,
                si_pairs_detected: 0,
                degenerate_cases: degenerate,
                converged: true,
            },
        ));
    }

    let candidates = broad_phase_candidates(verts, tris);

    let mut segments: Vec<Vec<(Point3, Point3)>> = vec![Vec::new(); m];
    let mut si_pairs = 0usize;

    for (i, j) in candidates {
        let t1 = Triangle::explicit(verts[tris[i][0]], verts[tris[i][1]], verts[tris[i][2]]);
        let t2 = Triangle::explicit(verts[tris[j][0]], verts[tris[j][1]], verts[tris[j][2]]);
        match intersect_triangles(&t1, &t2) {
            TriangleIntersection::ProperSegment { p, q } => {
                si_pairs += 1;
                segments[i].push((p.clone(), q.clone()));
                segments[j].push((p, q));
            }
            TriangleIntersection::TouchSegment { p, q } => {
                // Boundary contact; splitting keeps the shared boundary exact.
                segments[i].push((p.clone(), q.clone()));
                segments[j].push((p, q));
            }
            TriangleIntersection::TouchPoint(_) => {
                *degenerate.entry("touch_point".to_string()).or_insert(0) += 1;
            }
            TriangleIntersection::CoplanarOverlap => {
                *degenerate
                    .entry("coplanar_overlap".to_string())
                    .or_insert(0) += 1;
            }
            TriangleIntersection::Disjoint => {}
        }
    }

    let mut pool: Vec<[BigRational; 3]> = Vec::new();
    let mut weld: HashMap<[BigRational; 3], usize> = HashMap::new();
    let mut out_tris: Vec<[usize; 3]> = Vec::new();

    for (ti, tri) in tris.iter().enumerate() {
        let a = verts[tri[0]];
        let b = verts[tri[1]];
        let c = verts[tri[2]];

        if segments[ti].is_empty() {
            let ia = weld_point(&mut pool, &mut weld, rat3(a));
            let ib = weld_point(&mut pool, &mut weld, rat3(b));
            let ic = weld_point(&mut pool, &mut weld, rat3(c));
            out_tris.push([ia, ib, ic]);
            continue;
        }

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

        for (p, q) in &segments[ti] {
            let pi = match cdt.find_vertex(&host, p) {
                Some(v) => v,
                None => cdt
                    .insert_vertex(&host, p)
                    .ok_or("unprojectable intersection point")?,
            };
            let qi = match cdt.find_vertex(&host, q) {
                Some(v) => v,
                None => cdt
                    .insert_vertex(&host, q)
                    .ok_or("unprojectable intersection point")?,
            };
            cdt.add_constraint(pi, qi);
        }

        let sub = cdt.triangles();
        let v2 = cdt.vertices();
        for [u, w, z] in sub {
            let pu = host.point3d(&v2[u].s, &v2[u].t);
            let pw = host.point3d(&v2[w].s, &v2[w].t);
            let pz = host.point3d(&v2[z].s, &v2[z].t);
            let iu = weld_point(&mut pool, &mut weld, pu);
            let iw = weld_point(&mut pool, &mut weld, pw);
            let iz = weld_point(&mut pool, &mut weld, pz);
            out_tris.push([iu, iw, iz]);
        }
    }

    let report = Report {
        input_faces: m,
        output_faces: out_tris.len(),
        si_pairs_detected: si_pairs,
        degenerate_cases: degenerate,
        converged: true,
    };

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
}
