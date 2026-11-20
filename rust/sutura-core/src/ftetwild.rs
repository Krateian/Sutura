// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Standalone fTetWild binary bridge (no Python, no pyo3).
//!
//! Mirrors `sutura/ftetwild_bridge.py::build_binary_cmd` and `run_binary`:
//! write the input surface to a temporary OBJ, run the deterministic binary
//! with one thread, and read the generated `out.msh__sf.obj` boundary.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use sutura_geom::decimate::decimate_impl;
use sutura_geom::manifold::stage2_rebuild_core;
use sutura_geom::topology::compute_normal_per_vertex_core;

use crate::config::intensity::{IntensitySpec, LadderEntry};
use crate::fmt::{fmt_g, py_repr_f64};
use crate::graft::helpers::referenced_only;
use crate::proxy::{surface_samples, HAUSDORFF_SEED};
use crate::tiers::common::{self, GuardPolicy, TopologySummary};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temporary directory removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, String> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        for _ in 0..1000 {
            let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("sutura_ftw_{pid}_{seq}"));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(format!("failed to create fTetWild temp dir: {err}"));
                }
            }
        }
        Err("failed to create a unique fTetWild temp dir".to_string())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Where a user-installed binary lives: `<data>/bin/ftetwild`.
///
/// The GUI's Install button copies a binary here and Remove deletes it. There is
/// no download: the Rust build ships the sidecar in the package and there is no
/// release asset to fetch, so the alternative to this pair is no pair at all.
pub fn managed_binary_path() -> PathBuf {
    crate::config::learning::data_dir()
        .join("bin")
        .join("ftetwild")
}

/// Copy `source` into [`managed_binary_path`], and make it executable.
///
/// Refuses to overwrite a different file: a mis-click must not replace a working
/// binary. Returns the destination.
pub fn install_binary(source: &Path) -> Result<PathBuf, String> {
    if !source.is_file() {
        return Err(format!("{} is not a file", source.display()));
    }
    let dest = managed_binary_path();
    if dest.is_file() {
        return Err(format!(
            "{} already exists; remove it first",
            dest.display()
        ));
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::copy(source, &dest).map_err(|e| format!("copy: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

/// Delete the managed binary. `Ok(false)` means there was none.
pub fn remove_binary() -> Result<bool, String> {
    let dest = managed_binary_path();
    if !dest.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&dest).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Locate the standalone fTetWild CLI the way the Python manager does.
///
/// `FTETWILD_BIN` is Python's override; `SUTURA_FTETWILD_BIN` is the name the
/// Rust CLI already uses. The repo checkout is checked before `PATH`.
pub fn find_binary() -> Option<String> {
    for key in ["FTETWILD_BIN", "SUTURA_FTETWILD_BIN"] {
        if let Some(raw) = std::env::var_os(key) {
            let path = PathBuf::from(raw);
            if is_executable(&path) {
                return Some(abspath(&path));
            }
        }
    }

    // Packaged sidecar: macOS `.app/Contents/MacOS/ftetwild`, a tarball
    // beside `sutura`, and the user data dir the installers populate.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join("ftetwild"),
                dir.join("../Resources/bin/ftetwild"),
                dir.join("../MacOS/ftetwild"),
            ] {
                if is_executable(&candidate) {
                    return Some(abspath(&candidate));
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for candidate in [
            home.join(".local/share/sutura/bin/ftetwild"),
            home.join(".local/share/sutura/ftetwild/bin/ftetwild"),
        ] {
            if is_executable(&candidate) {
                return Some(abspath(&candidate));
            }
        }
    }

    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo_bin = repo.join("third_party/ftetwild/bin/ftetwild");
    if is_executable(&repo_bin) {
        return Some(abspath(&repo_bin));
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("ftetwild");
            if is_executable(&candidate) {
                return Some(abspath(&candidate));
            }
        }
    }
    None
}

/// Python's `ftetwild_params()`: `{'optimize': True}` only when enabled by
/// `SUTURA_FTETWILD_OPTIMIZE` or `~/.config/sutura/config.json`, else `None`.
pub fn params_from_env() -> Value {
    if resolve_optimize() {
        json!({"optimize": true})
    } else {
        Value::Null
    }
}

/// User parameters for one tier run: `--ftetwild-optimize` forces the
/// optimizing attempt; otherwise the env/config value is used (Python
/// `ftetwild_params()`).
pub fn params_for_optimize(flag: bool) -> Value {
    if flag {
        return json!({"optimize": true});
    }
    params_from_env()
}

fn resolve_optimize() -> bool {
    if let Ok(raw) = std::env::var("SUTURA_FTETWILD_OPTIMIZE") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {}
        }
    }
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let path = PathBuf::from(home).join(".config/sutura/config.json");
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    value
        .get("ftetwild_optimize")
        .map(value_truthy)
        .unwrap_or(false)
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_f64().map(|v| v != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Merge `DEFAULT_PARAMS={'optimize': False}` with caller parameters, like
/// `ftetwild_bridge.tetrahedralize_params`.
fn merged_params(params: &Value) -> Value {
    let mut map = Map::new();
    map.insert("optimize".to_string(), Value::Bool(false));
    if let Some(obj) = params.as_object() {
        for (key, value) in obj {
            map.insert(key.clone(), value.clone());
        }
    }
    Value::Object(map)
}

fn present<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|value| !value.is_null())
}

fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        Value::Number(number) => {
            if let Some(i) = number.as_i64() {
                i.to_string()
            } else if let Some(u) = number.as_u64() {
                u.to_string()
            } else {
                py_repr_f64(number.as_f64().unwrap_or(0.0))
            }
        }
        other => other.to_string(),
    }
}

fn push_value(cmd: &mut Vec<String>, flag: &str, value: &Value) {
    cmd.push(flag.to_string());
    cmd.push(py_str(value));
}

