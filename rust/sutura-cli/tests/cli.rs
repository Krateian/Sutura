use std::process::Command;

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "sutura-cli-{}-{}-{}",
        std::process::id(),
        name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn probe_reports_counts_as_json() {
    let path = tmp("probe").with_extension("obj");
    std::fs::write(&path, b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .args(["__io", "probe", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["format"], "obj");
    assert_eq!(v["meshes"][0]["verts"], 3);
    assert_eq!(v["meshes"][0]["tris"], 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn repair_is_rejected_with_exit_code_two() {
    let path = tmp("repair").with_extension("stl");
    std::fs::write(&path, b"solid x\nendsolid x\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .arg(&path)
        .output()
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not implemented"));
}

#[test]
fn version_prints_the_product_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sutura 0.9.9\n");
}

#[test]
fn help_lists_the_contract_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--help", "--version", "--list-intensities", "--json",
                 "--human", "--intensity", "--methods", "--output"] {
        assert!(text.contains(flag), "help misses {flag}");
    }
}

#[test]
fn invalid_flag_exits_two() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .arg("--definitely-not-a-flag")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
}

#[test]
fn missing_file_prints_the_json_error_and_exits_one() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .args(["definitely-missing.stl", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["input"], "definitely-missing.stl");
    assert_eq!(v["error"], "file not found: definitely-missing.stl");
}

#[test]
fn list_intensities_prints_the_presets() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .arg("--list-intensities")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Intensity presets (built-in, read-only):"));
    for name in ["quick", "balanced", "thorough", "extreme"] {
        assert!(text.contains(name), "listing misses {name}");
    }
}

#[test]
fn bare_io_is_a_file_path_not_a_subcommand() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .args(["io", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["input"], "io");
    assert_eq!(v["error"], "file not found: io");
}

#[test]
fn list_methods_prints_the_registry() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .arg("--list-methods")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "Repair methods:",
        "quick_clean",
        "coaptation",
        "Automatic deep-tier order:",
        "Policy: schema 1, hash abb52584b7c6eee4, packaged default",
    ] {
        assert!(text.contains(needle), "listing misses {needle}");
    }
}

#[test]
fn long_flag_prefixes_resolve_like_argparse() {
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .args(["--hum", "--version"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "sutura 0.9.9\n");
}
