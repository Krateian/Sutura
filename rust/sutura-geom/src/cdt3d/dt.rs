// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CDT3D Phase B (P2): initial Delaunay tetrahedralization.
//!
//! The PLC vertices are enclosed in a padded synthetic box and handed to the
//! vendored sequential Geogram `BDEL` engine together with the eight box
//! corners.  The box is the convex hull of the point set, so every finite cell
//! lies inside the box and the hull faces are exactly the ghost/sink boundary
//! used by Phase C.  The box is never emitted as a surface.
//!
//! Geogram owns the cell order (and, under degeneracies, may choose a different
//! order for the same valid cell set).  The returned cells are therefore
//! canonicalised: each tet is put in positive orientation with the exact
//! `orient3d`, its vertex ids are sorted, and the tet list is sorted.  The
//! neighbour array is rebuilt from the face incidences instead of trusting the
//! backend's indexing, then every link is verified.
//!
//! This module is opt-in.  Nothing here is called by the default Splint path.

use crate::point::Point3;
use crate::predicates3d::{insphere_sign, orient3d_sign};
use crate::splint::delaunay::delaunay3;
use std::collections::{BTreeMap, BTreeSet};

/// Default padding as a fraction of the bounding-box diagonal.
pub const DEFAULT_PAD_FRAC: f64 = 0.05;
const MAX_PAD_ULPS: usize = 64;
const BOX_CORNERS: usize = 8;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Options for the initial tetrahedralization.
#[derive(Clone, Copy, Debug)]
pub struct DtOptions {
    /// Box padding as a fraction of the bounding-box diagonal.
    pub pad_frac: f64,
}

impl Default for DtOptions {
    fn default() -> Self {
        Self {
            pad_frac: DEFAULT_PAD_FRAC,
        }
    }
}

/// The synthetic axis-aligned box around the PLC.
#[derive(Clone, Copy, Debug)]
pub struct BoundingBox3 {
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}

/// Validity report for one initial tetrahedralization.
#[derive(Clone, Debug, Default)]
pub struct DtReport {
    pub input_vertices: usize,
    pub box_vertices: usize,
    pub total_vertices: usize,
    pub tets: usize,
    pub hull_faces: usize,
    pub ghost_faces: usize,
    pub interior_faces: usize,
    pub hull_faces_off_box: usize,
    pub nonmanifold_faces: usize,
    pub duplicate_tets: usize,
    pub degenerate_tets: usize,
    pub nonpositive_tets: usize,
    pub inconsistent_neighbours: usize,
    pub empty_sphere_violations: usize,
    pub cospherical_faces: usize,
    pub unused_vertices: usize,
    pub dt_valid: bool,
    pub hash: u64,
}

/// Initial Delaunay tetrahedralization plus its tet/neighbour structure.
#[derive(Clone, Debug)]
pub struct Dt {
    /// PLC vertices in their input order, followed by the eight box corners.
    pub verts: Vec<[f64; 3]>,
    /// Number of leading vertices that belong to the PLC.
    pub plc_vertices: usize,
    /// The synthetic box; corners start at `plc_vertices`.
    pub box3: BoundingBox3,
    /// Canonical, positively oriented tetrahedra.
    pub tets: Vec<[usize; 4]>,
    /// `neighbours[t][lv]` is the tet across the face opposite local vertex
    /// `lv`, or `-1` on the box (ghost) boundary.
    pub neighbours: Vec<[i32; 4]>,
    pub report: DtReport,
}

