// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Boundary extraction between inside and outside tets.

use std::collections::HashMap;

use crate::dual_contour::mesh_is_manifold;

use super::normalize::Normalizer;

pub(crate) struct Emitted {
    pub(crate) verts: Vec<[f64; 3]>,
    pub(crate) tris: Vec<[usize; 3]>,
    pub(crate) out_manifold: bool,
    pub(crate) output_parts: usize,
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Deduplicate coordinate-triple faces into an indexed mesh. Insertion order
/// is the face order, so the vertex numbering is deterministic.
fn emit_faces(faces: &[[[f64; 3]; 3]]) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[usize; 3]> = Vec::new();
    let mut index: HashMap<[u64; 3], usize> = HashMap::new();
    for f in faces {
        let mut tri = [0usize; 3];
        for (j, p) in f.iter().enumerate() {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let id = *index.entry(key).or_insert_with(|| {
                verts.push(*p);
                verts.len() - 1
            });
            tri[j] = id;
        }
        if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            tris.push(tri);
        }
    }
    (verts, tris)
}

fn output_parts(verts: &[[f64; 3]], tris: &[[usize; 3]]) -> usize {
    let n = verts.len();
    if n == 0 || tris.is_empty() {
        return 0;
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for t in tris {
        let (a, b, c) = (t[0], t[1], t[2]);
        for (u, v) in [(a, b), (b, c), (a, c)] {
            let ru = find(&mut parent, u);
            let rv = find(&mut parent, v);
            if ru != rv {
                parent[ru] = rv;
            }
        }
    }
    let mut roots = std::collections::HashSet::new();
    for t in tris {
        roots.insert(find(&mut parent, t[0]));
    }
    roots.len()
}

/// H2 (O4): canonicalize world-space output vertices through `f32` and dedup on
/// the `f32` grid, so the emitted mesh already equals its reload-equivalent
/// `f32` weld (`p_weld`). Deterministic: the map is filled in first-seen vertex
/// order and faces keep their order; faces that collapse to a sliver (two
/// identical `f32` vertices) are dropped.
pub(crate) fn snap_and_dedup_f32(
    verts: &[[f64; 3]],
    tris: &[[usize; 3]],
) -> (Vec<[f64; 3]>, Vec<[usize; 3]>) {
    let mut map: HashMap<[u32; 3], usize> = HashMap::new();
    let mut new_v: Vec<[f64; 3]> = Vec::new();
    let mut remap: Vec<usize> = Vec::with_capacity(verts.len());
    for p in verts {
        let snapped = [p[0] as f32, p[1] as f32, p[2] as f32];
        let key = [snapped[0].to_bits(), snapped[1].to_bits(), snapped[2].to_bits()];
        let id = *map.entry(key).or_insert_with(|| {
            new_v.push([snapped[0] as f64, snapped[1] as f64, snapped[2] as f64]);
            new_v.len() - 1
        });
        remap.push(id);
    }
    let mut new_t: Vec<[usize; 3]> = Vec::with_capacity(tris.len());
    for t in tris {
        let a = remap[t[0]];
        let b = remap[t[1]];
        let c = remap[t[2]];
        if a != b && b != c && a != c {
            new_t.push([a, b, c]);
        }
    }
    (new_v, new_t)
}

pub(crate) fn emit_boundary(
    coords: &[[f64; 3]],
    centroids: &[[f64; 3]],
    internal: &[([usize; 3], usize, usize)],
    is_inside: &[bool],
    normalizer: &Normalizer,
    snap_f32: bool,
) -> Emitted {
    let mut faces: Vec<[[f64; 3]; 3]> = Vec::new();
    for (key, a, b) in internal {
        let la = is_inside[*a];
        let lb = is_inside[*b];
        if la == lb {
            continue;
        }
        let (int_t, ext_t) = if la { (*a, *b) } else { (*b, *a) };
        let p0 = coords[key[0]];
        let p1 = coords[key[1]];
        let p2 = coords[key[2]];
        let mut normal = cross(sub(p1, p0), sub(p2, p0));
        let dir = sub(centroids[ext_t], centroids[int_t]);
        if dot(normal, dir) < 0.0 {
            faces.push([p0, p2, p1]);
        } else {
            faces.push([p0, p1, p2]);
        }
    }
    let (gc_v, gc_t) = emit_faces(&faces);
    let mut out_v: Vec<[f64; 3]> = gc_v.iter().map(|q| normalizer.de_norm(*q)).collect();
    let mut out_t = gc_t;
    if snap_f32 {
        let (sv, st) = snap_and_dedup_f32(&out_v, &out_t);
        out_v = sv;
        out_t = st;
    }
    let u32_tris: Vec<[u32; 3]> = out_t
        .iter()
        .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
        .collect();
    let out_manifold = mesh_is_manifold(&out_v, &u32_tris);
    let parts = output_parts(&out_v, &out_t);
    Emitted {
        verts: out_v,
        tris: out_t,
        out_manifold,
        output_parts: parts,
    }
}
