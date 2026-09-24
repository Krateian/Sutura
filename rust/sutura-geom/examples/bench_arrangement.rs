//! Profiling harness for Phase C0.
//!
//! Reads an OBJ file, runs `arrangement_lite_core` on spatially local subsets
//! and on the full mesh, and appends per-subset markdown blocks to
//! `/tmp/phase-c0-profile.md` as soon as they finish.  A heartbeat line is
//! written to stderr every 10 s.
//!
//! Build and run with:
//!
//!     cargo run --release --example bench_arrangement --features profile -- <input.obj>
//!
//! Detached run:
//!
//!     env DYLD_LIBRARY_PATH=/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/lib \
//!         nohup ./target/release/examples/bench_arrangement /tmp/thingi10k_1038441.obj \
//!         > /tmp/phase-c0-run.log 2>&1 &

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use sutura_geom::arrangement::{arrangement_lite_core, broad_phase_candidates};
use sutura_geom::profile;

const REPORT_PATH: &str = "/tmp/phase-c0-profile.md";

struct Mesh {
    verts: Vec<[f64; 3]>,
    tris: Vec<[usize; 3]>,
    centroids: Vec<[f64; 3]>,
}

fn read_obj(path: &str) -> Mesh {
    let data = fs::read_to_string(path).expect("failed to read OBJ");
    let mut verts: Vec<[f64; 3]> = Vec::new();
    let mut tris: Vec<[usize; 3]> = Vec::new();
    for line in data.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "v" => {
                let x = parts[1].parse::<f64>().unwrap();
                let y = parts[2].parse::<f64>().unwrap();
                let z = parts[3].parse::<f64>().unwrap();
                verts.push([x, y, z]);
            }
            "f" => {
                let a = parts[1].split('/').next().unwrap().parse::<usize>().unwrap() - 1;
                let b = parts[2].split('/').next().unwrap().parse::<usize>().unwrap() - 1;
                let c = parts[3].split('/').next().unwrap().parse::<usize>().unwrap() - 1;
                tris.push([a, b, c]);
            }
            _ => {}
        }
    }
    let centroids: Vec<[f64; 3]> = tris
        .iter()
        .map(|t| {
            let a = verts[t[0]];
            let b = verts[t[1]];
            let c = verts[t[2]];
            [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0]
        })
        .collect();
    Mesh {
        verts,
        tris,
        centroids,
    }
}

fn write_obj(path: &str, mesh: &Mesh) {
    let mut s = String::new();
    for v in &mesh.verts {
        s.push_str(&format!("v {:.9} {:.9} {:.9}\n", v[0], v[1], v[2]));
    }
    for t in &mesh.tris {
        s.push_str(&format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1));
    }
    fs::write(path, s).expect("failed to write OBJ");
}

fn find_dense_center(mesh: &Mesh) -> Option<[f64; 3]> {
    let candidates = broad_phase_candidates(&mesh.verts, &mesh.tris);
    if candidates.is_empty() {
        return None;
    }
    let mut degree = vec![0usize; mesh.tris.len()];
    for (i, j) in candidates {
        degree[i] += 1;
        degree[j] += 1;
    }
    let best = degree
        .iter()
        .enumerate()
        .max_by_key(|&(_, d)| d)
        .map(|(i, _)| i)?;
    Some(mesh.centroids[best])
}

fn chebyshev_dist(center: [f64; 3], p: [f64; 3]) -> f64 {
    (p[0] - center[0])
        .abs()
        .max((p[1] - center[1]).abs())
        .max((p[2] - center[2]).abs())
}

