// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
use sutura_geom::diagnosis::report_json::report_to_value;
use sutura_geom::diagnosis::diagnose_core;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("mesh path");
    let detail = args.next().unwrap_or_else(|| "fast".into());
    let mesh = sutura_core::load_mesh(&path).expect("load mesh");
    let v: Vec<[f64; 3]> = mesh.verts.iter().map(|p| p.map(f64::from)).collect();
    let t: Vec<[usize; 3]> = mesh
        .tris
        .iter()
        .map(|f| [f[0] as usize, f[1] as usize, f[2] as usize])
        .collect();
    let report = diagnose_core(&v, &t, &detail);
    // `seconds` is wall-clock and cannot be compared across runs; the parity
    // harness normalizes it (and fast_budget_met) away.
    let value = report_to_value(&report, &detail, true, 0.0);
    println!("{}", serde_json::to_string(&value).unwrap());
}
