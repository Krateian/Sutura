//! Differential-test helper: runs `arrangement_lite_core` on an OBJ file and
//! prints the wall time, the output face count and an order-independent
//! digest of the exact output geometry.
//!
//! Two builds produce the same digest only if they emit the same set of
//! output triangles (exact rational vertex coordinates, same winding).  The
//! digest is independent of triangle order, vertex numbering and the cyclic
//! rotation of each triangle, so it can be compared across refactors that
//! change internal bookkeeping but must not change the result.
//!
//!     cargo run --release --example arrangement_digest -- <mesh.obj> [...]

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use sutura_geom::arrangement::arrangement_lite_core;

fn read_obj(path: &str) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let data = fs::read_to_string(path).expect("failed to read OBJ");
    let mut verts = Vec::new();
    let mut tris = Vec::new();
    for line in data.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        match p.first() {
            Some(&"v") => verts.push([
                p[1].parse::<f64>().unwrap(),
                p[2].parse::<f64>().unwrap(),
                p[3].parse::<f64>().unwrap(),
            ]),
            Some(&"f") => {
                let ix = |s: &str| s.split('/').next().unwrap().parse::<usize>().unwrap() - 1;
                tris.push([ix(p[1]), ix(p[2]), ix(p[3])]);
            }
            _ => {}
        }
    }
    (verts, tris)
}

fn main() {
    for path in env::args().skip(1) {
        let (verts, tris) = read_obj(&path);
        let start = Instant::now();
        let (pool, out, report) = arrangement_lite_core(&verts, &tris).expect("arrangement failed");
        let secs = start.elapsed().as_secs_f64();

        let keys: Vec<String> = pool
            .iter()
            .map(|p| format!("{}|{}|{}", p[0], p[1], p[2]))
            .collect();
        let mut canon: Vec<[&str; 3]> = out
            .iter()
            .map(|t| {
                let k = [keys[t[0]].as_str(), keys[t[1]].as_str(), keys[t[2]].as_str()];
                let r = (0..3).min_by_key(|&i| k[i]).unwrap();
                [k[r], k[(r + 1) % 3], k[(r + 2) % 3]]
            })
            .collect();
        canon.sort_unstable();
        let mut h = DefaultHasher::new();
        canon.hash(&mut h);
        println!(
            "{}\tinput_faces={}\toutput_faces={}\tsi_pairs={}\tdigest={:016x}\ttime={:.3}s",
            path,
            report.input_faces,
            report.output_faces,
            report.si_pairs_detected,
            h.finish(),
            secs
        );
    }
}
