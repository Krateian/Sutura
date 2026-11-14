//! STL reading and writing with the same float32/int32 contract as the
//! Python mesh_io implementation.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{MeshError, MeshResult};
use crate::mesh::Mesh;

/// Read a binary or ASCII STL into an unwelded mesh (three vertices per
/// triangle, sequential int32 indices), matching `mesh_io.read_stl`.
pub fn read_stl(path: impl AsRef<Path>) -> MeshResult<Mesh> {
    let path = path.as_ref();
    let size = std::fs::metadata(path)?.len();
    let mut file = File::open(path)?;
    let mut head = [0u8; 84];
    let nread = read_up_to(&mut file, &mut head)?;
    if nread == 84 {
        let n = u64::from(u32::from_le_bytes([head[80], head[81], head[82], head[83]]));
        if let Some(records) = n.checked_mul(50) {
            if size == 84 + records {
                return read_binary_records(&mut file, n as usize);
            }
            if n > 0 && size > 84 + records && !looks_like_ascii(path)? {
                return read_binary_records(&mut file, n as usize);
            }
        }
    }
    read_stl_ascii(path)
}

/// Read and weld an STL, matching `mesh_io.load_mesh` (which applies
/// `topology.remove_duplicate_vertices`).
pub fn load_stl(path: impl AsRef<Path>) -> MeshResult<Mesh> {
    let raw = read_stl(path)?;
    Ok(weld(raw))
}

/// Write a binary STL with the exact header and record layout used by
/// `mesh_io.stl_write_binary`.
pub fn write_stl_binary(path: impl AsRef<Path>, mesh: &Mesh) -> MeshResult<()> {
    let file = File::create(path)?;
    let mut w = BufWriter::new(file);
    encode_binary(mesh, &mut w)?;
    w.flush()?;
    Ok(())
}

/// In-memory variant used by golden tests.
pub fn write_stl_binary_bytes(mesh: &Mesh) -> MeshResult<Vec<u8>> {
    let mut out = Vec::new();
    encode_binary(mesh, &mut out)?;
    Ok(out)
}

/// Return a description of NaN/Inf STL coordinates, or `None` when clean.
///
/// Mirrors `mesh_io.scan_bad_coordinates` for `.stl`: a file whose first
/// bytes are `solid` is scanned as ASCII; everything else is treated as a
/// binary STL with the declared triangle count and exact 84 + 50*n size.
pub fn scan_stl_bad_coordinates(path: impl AsRef<Path>) -> MeshResult<Option<String>> {
    let path = path.as_ref();
    let mut file = File::open(path)?;
    let mut head = [0u8; 80];
    read_up_to(&mut file, &mut head)?;
    if head.starts_with(b"solid") {
        file.seek(SeekFrom::Start(0))?;
        let reader = BufReader::new(file);
        for line in reader.split(b'\n') {
            let line = line?;
            let trimmed = trim_ascii_start(&line);
            if !trimmed.starts_with(b"vertex") {
                continue;
            }
            let tokens: Vec<&[u8]> = trimmed
                .split(|b| b.is_ascii_whitespace())
                .filter(|t| !t.is_empty())
                .collect();
            for tok in tokens.iter().skip(1).take(3) {
                if let Ok(text) = std::str::from_utf8(tok) {
                    if let Ok(v) = text.parse::<f64>() {
                        if !v.is_finite() {
                            return Ok(Some("NaN or infinite coordinates".to_string()));
                        }
                    }
                }
            }
        }
        return Ok(None);
    }

    let mut count = [0u8; 4];
    let got = read_up_to(&mut file, &mut count)?;
    if got < 4 {
        return Ok(Some("malformed STL (no triangle count)".to_string()));
    }
    let n = u32::from_le_bytes(count) as u64;
    let size = std::fs::metadata(path)?.len();
    let expected = 84u64.checked_add(n.checked_mul(50).unwrap_or(u64::MAX));
    if expected != Some(size) {
        return Ok(Some(format!(
            "malformed STL (declared {} triangles, file size mismatch)",
            n
        )));
    }
    let mut remaining = n as usize;
    let mut record = [0u8; 50];
    while remaining > 0 {
        if read_up_to(&mut file, &mut record)? != 50 {
            return Ok(Some("malformed STL (truncated data)".to_string()));
        }
        for i in 0..9 {
            let off = 12 + i * 4;
            let v = f32::from_le_bytes([
                record[off],
                record[off + 1],
                record[off + 2],
                record[off + 3],
            ]);
            if !v.is_finite() {
                return Ok(Some("NaN or infinite coordinates".to_string()));
            }
        }
        remaining -= 1;
    }
    Ok(None)
}

