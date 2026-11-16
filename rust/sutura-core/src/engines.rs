// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! External-engine execution: write the input, substitute `{input}` /
//! `{output}`, run with a timeout and the pipeline's cancel token, then read
//! the result back. Placement/adoption live in `repair`; this module is the
//! process/format layer only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config::external_engines::EngineConfig;
use crate::mesh::Mesh;

pub struct EngineRun {
    pub verts: Vec<[f64; 3]>,
    pub tris: Vec<[i64; 3]>,
    pub report: Value,
}

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "sutura-engine-{}-{}",
        std::process::id(),
        nanos
    ))
}

fn write_mesh(path: &Path, v: &[[f64; 3]], t: &[[i64; 3]], fmt: &str) -> Result<(), String> {
    let mesh = Mesh::new(
        v.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect(),
        t.iter().map(|r| [r[0] as i32, r[1] as i32, r[2] as i32]).collect(),
    );
    match fmt {
        "stl" => crate::mesh_io::write_stl_binary(path, &mesh).map_err(|e| e.to_string()),
        "obj" => crate::mesh_io::write_obj(path, &mesh).map_err(|e| e.to_string()),
        "ply" => crate::mesh_io::write_ply(path, &mesh).map_err(|e| e.to_string()),
        other => Err(format!("unsupported input_format: {other}")),
    }
}

fn read_mesh(path: &Path, fmt: &str) -> Result<(Vec<[f64; 3]>, Vec<[i64; 3]>), String> {
    let mesh = crate::mesh_io::load_mesh(path).map_err(|e| e.to_string())?;
    if fmt != "stl" && fmt != "obj" && fmt != "ply" {
        return Err(format!("unsupported output_format: {fmt}"));
    }
    Ok((
        mesh.verts.iter().map(|p| [p[0] as f64, p[1] as f64, p[2] as f64]).collect(),
        mesh.tris.iter().map(|r| [r[0] as i64, r[1] as i64, r[2] as i64]).collect(),
    ))
}

/// Run one configured engine. `None` is returned when the engine is disabled
/// or unavailable; a process/format failure is an `Err` so the caller can
/// report it without losing the repair.
pub fn run_engine(
    engine: &EngineConfig,
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
) -> Result<Option<EngineRun>, String> {
    if !engine.enabled {
        return Ok(None);
    }
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("engine temp dir: {e}"))?;
    let in_path = dir.join(format!("input.{}", engine.input_format));
    let out_path = dir.join(format!("output.{}", engine.output_format));
    let _ = std::fs::remove_file(&out_path);
    write_mesh(&in_path, v, t, &engine.input_format)?;

    let mut args: Vec<String> = engine
        .command
        .iter()
        .map(|a| {
            a.replace("{input}", &in_path.to_string_lossy())
                .replace("{output}", &out_path.to_string_lossy())
        })
        .collect();
    if args.is_empty() {
        return Err(format!("engine '{}' has an empty command", engine.name));
    }
    let program = args.remove(0);
    let mut cmd = Command::new(program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = engine.cwd.as_ref() {
        cmd.current_dir(cwd);
    }
    for (k, val) in &engine.env {
        cmd.env(k, val);
    }
    let started = Instant::now();
    let deadline = Duration::from_secs_f64(engine.timeout.max(0.0));
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("engine '{}' spawn failed: {e}", engine.name))?;
    loop {
        if sutura_geom::cancel::cancelled(cancel) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            return Err("cancelled".to_string());
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!(
                "engine '{}' timed out after {}s",
                engine.name, engine.timeout
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(format!(
                        "engine '{}' exited with {}",
                        engine.name, status
                    ));
                }
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(format!("engine '{}' wait failed: {e}", engine.name));
            }
        }
    }
    if !out_path.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("engine '{}' produced no output", engine.name));
    }
    let (ov, ot) = read_mesh(&out_path, &engine.output_format)?;
    let output_faces = ot.len();
    let seconds = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(Some(EngineRun {
        verts: ov,
        tris: ot,
        report: json!({
            "engine": engine.name,
            "placement": engine.placement,
            "ran": true,
            "returncode": 0,
            "seconds": (seconds * 1000.0).round() / 1000.0,
            "input_faces": t.len(),
            "output_faces": output_faces,
        }),
    }))
}