/// Build the initial Delaunay tetrahedralization of `verts` plus a padded box.
///
/// `verts` must be finite and pairwise distinct (the P0 PLC already is).  The
/// function returns a typed error rather than a silently invalid mesh when the
/// backend returns a degenerate or duplicate cell.
pub fn build_dt(verts: &[[f64; 3]], opts: &DtOptions) -> Result<Dt, String> {
    if verts.is_empty() {
        return Err("initial Delaunay needs at least one PLC vertex".into());
    }
    if !opts.pad_frac.is_finite() || opts.pad_frac < 0.0 {
        return Err("box padding must be a finite non-negative fraction".into());
    }
    for (i, p) in verts.iter().enumerate() {
        if !p.iter().all(|x| x.is_finite()) {
            return Err(format!("non-finite PLC vertex at index {i}"));
        }
    }
    ensure_unique(verts, "PLC")?;

    let box3 = padded_box(verts, opts.pad_frac)?;
    let plc_vertices = verts.len();
    let mut points = Vec::with_capacity(plc_vertices + BOX_CORNERS);
    points.extend_from_slice(verts);
    points.extend_from_slice(&box_corners(&box3));
    ensure_unique(&points, "box")?;

    let raw = delaunay3(&points)?;
    if raw.is_empty() {
        return Err("Geogram returned no finite tetrahedra".into());
    }
    let tets = canonicalize_oriented(&points, raw)?;
    let neighbours = build_neighbours(&tets)?;
    let mut report = validate(&points, plc_vertices, &box3, &tets, &neighbours);
    report.hash = dt_hash(&points, &tets);

    Ok(Dt {
        verts: points,
        plc_vertices,
        box3,
        tets,
        neighbours,
        report,
    })
}

fn canonical_key(p: [f64; 3]) -> [u64; 3] {
    p.map(|x| if x == 0.0 { 0 } else { x.to_bits() })
}

fn ensure_unique(points: &[[f64; 3]], what: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for (i, p) in points.iter().enumerate() {
        if !seen.insert(canonical_key(*p)) {
            return Err(format!("{what}: duplicate vertex at index {i}"));
        }
    }
    Ok(())
}

fn padded_box(verts: &[[f64; 3]], pad_frac: f64) -> Result<BoundingBox3, String> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in verts {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let diag2: f64 = (0..3).map(|a| (hi[a] - lo[a]).powi(2)).sum();
    let pad = if diag2 > 0.0 {
        pad_frac * diag2.sqrt()
    } else {
        0.0
    };
    if !pad.is_finite() {
        return Err("box padding overflowed".into());
    }

    let mut blo = [0.0; 3];
    let mut bhi = [0.0; 3];
    for a in 0..3 {
        let mut l = lo[a] - pad;
        let mut steps = 0;
        while !(l < lo[a]) {
            l = l.next_down();
            steps += 1;
            if steps > MAX_PAD_ULPS || !l.is_finite() {
                return Err(format!("cannot place the lower box plane on axis {a}"));
            }
        }
        let mut h = hi[a] + pad;
        steps = 0;
        while !(h > hi[a]) {
            h = h.next_up();
            steps += 1;
            if steps > MAX_PAD_ULPS || !h.is_finite() {
                return Err(format!("cannot place the upper box plane on axis {a}"));
            }
        }
        if !(l < h) {
            return Err(format!("degenerate box on axis {a}"));
        }
        blo[a] = l;
        bhi[a] = h;
    }
    Ok(BoundingBox3 { lo: blo, hi: bhi })
}

fn box_corners(b: &BoundingBox3) -> [[f64; 3]; BOX_CORNERS] {
    std::array::from_fn(|i| {
        [
            if i & 1 == 0 { b.lo[0] } else { b.hi[0] },
            if i & 2 == 0 { b.lo[1] } else { b.hi[1] },
            if i & 4 == 0 { b.lo[2] } else { b.hi[2] },
        ]
    })
}

fn sorted_face(t: &[usize; 4], lv: usize) -> [usize; 3] {
    let mut f = [0usize; 3];
    let mut k = 0;
    for i in 0..4 {
        if i != lv {
            f[k] = t[i];
            k += 1;
        }
    }
    f.sort_unstable();
    f
}

fn orient(points: &[[f64; 3]], a: usize, b: usize, c: usize, d: usize) -> f64 {
    orient3d_sign(
        &Point3::Explicit(points[a]),
        &Point3::Explicit(points[b]),
        &Point3::Explicit(points[c]),
        &Point3::Explicit(points[d]),
    )
}

