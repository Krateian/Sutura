//! OBJ reading and writing with the Python mesh_io contract.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use crate::error::{MeshError, MeshResult};
use crate::fmt::fmt_g;
use crate::mesh::Mesh;

/// Read an OBJ file. Vertex records are kept as defined; faces are
/// fan-triangulated from their first vertex and resolved against the vertices
/// defined so far (including negative indices).
pub fn read_obj(path: impl AsRef<Path>) -> MeshResult<Mesh> {
    let path = path.as_ref();
    let reader = BufReader::new(File::open(path)?);
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<[i32; 3]> = Vec::new();
    let mut buf: Option<(usize, String)> = None;
    let mut line_no = 0usize;

    for raw in reader.split(b'\n') {
        let raw = raw?;
        line_no += 1;
        let text = String::from_utf8_lossy(&raw);
        let no_comment = text.split('#').next().unwrap_or("");
        let mut stripped = no_comment.trim_end();
        let continued = stripped.ends_with('\\');
        if continued {
            stripped = &stripped[..stripped.len() - 1];
        }
        match &mut buf {
            None => buf = Some((line_no, stripped.to_string())),
            Some((_, b)) => {
                b.push(' ');
                b.push_str(stripped);
            }
        }
        if !continued {
            let (start, logical) = buf.take().unwrap();
            parse_obj_line(path, start, &logical, &mut verts, &mut tris)?;
        }
    }
    if let Some((start, logical)) = buf {
        if !logical.trim().is_empty() {
            parse_obj_line(path, start, &logical, &mut verts, &mut tris)?;
        }
    }

    if verts.is_empty() {
        if !tris.is_empty() {
            return Err(MeshError::MalformedObj(format!(
                "{}: OBJ has face records but no vertex definitions",
                path.display()
            )));
        }
        return Ok(Mesh::default());
    }
    if let Some(max_idx) = tris.iter().flatten().max() {
        if *max_idx as usize >= verts.len() {
            return Err(MeshError::MalformedObj(format!(
                "{}: OBJ face index {} is out of range ({} vertices)",
                path.display(),
                *max_idx as i64 + 1,
                verts.len()
            )));
        }
    }
    Ok(Mesh::new(verts, tris))
}

/// Write an OBJ file with Python's `# sutura intermediate` header and `%.9g`
/// vertex formatting.
pub fn write_obj(path: impl AsRef<Path>, mesh: &Mesh) -> MeshResult<()> {
    mesh.validate()?;
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(b"# sutura intermediate\n")?;
    for v in &mesh.verts {
        writeln!(
            w,
            "v {} {} {}",
            fmt_g(f64::from(v[0]), 9),
            fmt_g(f64::from(v[1]), 9),
            fmt_g(f64::from(v[2]), 9)
        )?;
    }
    for t in &mesh.tris {
        writeln!(
            w,
            "f {} {} {}",
            i64::from(t[0]) + 1,
            i64::from(t[1]) + 1,
            i64::from(t[2]) + 1
        )?;
    }
    w.flush()?;
    Ok(())
}

/// True when an OBJ references `mtllib` or `usemtl` (repair drops materials).
pub fn obj_has_material_refs(path: impl AsRef<Path>) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    text.lines().any(|line| {
        let s = line.trim_start().to_ascii_lowercase();
        s.starts_with("mtllib ") || s.starts_with("usemtl ")
    })
}

fn parse_obj_line(
    path: &Path,
    line_no: usize,
    line: &str,
    verts: &mut Vec<[f32; 3]>,
    tris: &mut Vec<[i32; 3]>,
) -> MeshResult<()> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(());
    }
    match parts[0] {
        "v" => {
            if parts.len() < 4 {
                return Err(MeshError::MalformedObj(format!(
                    "{}:{}: OBJ vertex needs at least 3 coordinates",
                    path.display(),
                    line_no
                )));
            }
            let mut xyz = [0f64; 3];
            for (slot, tok) in xyz.iter_mut().zip(&parts[1..4]) {
                *slot = tok.parse::<f64>().map_err(|e| {
                    MeshError::MalformedObj(format!("{}:{}: {e}", path.display(), line_no))
                })?;
            }
            verts.push([xyz[0] as f32, xyz[1] as f32, xyz[2] as f32]);
        }
        "f" => {
            let mut idxs = Vec::with_capacity(parts.len().saturating_sub(1));
            for tok in &parts[1..] {
                idxs.push(obj_vertex_index(tok, verts.len()).map_err(|e| {
                    MeshError::MalformedObj(format!("{}:{}: {e}", path.display(), line_no))
                })?);
            }
            if idxs.len() < 3 {
                return Err(MeshError::MalformedObj(format!(
                    "{}:{}: OBJ face needs at least 3 vertices",
                    path.display(),
                    line_no
                )));
            }
            for i in 1..idxs.len() - 1 {
                tris.push([idxs[0], idxs[i], idxs[i + 1]]);
            }
        }
        _ => {}
    }
    Ok(())
}

