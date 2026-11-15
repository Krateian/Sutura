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

use crate::fmt::{fmt_g, py_repr_f64};

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
    let started = Instant::now();
    if bin_path.trim().is_empty() {
        return Err("fTetWild binary unavailable: empty binary path".to_string());
    }

    let kw = merged_params(params);
    let timeout_s = kw
        .get("timeout")
        .and_then(Value::as_f64)
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
}