fn normalize(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Python `os.path.abspath`: absolute, lexically normalised, no existence
/// requirement.
fn abspath(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize(absolute).to_string_lossy().into_owned()
}

/// Build the exact fTetWild command line used by the Python bridge.
pub fn build_binary_cmd(bin_path: &str, src: &Path, msh_out: &Path, params: &Value) -> Vec<String> {
    let merged = merged_params(params);
    let kw = merged.as_object().expect("merged params are an object");
    let mut cmd = vec![
        bin_path.to_string(),
        "-i".to_string(),
        abspath(src),
        "-o".to_string(),
        abspath(msh_out),
        "--max-threads".to_string(),
        "1".to_string(),
        "-q".to_string(),
        "--no-color".to_string(),
    ];

    if let Some(value) = present(kw, "max_its") {
        push_value(&mut cmd, "--max-its", value);
    } else if matches!(kw.get("optimize"), Some(Value::Bool(false))) {
        cmd.push("--max-its".to_string());
        cmd.push("0".to_string());
    }

    if matches!(kw.get("simplify"), Some(Value::Bool(false)))
        || matches!(kw.get("skip_simplify"), Some(Value::Bool(true)))
    {
        cmd.push("--skip-simplify".to_string());
    }

    if let Some(value) = present(kw, "stop_energy") {
        push_value(&mut cmd, "--stop-energy", value);
    }

    if let Some(value) = present(kw, "edge_length_abs")
        .or_else(|| present(kw, "ideal_edge_length"))
        .or_else(|| present(kw, "la"))
    {
        push_value(&mut cmd, "-a", value);
    }

    if let Some(value) = present(kw, "lr").or_else(|| present(kw, "edge_length_fac")) {
        push_value(&mut cmd, "-l", value);
    }

    if let Some(value) = present(kw, "epsr").or_else(|| present(kw, "epsilon")) {
        push_value(&mut cmd, "-e", value);
    }

    cmd
}

fn write_input_obj(path: &Path, verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Result<(), String> {
    let mut writer = BufWriter::new(
        File::create(path).map_err(|err| format!("failed writing fTetWild input OBJ: {err}"))?,
    );
    writer
        .write_all(b"# sutura intermediate\n")
        .map_err(|err| format!("failed writing fTetWild input OBJ: {err}"))?;
    for vertex in verts {
        writeln!(
            writer,
            "v {} {} {}",
            fmt_g(vertex[0], 9),
            fmt_g(vertex[1], 9),
            fmt_g(vertex[2], 9)
        )
        .map_err(|err| format!("failed writing fTetWild input OBJ: {err}"))?;
    }
    for tri in tris {
        writeln!(writer, "f {} {} {}", tri[0] + 1, tri[1] + 1, tri[2] + 1)
            .map_err(|err| format!("failed writing fTetWild input OBJ: {err}"))?;
    }
    writer
        .flush()
        .map_err(|err| format!("failed writing fTetWild input OBJ: {err}"))
}

fn tail(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        trimmed.to_string()
    } else {
        chars[chars.len() - limit..].iter().collect()
    }
}

fn round2(seconds: f64) -> f64 {
    (seconds * 100.0).round() / 100.0
}

fn is_watertight(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> bool {
    if verts.is_empty() || tris.is_empty() {
        return false;
    }
    let topo = crate::tiers::common::topology_summary(verts, tris);
    topo.boundary_edges == 0
        && topo.non_two_manifold_edges == 0
        && crate::tiers::common::is_two_manifold(verts.len(), tris)
}

/// Run the standalone fTetWild binary on an in-memory surface.
///
/// `Err` means the binary itself is unavailable (empty path) or the temporary
/// input could not be prepared. A process failure, timeout or unreadable
/// output returns `Ok(([], [], report))` with `report.error` set, mirroring the
/// Python bridge's report contract.
pub fn run_binary(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    bin_path: &str,
    params: &Value,
) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>, Value), String> {
    run_binary_with_timeout(verts, tris, bin_path, params, None)
}

/// [`run_binary`] with the watchdog budget supplied by the caller
/// (`_ftetwild_attempt` passes the remaining tier budget).
pub fn run_binary_with_timeout(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    bin_path: &str,
    params: &Value,
    timeout_override: Option<f64>,
) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>, Value), String> {
    run_binary_with_timeout_cancel(verts, tris, bin_path, params, timeout_override, None)
}

/// `run_binary_with_timeout` with a cooperative cancellation probe.
pub fn run_binary_with_timeout_cancel(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    bin_path: &str,
    params: &Value,
    timeout_override: Option<f64>,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>, Value), String> {
    let started = Instant::now();
    if bin_path.trim().is_empty() {
        return Err("fTetWild binary unavailable: empty binary path".to_string());
    }

    let kw = merged_params(params);
    let timeout_s = timeout_override
        .or_else(|| kw.get("timeout").and_then(Value::as_f64))
        .unwrap_or(180.0)
        .max(0.0);

    let tmp = TempDir::new()?;
    let src = tmp.path().join("in.obj");
    let msh_out = tmp.path().join("out.msh");
    write_input_obj(&src, verts, tris)?;

    let cmd = build_binary_cmd(bin_path, &src, &msh_out, &kw);
    let stdout_path = tmp.path().join("stdout.txt");
    let stderr_path = tmp.path().join("stderr.txt");
    let stdout = File::create(&stdout_path)
        .map_err(|err| format!("failed to open fTetWild stdout capture: {err}"))?;
    let stderr = File::create(&stderr_path)
        .map_err(|err| format!("failed to open fTetWild stderr capture: {err}"))?;

    let mut report = json!({
        "engine": "binary",
        "binary": bin_path,
        "params": kw,
        "input_vertices": verts.len(),
        "input_faces": tris.len(),
        "faces": 0,
        "watertight": false,
        "error": Value::Null,
        "time": 0.0,
    });

    let mut child = match Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(tmp.path())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            report["error"] = Value::from(format!("process launch error: {err}"));
            report["time"] = json!(round2(started.elapsed().as_secs_f64()));
            return Ok((Vec::new(), Vec::new(), report));
        }
    };

    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if sutura_geom::cancel::cancelled(cancel) {
                    let _ = child.kill();
                    let _ = child.wait();
                    report["error"] = Value::from("cancelled");
                    report["time"] = json!(round2(started.elapsed().as_secs_f64()));
                    return Ok((Vec::new(), Vec::new(), report));
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                report["error"] = Value::from(format!("process wait error: {err}"));
                report["time"] = json!(round2(started.elapsed().as_secs_f64()));
                return Ok((Vec::new(), Vec::new(), report));
            }
        }
    };

    report["time"] = json!(round2(started.elapsed().as_secs_f64()));
    if timed_out {
        report["error"] = Value::from("timeout");
        return Ok((Vec::new(), Vec::new(), report));
    }

    let status = status.expect("non-timeout child has a status");
    if !status.success() {
        let mut err_text = String::new();
        if let Ok(mut file) = File::open(&stderr_path) {
            let _ = file.read_to_string(&mut err_text);
        }
        report["returncode"] = json!(status.code().unwrap_or(-1));
        report["error"] = Value::from(format!(
            "fTetWild failed (rc={}): {}",
            status.code().unwrap_or(-1),
            tail(&err_text, 300)
        ));
        return Ok((Vec::new(), Vec::new(), report));
    }
    report["returncode"] = json!(0);

    let sf_obj = tmp.path().join("out.msh__sf.obj");
    if !sf_obj.is_file() {
        report["error"] = Value::from("fTetWild completed but produced no surface mesh (__sf.obj)");
        return Ok((Vec::new(), Vec::new(), report));
    }

    let mesh = match crate::mesh_io::read_obj(&sf_obj) {
        Ok(mesh) => mesh,
        Err(err) => {
            report["error"] = Value::from(format!("failed reading/copying surface mesh: {err}"));
            return Ok((Vec::new(), Vec::new(), report));
        }
    };
    if mesh.tris.is_empty() {
        report["error"] = Value::from("fTetWild produced empty surface mesh");
        return Ok((Vec::new(), Vec::new(), report));
    }

    let out_v = mesh
        .verts
        .iter()
        .map(|v| [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])])
        .collect::<Vec<_>>();
    let out_t = mesh
        .tris
        .iter()
        .map(|t| [i64::from(t[0]), i64::from(t[1]), i64::from(t[2])])
        .collect::<Vec<_>>();
    report["faces"] = json!(out_t.len());
    report["watertight"] = json!(is_watertight(&out_v, &out_t));
    report["output_vertices"] = json!(out_v.len());
    report["output_faces"] = json!(out_t.len());
    report["ok"] = Value::Bool(true);

    Ok((out_v, out_t, report))
}

/// `FTETWILD_RETRY_PARAMS`: the deterministic second attempt.
pub const RETRY_PARAMS: &str = "{\"optimize\":true}";

/// `FTETWILD_ABORT_MESSAGE`; the `%s` budget slot is filled by [`abort_message`].
pub const ABORT_MESSAGE: &str = "fTetWild was stopped for time (%s budget, checked between attempts); the previous stage result was kept";

/// How the tier is triggered (`resolve_ftetwild`): auto only for residual
/// damage, always also for a closed result that still self-intersects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Disabled,
    Auto,
    Always,
}

