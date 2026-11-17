// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! `sutura` command-line entry point.
//!
//! The Rust CLI implements the single-file repair contract directly through
//! `sutura_core::repair::repair`: STL/OBJ/PLY/3MF input, default output naming,
//! `--json` / `--human` / `--lang tr`, NDJSON progress and SIGINT (130). No
//! Python process is started. The read-only `validate` / `--dry-run` paths and
//! the hidden `__io` helper subcommands share the same parser.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sutura_core::{
    check_units, load_mesh, parse_3mf_mesh_blocks, read_3mf_units, save_mesh, scan_bad_coordinates,
    Mesh, SourceFormat,
};
// The method registry lives in the shared settings module: the GUI needs it too
// and must not depend on this binary crate.
use sutura_core::repair_settings::METHODS;

#[macro_use]
mod output;

mod human;
mod repair_cmd;

/// Product version reported by `--version`, read from `sutura/repair.py` at
/// build time (`build.rs`). The Python CLI is the single version source until
/// it is retired; then the Cargo workspace version takes over.
const PRODUCT_VERSION: &str = env!("SUTURA_PRODUCT_VERSION");

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
    // `allow_hyphen_values`: the value is "X,Y,Z" and negative coordinates are
    // legal, so `--repeat-source -3.5,0,0` must not be read as a flag.
    #[arg(long = "repeat-source", value_name = "X,Y,Z", global = true,
          allow_hyphen_values = true)]
    repeat_source: Option<String>,
    #[arg(long = "repeat-target", value_name = "X,Y,Z", global = true,
          allow_hyphen_values = true)]
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
    #[arg(long = "ftetwild-max-faces", value_name = "N", global = true)]
    ftetwild_max_faces: Option<i64>,
    #[arg(long = "ftetwild-timeout", value_name = "S", global = true)]
    ftetwild_timeout: Option<f64>,
    #[arg(long = "graft-max-faces", value_name = "N", global = true)]
    graft_max_faces: Option<i64>,
    #[arg(long = "graft-timeout", value_name = "S", global = true)]
    graft_timeout: Option<f64>,
    #[arg(long = "dense-ratio", value_name = "N", global = true)]
    dense_ratio: Option<i64>,
    #[arg(long = "dense-min-faces", value_name = "N", global = true)]
    dense_min_faces: Option<i64>,
    #[arg(long = "ftetwild-max-hausdorff", value_name = "F", global = true)]
    ftetwild_max_hausdorff: Option<f64>,
    #[arg(long = "ftetwild-optimize-retry", global = true)]
    ftetwild_optimize_retry: bool,
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
    /// Check for a newer release (`update` / `license` / `none`).
    #[arg(long = "check-update")]
    check_update: bool,
    /// Print the release notes saved by the last update (works offline).
    #[arg(long = "whats-new")]
    whats_new: bool,
    /// Migrate an existing Python install to this Rust build.
    ///
    /// Used by the bridge release's updater, which downloads the artifact and
    /// runs this; it is not a normal user-facing flag. `--yes` is required
    /// because it removes the old runtime.
    #[arg(long = "migrate-from-python")]
    migrate_from_python: bool,
    /// The verified release artifact to install (defaults to the running
    /// binary's own release, which is what a manual run wants).
    #[arg(long = "from-artifact", value_name = "PATH")]
    from_artifact: Option<std::path::PathBuf>,
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
    ///
    /// Hidden and renamed: the Python CLI has no `io` subcommand, so a bare
    /// `io` argument is a file path there. Keeping the helper under `__io`
    /// avoids that contract collision.
    #[command(name = "__io", hide = true)]
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
    if cli.list_methods {
        print_list_methods();
        return Ok(());
    }
    if cli.list_intensities {
        let loaded = sutura_core::config::policy::load_policy(None)
            .map_err(|e| (1, Some(e.to_string())))?;
        outln!(
            "{}",
            sutura_core::config::intensity::format_intensities(&loaded, None)
        );
        return Ok(());
    }
    // Migration from a Python install. Called by the bridge release's updater
    // with the downloaded artifact, because the migration (backup, data move,
    // swap, health gate, venv removal) must exist in exactly one place and that
    // place is here.
    if cli.migrate_from_python {
        return migrate_command(&cli);
    }
    if cli.self_check {
        let report = sutura_core::self_check::run(true);
        if cli.json {
            outln!("{}", crate::repair_cmd::py_json(&report));
        } else {
            outln!("{}", sutura_core::self_check::human_text(&report));
        }
        let code = sutura_core::self_check::exit_code(&report);
        return if code == 0 {
            Ok(())
        } else {
            Err((code, None))
        };
    }
    // `clear-cache` is a positional subcommand, like Python (`repair.py:1092`):
    // it must be the first file argument, and it exits 0 either way.
    if cli.files.first().map(|p| p.as_os_str()) == Some(std::ffi::OsStr::new("clear-cache")) {
        let freed = sutura_core::cache::clear();
        let human_bytes = sutura_core::cache::format_bytes(freed);
        if cli.json {
            outln!(
                "{}",
                crate::repair_cmd::py_json(&serde_json::json!({
                    "cleared": true, "freed_bytes": freed, "freed_human": human_bytes,
                }))
            );
        } else {
            outln!("Cache cleared: {human_bytes} freed");
        }
        return Ok(());
    }
    if cli.whats_new {
        // Pure filesystem read: the snapshot is written by the updater before it
        // installs, so this works with no network.
        let dir = sutura_core::whatsnew::current_dir();
        match (sutura_core::whatsnew::current_tag(), sutura_core::whatsnew::current_body()) {
            (Some(tag), Some(body)) => {
                outln!("{}", sutura_core::whatsnew::plain_text(&tag, &body, &dir));
            }
            _ => outln!(
                "no release notes saved yet; see {}",
                sutura_core::updater::RELEASES_URL
            ),
        }
        return Ok(());
    }
    if cli.check_update {
        let status = sutura_core::updater::check_for_update(PRODUCT_VERSION, true);
        if cli.json {
            outln!(
                "{}",
                crate::repair_cmd::py_json(&serde_json::json!({
                    "status": status.status,
                    "tag": status.tag,
                    "current": PRODUCT_VERSION,
                }))
            );
        } else {
            match (status.status, status.tag.as_deref()) {
                ("update", Some(tag)) => outln!("update available: {tag} (current {PRODUCT_VERSION})"),
                ("license", Some(tag)) => outln!(
                    "Sutura {tag} changes the license terms (PolyForm Noncommercial 1.0.0 - \
commercial use requires a separate agreement). This update will not be applied \
automatically - download it manually from {} if you accept the new terms.",
                    sutura_core::updater::RELEASES_URL
                ),
                _ => outln!("no update available (current {PRODUCT_VERSION})"),
            }
        }
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

    // `validate` as the first positional switches to read-only validation;
    // the Python CLI has no such subcommand, it is picked out of the file
    // list before anything else runs.
    // A single `-o` with several inputs is rejected before anything else
    // (the Python CLI checks this ahead of the validate/--dry-run dispatch).
    if cli.files.len() > 1 && cli.output.is_some() {
        return Err((
            2,
            Some("-o cannot be used with multiple input files".to_string()),
        ));
    }

    if matches!(cli.files.first().and_then(|f| f.to_str()), Some("validate")) {
        if cli.output.is_some() {
            return Err((2, Some("-o is not valid with validate".to_string())));
        }
        if cli.dry_run {
            return Err((2, Some("--dry-run is not valid with validate".to_string())));
        }
        if cli.methods.is_some() {
            return Err((2, Some("--methods is not valid with validate".to_string())));
        }
        if cli.engines.is_some() {
            return Err((2, Some("--engines is not valid with validate".to_string())));
        }
        let targets = cli.files[1..].to_vec();
        if targets.is_empty() {
            return Err((
                2,
                Some("validate requires at least one input file".to_string()),
            ));
        }
        return run_read_only(&cli, &targets, ReadOnly::Validate);
    }
    if cli.dry_run {
        if cli.output.is_some() {
            return Err((2, Some("-o is not valid with --dry-run".to_string())));
        }
        if cli.methods.is_some() {
            return Err((2, Some("--methods is not valid with --dry-run".to_string())));
        }
        if cli.engines.is_some() {
            return Err((2, Some("--engines is not valid with --dry-run".to_string())));
        }
        if cli.files.is_empty() {
            return Err((2, Some("no input files".to_string())));
        }
        return run_read_only(&cli, &cli.files.clone(), ReadOnly::DryRun);
    }
    if cli.analyze {
        // Same guards Python applies before touching the files.
        if cli.output.is_some() {
            return Err((2, Some("-o is not valid with --analyze".to_string())));
        }
        if cli.dry_run {
            return Err((2, Some("--analyze is not valid with --dry-run".to_string())));
        }
        if cli.methods.is_some() {
            return Err((2, Some("--methods is not valid with --analyze".to_string())));
        }
        if cli.engines.is_some() {
            return Err((2, Some("--engines is not valid with --analyze".to_string())));
        }
        if cli.files.is_empty() {
            return Err((
                2,
                Some("--analyze requires at least one input file".to_string()),
            ));
        }
        return run_read_only(&cli, &cli.files.clone(), ReadOnly::Analyze);
    }

    if cli.files.is_empty() {
        // No positional input: keep the Python CLI's usage error shape.
        return Err((2, Some("no input files".to_string())));
    }

    repair_cmd::run(&cli)
}