fn subset_around(mesh: &Mesh, center: [f64; 3], target: usize) -> Mesh {
    let mut order: Vec<usize> = (0..mesh.tris.len()).collect();
    order.sort_by(|a, b| {
        let da = chebyshev_dist(center, mesh.centroids[*a]);
        let db = chebyshev_dist(center, mesh.centroids[*b]);
        da.partial_cmp(&db).unwrap()
    });
    let take = target.min(mesh.tris.len());
    let radius = chebyshev_dist(center, mesh.centroids[order[take.saturating_sub(1)]]);
    let selected: Vec<usize> = order
        .into_iter()
        .filter(|&i| chebyshev_dist(center, mesh.centroids[i]) <= radius)
        .collect();

    let mut new_verts: Vec<[f64; 3]> = Vec::with_capacity(selected.len() * 3);
    let mut vert_map: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::with_capacity(selected.len() * 3);
    let mut tris_out: Vec<[usize; 3]> = Vec::with_capacity(selected.len());
    let mut centroids_out: Vec<[f64; 3]> = Vec::with_capacity(selected.len());

    for &ti in &selected {
        let t = mesh.tris[ti];
        let mut out = [0usize; 3];
        for k in 0..3 {
            out[k] = *vert_map.entry(t[k]).or_insert_with(|| {
                let idx = new_verts.len();
                new_verts.push(mesh.verts[t[k]]);
                idx
            });
        }
        centroids_out.push(mesh.centroids[ti]);
        tris_out.push(out);
    }

    Mesh {
        verts: new_verts,
        tris: tris_out,
        centroids: centroids_out,
    }
}

fn run_timed(
    mesh: &Mesh,
    timeout: Duration,
) -> Result<(Duration, String, String), (Duration, String, String)> {
    profile::reset();
    let start = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let verts = mesh.verts.clone();
    let tris = mesh.tris.clone();
    let _handle = thread::spawn(move || {
        let r = arrangement_lite_core(&verts, &tris);
        let _ = tx.send(r);
    });
    let result = match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => {
            profile::flush_partial_all();
            let wall = start.elapsed();
            let summary = format!("TIMEOUT after {} s", timeout.as_secs());
            return Err((wall, summary, profile::report()));
        }
    };
    let wall = start.elapsed();
    let summary = match result {
        Ok((_, _, r)) => format!(
            "OK input_faces={}, output_faces={}, si_pairs={}",
            r.input_faces, r.output_faces, r.si_pairs_detected
        ),
        Err(e) => format!("ERROR: {}", e),
    };
    profile::flush_partial_all();
    Ok((wall, summary, profile::report()))
}

fn append_block(block: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(REPORT_PATH)
        .expect("failed to open report");
    writeln!(f, "{}", block).expect("failed to write report");
    f.flush().expect("failed to flush report");
}

static STOP_HEARTBEAT: AtomicBool = AtomicBool::new(false);

fn start_heartbeat() {
    STOP_HEARTBEAT.store(false, Ordering::Relaxed);
    thread::spawn(|| {
        loop {
            for _ in 0..10 {
                thread::sleep(Duration::from_secs(1));
                if STOP_HEARTBEAT.load(Ordering::Relaxed) {
                    return;
                }
            }
            eprintln!("{}", profile::heartbeat_line());
        }
    });
}

fn stop_heartbeat() {
    STOP_HEARTBEAT.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(300));
}

fn child_mode(path: &str, timeout_secs: u64, label: &str) {
    let mesh = read_obj(path);
    start_heartbeat();
    let block = match run_timed(&mesh, Duration::from_secs(timeout_secs)) {
        Ok((wall, _summary, table)) => format!(
            "{}\n**Wall time:** {:.3} s\n{}",
            label,
            wall.as_secs_f64(),
            table
        ),
        Err((wall, summary, table)) => format!(
            "{}\n**Wall time:** {:.3} s (partial — {})\n{}",
            label,
            wall.as_secs_f64(),
            summary,
            table
        ),
    };
    stop_heartbeat();
    println!("{}", block);
}

