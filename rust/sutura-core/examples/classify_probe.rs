// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
use sutura_core::classify::classify_mesh;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("mesh path");
    let engine = args.next().unwrap_or_else(|| "experimental".to_string());
    let mesh = sutura_core::load_mesh(&path).expect("load mesh");
    let verts: Vec<[f64; 3]> = mesh
        .verts
        .iter()
        .map(|v| [v[0] as f64, v[1] as f64, v[2] as f64])
        .collect();
    let tris: Vec<[usize; 3]> = mesh
        .tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    println!("{}", classify_mesh(&verts, &tris, &engine));
}
