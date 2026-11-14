//! `sutura` command-line entry point.
//!
//! Slice 1 of the Rust CLI: clap derive skeleton with the contract flags,
//! `--help`, `--version`, `--list-intensities`, argument errors (exit 2) and
//! the missing-file JSON error (exit 1). Repair itself is still handled by the
//! Python CLI in this phase.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sutura_core::{
    check_units, load_mesh, parse_3mf_mesh_blocks, read_3mf_units, save_mesh, scan_bad_coordinates,
    Mesh, SourceFormat,
};

/// Product version reported by `--version`. The Rust CLI ships in the same
/// release as the Python CLI, so the contract string is the product version,
/// not the crate version.
const PRODUCT_VERSION: &str = "0.9.9";

#[derive(Parser, Debug)]
#[command(
    name = "sutura",
    version = PRODUCT_VERSION,
    disable_help_subcommand = true,
    infer_long_args = true,
    args_override_self = true
)]
struct Cli {
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short = 'o', long = "output", value_name = "OUTPUT", global = true)]
    output: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    human: bool,
    #[arg(long, value_name = "LANG", global = true)]
    lang: Option<String>,
    #[arg(long, global = true)]
    no_progress: bool,
    #[arg(long, requires = "json", global = true)]
    progress_json: bool,
    #[arg(long, global = true)]
    defects: bool,
    #[arg(long, global = true)]
    diff: bool,
    #[arg(long, value_name = "MODE", global = true)]
    mode: Option<String>,
    #[arg(long = "classifier-engine", value_name = "ENGINE", global = true)]
    classifier_engine: Option<String>,
    #[arg(long, value_name = "PROFILE", global = true)]
    profile: Option<String>,
    #[arg(long, value_name = "NAME", global = true)]
    intensity: Option<String>,
    #[arg(long)]
    list_intensities: bool,
    #[arg(long)]
    list_methods: bool,
    #[arg(long = "auto-reorder", global = true)]
    auto_reorder: bool,
    #[arg(long, global = true)]
    analyze: bool,
    #[arg(long, value_name = "LIST", global = true)]
    methods: Option<String>,
    #[arg(long, value_name = "LIST", global = true)]
    engines: Option<String>,
    #[arg(long = "repeat-source", value_name = "X,Y,Z", global = true)]
    repeat_source: Option<String>,
    #[arg(long = "repeat-target", value_name = "X,Y,Z", global = true)]
    repeat_target: Option<String>,
    #[arg(long = "wall-min-thickness", value_name = "T", global = true)]
    wall_min_thickness: Option<f64>,
    #[arg(long = "no-learning-triage", global = true)]
    no_learning_triage: bool,
    #[arg(long = "no-history", global = true)]
    no_history: bool,
    #[arg(long = "no-cache", global = true)]
    no_cache: bool,
    #[arg(long, value_name = "ZIP_FILE", global = true)]
    diagnostics: Option<PathBuf>,
    #[arg(long = "self-repair", global = true)]
    self_repair: bool,
    #[arg(long = "self-check", global = true)]
    self_check: bool,
    #[arg(long = "max-geometry-change", value_name = "PCT", global = true)]
    max_geometry_change: Option<f64>,
    #[arg(long = "max-risk", value_name = "SCORE", global = true)]
    max_risk: Option<f64>,
    #[arg(long, global = true)]
    force: bool,
    #[arg(long = "no-fidelity", global = true)]
    no_fidelity: bool,
    #[arg(long, value_name = "N", global = true)]
    last: Option<usize>,
    #[arg(long, global = true)]
    clear: bool,
    #[arg(long = "summary-only", global = true)]
    summary_only: bool,
    #[arg(long = "dry-run", global = true)]
    dry_run: bool,
    #[arg(short = 'y', long, global = true)]
    yes: bool,
    #[arg(long = "experimental-join-components", global = true)]
    experimental_join_components: bool,
    #[arg(long = "experimental-autorefine", global = true)]
    experimental_autorefine: bool,
    #[arg(long = "no-fallback-ftetwild", global = true)]
    no_fallback_ftetwild: bool,
    #[arg(long = "experimental-fallback-ftetwild", global = true)]
    experimental_fallback_ftetwild: bool,
    #[arg(long = "ftetwild-optimize", global = true)]
    ftetwild_optimize: bool,
    #[arg(long = "no-graft", global = true)]
    no_graft: bool,
    #[arg(long = "experimental-graft", global = true)]
    experimental_graft: bool,
    #[arg(long = "experimental-splint", global = true)]
    experimental_splint: bool,
    #[arg(long = "no-splint", global = true)]
    no_splint: bool,
    #[arg(long = "solid-engine", value_name = "ENGINE", global = true)]
    solid_engine: Option<String>,
    #[arg(long, global = true)]
    coaptation: bool,
    #[arg(long = "no-coaptation", global = true)]
    no_coaptation: bool,
    #[arg(long, global = true)]
    flap: bool,
    #[arg(long = "no-flap", global = true)]
    no_flap: bool,
    #[arg(long, global = true)]
    tamponade: bool,
    #[arg(long = "no-tamponade", global = true)]
    no_tamponade: bool,
    #[arg(long = "no-dressing", global = true)]
    no_dressing: bool,
    #[arg(long = "experimental-dressing", global = true)]
    experimental_dressing: bool,
    #[arg(long = "dressing-drain", value_name = "DRESSING_DRAIN", num_args = 0..=1, default_missing_value = "on", global = true)]
    dressing_drain: Option<String>,
    #[arg(long = "dressing-defects", value_name = "SET", global = true)]
    dressing_defects: Option<String>,
    #[arg(long = "dressing-rmax-scale", value_name = "F", global = true)]
    dressing_rmax_scale: Option<f64>,
    #[arg(long = "dressing-sigma-scale", value_name = "F", global = true)]
    dressing_sigma_scale: Option<f64>,
    #[arg(long = "dressing-force-adopt", global = true)]
    dressing_force_adopt: bool,
    #[arg(long = "deep-repair", value_name = "MODE", global = true)]
    deep_repair: Option<String>,
    #[arg(long = "si-mode", value_name = "MODE", global = true)]
    si_mode: Option<String>,
    #[arg(long = "experimental-indirect-autorefine", global = true)]
    experimental_indirect_autorefine: bool,
    #[arg(long = "experimental-edge-tiebreak", global = true)]
    experimental_edge_tiebreak: bool,
    #[arg(long, value_name = "PATH", global = true)]
    trace: Option<PathBuf>,
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// I/O probes and conversion (Rust-only helper commands).
    Io {
        #[command(subcommand)]
        command: IoCommand,
    },
}