fn spawn_run(
    binary: &str,
    path: &str,
    timeout_secs: u64,
    label: &str,
    dyld_path: &str,
) -> Result<String, String> {
    let mut child = Command::new(binary)
        .arg("--run")
        .arg(path)
        .arg(timeout_secs.to_string())
        .arg(label)
        .env("DYLD_LIBRARY_PATH", dyld_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;

    let mut stdout_pipe = child.stdout.take().unwrap();
    let stdout_thread = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout_pipe.read_to_string(&mut s);
        s
    });

    let orchestrator_timeout = Duration::from_secs(timeout_secs + 30);
    let deadline = Instant::now() + orchestrator_timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout_thread
                    .join()
                    .map_err(|_| "stdout thread panicked".to_string())?;
                if !status.success() {
                    return Err(format!("child exited with status {}", status));
                }
                return Ok(stdout);
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    return Err("orchestrator killed hung child".to_string());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("wait error: {}", e)),
        }
    }
}

fn orchestrator_mode(path: &str, binary: &str, dyld_path: &str) {
    let mesh = read_obj(path);
    eprintln!(
        "loaded {} verts, {} tris",
        mesh.verts.len(),
        mesh.tris.len()
    );

    let center = find_dense_center(&mesh).unwrap_or_else(|| {
        let mut c = [0.0; 3];
        for p in &mesh.centroids {
            c[0] += p[0];
            c[1] += p[1];
            c[2] += p[2];
        }
        let n = mesh.centroids.len() as f64;
        [c[0] / n, c[1] / n, c[2] / n]
    });
    eprintln!("subset center around densest candidate region at {:?}", center);

    let header = format!(
        "# Phase C0 profiling report\n\n**Command:** `cargo run --release --example bench_arrangement --features profile -- {}`\n\n**Loaded:** {} vertices, {} triangles\n",
        path,
        mesh.verts.len(),
        mesh.tris.len()
    );
    fs::write(REPORT_PATH, header).expect("failed to write header");

    let targets: [(usize, u64); 4] = [(100, 60), (500, 60), (1000, 120), (5000, 180)];
    for (target, secs) in targets {
        let subset = subset_around(&mesh, center, target);
        let tmp = format!("/tmp/sutura_subset_{}.obj", target);
        write_obj(&tmp, &subset);
        let label = format!(
            "\n## Subset ~{} faces (actual {})",
            target,
            subset.tris.len()
        );
        eprintln!(
            "running subset target={} (actual {}) with {} s time box",
            target,
            subset.tris.len(),
            secs
        );
        let block = match spawn_run(binary, &tmp, secs, &label, dyld_path) {
            Ok(stdout) => stdout,
            Err(e) => format!(
                "{}\n**Wall time:** — \n**Orchestrator error:** {}\n",
                label, e
            ),
        };
        append_block(&block);
        eprintln!("subset target={} done", target);
    }

    let tmp = "/tmp/sutura_subset_full.obj".to_string();
    write_obj(&tmp, &mesh);
    let label = "\n## Full mesh".to_string();
    eprintln!("running full mesh with 600 s time box");
    let block = match spawn_run(binary, &tmp, 600, &label, dyld_path) {
        Ok(stdout) => stdout,
        Err(e) => format!(
            "{}\n**Wall time:** — \n**Orchestrator error:** {}\n",
            label, e
        ),
    };
    append_block(&block);

    let verdict = "\n## Verdict\n_TBD after inspection. The phase with the largest share of instrumented time on the full (or largest completed) input is the recommended next chunk._\n";
    append_block(verdict);
    eprintln!("report written to {}", REPORT_PATH);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() > 2 && args[1] == "--run" {
        let path = &args[2];
        let timeout_secs = args[3].parse::<u64>().expect("bad timeout");
        let label = &args[4];
        child_mode(path, timeout_secs, label);
        return;
    }

    if args.len() != 2 {
        eprintln!("usage: bench_arrangement <input.obj>");
        std::process::exit(1);
    }

    let binary = &args[0];
    let dyld_path = env::var("DYLD_LIBRARY_PATH").unwrap_or_default();
    orchestrator_mode(&args[1], binary, &dyld_path);
}