fn read_binary_records(file: &mut File, n: usize) -> MeshResult<Mesh> {
    file.seek(SeekFrom::Start(84))?;
    let mut data = vec![0u8; 50 * n];
    file.read_exact(&mut data)
        .map_err(|_| MeshError::MalformedStl("truncated binary records".to_string()))?;
    let mut verts = Vec::with_capacity(n * 3);
    let mut tris = Vec::with_capacity(n);
    for (i, rec) in data.chunks_exact(50).enumerate() {
        for k in 0..3 {
            let off = 12 + k * 12;
            verts.push([
                f32::from_le_bytes([rec[off], rec[off + 1], rec[off + 2], rec[off + 3]]),
                f32::from_le_bytes([rec[off + 4], rec[off + 5], rec[off + 6], rec[off + 7]]),
                f32::from_le_bytes([rec[off + 8], rec[off + 9], rec[off + 10], rec[off + 11]]),
            ]);
        }
        let base = (i * 3) as i32;
        tris.push([base, base + 1, base + 2]);
    }
    Ok(Mesh::new(verts, tris))
}

fn read_stl_ascii(path: &Path) -> MeshResult<Mesh> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let (mut saw_solid, mut saw_endsolid, mut saw_facet) = (false, false, false);
    for line in reader.split(b'\n') {
        let line = line?;
        let text = String::from_utf8_lossy(&line);
        let mut tokens = text.split_ascii_whitespace();
        let Some(keyword) = tokens.next() else {
            continue;
        };
        let kw = keyword.to_ascii_lowercase();
        match kw.as_str() {
            "solid" => saw_solid = true,
            "endsolid" => saw_endsolid = true,
            "facet" => saw_facet = true,
            "vertex" => {
                let mut xyz = [0f64; 3];
                for slot in xyz.iter_mut() {
                    let Some(tok) = tokens.next() else {
                        return Err(MeshError::MalformedStl(format!(
                            "incomplete vertex line {:?}",
                            text.trim()
                        )));
                    };
                    *slot = tok.parse::<f64>().map_err(|_| {
                        MeshError::MalformedStl(format!(
                            "non-numeric vertex line {:?}",
                            text.trim()
                        ))
                    })?;
                }
                verts.push([xyz[0] as f32, xyz[1] as f32, xyz[2] as f32]);
            }
            _ => {}
        }
    }
    if verts.is_empty() {
        if saw_solid && saw_endsolid && !saw_facet {
            return Ok(Mesh::default());
        }
        return Err(MeshError::MalformedStl(
            "no vertex records found in binary or ASCII form".to_string(),
        ));
    }
    if verts.len() % 3 != 0 {
        return Err(MeshError::MalformedStl(format!(
            "ASCII vertex count {} is not a multiple of 3",
            verts.len()
        )));
    }
    let n = verts.len() / 3;
    let tris = (0..n)
        .map(|i| {
            let b = (i * 3) as i32;
            [b, b + 1, b + 2]
        })
        .collect();
    Ok(Mesh::new(verts, tris))
}

fn encode_binary(mesh: &Mesh, w: &mut impl Write) -> MeshResult<()> {
    mesh.validate()?;
    let n = u32::try_from(mesh.tris.len())
        .map_err(|_| MeshError::MalformedStl("too many triangles for a binary STL".to_string()))?;
    let mut header = [0u8; 80];
    let tag = b"Sutura intermediate";
    header[..tag.len()].copy_from_slice(tag);
    w.write_all(&header)?;
    w.write_all(&n.to_le_bytes())?;
    for t in &mesh.tris {
        w.write_all(&[0u8; 12])?;
        for &idx in t {
            let v = mesh.verts[idx as usize];
            for c in v {
                w.write_all(&c.to_le_bytes())?;
            }
        }
        w.write_all(&0u16.to_le_bytes())?;
    }
    Ok(())
}

fn weld(mesh: Mesh) -> Mesh {
    let tris64: Vec<[i64; 3]> = mesh
        .tris
        .iter()
        .map(|t| [i64::from(t[0]), i64::from(t[1]), i64::from(t[2])])
        .collect();
    let (verts, tris64) = sutura_geom::remove_duplicate_vertices_core(&mesh.verts, &tris64);
    let tris = tris64
        .into_iter()
        .map(|t| [t[0] as i32, t[1] as i32, t[2] as i32])
        .collect();
    Mesh::new(verts, tris)
}