/// Write a tiny ASCII-STL cube for tests/fixtures.
pub fn write_cube_stl(path: &Path) -> Result<(), String> {
    let mut out = std::fs::File::create(path).map_err(|e| e.to_string())?;
    out.write_all(b"solid cube\n").map_err(|e| e.to_string())?;
    // 12 triangles, unit cube.
    let pts = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.0, 1.0, 1.0],
    ];
    let faces = [
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
        [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
    ];
    for face in faces {
        let [a, b, c] = [pts[face[0]], pts[face[1]], pts[face[2]]];
        let u: [f64; 3] = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w: [f64; 3] = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n: [f64; 3] = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let len: f64 = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        let nn = [n[0] / len, n[1] / len, n[2] / len];
        writeln!(out, " facet normal {} {} {}", nn[0], nn[1], nn[2])
            .map_err(|e| e.to_string())?;
        writeln!(out, "  outer loop").map_err(|e| e.to_string())?;
        for p in [a, b, c] {
            writeln!(out, "   vertex {} {} {}", p[0], p[1], p[2]).map_err(|e| e.to_string())?;
        }
        writeln!(out, "  endloop").map_err(|e| e.to_string())?;
        writeln!(out, " endfacet").map_err(|e| e.to_string())?;
    }
    writeln!(out, "endsolid cube").map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn copy_engine_is_adopted_by_the_placement_guard() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("cube2.stl");
        write_cube_stl(&src).unwrap();
        let mesh = crate::mesh_io::load_mesh(&src).unwrap();
        let v: Vec<[f64; 3]> = mesh
            .verts
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let t: Vec<[i64; 3]> = mesh
            .tris
            .iter()
            .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
            .collect();
        let cfg = EngineConfig {
            name: "copy".into(),
            command: vec!["/bin/cp".into(), "{input}".into(), "{output}".into()],
            input_format: "stl".into(),
            output_format: "stl".into(),
            timeout: 30.0,
            placement: "after_stage1".into(),
            enabled: true,
            env: BTreeMap::new(),
            cwd: None,
            source_path: None,
        };
        let mut entries = Vec::new();
        let adopted = run_placement(&[cfg], "after_stage1", &v, &t, None, 0.01, &mut entries);
        assert!(adopted.is_some());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["adopted"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_engine_round_trips_the_mesh() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("cube.stl");
        write_cube_stl(&src).unwrap();
        let mesh = crate::mesh_io::load_mesh(&src).unwrap();
        let v: Vec<[f64; 3]> = mesh
            .verts
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let t: Vec<[i64; 3]> = mesh
            .tris
            .iter()
            .map(|r| [r[0] as i64, r[1] as i64, r[2] as i64])
            .collect();
        let cfg = EngineConfig {
            name: "copy".into(),
            command: vec![
                "/bin/cp".into(),
                "{input}".into(),
                "{output}".into(),
            ],
            input_format: "stl".into(),
            output_format: "stl".into(),
            timeout: 30.0,
            placement: "after_stage1".into(),
            enabled: true,
            env: BTreeMap::new(),
            cwd: None,
            source_path: None,
        };
        let run = run_engine(&cfg, &v, &t, None).unwrap().expect("ran");
        assert_eq!(run.verts.len(), v.len());
        assert_eq!(run.tris.len(), t.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Load the configured engines, keep only enabled ones and (when given) the
/// names selected by `--engines`. Returns the configs plus loader warnings.
pub fn selected_engines(selected: Option<&[String]>) -> (Vec<EngineConfig>, Vec<String>) {
    let (all, warnings) = crate::config::external_engines::load_all_engines(None);
    let mut out: Vec<EngineConfig> = match selected {
        Some(names) => names
            .iter()
            .filter_map(|n| all.get(n).cloned())
            .filter(|e| e.enabled)
            .collect(),
        None => all.values().filter(|e| e.enabled).cloned().collect(),
    };
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, warnings)
}

/// Run the engines configured for one placement and adopt the first output
/// that passes the same holes/non-manifold guard as the fTetWild tier.
/// Returns the adopted arrays; one report entry per attempted engine is
/// appended to `entries`.
pub fn run_placement(
    engines: &[EngineConfig],
    placement: &str,
    v: &[[f64; 3]],
    t: &[[i64; 3]],
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
    max_hausdorff_rel: f64,
    entries: &mut Vec<Value>,
) -> Option<(Vec<[f64; 3]>, Vec<[i64; 3]>)> {
    let base = crate::tiers::common::topology_summary(v, t);
    let base_holes = crate::repair::boundary_loop_stats(t).0;
    for engine in engines.iter().filter(|e| e.placement == placement) {
        let mut entry = json!({
            "name": engine.name,
            "placement": placement,
            "adopted": false,
        });
        match run_engine(engine, v, t, cancel) {
            Ok(Some(run)) => {
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("rc".into(), json!(0));
                    obj.insert(
                        "time".into(),
                        run.report.get("seconds").cloned().unwrap_or(Value::Null),
                    );
                    obj.insert("output_faces".into(), json!(run.tris.len()));
                }
                if run.tris.is_empty() {
                    if let Some(obj) = entry.as_object_mut() {
                        obj.insert("reject_reason".into(), json!("engine output has no faces"));
                    }
                    entries.push(entry);
                    continue;
                }
                let cand = crate::tiers::common::topology_summary(&run.verts, &run.tris);
                let cand_holes = crate::repair::boundary_loop_stats(&run.tris).0;
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("output_holes".into(), json!(cand_holes));
                    obj.insert(
                        "output_non_manifold".into(),
                        json!(cand.non_two_manifold_edges),
                    );
                }
                if cand_holes <= base_holes
                    && cand.non_two_manifold_edges <= base.non_two_manifold_edges
                {
                    let hd = crate::local_remesh::hausdorff_rel(v, t, &run.verts, &run.tris);
                    if let Some(obj) = entry.as_object_mut() {
                        obj.insert("hausdorff_rel".into(), json!(hd));
                        obj.insert(
                            "shape_changed".into(),
                            json!(hd.map(|x| x > max_hausdorff_rel).unwrap_or(false)),
                        );
                        obj.insert("adopted".into(), json!(true));
                    }
                    entries.push(entry);
                    return Some((run.verts, run.tris));
                }
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("reject_reason".into(), json!("holes_nm"));
                }
                entries.push(entry);
            }
            Ok(None) => {
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("reject_reason".into(), json!("engine disabled"));
                }
                entries.push(entry);
            }
            Err(message) => {
                if message == "cancelled" {
                    return None;
                }
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("reject_reason".into(), json!(message));
                }
                entries.push(entry);
            }
        }
    }
    None
}

/// `run_placement` that replaces `v`/`t` in place on adoption.
pub fn run_and_replace(
    engines: &[EngineConfig],
    placement: &str,
    v: &mut Vec<[f64; 3]>,
    t: &mut Vec<[i64; 3]>,
    cancel: Option<&dyn sutura_geom::cancel::Cancel>,
    max_hausdorff_rel: f64,
    entries: &mut Vec<Value>,
) -> bool {
    if let Some((nv, nt)) =
        run_placement(engines, placement, v, t, cancel, max_hausdorff_rel, entries)
    {
        *v = nv;
        *t = nt;
        true
    } else {
        false
    }
}
