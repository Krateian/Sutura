//! `sutura` command-line skeleton.
//!
//! Phase 1 exposes I/O probes and conversion; `repair` is deliberately not
//! implemented yet and fails with a clear message.

use std::path::Path;
use std::process::ExitCode;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sutura_core::{
    check_units, load_mesh, parse_3mf_mesh_blocks, read_3mf_units, save_mesh, scan_bad_coordinates,
    Mesh, SourceFormat,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("sutura: {message}");
            ExitCode::from(code)
        }
    }
}

fn run(args: &[String]) -> Result<(), (u8, String)> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("sutura {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"))
        .collect();

    match positional.as_slice() {
        [] | ["help"] | ["--help"] => {
            print_usage();
            Ok(())
        }
        ["version"] => {
            println!("sutura {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["io", "probe", file] => probe(Path::new(file), json),
        ["io", "convert", input, output] => convert(Path::new(input), Path::new(output), json),
        ["io", "roundtrip", input, output] => convert(Path::new(input), Path::new(output), json),
        ["repair", ..] => Err((
            2,
            "repair is not implemented in the Rust CLI yet; the Python CLI remains the default"
                .to_string(),
        )),
        _ => {
            print_usage();
            Err((2, "unknown command".to_string()))
        }
    }
}

fn print_usage() {
    eprintln!(
        "sutura <command> [--json]\n\
\n\
  io probe <FILE>               inspect a mesh file\n\
  io convert <IN> <OUT>        read and write one mesh\n\
  io roundtrip <IN> <OUT>      alias for io convert\n\
  repair ...                    not implemented in Phase 1\n\
  version                       print the CLI version\n\
\n\
Supported I/O: STL, OBJ, PLY; 3MF probe (multi-object) is supported."
    );
}

fn probe(path: &Path, json_out: bool) -> Result<(), (u8, String)> {
    let bytes_in = std::fs::metadata(path)
        .map_err(|e| (1, format!("{}: {e}", path.display())))?
        .len();
    let format = format_of(path)?;
    let mut meshes = Vec::new();
    let mut unit_warnings = Vec::new();

    match format {
        SourceFormat::ThreeMf => {
            let units = read_3mf_units(path);
            let blocks =
                parse_3mf_mesh_blocks(path).map_err(|e| (1, format!("{}: {e}", path.display())))?;
            for block in &blocks {
                let unit = units
                    .iter()
                    .find(|(name, _)| name == &block.model_name)
                    .map(|(_, u)| u.as_str());
                if let Some(w) = check_units(&block.mesh, unit) {
                    unit_warnings.push(json!({
                        "model": block.model_name,
                        "block": block.index,
                        "unit_warning": w.unit_warning,
                        "unit_hint": w.unit_hint,
                    }));
                }
                meshes.push(mesh_json(
                    Some(&block.model_name),
                    Some(block.index),
                    &block.mesh,
                ));
            }
        }
        _ => {
            let mesh = load_mesh(path).map_err(|e| (1, format!("{}: {e}", path.display())))?;
            if let Some(w) = check_units(&mesh, None) {
                unit_warnings.push(json!({
                    "model": Value::Null,
                    "block": Value::Null,
                    "unit_warning": w.unit_warning,
                    "unit_hint": w.unit_hint,
                }));
            }
            meshes.push(mesh_json(None, None, &mesh));
        }
    }

    let material_refs = matches!(format, SourceFormat::Obj) && material_refs(path);
    let scan = scan_bad_coordinates(path).map_err(|e| (1, format!("{}: {e}", path.display())))?;

    let payload = json!({
        "format": format_name(format),
        "bytes_in": bytes_in,
        "meshes": meshes,
        "unit_warnings": unit_warnings,
        "material_refs": material_refs,
        "scan": scan,
    });
    if json_out {
        println!("{}", serde_json::to_string(&payload).unwrap());
    } else {
        println!("format: {}", format_name(format));
        println!("bytes: {bytes_in}");
        for m in payload["meshes"].as_array().unwrap() {
            println!(
                "mesh: name={} index={} verts={} tris={} bbox={}",
                m["name"], m["index"], m["verts"], m["tris"], m["bbox"]
            );
        }
        for w in payload["unit_warnings"].as_array().unwrap() {
            println!("warning: {}", w["unit_hint"]);
        }
        if material_refs {
            println!("warning: OBJ material/texture references are not preserved");
        }
        if let Some(s) = scan {
            println!("scan: {s}");
        }
    }
    Ok(())
}

fn convert(input: &Path, output: &Path, json_out: bool) -> Result<(), (u8, String)> {
    let mesh = load_mesh(input).map_err(|e| (1, format!("{}: {e}", input.display())))?;
    save_mesh(output, &mesh).map_err(|e| (1, format!("{}: {e}", output.display())))?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "input": input.display().to_string(),
                "output": output.display().to_string(),
                "verts": mesh.vertex_count(),
                "tris": mesh.triangle_count(),
            }))
            .unwrap()
        );
    } else {
        println!(
            "wrote {} ({} verts, {} tris)",
            output.display(),
            mesh.vertex_count(),
            mesh.triangle_count()
        );
    }
    Ok(())
}

fn mesh_json(name: Option<&str>, index: Option<usize>, mesh: &Mesh) -> Value {
    let bbox = mesh.bbox().map(|(lo, hi)| {
        vec![
            f64::from(lo[0]),
            f64::from(lo[1]),
            f64::from(lo[2]),
            f64::from(hi[0]),
            f64::from(hi[1]),
            f64::from(hi[2]),
        ]
    });
    json!({
        "name": name,
        "index": index,
        "verts": mesh.vertex_count(),
        "tris": mesh.triangle_count(),
        "bbox": bbox,
        "array_sha256": array_sha256(mesh),
    })
}

fn array_sha256(mesh: &Mesh) -> String {
    let mut h = Sha256::new();
    for v in &mesh.verts {
        for c in v {
            h.update(c.to_le_bytes());
        }
    }
    for t in &mesh.tris {
        for i in t {
            h.update(i.to_le_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn format_of(path: &Path) -> Result<SourceFormat, (u8, String)> {
    let s = path
        .to_str()
        .ok_or_else(|| (2, "path is not valid UTF-8".to_string()))?;
    SourceFormat::from_extension(s).map_err(|e| (2, format!("{}: {e}", path.display())))
}

fn format_name(f: SourceFormat) -> &'static str {
    match f {
        SourceFormat::Stl => "stl",
        SourceFormat::Obj => "obj",
        SourceFormat::Ply => "ply",
        SourceFormat::ThreeMf => "3mf",
    }
}

fn material_refs(path: &Path) -> bool {
    sutura_core::obj_has_material_refs(path)
}
