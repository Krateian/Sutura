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
        .args(["io", "probe", path.to_str().unwrap(), "--json"])
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
    let out = Command::new(env!("CARGO_BIN_EXE_sutura"))
        .args(["repair", "x.stl"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not implemented"));
}
