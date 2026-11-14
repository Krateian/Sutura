//! PLY reading and writing (ASCII and binary_little_endian).

use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::error::{MeshError, MeshResult};
use crate::mesh::Mesh;

#[derive(Clone)]
enum PropKind {
    Scalar(String),
    List { count: String, item: String },
}

#[derive(Clone)]
struct Prop {
    name: String,
    kind: PropKind,
}

#[derive(Clone)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

/// Read an ASCII or binary_little_endian PLY with vertex xyz + face lists.
pub fn read_ply(path: impl AsRef<Path>) -> MeshResult<Mesh> {
    let raw = std::fs::read(path)?;
    let cut = raw
        .windows(b"end_header".len())
        .position(|w| w == b"end_header")
        .ok_or_else(|| MeshError::MalformedPly("missing end_header".to_string()))?;
    let nl = raw[cut..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| cut + i)
        .ok_or_else(|| MeshError::MalformedPly("missing header newline".to_string()))?;
    let header = String::from_utf8_lossy(&raw[..=nl]).into_owned();
    let body = &raw[nl + 1..];

    let mut fmt: Option<String> = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in header.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "format" if parts.len() >= 2 => fmt = Some(parts[1].to_string()),
            "element" if parts.len() >= 3 => elements.push(Element {
                name: parts[1].to_string(),
                count: parts[2]
                    .parse()
                    .map_err(|_| MeshError::MalformedPly("bad element count".to_string()))?,
                props: Vec::new(),
            }),
            "property" if !elements.is_empty() => {
                let cur = elements.last_mut().unwrap();
                match parts.get(1).copied() {
                    Some("list") if parts.len() >= 5 => cur.props.push(Prop {
                        name: parts[4].to_string(),
                        kind: PropKind::List {
                            count: parts[2].to_string(),
                            item: parts[3].to_string(),
                        },
                    }),
                    Some(ty) if parts.len() >= 3 => cur.props.push(Prop {
                        name: parts[2].to_string(),
                        kind: PropKind::Scalar(ty.to_string()),
                    }),
                    _ => return Err(MeshError::MalformedPly("bad property line".to_string())),
                }
            }
            _ => {}
        }
    }

    let v_el = elements
        .iter()
        .find(|e| e.name == "vertex")
        .ok_or_else(|| MeshError::MalformedPly("PLY has no vertex element".to_string()))?;
    let f_el = elements.iter().find(|e| e.name == "face");

    let mut axes = [0usize; 3];
    for (k, axis) in ["x", "y", "z"].iter().enumerate() {
        axes[k] = v_el
            .props
            .iter()
            .position(|p| p.name == *axis && matches!(p.kind, PropKind::Scalar(_)))
            .ok_or_else(|| MeshError::MalformedPly(format!("PLY vertex has no {axis} property")))?;
    }

    let fmt = fmt.ok_or_else(|| MeshError::MalformedPly("PLY has no format".to_string()))?;
    let (verts, faces) = match fmt.as_str() {
        "ascii" => read_ascii(body, v_el, f_el, axes)?,
        "binary_little_endian" => read_binary(body, v_el, f_el, axes)?,
        other => {
            return Err(MeshError::MalformedPly(format!(
                "unsupported PLY format: {other}"
            )))
        }
    };
    let mut flat = Vec::new();
    for f in faces {
        flat.extend(f);
    }
    if flat.len() % 3 != 0 {
        return Err(MeshError::MalformedPly(
            "PLY face list is not triangular".to_string(),
        ));
    }
    let tris = flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    Ok(Mesh::new(verts, tris))
}

/// Write a binary_little_endian PLY with vertex xyz and triangle face lists.
pub fn write_ply(path: impl AsRef<Path>, mesh: &Mesh) -> MeshResult<()> {
    mesh.validate()?;
    let mut w = std::io::BufWriter::new(File::create(path)?);
    let head = format!(
        "ply\nformat binary_little_endian 1.0\n\
element vertex {}\nproperty float x\nproperty float y\n\
property float z\nelement face {}\n\
property list uchar int vertex_indices\nend_header\n",
        mesh.verts.len(),
        mesh.tris.len()
    );
    w.write_all(head.as_bytes())?;
    for v in &mesh.verts {
        for c in v {
            w.write_all(&c.to_le_bytes())?;
        }
    }
    for t in &mesh.tris {
        w.write_all(&[3u8])?;
        for i in t {
            w.write_all(&i.to_le_bytes())?;
        }
    }
    w.flush()?;
    Ok(())
}

