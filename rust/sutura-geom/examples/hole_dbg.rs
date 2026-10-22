use std::collections::HashSet;

use sutura_geom::hole::close_holes_core;
use sutura_geom::triangle_intersection::{intersect_triangles, Triangle, TriangleIntersection};
use sutura_geom::winding::MeshBvh;

fn load_obj(path: &str) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
    let txt = std::fs::read_to_string(path).unwrap();
    let mut verts = Vec::new();
    let mut tris = Vec::new();
    for line in txt.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let x: f64 = it.next().unwrap().parse().unwrap();
                let y: f64 = it.next().unwrap().parse().unwrap();
                let z: f64 = it.next().unwrap().parse().unwrap();
                verts.push([x, y, z]);
            }
            Some("f") => {
                let a: i64 = it.next().unwrap().split('/').next().unwrap().parse().unwrap();
                let b: i64 = it.next().unwrap().split('/').next().unwrap().parse().unwrap();
                let c: i64 = it.next().unwrap().split('/').next().unwrap().parse().unwrap();
                tris.push([a - 1, b - 1, c - 1]);
            }
            _ => {}
        }
    }
    (verts, tris)
}

fn main() {
    let path = std::env::args().nth(1).expect("obj path");
    let (v, t) = load_obj(&path);
    let n0 = t.len();
    let rep = close_holes_core(&v, &t, 1_000_000, true).unwrap();
    let mv = rep.verts;
    let mt_i64 = rep.tris;
    let mt: Vec<[usize; 3]> = mt_i64
        .iter()
        .map(|x| [x[0] as usize, x[1] as usize, x[2] as usize])
        .collect();
    println!("orig={} closed={} cap={}", n0, mt.len(), mt.len() - n0);

    let bvh = MeshBvh::from_arrays(&mv, &mt);
    let (mask, cnt, skipped) = bvh.self_intersections(&mv, &mt);
    println!("SI faces={cnt} skipped={skipped}");
    let flagged: Vec<usize> = (0..mt.len()).filter(|&i| mask[i]).collect();
    println!("flagged: {flagged:?}");

    // For each flagged face, find the proper-crossing partners.
    let tris_l: Vec<Triangle> = mt
        .iter()
        .map(|t| Triangle::explicit(mv[t[0]], mv[t[1]], mv[t[2]]))
        .collect();
    let mut cand_face_verts: HashSet<usize> = HashSet::new();
    for &fi in flagged.iter().filter(|&&i| i < n0) {
        for &x in &mt[fi] {
            cand_face_verts.insert(x);
        }
    }
    for &i in flagged.iter().filter(|&&i| i >= n0) {
        for j in 0..n0 {
            if mask[j] {
                let a = mt[i];
                let b = mt[j];
                let shared = a.iter().filter(|x| b.contains(x)).count();
                if let TriangleIntersection::ProperSegment { .. } =
                    intersect_triangles(&tris_l[i], &tris_l[j])
                {
                    println!(
                        "cap face {i} X body face {j}: shared={shared} incident_to_capvert={}",
                        mt[j].iter().any(|x| cand_face_verts.contains(x))
                    );
                }
            }
        }
    }
}
