// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
use sutura_core::analysis::analyze_mesh;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("mesh path");
    let engine = args.next().unwrap_or_else(|| "experimental".into());
    let mesh = sutura_core::load_mesh(&path).expect("load mesh");
    let a = analyze_mesh(&mesh, &engine, true).expect("analyze");
    println!("{}", serde_json::to_string(&a.to_json()).unwrap());
}