fn read_ascii(
    body: &[u8],
    v_el: &Element,
    f_el: Option<&Element>,
    axes: [usize; 3],
) -> MeshResult<(Vec<[f32; 3]>, Vec<Vec<i32>>)> {
    let text = String::from_utf8_lossy(body);
    let rows: Vec<Vec<f64>> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split_whitespace()
                .map(|t| t.parse::<f64>())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| MeshError::MalformedPly("bad ASCII number".to_string()))
        })
        .collect::<MeshResult<_>>()?;
    if rows.len() < v_el.count {
        return Err(MeshError::MalformedPly(
            "truncated ASCII vertices".to_string(),
        ));
    }
    let mut verts = Vec::with_capacity(v_el.count);
    for row in &rows[..v_el.count] {
        if row.len() <= axes[2] {
            return Err(MeshError::MalformedPly(
                "short ASCII vertex row".to_string(),
            ));
        }
        verts.push([
            row[axes[0]] as f32,
            row[axes[1]] as f32,
            row[axes[2]] as f32,
        ]);
    }
    let mut faces = Vec::new();
    if let Some(f_el) = f_el {
        let rest = &rows[v_el.count..];
        if rest.len() < f_el.count {
            return Err(MeshError::MalformedPly("truncated ASCII faces".to_string()));
        }
        for row in &rest[..f_el.count] {
            if row.is_empty() {
                return Err(MeshError::MalformedPly("empty face row".to_string()));
            }
            let n = row[0] as i64;
            if n < 0 || row.len() < 1 + n as usize {
                return Err(MeshError::MalformedPly("short face row".to_string()));
            }
            faces.push(row[1..1 + n as usize].iter().map(|v| *v as i32).collect());
        }
    }
    Ok((verts, faces))
}

fn read_binary(
    body: &[u8],
    v_el: &Element,
    f_el: Option<&Element>,
    axes: [usize; 3],
) -> MeshResult<(Vec<[f32; 3]>, Vec<Vec<i32>>)> {
    let mut offsets = Vec::with_capacity(v_el.props.len());
    let mut off = 0usize;
    for p in &v_el.props {
        match &p.kind {
            PropKind::Scalar(ty) => {
                offsets.push((off, ty.clone()));
                off += type_size(ty)?;
            }
            PropKind::List { .. } => {
                return Err(MeshError::MalformedPly(
                    "list property in vertex element is unsupported".to_string(),
                ))
            }
        }
    }
    let rec = off;
    if body.len() < v_el.count.saturating_mul(rec) {
        return Err(MeshError::MalformedPly(
            "truncated binary vertices".to_string(),
        ));
    }
    let mut verts = Vec::with_capacity(v_el.count);
    for i in 0..v_el.count {
        let base = i * rec;
        let mut xyz = [0f32; 3];
        for (k, &axis) in axes.iter().enumerate() {
            let (o, ty) = &offsets[axis];
            let v = read_scalar(body, base + o, ty)?;
            xyz[k] = v as f32;
        }
        verts.push(xyz);
    }
    let mut faces = Vec::new();
    if let Some(f_el) = f_el {
        let list = f_el
            .props
            .iter()
            .find_map(|p| match &p.kind {
                PropKind::List { count, item } => Some((count.clone(), item.clone())),
                _ => None,
            })
            .ok_or_else(|| {
                MeshError::MalformedPly("PLY face element has no list property".to_string())
            })?;
        let mut pos = v_el.count * rec;
        for _ in 0..f_el.count {
            let n = read_scalar(body, pos, &list.0)? as i64;
            pos += type_size(&list.0)?;
            if n < 0 {
                return Err(MeshError::MalformedPly("negative face size".to_string()));
            }
            let mut idx = Vec::with_capacity(n as usize);
            for _ in 0..n {
                idx.push(read_scalar(body, pos, &list.1)? as i32);
                pos += type_size(&list.1)?;
            }
            faces.push(idx);
        }
    }
    Ok((verts, faces))
}