fn looks_like_ascii(path: &Path) -> MeshResult<bool> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut file, &mut buf)?;
    let prefix = &buf[..n];
    Ok(contains_ascii_ci(prefix, b"facet") || contains_ascii_ci(prefix, b"outer loop"))
}

fn contains_ascii_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| {
        w.iter()
            .zip(needle)
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

fn trim_ascii_start(mut b: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = b {
        if first.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sutura-stl-{}-{}-{}.stl",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn triangle() -> Mesh {
        Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
    }

    const PYTHON_TRIANGLE_STL: &str = "53757475726120696e7465726d65646961746500000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000803f0000000000000000000000000000803f000000000000";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn binary_writer_matches_python_bytes() {
        let got = write_stl_binary_bytes(&triangle()).unwrap();
        assert_eq!(got, hex(PYTHON_TRIANGLE_STL));
    }

    #[test]
    fn binary_reader_keeps_unwelded_contract_and_load_welds() {
        let path = tmp("binary");
        std::fs::write(&path, hex(PYTHON_TRIANGLE_STL)).unwrap();
        let raw = read_stl(&path).unwrap();
        assert_eq!(raw.vertex_count(), 3);
        assert_eq!(raw.triangle_count(), 1);
        assert_eq!(raw.tris, vec![[0, 1, 2]]);
        let welded = load_stl(&path).unwrap();
        assert_eq!(welded, raw);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ascii_reader_welds_shared_vertices() {
        let path = tmp("ascii");
        std::fs::write(
            &path,
            b"solid t\n\
facet normal 0 0 1\nouter loop\n\
vertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\n\
endloop\nendfacet\n\
facet normal 0 0 1\nouter loop\n\
vertex 1 0 0\nvertex 1 1 0\nvertex 0 1 0\n\
endloop\nendfacet\nendsolid t\n",
        )
        .unwrap();
        let raw = read_stl(&path).unwrap();
        assert_eq!((raw.vertex_count(), raw.triangle_count()), (6, 2));
        let welded = load_stl(&path).unwrap();
        assert_eq!((welded.vertex_count(), welded.triangle_count()), (4, 2));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_ascii_solid_is_valid_and_empty() {
        let path = tmp("empty");
        std::fs::write(&path, b"solid nothing\nendsolid nothing\n").unwrap();
        let m = load_stl(&path).unwrap();
        assert!(m.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn binary_size_mismatch_falls_back_to_ascii_like_python() {
        let path = tmp("truncated");
        let bytes = hex(PYTHON_TRIANGLE_STL);
        std::fs::write(&path, &bytes[..100]).unwrap();
        assert!(matches!(read_stl(&path), Err(MeshError::MalformedStl(_))));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn binary_with_trailing_bytes_is_accepted() {
        let path = tmp("trailing");
        let mut bytes = hex(PYTHON_TRIANGLE_STL);
        bytes.extend_from_slice(b"trailing metadata");
        std::fs::write(&path, bytes).unwrap();
        let m = read_stl(&path).unwrap();
        assert_eq!((m.vertex_count(), m.triangle_count()), (3, 1));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ascii_vertex_count_must_be_a_multiple_of_three() {
        let path = tmp("badcount");
        std::fs::write(&path, b"solid x\nvertex 0 0 0\nendsolid x\n").unwrap();
        assert!(matches!(read_stl(&path), Err(MeshError::MalformedStl(_))));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn scan_reports_nan_and_clean_binary() {
        let clean = tmp("clean");
        std::fs::write(&clean, hex(PYTHON_TRIANGLE_STL)).unwrap();
        assert_eq!(scan_stl_bad_coordinates(&clean).unwrap(), None);
        std::fs::remove_file(clean).unwrap();

        let bad = tmp("nan");
        let m = Mesh::new(
            vec![[f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        );
        std::fs::write(&bad, write_stl_binary_bytes(&m).unwrap()).unwrap();
        assert_eq!(
            scan_stl_bad_coordinates(&bad).unwrap().as_deref(),
            Some("NaN or infinite coordinates")
        );
        std::fs::remove_file(bad).unwrap();
    }

    #[test]
    fn scan_reports_malformed_binary_size() {
        let path = tmp("scansize");
        let bytes = hex(PYTHON_TRIANGLE_STL);
        std::fs::write(&path, &bytes[..100]).unwrap();
        let msg = scan_stl_bad_coordinates(&path).unwrap().unwrap();
        assert!(msg.contains("file size mismatch"), "{msg}");
        std::fs::remove_file(path).unwrap();
    }
}