/// Sort each tet's ids, force positive orientation with the exact predicate,
/// then sort the tet list.  This is a pure function of the cell set.
fn canonicalize_oriented(
    points: &[[f64; 3]],
    raw: Vec<[usize; 4]>,
) -> Result<Vec<[usize; 4]>, String> {
    let n = points.len();
    let mut out = Vec::with_capacity(raw.len());
    for mut t in raw {
        if t.iter().any(|&v| v >= n) {
            return Err(format!(
                "tet {t:?} references a vertex outside the point set"
            ));
        }
        t.sort_unstable();
        if t[0] == t[1] || t[1] == t[2] || t[2] == t[3] {
            return Err(format!("tet {t:?} repeats a vertex"));
        }
        let s = orient(points, t[0], t[1], t[2], t[3]);
        if s == 0.0 {
            return Err(format!("degenerate zero-volume tet {t:?}"));
        }
        if s < 0.0 {
            t.swap(2, 3);
        }
        out.push(t);
    }
    out.sort_unstable();
    if let Some(w) = out.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("duplicate tet {:?}", w[0]));
    }
    Ok(out)
}

fn build_neighbours(tets: &[[usize; 4]]) -> Result<Vec<[i32; 4]>, String> {
    let mut occurrences: BTreeMap<[usize; 3], Vec<(usize, usize)>> = BTreeMap::new();
    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            occurrences
                .entry(sorted_face(t, lv))
                .or_default()
                .push((ti, lv));
        }
    }

    let mut neighbours = vec![[-1i32; 4]; tets.len()];
    for (face, list) in occurrences {
        match list.len() {
            1 => {}
            2 => {
                let (t0, l0) = list[0];
                let (t1, l1) = list[1];
                neighbours[t0][l0] = t1 as i32;
                neighbours[t1][l1] = t0 as i32;
            }
            n => {
                return Err(format!(
                    "non-manifold face {:?} is incident to {n} tetrahedra",
                    face
                ))
            }
        }
    }
    Ok(neighbours)
}

fn oriented_face_apex(
    points: &[[f64; 3]],
    t: &[usize; 4],
    lv: usize,
) -> (usize, usize, usize, usize) {
    let mut f = [0usize; 3];
    let mut k = 0;
    for i in 0..4 {
        if i != lv {
            f[k] = t[i];
            k += 1;
        }
    }
    let d = t[lv];
    if orient(points, f[0], f[1], f[2], d) < 0.0 {
        f.swap(1, 2);
    }
    (f[0], f[1], f[2], d)
}

fn face_on_box(
    points: &[[f64; 3]],
    plc_vertices: usize,
    b: &BoundingBox3,
    face: [usize; 3],
) -> bool {
    if face.iter().any(|&v| v < plc_vertices) {
        return false;
    }
    let p = [points[face[0]], points[face[1]], points[face[2]]];
    (0..3).any(|a| {
        let l = b.lo[a];
        let h = b.hi[a];
        (p[0][a] == l && p[1][a] == l && p[2][a] == l)
            || (p[0][a] == h && p[1][a] == h && p[2][a] == h)
    })
}