/// Whether the optional fTetWild binary is installed. The Rust CLI cannot
/// import Python's pytetwild; until the Rust tier is wired this mirrors the
/// filesystem check used by the installer.
fn ftetwild_available() -> bool {
    if let Some(path) = std::env::var_os("SUTURA_FTETWILD_BIN") {
        return !path.is_empty() && Path::new(&path).exists();
    }
    match std::env::var_os("HOME") {
        Some(home) => Path::new(&home)
            .join(".local/share/sutura/ftetwild/bin/ftetwild")
            .exists(),
        None => false,
    }
}

fn print_list_methods() {
    outln!("Repair methods:");
    outln!("  num  id                    family      available  name");
    let ftw = ftetwild_available();
    for (num, id, family, name) in METHODS {
        let (ok, reason) = if id == "ftetwild" && !ftw {
            (false, "fTetWild (pytetwild) is not installed")
        } else {
            (true, "")
        };
        let suffix = if ok {
            String::new()
        } else {
            format!("  ({reason})")
        };
        outln!(
            "  {:<4} {:<21} {:<11} {:<10} {}{}",
            num,
            id,
            family,
            if ok { "yes" } else { "no" },
            name,
            suffix
        );
    }
    outln!();
    let loaded = match sutura_core::config::policy::load_policy(None) {
        Ok(policy) => policy,
        Err(e) => {
            eprintln!("sutura: {e}");
            return;
        }
    };
    let order = loaded
        .get("order")
        .and_then(|o| o.get("deep_tier_order"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    outln!("Automatic deep-tier order: {order}");
    outln!(
        "Policy: schema {}, hash {}, {}",
        loaded.schema_version,
        loaded.content_hash,
        if loaded.user_override {
            "user override"
        } else {
            "packaged default"
        }
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
        outln!("{}", serde_json::to_string(&payload).unwrap());
    } else {
        outln!("format: {}", format_name(format));
        outln!("bytes: {bytes_in}");
        for m in payload["meshes"].as_array().unwrap() {
            outln!(
                "mesh: name={} index={} verts={} tris={} bbox={}",
                m["name"], m["index"], m["verts"], m["tris"], m["bbox"]
            );
        }
        for w in payload["unit_warnings"].as_array().unwrap() {
            outln!("warning: {}", w["unit_hint"]);
        }
        if material_refs {
            outln!("warning: OBJ material/texture references are not preserved");
        }
        if let Some(s) = scan {
            outln!("scan: {s}");
        }
    }
    Ok(())
}

fn convert(input: &Path, output: &Path, json_out: bool) -> Result<(), (u8, String)> {
    let mesh = load_mesh(input).map_err(|e| (1, format!("{}: {e}", input.display())))?;
    save_mesh(output, &mesh).map_err(|e| (1, format!("{}: {e}", output.display())))?;
    if json_out {
        outln!(
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
        outln!(
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

#[derive(Clone, Copy, PartialEq)]
enum ReadOnly {
    Validate,
    DryRun,
    Analyze,
}

/// `validate` / `--dry-run`: analyse every input, write nothing.
///
/// Output rules match the Python CLI: one file prints the bare report, several
/// print `{"files": [...]}`; `--human` switches to the text renderers; the
/// exit code is 1 when any file reported an error.
fn run_read_only(cli: &Cli, files: &[PathBuf], kind: ReadOnly) -> Result<(), (u8, Option<String>)> {
    let human = cli.human && !cli.json;
    let loaded = sutura_core::config::policy::load_policy(None).map_err(|e| (1, Some(e.to_string())))?;
    let engine = cli.classifier_engine.as_deref().unwrap_or("experimental");
    let si_mode = cli.si_mode.as_deref().unwrap_or("auto");
    let mode = cli.mode.as_deref().unwrap_or("auto");

    let mut reports: Vec<Value> = Vec::with_capacity(files.len());
    let mut errors = 0usize;
    for path in files {
        let input = path.display().to_string();
        let report = match read_only_one(cli, path, kind, &loaded, engine, si_mode, mode) {
            Ok(value) => value,
            Err(message) => {
                errors += 1;
                json!({"input": input, "error": message})
            }
        };
        // `--analyze` reports failures inside the payload, so count them here;
        // Python exits 1 whenever any result carries an `error`.
        if kind == ReadOnly::Analyze && report.get("error").is_some() {
            errors += 1;
        }
        reports.push(report);
    }

    if reports.len() == 1 {
        let report = &reports[0];
        if human {
            outln!("{}", match kind {
                ReadOnly::Validate => human_validate(report),
                ReadOnly::DryRun => human_dry_run(report),
                ReadOnly::Analyze => sutura_core::analyze::format_analyze_human(report),
            });
        } else {
            outln!("{}", serde_json::to_string(report).unwrap());
        }
    } else if human {
        for report in &reports {
            outln!("{}", match kind {
                ReadOnly::Validate => human_validate(report),
                ReadOnly::DryRun => human_dry_run(report),
                ReadOnly::Analyze => sutura_core::analyze::format_analyze_human(report),
            });
            outln!();
        }
    } else {
        outln!("{}", serde_json::to_string(&json!({"files": reports})).unwrap());
    }
    if errors > 0 {
        return Err((1, None));
    }
    Ok(())
}

/// One read-only report, or the Python `error` string for hard failures.
fn read_only_one(
    cli: &Cli,
    path: &Path,
    kind: ReadOnly,
    loaded: &sutura_core::config::policy::LoadedPolicy,
    engine: &str,
    si_mode: &str,
    mode: &str,
) -> Result<Value, String> {
    // `--analyze` handles the file itself: it walks every 3MF object through
    // `mesh_io::load_meshes`, so it must not go through the single-mesh load
    // below. Its failures are reported INSIDE the payload (Python's
    // `analyze_report` catches them and sets `result['error']`), and it exits 1
    // when any file has an error.
    if kind == ReadOnly::Analyze {
        let input = path.display().to_string();
        if !path.exists() {
            return Ok(json!({
                "input": input,
                "error": format!("analysis failed: file not found: {}", path.display()),
            }));
        }
        return Ok(sutura_core::analyze::analyze_report(
            &input,
            loaded,
            engine,
            // `diagnosis.analyze_file` never overrides the default, so SI is
            // always counted for `--analyze`.
            true,
            // `--no-cache` used to be parsed and dropped; it now really opts out,
            // and `SUTURA_NO_CACHE` is honoured inside the same gate.
            !cli.no_cache && sutura_core::cache::enabled(),
        ));
    }
    let prefix = match kind {
        ReadOnly::Validate => "validation failed: ",
        ReadOnly::DryRun => "dry run failed: ",
        ReadOnly::Analyze => unreachable!("handled above"),
    };
    if !path.exists() {
        return Err(format!("{prefix}file not found: {}", path.display()));
    }
    if let Ok(Some(bad)) = sutura_core::scan_bad_coordinates(path) {
        return Err(format!("{prefix}input mesh contains {bad}"));
    }
    let mesh = sutura_core::load_mesh(path).map_err(|e| format!("{prefix}{e}"))?;
    let mut value = match kind {
        ReadOnly::Validate => {
            sutura_core::validate::validate_mesh(&mesh, loaded, engine, None, si_mode)
                .map_err(|e| format!("{prefix}{e}"))?
        }
        ReadOnly::DryRun => sutura_core::validate::dry_run_mesh(
            &mesh,
            loaded,
            mode,
            cli.profile.as_deref(),
            engine,
            None,
            None,
            si_mode,
            !cli.no_flap,
        )
        .map_err(|e| format!("{prefix}{e}"))?,
        ReadOnly::Analyze => unreachable!("handled before the single-mesh load"),
    };
    let mut out = serde_json::Map::new();
    out.insert("input".into(), json!(path.display().to_string()));
    if let Value::Object(body) = value.take() {
        for (k, v) in body {
            out.insert(k, v);
        }
    }
    Ok(Value::Object(out))
}

/// `report.human_validate`.
fn human_validate(report: &Value) -> String {
    if let Some(err) = report.get("error").and_then(Value::as_str) {
        if report.get("validation").is_none() {
            return format!("ERROR: {err}");
        }
    }
    let v = report.get("validation").cloned().unwrap_or(Value::Null);
    let mut lines = vec![format!(
        "Input : {}",
        report.get("input").and_then(Value::as_str).unwrap_or("")
    )];
    if v.get("unit_warning").and_then(Value::as_bool).unwrap_or(false) {
        lines.push(format!(
            "WARNING: {}",
            v.get("unit_hint").and_then(Value::as_str).unwrap_or("")
        ));
    }
    lines.push(format!(
        "Type  : {} (confidence {:.2})",
        report.get("detected_type").and_then(Value::as_str).unwrap_or("?"),
        report
            .get("detected_confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    ));
    if let Some(engine) = report.get("classifier_engine").and_then(Value::as_str) {
        lines.push(format!("Classifier: {engine}"));
    }
    if let Some(ec) = report.get("estimated_confidence").and_then(Value::as_i64) {
        let label = report
            .get("estimated_confidence_label")
            .and_then(Value::as_str)
            .unwrap_or("?");
        lines.push(format!(
            "Estimated confidence: {ec}/100 ({}) — actual result may differ after repair",
            title_case(label)
        ));
    }
    lines.push(String::new());
    lines.push("Validation:".to_string());
    let count = |key: &str| {
        v.get(key)
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    };
    lines.push(format!(
        "  Vertices            : {}",
        v.get("vertices").and_then(Value::as_i64).unwrap_or(0)
    ));
    lines.push(format!(
        "  Faces               : {}",
        v.get("faces").and_then(Value::as_i64).unwrap_or(0)
    ));
    lines.push(format!("  Holes               : {}", count("holes")));
    lines.push(format!(
        "  Non-manifold regions: {}",
        count("non_manifold")
    ));
    lines.push(format!(
        "  Self-intersections  : {}",
        v.get("self_intersections")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    ));
    lines.push(format!(
        "  Connected components: {}",
        v.get("connected_components")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    ));
    lines.push(format!(
        "  Watertight          : {}",
        if v.get("watertight").and_then(Value::as_bool).unwrap_or(false) {
            "YES"
        } else {
            "NO"
        }
    ));
    lines.push(format!(
        "  Signed volume       : {:.6}",
        v.get("signed_volume").and_then(Value::as_f64).unwrap_or(0.0)
    ));
    lines.push(format!(
        "  Surface area        : {:.3}",
        v.get("surface_area").and_then(Value::as_f64).unwrap_or(0.0)
    ));
    lines.push(format!(
        "  Orientation         : {}",
        v.get("orientation").and_then(Value::as_str).unwrap_or("?")
    ));
    lines.join("\n")
}

/// `report.human_dry_run`.
fn human_dry_run(report: &Value) -> String {
    if let Some(err) = report.get("error").and_then(Value::as_str) {
        return format!("ERROR: {err}");
    }
    let mut lines = vec![
        format!(
            "Input : {}",
            report.get("input").and_then(Value::as_str).unwrap_or("")
        ),
        "Dry-run: no output file will be written".to_string(),
        format!(
            "Mode  : {}",
            report.get("repair_mode").and_then(Value::as_str).unwrap_or("?")
        ),
    ];
    if report
        .get("unit_warning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        lines.push(format!(
            "WARNING: {}",
            report.get("unit_hint").and_then(Value::as_str).unwrap_or("")
        ));
    }
    lines.push(format!(
        "Type  : {} (confidence {:.2})",
        report.get("detected_type").and_then(Value::as_str).unwrap_or("?"),
        report
            .get("detected_confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "Classifier: {}",
        report
            .get("classifier_engine")
            .and_then(Value::as_str)
            .unwrap_or("?")
    ));
    lines.push(format!(
        "Tuning: {}",
        if report
            .get("tuning_applied")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "tuned thresholds"
        } else {
            "default thresholds"
        }
    ));
    if let Some(ec) = report.get("estimated_confidence").and_then(Value::as_i64) {
        let label = report
            .get("estimated_confidence_label")
            .and_then(Value::as_str)
            .unwrap_or("?");
        lines.push(format!(
            "Estimated confidence: {ec}/100 ({}) — actual result may differ after repair",
            title_case(label)
        ));
    }
    let wa = report.get("would_apply").cloned().unwrap_or(Value::Null);
    lines.push(format!(
        "Would apply: mincomponentsize={}, maxholesize={}",
        wa.get("mincomponentsize").and_then(Value::as_i64).unwrap_or(0),
        wa.get("maxholesize").and_then(Value::as_i64).unwrap_or(0)
    ));
    lines.push(format!(
        "Found  : {} hole(s) (largest {:.3}), {} non-manifold region(s), {} self-intersection(s), {} debris face(s) (< mincomponentsize)",
        report.get("holes_found").and_then(Value::as_i64).unwrap_or(0),
        report
            .get("largest_hole_diameter")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        report
            .get("non_manifold_regions")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        report.get("self_intersections").and_then(Value::as_i64).unwrap_or(0),
        report
            .get("debris_faces_removable")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    ));
    lines.push(if report
        .get("stage2_bridge_available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "Stage 2: would run if stage 1 closes the mesh (bridge available)".to_string()
    } else {
        "Stage 2: would be skipped (bridge unavailable)".to_string()
    });
    if report.get("flap_step").and_then(Value::as_bool).unwrap_or(false) {
        lines.push("Flap  : would run first (pre-pass on the input)".to_string());
    }
    lines.join("\n")
}

fn title_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `--migrate-from-python`: turn an existing Python install into this build.
///
/// The bridge release's updater downloads and verifies the artifact and then
/// runs the binary inside it with this flag. Doing the work here means the
/// migration exists once, in `sutura_core::migrate`, rather than being spelled
/// out a second time in Python.
fn migrate_command(cli: &Cli) -> Result<(), (u8, Option<String>)> {
    // `--yes` is not a formality: this removes the previous runtime.
    if !cli.yes {
        return Err((
            2,
            Some(
                "--migrate-from-python removes the previous Python install; \
                 pass --yes to confirm"
                    .to_string(),
            ),
        ));
    }
    let app = sutura_core::install::app_dir();
    let data = sutura_core::config::learning::data_dir();
    let plan = sutura_core::migrate::detect(&app, &data);
    if plan.legacy.is_none() {
        outln!("Nothing to migrate: no Python install at {}", app.display());
        return Ok(());
    }

    // The artifact to install. When invoked from the bridge release the binary
    // runs from the unpacked artifact, so its own directory holds the tree to
    // install; `--from-artifact` overrides that for a manual run.
    let payload = match &cli.from_artifact {
        Some(path) => std::fs::read(path)
            .map_err(|e| (1, Some(format!("{}: {e}", path.display()))))?,
        None => {
            return Err((
                2,
                Some(
                    "--from-artifact is required: pass the downloaded release \
                     artifact so the migration installs a verified tree"
                        .to_string(),
                ),
            ))
        }
    };

    let tag = format!("v{}", env!("SUTURA_PRODUCT_VERSION"));
    let stamp = sutura_core::report::history::now_ts();
    let report = sutura_core::migrate::run(&plan, &tag, &payload, &stamp, &run_quiet)
        .map_err(|e| (1, Some(e)))?;

    for note in &report.notes {
        outln!("  {note}");
    }
    outln!(
        "Migrated to {tag}: {} file(s) moved, {} removed.",
        report.moved.len(),
        report.removed.len()
    );
    Ok(())
}

/// Run a binary for the health check without inheriting the caller's stdio.
///
/// The migration's own output is what the user reads; a child that prints to
/// the same stream would interleave with it.
fn run_quiet(binary: &Path, args: &[&str]) -> (i32, String) {
    match std::process::Command::new(binary).args(args).output() {
        Ok(out) => (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        ),
        Err(_) => (-1, String::new()),
    }
}

#[cfg(test)]
mod migrate_flag_tests {
    use clap::Parser;

    /// `--migrate-from-python` must refuse without `--yes`.
    ///
    /// The flag removes the previous runtime, so a bare invocation has to be a
    /// no-op with a clear reason rather than a surprise.
    #[test]
    fn migrate_requires_confirmation() {
        let cli = <super::Cli as Parser>::try_parse_from([
            "sutura",
            "--migrate-from-python",
            "--from-artifact",
            "/nonexistent/asset.tar.gz",
        ])
        .expect("parses");
        let err = super::migrate_command(&cli).expect_err("must refuse without --yes");
        assert_eq!(err.0, 2, "usage error, not a crash");
        assert!(
            err.1.unwrap_or_default().contains("--yes"),
            "the message must say what is missing"
        );
    }

    /// With `--yes` but no artifact there is nothing safe to install.
    #[test]
    fn migrate_needs_an_artifact() {
        let cli = <super::Cli as Parser>::try_parse_from([
            "sutura",
            "--migrate-from-python",
            "--yes",
        ])
        .expect("parses");
        // Only asserted when this machine actually has a Python install; on a
        // clean machine the command correctly reports nothing to migrate, which
        // is a success, so the two outcomes are both acceptable but the message
        // must never be a panic.
        match super::migrate_command(&cli) {
            Ok(()) => {}
            Err((code, msg)) => {
                assert_eq!(code, 2);
                assert!(msg.unwrap_or_default().contains("--from-artifact"));
            }
        }
    }
}