fn obj_vertex_index(token: &str, vertex_count: usize) -> Result<i32, String> {
    let raw = token.split('/').next().unwrap_or("");
    if raw.is_empty() {
        return Err(format!("OBJ face token has no vertex index: {token:?}"));
    }
    let idx: i64 = raw
        .parse()
        .map_err(|_| format!("OBJ face token is not an integer: {token:?}"))?;
    if idx == 0 {
        return Err("OBJ vertex index 0 is invalid (indices are 1-based)".to_string());
    }
    if idx < 0 {
        let resolved = vertex_count as i64 + idx;
        if resolved < 0 {
            return Err(format!(
                "OBJ negative vertex index {idx} is out of range ({vertex_count} vertices defined)"
            ));
        }
        return Ok(resolved as i32);
    }
    if idx > vertex_count as i64 {
        return Err(format!(
            "OBJ vertex index {idx} is out of range ({vertex_count} vertices defined)"
        ));
    }
    Ok((idx - 1) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sutura-obj-{}-{}-{}.obj",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn fan_triangulates_quads_and_resolves_negative_indices() {
        let path = tmp("quad");
        std::fs::write(
            &path,
            b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\n\
f 1 2 3 4\nf -4/-1 -3/-1 -2/-1\n",
        )
        .unwrap();
        let m = read_obj(&path).unwrap();
        assert_eq!(m.verts.len(), 4);
        assert_eq!(m.tris, vec![[0, 1, 2], [0, 2, 3], [0, 1, 2]]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handles_comments_and_backslash_continuation() {
        let path = tmp("cont");
        std::fs::write(
            &path,
            b"# leading comment\nv 0 0 \\\n 0\nv 1 0 0 # inline\nv 0 1 0\nf 1 2 3\n",
        )
        .unwrap();
        let m = read_obj(&path).unwrap();
        assert_eq!(m.verts.len(), 3);
        assert_eq!(m.tris, vec![[0, 1, 2]]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_zero_and_out_of_range_indices() {
        for (name, body) in [
            ("zero", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 0 2 3\n"),
            ("range", "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 4\n"),
        ] {
            let path = tmp(name);
            std::fs::write(&path, body).unwrap();
            assert!(matches!(read_obj(&path), Err(MeshError::MalformedObj(_))));
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn empty_file_is_an_empty_mesh() {
        let path = tmp("empty");
        std::fs::write(&path, b"").unwrap();
        assert!(read_obj(&path).unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn writer_matches_python_bytes() {
        let m = Mesh::new(
            vec![
                [0.1f32, 1.0f32 / 3.0f32, -0.0f32],
                [1e-5f32, 1e9f32, 123.456f32],
                [0.0001f32, 999_999_999.5f32, 1.234_567_890_123f32],
            ],
            vec![[0, 1, 2]],
        );
        let path = tmp("write");
        write_obj(&path, &m).unwrap();
        let got = std::fs::read_to_string(&path).unwrap();
        let want = "# sutura intermediate\n\
v 0.100000001 0.333333343 -0\n\
v 9.99999975e-06 1e+09 123.456001\n\
v 9.99999975e-05 1e+09 1.23456788\n\
f 1 2 3\n";
        assert_eq!(got, want);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn material_references_are_detected() {
        let path = tmp("mtl");
        std::fs::write(&path, b"mtllib a.mtl\nv 0 0 0\nusemtl red\n").unwrap();
        assert!(obj_has_material_refs(&path));
        std::fs::write(&path, b"v 0 0 0\nf 1 1 1\n").unwrap();
        assert!(!obj_has_material_refs(&path));
        std::fs::remove_file(path).unwrap();
    }
}