/// Resolve the trigger from the optional `ftetwild` repair option and the
/// intensity preset, mirroring `resolve_ftetwild` + `spec.ftetwild_enabled`.
pub fn resolve_trigger(option: Option<&Value>, enabled: bool) -> Trigger {
    let auto = || {
        if enabled {
            Trigger::Auto
        } else {
            Trigger::Disabled
        }
    };
    let Some(value) = option else {
        return auto();
    };
    match value {
        Value::Bool(true) => Trigger::Always,
        Value::Bool(false) => Trigger::Disabled,
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "always" | "force" | "on" | "true" | "yes" | "1" => Trigger::Always,
            "off" | "no" | "false" | "0" => Trigger::Disabled,
            "auto" => auto(),
            _ => auto(),
        },
        Value::Null => auto(),
        _ => auto(),
    }
}

/// Reloaded Hausdorff and adoption gates for the fTetWild tier.
#[derive(Clone, Debug)]
pub struct TierTuning {
    pub max_hausdorff_rel: f64,
    pub decimate_hd_margin: f64,
    pub guard: GuardPolicy,
}

struct Candidate {
    verts: Vec<[f64; 3]>,
    tris: Vec<[i64; 3]>,
    after: TopologySummary,
    holes: usize,
    nm: usize,
    postprocessed: bool,
}

struct Attempt {
    report: Map<String, Value>,
    candidate: Option<Candidate>,
    dense_failed: bool,
}