fn validate(
    points: &[[f64; 3]],
    plc_vertices: usize,
    b: &BoundingBox3,
    tets: &[[usize; 4]],
    neighbours: &[[i32; 4]],
) -> DtReport {
    let mut r = DtReport {
        input_vertices: plc_vertices,
        box_vertices: BOX_CORNERS,
        total_vertices: points.len(),
        tets: tets.len(),
        ..DtReport::default()
    };

    for w in tets.windows(2) {
        if w[0] == w[1] {
            r.duplicate_tets += 1;
        }
    }

    let mut used = vec![false; points.len()];
    for t in tets {
        for &v in t {
            used[v] = true;
        }
        let s = orient(points, t[0], t[1], t[2], t[3]);
        if s <= 0.0 {
            r.nonpositive_tets += 1;
            if s == 0.0 {
                r.degenerate_tets += 1;
            }
        }
    }
    r.unused_vertices = used.iter().filter(|u| !**u).count();

    let mut occurrences: BTreeMap<[usize; 3], Vec<(usize, usize)>> = BTreeMap::new();
    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            occurrences
                .entry(sorted_face(t, lv))
                .or_default()
                .push((ti, lv));
        }
    }
    for (face, list) in &occurrences {
        match list.len() {
            1 => {
                r.hull_faces += 1;
                if face_on_box(points, plc_vertices, b, *face) {
                    r.ghost_faces += 1;
                } else {
                    r.hull_faces_off_box += 1;
                }
            }
            2 => r.interior_faces += 1,
            _ => r.nonmanifold_faces += 1,
        }
    }

    for (ti, t) in tets.iter().enumerate() {
        for lv in 0..4 {
            let face = sorted_face(t, lv);
            let n = neighbours[ti][lv];
            if n < 0 {
                if occurrences.get(&face).map(|v| v.len()) != Some(1) {
                    r.inconsistent_neighbours += 1;
                }
                continue;
            }
            let n = n as usize;
            if n >= tets.len() || n == ti {
                r.inconsistent_neighbours += 1;
                continue;
            }
            let Some(lv2) = (0..4).find(|&j| sorted_face(&tets[n], j) == face) else {
                r.inconsistent_neighbours += 1;
                continue;
            };
            if neighbours[n][lv2] != ti as i32 || tets[n][lv2] == t[lv] {
                r.inconsistent_neighbours += 1;
            }

            let (ia, ib, ic, id) = oriented_face_apex(points, t, lv);
            let ie = tets[n][lv2];
            let s = insphere_sign(
                &Point3::Explicit(points[ia]),
                &Point3::Explicit(points[ib]),
                &Point3::Explicit(points[ic]),
                &Point3::Explicit(points[id]),
                &Point3::Explicit(points[ie]),
            );
            if s > 0.0 {
                r.empty_sphere_violations += 1;
            } else if s == 0.0 {
                r.cospherical_faces += 1;
            }
        }
    }

    r.dt_valid = r.tets > 0
        && r.duplicate_tets == 0
        && r.degenerate_tets == 0
        && r.nonpositive_tets == 0
        && r.nonmanifold_faces == 0
        && r.inconsistent_neighbours == 0
        && r.empty_sphere_violations == 0
        && r.hull_faces_off_box == 0
        && r.unused_vertices == 0;
    r
}