#[derive(Subcommand, Debug)]
enum IoCommand {
    Probe {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Convert {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Roundtrip {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, Some(message))) => {
            if !message.is_empty() {
                eprintln!("sutura: {message}");
            }
            ExitCode::from(code)
        }
        Err((code, None)) => ExitCode::from(code),
    }
}

fn run(cli: Cli) -> Result<(), (u8, Option<String>)> {
    if cli.list_intensities {
        let loaded = sutura_core::config::policy::load_policy(None)
            .map_err(|e| (1, Some(e.to_string())))?;
        println!(
            "{}",
            sutura_core::config::intensity::format_intensities(&loaded, None)
        );
        return Ok(());
    }
    if let Some(Command::Io { command }) = cli.command {
        return match command {
            IoCommand::Probe { file, json } => {
                probe(&file, json || cli.json).map_err(|(c, m)| (c, Some(m)))
            }
            IoCommand::Convert {
                input,
                output,
                json,
            }
            | IoCommand::Roundtrip {
                input,
                output,
                json,
            } => convert(&input, &output, json || cli.json).map_err(|(c, m)| (c, Some(m))),
        };
    }

    if cli.files.is_empty() {
        // No positional input: keep the Python CLI's usage error shape.
        return Err((2, Some("no input files".to_string())));
    }
    for file in &cli.files {
        if !file.exists() {
            let input = file.display().to_string();
            let payload = json!({
                "input": input,
                "error": format!("file not found: {}", file.display()),
            });
            println!("{}", serde_json::to_string(&payload).unwrap());
            return Err((1, None));
        }
    }

    Err((
        2,
        Some(
            "repair is not implemented in the Rust CLI yet; the Python CLI remains the default"
                .to_string(),
        ),
    ))
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