/// The tier result; `adopted` false keeps the caller's current arrays.
pub struct TierResult {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[i64; 3]>,
    pub after: TopologySummary,
    pub report: Value,
    pub adopted: bool,
    pub shape_changed: bool,
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

fn json_opt_f64(value: Option<f64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

fn abort_message(timeout: f64) -> String {
    let budget = if timeout != 0.0 {
        format!("{} s", fmt_g(timeout, 6))
    } else {
        "time".to_string()
    };
    ABORT_MESSAGE.replace("%s", &budget)
}

fn to_i32(tris: &[[i64; 3]]) -> Vec<[i32; 3]> {
    tris.iter()
        .map(|t| [t[0] as i32, t[1] as i32, t[2] as i32])
        .collect()
}

fn measure(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> (TopologySummary, usize, usize) {
    let after = common::topology_summary(verts, tris);
    let holes = crate::repair::boundary_loop_stats(tris).0;
    (after, holes, after.non_two_manifold_edges)
}

fn exact_si_count(verts: &[[f64; 3]], tris: &[[i64; 3]]) -> Option<i64> {
    if tris.len() < 2 {
        return Some(0);
    }
    let st: Vec<[usize; 3]> = tris
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let (_mask, count, skipped) = sutura_geom::self_intersections_core(verts, &st);
    if skipped || count < 0 {
        None
    } else {
        Some(count)
    }
}

/// `_hausdorff_rel`: one-sided max distance from samples on the output to the
/// input, relative to the input bbox diagonal. The shared PCG64 sampler keeps
/// both languages on the same samples/seed.
fn hausdorff_rel(
    in_v: &[[f64; 3]],
    in_t: &[[i64; 3]],
    out_v: &[[f64; 3]],
    out_t: &[[i64; 3]],
    samples: usize,
) -> Option<f64> {
    if in_t.is_empty() || out_t.is_empty() {
        return None;
    }
    let (in_v, in_t) = referenced_only(in_v, in_t);
    let (out_v, out_t) = referenced_only(out_v, out_t);
    if in_v.is_empty() || out_v.is_empty() {
        return None;
    }
    let diag = common::bbox_diagonal(&in_v);
    if diag == 0.0 {
        return None;
    }
    let pts = surface_samples(&out_v, &out_t, samples, HAUSDORFF_SEED);
    if pts.is_empty() {
        return None;
    }
    let closest = common::closest_points(&in_v, &in_t, &pts);
    if closest.is_empty() {
        return None;
    }
    let max = closest.iter().fold(f64::NEG_INFINITY, |m, x| m.max(x.1));
    Some(round6(max / diag))
}

/// Rebuild a candidate that is not already `(holes=0, nm=0, two-manifold)`
/// as a Manifold solid, keeping the rebuild only when holes and non-manifold
/// edges do not regress (`_ftetwild_manifold_postprocess`).
fn manifold_postprocess(
    verts: Vec<[f64; 3]>,
    tris: Vec<[i64; 3]>,
    after: TopologySummary,
    holes: usize,
    nm: usize,
) -> Candidate {
    if nm == 0 && holes == 0 && common::is_two_manifold(verts.len(), &tris) {
        return Candidate {
            verts,
            tris,
            after,
            holes,
            nm,
            postprocessed: false,
        };
    }
    let (m_v, m_t, rep) = stage2_rebuild_core(&verts, &to_i32(&tris));
    if rep.ok && rep.error.is_none() && !m_t.is_empty() {
        let m3_v: Vec<[f64; 3]> = m_v
            .iter()
            .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
            .collect();
        let m3_t: Vec<[i64; 3]> = m_t
            .iter()
            .map(|t| [i64::from(t[0]), i64::from(t[1]), i64::from(t[2])])
            .collect();
        let (m3_after, m3_holes, m3_nm) = measure(&m3_v, &m3_t);
        if m3_holes <= holes && m3_nm <= nm {
            return Candidate {
                verts: m3_v,
                tris: m3_t,
                after: m3_after,
                holes: m3_holes,
                nm: m3_nm,
                postprocessed: true,
            };
        }
    }
    Candidate {
        verts,
        tris,
        after,
        holes,
        nm,
        postprocessed: false,
    }
}

/// Measure a boundary and run the Manifold post-process (`_ftetwild_candidate`).
fn candidate(verts: Vec<[f64; 3]>, tris: Vec<[i64; 3]>) -> Candidate {
    let (after, holes, nm) = measure(&verts, &tris);
    manifold_postprocess(verts, tris, after, holes, nm)
}

/// `_decimate_boundary`: drop unreferenced vertices, then run the native
/// quadric collapse with the border locked and per-vertex normals as
/// attributes.
fn decimate_boundary(
    verts: &[[f64; 3]],
    tris: &[[i64; 3]],
    target: usize,
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>)> {
    let (rv, rt) = referenced_only(verts, tris);
    if rt.is_empty() {
        return None;
    }
    let normals = compute_normal_per_vertex_core(&rv, &rt);
    let flat: Vec<f64> = normals
        .iter()
        .flat_map(|n| [f64::from(n[0]), f64::from(n[1]), f64::from(n[2])])
        .collect();
    let rep = decimate_impl(&rv, &rt, target, None, true, Some((&flat, 3))).ok()?;
    if rep.tris.is_empty() {
        return None;
    }
    Some((rep.verts, rep.tris))
}

fn dense_targets(input_faces: usize, spec: &IntensitySpec) -> Vec<usize> {
    let mut targets: Vec<usize> = Vec::new();
    for entry in &spec.dense_target_ladder {
        let target = match entry {
            LadderEntry::Threshold => (spec.dense_ratio
                * input_faces.max(spec.dense_min_faces.max(0) as usize) as i64)
                .max(0) as usize,
            LadderEntry::Number(mult) => {
                let scaled = (mult * input_faces as f64).round_ties_even();
                (scaled as i64).max(spec.dense_min_faces).max(0) as usize
            }
        };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

/// `_ftetwild_decimate_and_postprocess`: try the escalating ladder, accepting
/// only a strictly watertight candidate whose one-sided Hausdorff distance
/// stays within the raw boundary's own distance plus the margin.
fn decimate_and_postprocess(
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    cand_v: &[[f64; 3]],
    cand_t: &[[i64; 3]],
    spec: &IntensitySpec,
    tuning: &TierTuning,
) -> (Option<Candidate>, Map<String, Value>) {
    let input_faces = input_t.len();
    let raw_faces = cand_t.len();
    let mut info = Map::new();
    info.insert("ftetwild_decimated".into(), Value::Bool(false));
    info.insert("ftetwild_faces_final".into(), Value::from(raw_faces));
    let mut decimate_time = 0.0f64;
    info.insert("ftetwild_decimate_time".into(), Value::from(0.0));

    let samples = spec.ftetwild_hausdorff_samples.max(0) as usize;
    let hd_raw = hausdorff_rel(input_v, input_t, cand_v, cand_t, samples);
    info.insert("ftetwild_hausdorff_raw".into(), json_opt_f64(hd_raw));
    let limit = hd_raw.unwrap_or(0.0) + tuning.decimate_hd_margin;
    let raw_within_shape = hd_raw
        .map(|h| h <= tuning.max_hausdorff_rel)
        .unwrap_or(false);

    let mut attempts: Vec<Value> = Vec::new();
    for target in dense_targets(input_faces, spec) {
        let started = Instant::now();
        let dec = decimate_boundary(cand_v, cand_t, target);
        decimate_time = round3(decimate_time + started.elapsed().as_secs_f64());
        info.insert("ftetwild_decimate_time".into(), Value::from(decimate_time));
        let mut rec = Map::new();
        rec.insert("target".into(), Value::from(target as i64));
        let Some((dv, dt)) = dec else {
            rec.insert("reason".into(), Value::from("decimation_failed"));
            attempts.push(Value::Object(rec));
            continue;
        };
        let dec_faces = dt.len();
        rec.insert("faces".into(), Value::from(dec_faces));
        let payload = candidate(dv, dt);
        rec.insert("holes".into(), Value::from(payload.holes));
        rec.insert("non_manifold".into(), Value::from(payload.nm));
        if !(payload.holes == 0
            && payload.nm == 0
            && common::is_two_manifold(payload.verts.len(), &payload.tris))
        {
            rec.insert("reason".into(), Value::from("not_watertight"));
            attempts.push(Value::Object(rec));
            continue;
        }
        let hd_dec = hausdorff_rel(input_v, input_t, &payload.verts, &payload.tris, samples);
        rec.insert("hausdorff_rel".into(), json_opt_f64(hd_dec));
        if hd_dec.is_none()
            || hd_dec.unwrap() > limit
            || (raw_within_shape && hd_dec.unwrap() > tuning.max_hausdorff_rel)
        {
            rec.insert("reason".into(), Value::from("hausdorff"));
            attempts.push(Value::Object(rec));
            continue;
        }
        info.insert("ftetwild_decimated".into(), Value::Bool(true));
        info.insert(
            "ftetwild_faces_final".into(),
            Value::from(payload.tris.len()),
        );
        info.insert("ftetwild_faces_decimated".into(), Value::from(dec_faces));
        info.insert(
            "ftetwild_decimate_target".into(),
            Value::from(target as i64),
        );
        info.insert("ftetwild_hausdorff_decimated".into(), json_opt_f64(hd_dec));
        attempts.push(Value::Object(rec));
        info.insert("ftetwild_decimate_attempts".into(), Value::Array(attempts));
        info.insert("hausdorff_rel".into(), json_opt_f64(hd_dec));
        return (Some(payload), info);
    }
    info.insert(
        "ftetwild_decimate_attempts".into(),
        Value::Array(attempts.clone()),
    );
    if let Some(last) = attempts.last() {
        info.insert(
            "ftetwild_decimate_fallback".into(),
            last.get("reason").cloned().unwrap_or(Value::Null),
        );
    }
    (None, info)
}

fn raw_attempt_report(out_tris: usize) -> Map<String, Value> {
    let mut att = Map::new();
    att.insert("ftetwild_faces_raw".into(), Value::from(out_tris));
    att.insert("ftetwild_decimated".into(), Value::Bool(false));
    att.insert("ftetwild_faces_final".into(), Value::from(out_tris));
    att.insert("ftetwild_decimate_time".into(), Value::from(0.0));
    att
}

/// One fTetWild run plus dense-boundary handling (`_ftetwild_boundary`).
fn boundary_attempt(
    tag: &str,
    params: &Value,
    timeout: f64,
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    spec: &IntensitySpec,
    tuning: &TierTuning,
    bin: &str,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> Attempt {
    let started = Instant::now();
    let mut att = Map::new();
    att.insert("attempt".into(), Value::from(tag));
    att.insert(
        "wall_time".into(),
        Value::from(round2(started.elapsed().as_secs_f64())),
    );
    let run = run_binary_with_timeout_cancel(
        input_v, input_t, bin, params, Some(timeout.max(0.0)), cancel);
    let (cand_v, cand_t, report) = match run {
        Ok(out) => out,
        Err(err) => {
            att.insert("error".into(), Value::from(err));
            att.insert(
                "wall_time".into(),
                Value::from(round2(started.elapsed().as_secs_f64())),
            );
            return Attempt {
                report: att,
                candidate: None,
                dense_failed: false,
            };
        }
    };
    if let Some(obj) = report.as_object() {
        for (key, value) in obj {
            att.insert(key.clone(), value.clone());
        }
    }
    att.insert(
        "wall_time".into(),
        Value::from(round2(started.elapsed().as_secs_f64())),
    );
    if report.get("error").map(|v| !v.is_null()).unwrap_or(false) || cand_t.is_empty() {
        return Attempt {
            report: att,
            candidate: None,
            dense_failed: false,
        };
    }

    let raw_faces = cand_t.len();
    att.extend(raw_attempt_report(raw_faces));
    let mut dense_failed = false;
    let dense = !spec.dense_target_ladder.is_empty()
        && raw_faces
            > (spec.dense_ratio * input_t.len().max(spec.dense_min_faces.max(0) as usize) as i64)
                .max(0) as usize;
    if dense {
        let (payload, dec_info) =
            decimate_and_postprocess(input_v, input_t, &cand_v, &cand_t, spec, tuning);
        for (key, value) in dec_info {
            att.insert(key, value);
        }
        if let Some(payload) = payload {
            att.insert(
                "manifold_postprocessed".into(),
                Value::Bool(payload.postprocessed),
            );
            att.insert("output_holes".into(), Value::from(payload.holes));
            att.insert("output_non_manifold".into(), Value::from(payload.nm));
            return Attempt {
                report: att,
                candidate: Some(payload),
                dense_failed: false,
            };
        }
        dense_failed = true;
    }

    let payload = candidate(cand_v, cand_t);
    att.insert(
        "manifold_postprocessed".into(),
        Value::Bool(payload.postprocessed),
    );
    att.insert("output_holes".into(), Value::from(payload.holes));
    att.insert("output_non_manifold".into(), Value::from(payload.nm));
    Attempt {
        report: att,
        candidate: Some(payload),
        dense_failed,
    }
}

fn params_optimize(params: &Value) -> bool {
    params.get("optimize").map(value_truthy).unwrap_or(false)
}

/// One attempt, including the Extreme dense-failure optimize retry
/// (`_ftetwild_attempt`).
fn tier_attempt(
    tag: &str,
    params: &Value,
    timeout: f64,
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    spec: &IntensitySpec,
    tuning: &TierTuning,
    bin: &str,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> (Map<String, Value>, Option<Candidate>) {
    let mut attempt = boundary_attempt(
        tag, params, timeout, input_v, input_t, spec, tuning, bin, cancel);
    if attempt.candidate.is_some()
        && attempt.dense_failed
        && spec.ftetwild_optimize_retry_on_dense_fail
        && !params_optimize(params)
    {
        let retry_tag = format!("{tag}_optimize");
        let retry_params = json!({"optimize": true});
        let retry = boundary_attempt(
            &retry_tag,
            &retry_params,
            timeout,
            input_v,
            input_t,
            spec,
            tuning,
            bin,
            cancel,
        );
        let mut retry_report = retry.report.clone();
        retry_report.insert("ran".into(), Value::Bool(true));
        attempt
            .report
            .insert("optimize_retry".into(), Value::Object(retry_report));
        if let Some(candidate) = retry.candidate {
            attempt
                .report
                .insert("optimize_retry_adopted".into(), Value::Bool(true));
            attempt.report.insert(
                "ftetwild_faces_final".into(),
                Value::from(candidate.tris.len()),
            );
            attempt.report.insert(
                "manifold_postprocessed".into(),
                retry
                    .report
                    .get("manifold_postprocessed")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            attempt
                .report
                .insert("output_holes".into(), Value::from(candidate.holes));
            attempt
                .report
                .insert("output_non_manifold".into(), Value::from(candidate.nm));
            return (attempt.report, Some(candidate));
        }
        attempt
            .report
            .insert("optimize_retry_adopted".into(), Value::Bool(false));
    }
    (attempt.report, attempt.candidate)
}

/// Run the fTetWild tier (`_ftetwild_tier`).
///
/// Returns `None` when the tier did not start (no residual damage, or `auto`
/// on a preset with fTetWild disabled). Otherwise the report is the
/// `experimental_ftetwild` object and `adopted` tells the caller whether to
/// replace its current arrays.
#[allow(clippy::too_many_arguments)]
pub fn run_tier(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    after: &TopologySummary,
    bin: &str,
    spec: &IntensitySpec,
    tuning: &TierTuning,
    force_si: bool,
) -> Option<TierResult> {
    run_tier_with_params(
        cur_v,
        cur_t,
        input_v,
        input_t,
        after,
        bin,
        spec,
        tuning,
        force_si,
        params_from_env(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_tier_with_cancel(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    after: &TopologySummary,
    bin: &str,
    spec: &IntensitySpec,
    tuning: &TierTuning,
    force_si: bool,
    user_params: Value,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> Option<TierResult> {
    run_tier_with_params_cancel(
        cur_v, cur_t, input_v, input_t, after, bin, spec, tuning, force_si,
        user_params, cancel,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_tier_with_params(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    after: &TopologySummary,
    bin: &str,
    spec: &IntensitySpec,
    tuning: &TierTuning,
    force_si: bool,
    user_params: Value,
) -> Option<TierResult> {
    run_tier_with_params_cancel(
        cur_v, cur_t, input_v, input_t, after, bin, spec, tuning, force_si,
        user_params, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_tier_with_params_cancel(
    cur_v: &[[f64; 3]],
    cur_t: &[[i64; 3]],
    input_v: &[[f64; 3]],
    input_t: &[[i64; 3]],
    after: &TopologySummary,
    bin: &str,
    spec: &IntensitySpec,
    tuning: &TierTuning,
    force_si: bool,
    user_params: Value,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> Option<TierResult> {
    if input_t.is_empty() {
        return None;
    }
    let cur_holes = crate::repair::boundary_loop_stats(cur_t).0;
    let cur_nm = after.non_two_manifold_edges;
    let cur_si = if force_si {
        exact_si_count(cur_v, cur_t).unwrap_or(0).max(0)
    } else {
        0
    };
    if cur_si <= 0 && cur_holes == 0 && cur_nm == 0 {
        return None;
    }

    let trigger = if force_si { "always" } else { "auto" };
    if let Some(cap) = spec.ftetwild_max_faces {
        if input_t.len() as i64 > cap {
            let report = json!({
                "ran": false,
                "adopted": false,
                "reject_reason": "too_large",
                "input_faces": input_t.len(),
                "max_faces": cap,
                "trigger": trigger,
            });
            return Some(TierResult {
                verts: cur_v.to_vec(),
                tris: cur_t.to_vec(),
                after: *after,
                report,
                adopted: false,
                shape_changed: false,
            });
        }
    }

    let mut ft_rep = Map::new();
    ft_rep.insert("ran".into(), Value::Bool(true));
    ft_rep.insert("trigger".into(), Value::from(trigger));

    let has_user_params = user_params
        .as_object()
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    let plan: Vec<(String, Option<Value>)> = if has_user_params {
        vec![("optimize".to_string(), Some(user_params))]
    } else {
        vec![
            ("default".to_string(), None),
            (
                "optimize".to_string(),
                Some(serde_json::from_str(RETRY_PARAMS).expect("valid retry params")),
            ),
        ]
    };

    let mut attempts: Vec<Value> = Vec::new();
    let started = Instant::now();
    let mut chosen: Option<(Map<String, Value>, Candidate)> = None;
    let mut aborted = false;
    for (tag, params) in plan {
        if let Some(last) = attempts.last() {
            let retry_ran = last
                .get("optimize_retry")
                .and_then(|r| r.get("ran"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if tag == "optimize" && retry_ran {
                break;
            }
            let reason = last.get("reject_reason").and_then(Value::as_str);
            if reason != Some("holes_nm") {
                break;
            }
        }
        let remaining = spec.ftetwild_timeout - started.elapsed().as_secs_f64();
        if remaining <= 0.0 {
            aborted = true;
            break;
        }
        let params_value = params.unwrap_or(Value::Null);
        let (mut att, cand) = tier_attempt(
            &tag,
            &params_value,
            remaining,
            input_v,
            input_t,
            spec,
            tuning,
            bin,
            cancel,
        );
        let timed_out = att.get("error").and_then(Value::as_str) == Some("timeout");
        let candidate = cand;
        attempts.push(Value::Object(att.clone()));
        if timed_out {
            aborted = true;
            break;
        }
        let Some(candidate) = candidate else {
            continue;
        };
        if candidate.holes <= cur_holes && candidate.nm <= cur_nm {
            chosen = Some((att, candidate));
            break;
        }
        att.insert("reject_reason".into(), Value::from("holes_nm"));
        if let Some(last) = attempts.last_mut() {
            *last = Value::Object(att);
        }
    }

    if !attempts.is_empty() {
        let final_report = chosen
            .as_ref()
            .map(|(att, _)| att.clone())
            .or_else(|| {
                attempts
                    .last()
                    .and_then(Value::as_object)
                    .map(|m| m.clone())
            })
            .unwrap_or_default();
        for (key, value) in final_report {
            if key != "attempt" {
                ft_rep.insert(key, value);
            }
        }
        ft_rep.insert("attempts".into(), Value::Array(attempts));
        ft_rep.insert("adopted".into(), Value::Bool(chosen.is_some()));
    }
    if aborted {
        ft_rep.insert("adopted".into(), Value::Bool(false));
        ft_rep.insert("aborted".into(), Value::Bool(true));
        ft_rep.insert("reject_reason".into(), Value::from("aborted_time"));
        ft_rep.insert(
            "reason".into(),
            Value::from(abort_message(spec.ftetwild_timeout)),
        );
    }

    let mut adopted = false;
    let mut shape_changed = false;
    let mut out_v = cur_v.to_vec();
    let mut out_t = cur_t.to_vec();
    let mut out_after = *after;
    if let Some((att, candidate)) = chosen.take() {
        let improved_topology = candidate.holes < cur_holes || candidate.nm < cur_nm;
        let blowup = common::blowup_reason(
            input_t.len(),
            input_v.len(),
            candidate.tris.len(),
            candidate.verts.len(),
            0,
            &tuning.guard,
        );
        let measure_si =
            blowup.is_none() || (tuning.guard.allow_growth_if_improved && improved_topology);
        let (base_si, candidate_si) = if measure_si {
            (
                exact_si_count(cur_v, cur_t),
                exact_si_count(&candidate.verts, &candidate.tris),
            )
        } else {
            (None, None)
        };
        let mut fields = common::guard_fields(
            input_t.len(),
            input_v.len(),
            candidate.tris.len(),
            candidate.verts.len(),
            0,
            base_si,
            candidate_si,
            &tuning.guard,
            improved_topology,
        );
        let guard_ok = fields
            .remove("guard_ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let guard_reason = fields
            .get("guard_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        for (key, value) in fields {
            ft_rep.insert(key, value);
        }
        if !guard_ok {
            ft_rep.insert("adopted".into(), Value::Bool(false));
            ft_rep.insert("reject_reason".into(), Value::from("guard"));
            ft_rep.insert(
                "reason".into(),
                Value::from(format!(
                    "fTetWild candidate rejected: {}",
                    guard_reason.unwrap_or_default()
                )),
            );
        } else {
            ft_rep.insert(
                "adopted_attempt".into(),
                att.get("attempt").cloned().unwrap_or(Value::Null),
            );
            ft_rep.remove("reject_reason");
            let hd_max = ft_rep
                .get("hausdorff_rel")
                .and_then(Value::as_f64)
                .or_else(|| {
                    hausdorff_rel(
                        input_v,
                        input_t,
                        &candidate.verts,
                        &candidate.tris,
                        spec.ftetwild_hausdorff_samples.max(0) as usize,
                    )
                });
            ft_rep.insert("hausdorff_rel".into(), json_opt_f64(hd_max));
            shape_changed = hd_max
                .map(|h| h > tuning.max_hausdorff_rel)
                .unwrap_or(false);
            ft_rep.insert("shape_changed".into(), Value::Bool(shape_changed));
            out_v = candidate.verts;
            out_t = candidate.tris;
            out_after = candidate.after;
            adopted = true;
        }
    }

    Some(TierResult {
        verts: out_v,
        tris: out_t,
        after: out_after,
        report: Value::Object(ft_rep),
        adopted,
        shape_changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        (
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
    }

    #[test]
    fn command_builder_keeps_python_flag_order() {
        let params = json!({
            "max_its": 12,
            "simplify": false,
            "stop_energy": 15.5,
            "edge_length_abs": 0.75,
            "lr": 0.08,
            "epsr": 0.005,
            "optimize": false
        });
        let cmd = build_binary_cmd(
            "/mock/bin/ftetwild",
            Path::new("/mock/input.obj"),
            Path::new("/mock/output.msh"),
            &params,
        );
        assert_eq!(
            cmd,
            vec![
                "/mock/bin/ftetwild",
                "-i",
                "/mock/input.obj",
                "-o",
                "/mock/output.msh",
                "--max-threads",
                "1",
                "-q",
                "--no-color",
                "--max-its",
                "12",
                "--skip-simplify",
                "--stop-energy",
                "15.5",
                "-a",
                "0.75",
                "-l",
                "0.08",
                "-e",
                "0.005",
            ]
        );
    }

    #[test]
    fn default_params_disable_optimization_after_no_color() {
        let cmd = build_binary_cmd(
            "ftetwild",
            Path::new("/mock/input.obj"),
            Path::new("/mock/output.msh"),
            &Value::Null,
        );
        assert_eq!(
            &cmd[cmd.len() - 2..],
            &["--max-its".to_string(), "0".to_string()]
        );
    }

    #[test]
    fn empty_binary_path_is_unavailable() {
        let (verts, tris) = triangle();
        let err = run_binary(&verts, &tris, "", &Value::Null).unwrap_err();
        assert!(err.contains("unavailable"), "{err}");
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn fake_binary_writes_the_expected_sf_obj() {
        let script_dir = TempDir::new().unwrap();
        let script = write_script(
            script_dir.path(),
            "fake_ftetwild.sh",
            r#"#!/bin/sh
out=""
while [ $# -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        shift
        out="$1"
    fi
    shift
done
cat > "${out}__sf.obj" <<'EOF'
v 0 0 0
v 1 0 0
v 0 1 0
f 1 2 3
EOF
exit 0
"#,
        );
        let (verts, tris) = triangle();
        let (out_v, out_t, report) =
            run_binary(&verts, &tris, script.to_str().unwrap(), &Value::Null).unwrap();
        assert_eq!(report.get("engine").and_then(Value::as_str), Some("binary"));
        assert_eq!(
            report.get("input_vertices").and_then(Value::as_u64),
            Some(3)
        );
        assert_eq!(report.get("input_faces").and_then(Value::as_u64), Some(1));
        assert_eq!(report.get("faces").and_then(Value::as_u64), Some(1));
        assert_eq!(
            report.get("watertight").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(out_v.len(), 3);
        assert_eq!(out_t, vec![[0, 1, 2]]);
    }

    /// Binary harness for `/tmp/p10-ftetwild-gate.sh`; never run by the
    /// normal test suite. Environment: `FTW_GATE_IN`, `FTW_GATE_OUT`,
    /// `FTW_GATE_BIN`, optional `FTW_GATE_PARAMS` (JSON).
    #[test]
    #[ignore = "driven by /tmp/p10-ftetwild-gate.sh"]
    fn gate_harness() {
        let in_path = std::env::var("FTW_GATE_IN").expect("FTW_GATE_IN");
        let out_path = std::env::var("FTW_GATE_OUT").expect("FTW_GATE_OUT");
        let bin = std::env::var("FTW_GATE_BIN").expect("FTW_GATE_BIN");
        let params: Value = std::env::var("FTW_GATE_PARAMS")
            .ok()
            .map(|raw| serde_json::from_str(&raw).expect("FTW_GATE_PARAMS JSON"))
            .unwrap_or(Value::Null);

        let mesh = crate::mesh_io::read_obj(&in_path).expect("read gate input OBJ");
        let verts = mesh
            .verts
            .iter()
            .map(|v| [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])])
            .collect::<Vec<_>>();
        let tris = mesh
            .tris
            .iter()
            .map(|t| [i64::from(t[0]), i64::from(t[1]), i64::from(t[2])])
            .collect::<Vec<_>>();
        let (out_v, out_t, report) = run_binary(&verts, &tris, &bin, &params)
            .unwrap_or_else(|err| panic!("run_binary: {err}"));
        if let Some(err) = report.get("error").and_then(Value::as_str) {
            panic!("run_binary report error: {err}");
        }

        let mut bytes = Vec::with_capacity(16 + out_v.len() * 24 + out_t.len() * 24);
        bytes.extend_from_slice(b"SFTW1");
        bytes.extend_from_slice(&(out_v.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&(out_t.len() as u64).to_le_bytes());
        for vertex in &out_v {
            for coordinate in vertex {
                bytes.extend_from_slice(&coordinate.to_le_bytes());
            }
        }
        for tri in &out_t {
            for index in tri {
                bytes.extend_from_slice(&index.to_le_bytes());
            }
        }
        fs::write(out_path, bytes).expect("write gate output");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_child_and_reports_timeout() {
        let script_dir = TempDir::new().unwrap();
        let script = write_script(
            script_dir.path(),
            "slow_ftetwild.sh",
            "#!/bin/sh\nexec sleep 5\n",
        );
        let (verts, tris) = triangle();
        let (out_v, out_t, report) = run_binary(
            &verts,
            &tris,
            script.to_str().unwrap(),
            &json!({"timeout": 0.05}),
        )
        .unwrap();
        assert!(out_v.is_empty());
        assert!(out_t.is_empty());
        assert_eq!(report.get("error").and_then(Value::as_str), Some("timeout"));
        assert_eq!(report.get("faces").and_then(Value::as_u64), Some(0));
    }
    fn test_tuning() -> TierTuning {
        TierTuning {
            max_hausdorff_rel: 0.01,
            decimate_hd_margin: 0.005,
            guard: common::GuardPolicy {
                face_ratio: 4.0,
                vertex_ratio: 4.0,
                min_slack: 256,
                loop_face_slack: 16,
                loop_vertex_slack: 16,
                si_policy: "improved_flag".to_string(),
                si_rel_tol: 0.0,
                si_abs_tol: 0,
                face_abs_floor: 50_000,
                vertex_abs_floor: 50_000,
                allow_growth_if_improved: true,
            },
        }
    }

    fn test_spec() -> IntensitySpec {
        IntensitySpec {
            name: "balanced".to_string(),
            ftetwild_timeout: 5.0,
            ..IntensitySpec::default()
        }
    }

    fn tetra() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        (
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]],
        )
    }

    fn two_triangles() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        (
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [0.0, 1.0, 1.0],
            ],
            vec![[0, 1, 2], [3, 4, 5]],
        )
    }

    fn shared_face_tets() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        (
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, -1.0],
            ],
            vec![
                [0, 2, 1],
                [0, 1, 3],
                [1, 2, 3],
                [2, 0, 3],
                [0, 2, 1],
                [0, 1, 4],
                [1, 2, 4],
                [2, 0, 4],
            ],
        )
    }

    fn uv_sphere(rings: usize, segments: usize) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let mut verts = vec![[0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        for i in 1..rings {
            let theta = std::f64::consts::PI * i as f64 / rings as f64;
            for j in 0..segments {
                let phi = 2.0 * std::f64::consts::PI * j as f64 / segments as f64;
                verts.push([
                    theta.sin() * phi.cos(),
                    theta.sin() * phi.sin(),
                    theta.cos(),
                ]);
            }
        }
        let mut tris = Vec::new();
        for j in 0..segments {
            let a = 2 + j;
            let b = 2 + (j + 1) % segments;
            tris.push([0, a as i64, b as i64]);
        }
        for i in 0..rings - 2 {
            let ring = 2 + i * segments;
            let next = ring + segments;
            for j in 0..segments {
                let a = ring + j;
                let b = ring + (j + 1) % segments;
                let c = next + j;
                let d = next + (j + 1) % segments;
                tris.push([a as i64, c as i64, b as i64]);
                tris.push([b as i64, c as i64, d as i64]);
            }
        }
        let last = 2 + (rings - 2) * segments;
        for j in 0..segments {
            let a = last + j;
            let b = last + (j + 1) % segments;
            tris.push([1, b as i64, a as i64]);
        }
        (verts, tris)
    }

    #[cfg(unix)]
    fn obj_script(dir: &Path, name: &str, obj: &str) -> PathBuf {
        let body = format!(
            "#!/bin/sh\nout=\"\"\nwhile [ $# -gt 0 ]; do\n    if [ \"$1\" = \"-o\" ]; then\n        shift\n        out=\"$1\"\n    fi\n    shift\ndone\ncat > \"${{out}}__sf.obj\" <<'EOF'\n{obj}\nEOF\nexit 0\n"
        );
        write_script(dir, name, &body)
    }

    const TETRA_OBJ: &str =
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 3 2\nf 1 2 4\nf 2 3 4\nf 3 1 4";
    const TWO_TRI_OBJ: &str =
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 0 1 1\nf 1 2 3\nf 4 5 6";
    const FAR_CUBE_OBJ: &str = "v 10 10 10\nv 11 10 10\nv 11 11 10\nv 10 11 10\nv 10 10 11\nv 11 10 11\nv 11 11 11\nv 10 11 11\nf 1 2 3\nf 1 3 4\nf 5 6 7\nf 5 7 8\nf 1 2 6\nf 1 6 5\nf 2 3 7\nf 2 7 6\nf 3 4 8\nf 3 8 7\nf 4 1 5\nf 4 5 8";

    #[cfg(unix)]
    #[test]
    fn ftetwild_flags_a_far_candidate_instead_of_rejecting_it() {
        let dir = TempDir::new().unwrap();
        let bin = obj_script(dir.path(), "far_cube.sh", FAR_CUBE_OBJ);
        let (input_v, input_t) = triangle();
        let (cur_v, cur_t) = triangle();
        let after = common::topology_summary(&cur_v, &cur_t);
        let spec = test_spec();
        let result = run_tier_with_params(
            &cur_v,
            &cur_t,
            &input_v,
            &input_t,
            &after,
            bin.to_str().unwrap(),
            &spec,
            &test_tuning(),
            false,
            Value::Null,
        )
        .expect("tier runs on a holed input");
        assert!(result.adopted);
        assert!(result.shape_changed);
        let report = result.report.as_object().unwrap();
        assert_eq!(
            report
                .get("hausdorff_rel")
                .and_then(Value::as_f64)
                .map(|h| h > 0.01),
            Some(true)
        );
        assert_eq!(report.get("output_holes").and_then(Value::as_u64), Some(0));
        assert_eq!(
            report.get("adopted_attempt").and_then(Value::as_str),
            Some("default")
        );
        assert_eq!(report.get("trigger").and_then(Value::as_str), Some("auto"));
    }

    #[cfg(unix)]
    #[test]
    fn ftetwild_retries_once_when_holes_nm_do_not_improve() {
        let dir = TempDir::new().unwrap();
        let bin = obj_script(dir.path(), "two_tri.sh", TWO_TRI_OBJ);
        let (input_v, input_t) = shared_face_tets();
        let after = common::topology_summary(&input_v, &input_t);
        assert_eq!(after.non_two_manifold_edges > 0, true);
        assert_eq!(crate::repair::boundary_loop_stats(&input_t).0, 0);
        let result = run_tier_with_params(
            &input_v,
            &input_t,
            &input_v,
            &input_t,
            &after,
            bin.to_str().unwrap(),
            &test_spec(),
            &test_tuning(),
            false,
            Value::Null,
        )
        .expect("tier runs on a non-manifold input");
        assert!(!result.adopted);
        let report = result.report.as_object().unwrap();
        assert_eq!(
            report.get("reject_reason").and_then(Value::as_str),
            Some("holes_nm")
        );
        let attempts = report.get("attempts").and_then(Value::as_array).unwrap();
        assert_eq!(attempts.len(), 2);
        for attempt in attempts {
            assert_eq!(
                attempt.get("reject_reason").and_then(Value::as_str),
                Some("holes_nm")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ftetwild_dense_failure_retries_with_optimize() {
        let dir = TempDir::new().unwrap();
        let body = format!(
            "#!/bin/sh\nout=\"\"\nhas_its=0\nwhile [ $# -gt 0 ]; do\n    if [ \"$1\" = \"-o\" ]; then\n        shift\n        out=\"$1\"\n    elif [ \"$1\" = \"--max-its\" ]; then\n        has_its=1\n    fi\n    shift\ndone\nif [ \"$has_its\" = \"1\" ]; then\n    cat > \"${{out}}__sf.obj\" <<'EOF'\n{TWO_TRI_OBJ}\nEOF\nelse\n    cat > \"${{out}}__sf.obj\" <<'EOF'\n{TETRA_OBJ}\nEOF\nfi\nexit 0\n"
        );
        let bin = write_script(dir.path(), "dense_retry.sh", &body);
        let (input_v, input_t) = triangle();
        let after = common::topology_summary(&input_v, &input_t);
        let spec = IntensitySpec {
            name: "extreme".to_string(),
            ftetwild_timeout: 5.0,
            dense_ratio: 1,
            dense_min_faces: 1,
            dense_target_ladder: vec![LadderEntry::Number(1.5)],
            ftetwild_optimize_retry_on_dense_fail: true,
            ..IntensitySpec::default()
        };
        let result = run_tier_with_params(
            &input_v,
            &input_t,
            &input_v,
            &input_t,
            &after,
            bin.to_str().unwrap(),
            &spec,
            &test_tuning(),
            false,
            Value::Null,
        )
        .expect("tier runs on a holed input");
        assert!(result.adopted);
        let report = result.report.as_object().unwrap();
        assert_eq!(
            report
                .get("optimize_retry_adopted")
                .and_then(Value::as_bool),
            Some(true)
        );
        let retry = report
            .get("optimize_retry")
            .and_then(Value::as_object)
            .unwrap();
        assert_eq!(retry.get("ran").and_then(Value::as_bool), Some(true));
        assert_eq!(report.get("output_holes").and_then(Value::as_u64), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn ftetwild_rejects_above_the_face_cap_with_too_large() {
        let (input_v, input_t) = two_triangles();
        let after = common::topology_summary(&input_v, &input_t);
        let spec = IntensitySpec {
            ftetwild_max_faces: Some(1),
            ..test_spec()
        };
        let result = run_tier_with_params(
            &input_v,
            &input_t,
            &input_v,
            &input_t,
            &after,
            "unused-binary",
            &spec,
            &test_tuning(),
            false,
            Value::Null,
        )
        .expect("the cap short-circuits the tier");
        assert!(!result.adopted);
        let report = result.report.as_object().unwrap();
        assert_eq!(report.get("ran").and_then(Value::as_bool), Some(false));
        assert_eq!(
            report.get("reject_reason").and_then(Value::as_str),
            Some("too_large")
        );
        assert_eq!(report.get("max_faces").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn ftetwild_manifold_postprocess_only_keeps_a_non_regressing_rebuild() {
        let (verts, tris) = shared_face_tets();
        let (after, holes, nm) = measure(&verts, &tris);
        assert!(nm > 0);
        let rebuilt = manifold_postprocess(verts.clone(), tris.clone(), after, holes, nm);
        assert!(rebuilt.holes <= holes && rebuilt.nm <= nm);
        if rebuilt.postprocessed {
            assert_eq!((rebuilt.holes, rebuilt.nm), (0, 0));
        }
        let (clean_v, clean_t) = tetra();
        let (clean_after, clean_holes, clean_nm) = measure(&clean_v, &clean_t);
        assert_eq!((clean_holes, clean_nm), (0, 0));
        let unchanged = manifold_postprocess(clean_v, clean_t, clean_after, clean_holes, clean_nm);
        assert!(!unchanged.postprocessed);
        assert_eq!((unchanged.holes, unchanged.nm), (0, 0));
    }

    #[test]
    fn ftetwild_decimation_uses_the_attribute_path_and_is_deterministic() {
        let (mut verts, tris) = tetra();
        verts.push([99.0, 99.0, 99.0]);
        let first = decimate_boundary(&verts, &tris, 4).expect("decimation runs");
        let second = decimate_boundary(&verts, &tris, 4).expect("decimation is repeatable");
        assert_eq!(first, second);
        assert_eq!(first.0.len(), 4);
        assert_eq!(first.1.len(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn ftetwild_dense_decimation_accepts_a_watertight_candidate_and_falls_back() {
        let dir = TempDir::new().unwrap();
        let (input_v, input_t) = uv_sphere(6, 12);
        let raw_faces = input_t.len();
        let spec = IntensitySpec {
            dense_ratio: 1,
            dense_min_faces: 1,
            dense_target_ladder: vec![LadderEntry::Number(0.75)],
            ..test_spec()
        };
        let mut tuning = test_tuning();
        tuning.max_hausdorff_rel = 1.0;
        tuning.decimate_hd_margin = 1.0;
        let (payload, info) =
            decimate_and_postprocess(&input_v, &input_t, &input_v, &input_t, &spec, &tuning);
        assert!(payload.is_some());
        assert_eq!(
            info.get("ftetwild_decimated").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            info.get("ftetwild_faces_decimated")
                .and_then(Value::as_u64)
                .unwrap()
                < raw_faces as u64
        );
        assert_eq!(
            info.get("ftetwild_decimate_target").and_then(Value::as_u64),
            Some((0.75 * raw_faces as f64).round() as u64)
        );

        let (bad_v, bad_t) = two_triangles();
        let (fallback, info) =
            decimate_and_postprocess(&bad_v, &bad_t, &bad_v, &bad_t, &spec, &tuning);
        assert!(fallback.is_none());
        assert_eq!(
            info.get("ftetwild_decimated").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            info.get("ftetwild_decimate_fallback")
                .and_then(Value::as_str),
            Some("not_watertight")
        );
        let _ = dir;
    }

    #[cfg(unix)]
    #[test]
    fn ftetwild_tier_is_deterministic() {
        let dir = TempDir::new().unwrap();
        let bin = obj_script(dir.path(), "far_cube.sh", FAR_CUBE_OBJ);
        let (input_v, input_t) = triangle();
        let after = common::topology_summary(&input_v, &input_t);
        let run = || {
            run_tier_with_params(
                &input_v,
                &input_t,
                &input_v,
                &input_t,
                &after,
                bin.to_str().unwrap(),
                &test_spec(),
                &test_tuning(),
                false,
                Value::Null,
            )
            .expect("tier runs")
        };
        let first = run();
        let second = run();
        assert_eq!(first.verts, second.verts);
        assert_eq!(first.tris, second.tris);
        assert_eq!(first.adopted, second.adopted);
        assert_eq!(first.shape_changed, second.shape_changed);
        assert_eq!(
            first.report.get("output_holes"),
            second.report.get("output_holes")
        );
        assert_eq!(
            first.report.get("ftetwild_faces_final"),
            second.report.get("ftetwild_faces_final")
        );
    }
    fn cube_mesh(offset: [f64; 3]) -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let mut verts = Vec::with_capacity(8);
        for z in [0.0, 1.0] {
            for y in [0.0, 1.0] {
                for x in [0.0, 1.0] {
                    verts.push([offset[0] + x, offset[1] + y, offset[2] + z]);
                }
            }
        }
        let tris = vec![
            [0, 1, 2],
            [0, 2, 3],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [2, 3, 7],
            [2, 7, 6],
            [1, 2, 6],
            [1, 6, 5],
            [0, 3, 7],
            [0, 7, 4],
        ];
        (verts, tris)
    }

    fn overlapping_cubes() -> (Vec<[f64; 3]>, Vec<[i64; 3]>) {
        let (mut a_v, mut a_t) = cube_mesh([0.0, 0.0, 0.0]);
        let (b_v, b_t) = cube_mesh([0.5, 0.5, 0.5]);
        let base = a_v.len() as i64;
        a_v.extend(b_v);
        a_t.extend(b_t.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
        (a_v, a_t)
    }

    #[cfg(unix)]
    #[test]
    fn ftetwild_si_trigger_only_runs_when_forced() {
        let dir = TempDir::new().unwrap();
        let bin = obj_script(dir.path(), "far_cube.sh", FAR_CUBE_OBJ);
        let (verts, tris) = overlapping_cubes();
        let after = common::topology_summary(&verts, &tris);
        assert_eq!((after.non_two_manifold_edges, after.boundary_edges), (0, 0));
        assert!(exact_si_count(&verts, &tris).unwrap_or(0) > 0);
        let auto = run_tier_with_params(
            &verts,
            &tris,
            &verts,
            &tris,
            &after,
            bin.to_str().unwrap(),
            &test_spec(),
            &test_tuning(),
            false,
            Value::Null,
        );
        assert!(auto.is_none());
        let forced = run_tier_with_params(
            &verts,
            &tris,
            &verts,
            &tris,
            &after,
            bin.to_str().unwrap(),
            &test_spec(),
            &test_tuning(),
            true,
            Value::Null,
        )
        .expect("the forced SI trigger runs the tier");
        assert_eq!(
            forced.report.get("trigger").and_then(Value::as_str),
            Some("always")
        );
    }
}