fn dt_hash(points: &[[f64; 3]], tets: &[[usize; 4]]) -> u64 {
    let mut h = FNV_OFFSET;
    let mut step = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(FNV_PRIME);
    };
    step(points.len() as u64);
    for p in points {
        for x in canonical_key(*p) {
            step(x);
        }
    }
    step(tets.len() as u64);
    for t in tets {
        for &v in t {
            step(v as u64);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdt3d::plc::{build_plc, Plc, PlcOptions};
    use std::collections::HashMap;
    use std::time::Instant;

    fn real_sample(name: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let path = format!(
            "{}/../../tests/real-world-samples/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        read_stl_welded(&path)
    }

    fn fixture(name: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        read_stl_welded(&path)
    }

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

    fn plc_for(kind: &str, name: &str) -> Plc {
        let (v, t) = if kind == "real" {
            real_sample(name)
        } else {
            fixture(name)
        };
        build_plc(&v, &t, &PlcOptions::default()).expect("build PLC")
    }

    fn opts() -> DtOptions {
        DtOptions::default()
    }

    #[test]
    fn box_is_strictly_outside_the_input() {
        let verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let b = padded_box(&verts, DEFAULT_PAD_FRAC).unwrap();
        for p in &verts {
            for a in 0..3 {
                assert!(b.lo[a] < p[a] && p[a] < b.hi[a]);
            }
        }
        let dt = build_dt(&verts, &opts()).unwrap();
        assert_eq!(dt.verts.len(), verts.len() + BOX_CORNERS);
        assert!(dt.report.dt_valid, "{:?}", dt.report);
    }

    #[test]
    fn initial_dt_of_a_tetrahedron_is_valid_and_box_ghosted() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
        let dt = build_dt(&verts, &opts()).unwrap();
        assert!(dt.report.dt_valid, "{:?}", dt.report);
        assert_eq!(dt.plc_vertices, 4);
        assert_eq!(dt.report.box_vertices, BOX_CORNERS);
        assert_eq!(dt.report.hull_faces, 12);
        assert_eq!(dt.report.ghost_faces, 12);
        assert_eq!(dt.report.hull_faces_off_box, 0);
        assert_eq!(dt.report.inconsistent_neighbours, 0);
        assert_eq!(dt.report.empty_sphere_violations, 0);
        assert_eq!(dt.report.unused_vertices, 0);
    }

    #[test]
    fn empty_sphere_check_rejects_a_non_delaunay_pair() {
        // Two tets share the z=0 face; the upper apex is inside the lower
        // tet's circumsphere, so this is not locally Delaunay.
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, -1.0],
            [0.1, 0.1, 0.1],
        ];
        let tets = canonicalize_oriented(&points, vec![[0, 1, 2, 3], [0, 1, 2, 4]]).unwrap();
        let neighbours = build_neighbours(&tets).unwrap();
        let b = BoundingBox3 {
            lo: [-2.0; 3],
            hi: [2.0; 3],
        };
        let r = validate(&points, 0, &b, &tets, &neighbours);
        assert!(r.empty_sphere_violations > 0, "{r:?}");
        assert!(!r.dt_valid);
    }

    #[test]
    fn duplicate_input_vertices_are_rejected() {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
        ];
        assert!(build_dt(&verts, &opts()).is_err());
    }

    #[test]
    fn flat_tets_are_rejected_by_canonicalisation() {
        let points = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ];
        assert!(canonicalize_oriented(&points, vec![[0, 1, 2, 3]]).is_err());
    }

    #[test]
    fn thingi10k_100827_initial_dt_is_deterministic() {
        let plc = plc_for("real", "thingi10k_100827.stl");
        assert!(plc.report.plc_valid, "PLC report: {:?}", plc.report);
        let mut hashes = Vec::new();
        let mut last = None;
        for _ in 0..5 {
            let dt = build_dt(&plc.verts, &opts()).unwrap();
            assert!(dt.report.dt_valid, "DT report: {:?}", dt.report);
            hashes.push(dt.report.hash);
            last = Some(dt);
        }
        assert!(
            hashes.windows(2).all(|w| w[0] == w[1]),
            "hashes: {hashes:?}"
        );
        let dt = last.unwrap();
        println!(
            "100827: plcV={} totalV={} tets={} hull={} ghost={} empty={} hash={:016x}",
            dt.report.input_vertices,
            dt.report.total_vertices,
            dt.report.tets,
            dt.report.hull_faces,
            dt.report.ghost_faces,
            dt.report.empty_sphere_violations,
            dt.report.hash
        );
    }

    #[test]
    #[ignore = "P2 public-corpus gate; run with --release -- --ignored"]
    fn dt_public_corpus_gate() {
        let samples = [
            ("real", "thingi10k_100827.stl"),
            ("fixture", "case1_open_cup.stl"),
            ("fixture", "case2_pipe.stl"),
            ("fixture", "case3_broken_cup.stl"),
            ("fixture", "case4_cracked_sphere.stl"),
            ("fixture", "case5_box_missing_face.stl"),
            ("fixture", "case6_thin_cup.stl"),
            ("fixture", "plate_a_dent.stl"),
        ];
        for (kind, name) in samples {
            let plc = plc_for(kind, name);
            assert!(plc.report.plc_valid, "{name}: PLC report {:?}", plc.report);
            let start = Instant::now();
            let mut hashes = Vec::new();
            let mut last = None;
            for _ in 0..5 {
                let dt = build_dt(&plc.verts, &opts()).unwrap();
                assert!(dt.report.dt_valid, "{name}: DT report {:?}", dt.report);
                hashes.push(dt.report.hash);
                last = Some(dt);
            }
            let secs = start.elapsed().as_secs_f64() / 5.0;
            assert!(
                hashes.windows(2).all(|w| w[0] == w[1]),
                "{name}: hashes {hashes:?}"
            );
            let dt = last.unwrap();
            println!(
                "{name}: plcV={} totalV={} tets={} hull={} ghost={} interior={} off_box={} ngb={} empty={} cosph={} unused={} secs={:.3} hash={:016x}",
                dt.report.input_vertices,
                dt.report.total_vertices,
                dt.report.tets,
                dt.report.hull_faces,
                dt.report.ghost_faces,
                dt.report.interior_faces,
                dt.report.hull_faces_off_box,
                dt.report.inconsistent_neighbours,
                dt.report.empty_sphere_violations,
                dt.report.cospherical_faces,
                dt.report.unused_vertices,
                secs,
                dt.report.hash,
            );
        }
    }
}