fn type_size(ty: &str) -> MeshResult<usize> {
    Ok(match ty {
        "char" | "uchar" | "int8" | "uint8" => 1,
        "short" | "ushort" | "int16" | "uint16" => 2,
        "int" | "uint" | "int32" | "uint32" | "float" | "float32" => 4,
        "double" | "float64" => 8,
        other => {
            return Err(MeshError::MalformedPly(format!(
                "unknown PLY type: {other}"
            )))
        }
    })
}

fn read_scalar(data: &[u8], off: usize, ty: &str) -> MeshResult<f64> {
    let size = type_size(ty)?;
    if off + size > data.len() {
        return Err(MeshError::MalformedPly("truncated PLY body".to_string()));
    }
    let b = &data[off..off + size];
    Ok(match ty {
        "char" | "int8" => i8::from_le_bytes([b[0]]) as f64,
        "uchar" | "uint8" => b[0] as f64,
        "short" | "int16" => i16::from_le_bytes([b[0], b[1]]) as f64,
        "ushort" | "uint16" => u16::from_le_bytes([b[0], b[1]]) as f64,
        "int" | "int32" => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        "uint" | "uint32" => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        "float" | "float32" => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        "double" | "float64" => {
            f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        }
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sutura-ply-{}-{}-{}.ply",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn tri_mesh() -> Mesh {
        Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
    }

    #[test]
    fn binary_roundtrip_matches_python_layout() {
        let path = tmp("bin");
        let m = tri_mesh();
        write_ply(&path, &m).unwrap();
        let got = read_ply(&path).unwrap();
        assert_eq!(got, m);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"ply\nformat binary_little_endian 1.0\n"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ascii_with_extra_vertex_properties() {
        let path = tmp("ascii");
        std::fs::write(
            &path,
            b"ply\nformat ascii 1.0\n\
element vertex 3\nproperty float nx\nproperty float x\nproperty float y\nproperty float z\n\
element face 1\nproperty list uchar int vertex_indices\nend_header\n\
9 0 0 0\n8 1 0 0\n7 0 1 0\n3 0 1 2\n",
        )
        .unwrap();
        let m = read_ply(&path).unwrap();
        assert_eq!(m.verts, tri_mesh().verts);
        assert_eq!(m.tris, tri_mesh().tris);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn binary_with_extra_vertex_properties() {
        let path = tmp("bin-extra");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            b"ply\nformat binary_little_endian 1.0\nelement vertex 1\n\
property uchar red\nproperty float x\nproperty float y\nproperty float z\n\
element face 0\nproperty list uchar int vertex_indices\nend_header\n",
        );
        bytes.push(7);
        for c in [1.0f32, 2.0, 3.0] {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        std::fs::write(&path, bytes).unwrap();
        let m = read_ply(&path).unwrap();
        assert_eq!(m.verts, vec![[1.0, 2.0, 3.0]]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_missing_end_header_and_short_body() {
        let path = tmp("bad");
        std::fs::write(&path, b"ply\nformat ascii 1.0\n").unwrap();
        assert!(matches!(read_ply(&path), Err(MeshError::MalformedPly(_))));
        std::fs::write(
            &path,
            b"ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\n\
property float y\nproperty float z\nend_header\n0 0 0\n",
        )
        .unwrap();
        assert!(matches!(read_ply(&path), Err(MeshError::MalformedPly(_))));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn writer_bytes_match_python_golden() {
        let path = tmp("golden");
        write_ply(&path, &tri_mesh()).unwrap();
        let got = std::fs::read(&path).unwrap();
        let head = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\n\
property float x\nproperty float y\nproperty float z\nelement face 1\n\
property list uchar int vertex_indices\nend_header\n";
        assert_eq!(&got[..head.len()], head);
        let mut pos = head.len();
        for v in tri_mesh().verts {
            for c in v {
                assert_eq!(&got[pos..pos + 4], &c.to_le_bytes());
                pos += 4;
            }
        }
        assert_eq!(got[pos], 3);
        pos += 1;
        for i in [0i32, 1, 2] {
            assert_eq!(&got[pos..pos + 4], &i.to_le_bytes());
            pos += 4;
        }
        assert_eq!(pos, got.len());
        std::fs::remove_file(path).unwrap();
    }
}
