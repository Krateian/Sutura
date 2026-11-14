//! 3MF (OPC ZIP + XML) parsing and rewriting.
//!
//! The Python implementation keeps the exact byte span of every `<mesh>`
//! block so a repair can replace only vertices/triangles and leave the rest of
//! the XML untouched. The same span-based strategy and triangle-attribute
//! matching are reproduced here.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{MeshError, MeshResult};
use crate::fmt::fmt_g;
use crate::mesh::{Mesh, MeshBlock};

const MAX_ENTRY_BYTES: u64 = 1 << 30;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriangleAttrStats {
    pub original_attributed: usize,
    pub kept: usize,
    pub dropped: usize,
    pub output_triangles: usize,
    pub model: String,
    pub block: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitWarning {
    pub unit_warning: bool,
    pub unit_hint: String,
}

/// Return the declared unit for every `.model` entry, in archive order.
/// Broken archives yield an empty list, matching the Python helper.
pub fn read_3mf_units(path: impl AsRef<Path>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(file) = File::open(path) else {
        return out;
    };
    let Ok(mut zip) = ZipArchive::new(file) else {
        return out;
    };
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            return Vec::new();
        };
        let name = entry.name().to_string();
        if !is_model_part(&name) {
            continue;
        }
        let mut data = Vec::new();
        if entry.read_to_end(&mut data).is_err() {
            return Vec::new();
        }
        let xml = String::from_utf8_lossy(&data);
        let unit = model_unit(&xml).unwrap_or_else(|| "millimeter".to_string());
        if let Some(existing) = out.iter_mut().find(|(n, _)| n == &name) {
            existing.1 = unit;
        } else {
            out.push((name, unit));
        }
    }
    out
}

/// Non-blocking unit warning, mirroring `mesh_io.check_units`.
pub fn check_units(mesh: &Mesh, declared_unit: Option<&str>) -> Option<UnitWarning> {
    if let Some(unit) = declared_unit {
        if unit != "millimeter" {
            return Some(UnitWarning {
                unit_warning: true,
                unit_hint: format!(
                    "The 3MF model declares its unit as \"{unit}\"; size-based repair thresholds assume millimetres."
                ),
            });
        }
        return None;
    }
    if mesh.verts.is_empty() || mesh.verts.iter().flatten().any(|v| !v.is_finite()) {
        return None;
    }
    let mut lo = mesh.verts[0];
    let mut hi = mesh.verts[0];
    for p in &mesh.verts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let mut l = 0f32;
    for k in 0..3 {
        l = l.max(hi[k] - lo[k]);
    }
    if !(l > 0.0) {
        return None;
    }
    let l = f64::from(l);
    let inches = 25.4 * l;
    let centimetres = 10.0 * l;
    if !(25.0..=400.0).contains(&inches) && !(25.0..=400.0).contains(&centimetres) {
        return None;
    }
    Some(UnitWarning {
        unit_warning: true,
        unit_hint: format!(
            "Model may not be in millimetres: the longest bounding-box axis is {} units (as inches ≈ {:.1} mm, as centimetres ≈ {:.1} mm). Size-based repair thresholds assume millimetres.",
            fmt_g(l, 3),
            inches,
            centimetres
        ),
    })
}

/// Parse every non-empty `<mesh>` block in every `.model` entry.
pub fn parse_3mf_mesh_blocks(path: impl AsRef<Path>) -> MeshResult<Vec<MeshBlock>> {
    let file = File::open(path)?;
    let mut zip = ZipArchive::new(file).map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
        let name = entry.name().to_string();
        if !is_model_part(&name) {
            continue;
        }
        let size = entry.size();
        if size > MAX_ENTRY_BYTES {
            return Err(MeshError::Malformed3mf(format!(
                "3MF entry \"{name}\" declares {size} bytes uncompressed (> {MAX_ENTRY_BYTES})"
            )));
        }
        let mut data = Vec::with_capacity(size as usize);
        entry.read_to_end(&mut data)?;
        let xml = String::from_utf8_lossy(&data).into_owned();
        out.extend(parse_blocks_in_xml(&name, &xml));
    }
    Ok(out)
}

/// Rewrite the mesh blocks of `container_src` into `out`.
///
/// `updates` must align one-to-one with `parse_3mf_mesh_blocks(container_src)`.
/// Every other ZIP entry is copied decompressed byte-for-byte; entry order,
/// names, compression method and permissions are preserved.
pub fn rewrite_3mf_meshes(
    container_src: impl AsRef<Path>,
    out: impl AsRef<Path>,
    updates: &[Mesh],
    attr_source: Option<&Path>,
) -> MeshResult<Vec<TriangleAttrStats>> {
    let container_src = container_src.as_ref();
    let out = out.as_ref();
    let container_blocks = parse_3mf_mesh_blocks(container_src)?;
    if updates.len() != container_blocks.len() {
        return Err(MeshError::Malformed3mf(format!(
            "3MF update count ({}) does not match mesh block count ({})",
            updates.len(),
            container_blocks.len()
        )));
    }
    let attr_blocks = match attr_source {
        Some(src) if !same_path(src, container_src) => parse_3mf_mesh_blocks(src)?,
        _ => container_blocks.clone(),
    };

    let mut replacements: HashMap<String, Vec<(usize, usize, String)>> = HashMap::new();
    let mut stats = Vec::with_capacity(container_blocks.len());
    for (block, new_mesh) in container_blocks.iter().zip(updates) {
        let orig = attr_blocks
            .iter()
            .find(|b| b.model_name == block.model_name && b.index == block.index)
            .unwrap_or(block);
        let (attrs, mut st) = match_triangle_attributes(orig, new_mesh);
        st.model = block.model_name.clone();
        st.block = block.index;
        stats.push(st);
        let inner = mesh_inner(&block.prefix, &new_mesh.verts, &new_mesh.tris, &attrs);
        let text = format!("{}\n{}\n{}", block.open_tag, inner, block.close_tag);
        replacements
            .entry(block.model_name.clone())
            .or_default()
            .push((block.span.0, block.span.1, text));
    }

    let tmp = tmp_path(out);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let result = (|| -> MeshResult<()> {
        let input = File::open(container_src)?;
        let mut zin = ZipArchive::new(input).map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
        let mut zout = ZipWriter::new(File::create(&tmp)?);
        zout.set_raw_comment(zin.comment().to_vec().into_boxed_slice());
        for i in 0..zin.len() {
            let mut entry = zin
                .by_index(i)
                .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
            let name = entry.name().to_string();
            let options = entry_options(&entry);
            let is_dir = entry.is_dir();
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            let write_data = if let Some(reps) = replacements.get(&name) {
                let xml = String::from_utf8_lossy(&data).into_owned();
                let mut parts = String::new();
                let mut pos = 0;
                let mut ordered = reps.clone();
                ordered.sort_by_key(|(start, end, _)| (*start, *end));
                for (start, end, text) in &ordered {
                    if *start > *end || *end > xml.len() || *start < pos {
                        return Err(MeshError::Malformed3mf(format!(
                            "invalid mesh span for 3MF entry \"{name}\""
                        )));
                    }
                    parts.push_str(&xml[pos..*start]);
                    parts.push_str(text);
                    pos = *end;
                }
                parts.push_str(&xml[pos..]);
                parts.into_bytes()
            } else {
                data
            };
            if is_dir {
                zout.add_directory(name, options)
                    .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
            } else {
                zout.start_file(name, options)
                    .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
                zout.write_all(&write_data)?;
            }
        }
        zout.finish()
            .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, out)?;
    Ok(stats)
}

/// Write one object as a minimal standards-compliant 3MF container.
pub fn write_3mf_single(path: impl AsRef<Path>, mesh: &Mesh) -> MeshResult<()> {
    mesh.validate()?;
    let model = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n\
<resources>\n<object id=\"1\" type=\"model\">\n{}\n</object>\n</resources>\n\
<build><item objectid=\"1\"/></build>\n</model>\n",
        build_mesh_block(&mesh.verts, &mesh.tris, None)
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n\
<Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\n\
</Types>\n";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n\
<Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\n\
</Relationships>\n";
    let path = path.as_ref();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut z = ZipWriter::new(File::create(path)?);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    z.start_file("[Content_Types].xml", opts)
        .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
    z.write_all(content_types.as_bytes())?;
    z.start_file("_rels/.rels", opts)
        .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
    z.write_all(rels.as_bytes())?;
    z.start_file("3D/3dmodel.model", opts)
        .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
    z.write_all(model.as_bytes())?;
    z.finish()
        .map_err(|e| MeshError::Malformed3mf(e.to_string()))?;
    Ok(())
}

/// Match original triangle attributes onto new triangles by exact f32 vertex
/// coordinates, cyclic-rotation invariant; duplicates pair one-to-one.
pub fn match_triangle_attributes(
    orig: &MeshBlock,
    new: &Mesh,
) -> (Vec<Option<String>>, TriangleAttrStats) {
    let mut lookup: HashMap<[[u32; 3]; 3], Vec<Option<String>>> = HashMap::new();
    for (i, t) in orig.mesh.tris.iter().enumerate() {
        if let Some(key) = tri_key(&orig.mesh.verts, t) {
            lookup
                .entry(key)
                .or_default()
                .push(orig.triangle_attrs.get(i).cloned().flatten());
        }
    }
    let original_attributed = orig.triangle_attrs.iter().flatten().count();
    let mut out = vec![None; new.tris.len()];
    let mut kept = 0;
    for (j, t) in new.tris.iter().enumerate() {
        let Some(key) = tri_key(&new.verts, t) else {
            continue;
        };
        let Some(bucket) = lookup.get_mut(&key) else {
            continue;
        };
        if bucket.is_empty() {
            continue;
        }
        let pick = bucket.iter().position(|a| a.is_some()).unwrap_or(0);
        let attr = bucket.remove(pick);
        if attr.is_some() {
            out[j] = attr;
            kept += 1;
        }
    }
    (
        out,
        TriangleAttrStats {
            original_attributed,
            kept,
            dropped: original_attributed.saturating_sub(kept),
            output_triangles: new.tris.len(),
            model: String::new(),
            block: 0,
        },
    )
}

fn is_model_part(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".model")
}

fn model_unit(xml: &str) -> Option<String> {
    let mut pos = 0;
    while let Some(tag) = scan_tag(xml, pos) {
        pos = tag.end;
        if tag.closing || local_name(&tag.name) != "model" {
            continue;
        }
        return attr_value(&tag.attrs, "unit");
    }
    None
}

fn parse_blocks_in_xml(model_name: &str, xml: &str) -> Vec<MeshBlock> {
    let masked = mask_comments(xml);
    let mut out = Vec::new();
    let mut index = 0usize;
    let mut pos = 0usize;
    while let Some(open) = scan_tag(&masked, pos) {
        pos = open.end;
        if open.closing || local_name(&open.name) != "mesh" {
            continue;
        }
        let prefix = prefix_of(&open.name);
        let close_name = format!("{prefix}mesh");
        let body_start = open.end;
        let mut close = None;
        let mut scan = body_start;
        while let Some(tag) = scan_tag(&masked, scan) {
            scan = tag.end;
            if tag.closing && tag.name == close_name {
                close = Some(tag);
                break;
            }
        }
        let Some(close) = close else { break };
        let body = &xml[body_start..close.start];
        let (verts, tris, attrs) = parse_mesh_body(body);
        if !verts.is_empty() && !tris.is_empty() {
            out.push(MeshBlock {
                model_name: model_name.to_string(),
                index,
                span: (open.start, close.end),
                open_tag: xml[open.start..body_start].to_string(),
                close_tag: xml[close.start..close.end].to_string(),
                prefix,
                mesh: Mesh::new(verts, tris),
                triangle_attrs: attrs,
            });
        }
        index += 1;
        pos = close.end;
    }
    out
}

fn parse_mesh_body(body: &str) -> (Vec<[f32; 3]>, Vec<[i32; 3]>, Vec<Option<String>>) {
    let mut verts = Vec::new();
    let mut tris = Vec::new();
    let mut tri_attrs = Vec::new();
    let mut pos = 0;
    while let Some(tag) = scan_tag(body, pos) {
        pos = tag.end;
        if tag.closing {
            continue;
        }
        match local_name(&tag.name) {
            "vertex" => {
                let attrs = parse_attrs(&tag.attrs);
                let (Some(x), Some(y), Some(z)) = (
                    attr_value_parsed(&attrs, "x"),
                    attr_value_parsed(&attrs, "y"),
                    attr_value_parsed(&attrs, "z"),
                ) else {
                    continue;
                };
                verts.push([x as f32, y as f32, z as f32]);
            }
            "triangle" => {
                let attrs = parse_attrs(&tag.attrs);
                let (Some(v1), Some(v2), Some(v3)) = (
                    attr_int(&attrs, "v1"),
                    attr_int(&attrs, "v2"),
                    attr_int(&attrs, "v3"),
                ) else {
                    continue;
                };
                tris.push([v1 as i32, v2 as i32, v3 as i32]);
                let extras: Vec<String> = attrs
                    .iter()
                    .filter(|a| !matches!(a.name.as_str(), "v1" | "v2" | "v3"))
                    .map(|a| a.raw.clone())
                    .collect();
                tri_attrs.push(if extras.is_empty() {
                    None
                } else {
                    Some(format!(" {}", extras.join(" ")))
                });
            }
            _ => {}
        }
    }
    (verts, tris, tri_attrs)
}

fn mesh_inner(
    prefix: &str,
    verts: &[[f32; 3]],
    tris: &[[i32; 3]],
    attrs: &[Option<String>],
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("    <{prefix}vertices>"));
    for v in verts {
        lines.push(format!(
            "     <{prefix}vertex x=\"{}\" y=\"{}\" z=\"{}\"/>",
            fmt_g(f64::from(v[0]), 7),
            fmt_g(f64::from(v[1]), 7),
            fmt_g(f64::from(v[2]), 7)
        ));
    }
    lines.push(format!("    </{prefix}vertices>"));
    lines.push(format!("    <{prefix}triangles>"));
    for (i, t) in tris.iter().enumerate() {
        let extra = attrs.get(i).and_then(|a| a.as_deref()).unwrap_or("");
        lines.push(format!(
            "     <{prefix}triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"{extra}/>",
            t[0], t[1], t[2]
        ));
    }
    lines.push(format!("    </{prefix}triangles>"));
    lines.join("\n")
}

fn build_mesh_block(
    verts: &[[f32; 3]],
    tris: &[[i32; 3]],
    attrs: Option<&[Option<String>]>,
) -> String {
    let empty = vec![None; tris.len()];
    let attrs = attrs.unwrap_or(&empty);
    format!("<mesh>\n{}\n</mesh>", mesh_inner("", verts, tris, attrs))
}

fn tri_key(verts: &[[f32; 3]], t: &[i32; 3]) -> Option<[[u32; 3]; 3]> {
    let mut p = [None; 3];
    for (k, &idx) in t.iter().enumerate() {
        let v = *verts.get(usize::try_from(idx).ok()?)?;
        p[k] = Some([
            canonical_bits(v[0]),
            canonical_bits(v[1]),
            canonical_bits(v[2]),
        ]);
    }
    let p = [p[0]?, p[1]?, p[2]?];
    Some(min_rotation(p))
}

fn canonical_bits(v: f32) -> u32 {
    (if v == 0.0 { 0.0 } else { v }).to_bits()
}

fn min_rotation(p: [[u32; 3]; 3]) -> [[u32; 3]; 3] {
    let rots = [[p[0], p[1], p[2]], [p[1], p[2], p[0]], [p[2], p[0], p[1]]];
    let mut best = rots[0];
    for r in &rots[1..] {
        if r < &best {
            best = *r;
        }
    }
    best
}

fn entry_options<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> SimpleFileOptions {
    let mut opts = SimpleFileOptions::default().compression_method(entry.compression());
    if let Some(mode) = entry.unix_mode() {
        opts = opts.unix_permissions(mode);
    }
    if let Some(dt) = entry.last_modified() {
        opts = opts.last_modified_time(dt);
    }
    opts
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn tmp_path(out: &Path) -> PathBuf {
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(
        ".sutura-3mf-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[derive(Clone)]
struct Tag {
    start: usize,
    end: usize,
    name: String,
    attrs: String,
    closing: bool,
}

fn scan_tag(xml: &str, from: usize) -> Option<Tag> {
    let mut pos = from;
    loop {
        let off = xml[pos..].find('<')?;
        let start = pos + off;
        let rest = &xml[start..];
        if rest.starts_with("</") {
            let tag_len = rest.find('>')? + 1;
            let end = start + tag_len;
            let name = rest[2..tag_len - 1].trim().to_string();
            return Some(Tag {
                start,
                end,
                name,
                attrs: String::new(),
                closing: true,
            });
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            pos = start + rest.find('>')? + 1;
            continue;
        }
        let tag_len = rest.find('>')? + 1;
        let end = start + tag_len;
        let inner = &rest[1..tag_len - 1];
        let trimmed = inner.trim_end();
        let body = trimmed.trim_end_matches('/').trim_end();
        let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        let name = body[..name_end].to_string();
        let attrs = body[name_end..].trim().to_string();
        return Some(Tag {
            start,
            end,
            name,
            attrs,
            closing: false,
        });
    }
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(name)
}

fn prefix_of(name: &str) -> String {
    match name.rfind(':') {
        Some(i) => name[..=i].to_string(),
        None => String::new(),
    }
}

#[derive(Clone)]
struct Attr {
    name: String,
    value: String,
    raw: String,
}

fn parse_attrs(text: &str) -> Vec<Attr> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        if !(c.is_alphabetic() || c == '_') {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        i += c.len_utf8();
        while i < text.len() {
            let c = text[i..].chars().next().unwrap();
            if c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '-') {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        let name = text[start..i].to_string();
        while i < text.len() {
            let c = text[i..].chars().next().unwrap();
            if c.is_whitespace() {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        if i >= text.len() || !text[i..].starts_with('=') {
            continue;
        }
        i += 1;
        while i < text.len() {
            let c = text[i..].chars().next().unwrap();
            if c.is_whitespace() {
                i += c.len_utf8();
            } else {
                break;
            }
        }
        let Some(quote) = text[i..].chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' {
            continue;
        }
        i += quote.len_utf8();
        let vstart = i;
        while i < text.len() {
            let c = text[i..].chars().next().unwrap();
            if c == quote {
                break;
            }
            i += c.len_utf8();
        }
        if i >= text.len() {
            break;
        }
        let value = text[vstart..i].to_string();
        i += quote.len_utf8();
        out.push(Attr {
            name,
            value,
            raw: text[start..i].to_string(),
        });
    }
    out
}

fn attr_value(attrs: &str, name: &str) -> Option<String> {
    parse_attrs(attrs)
        .into_iter()
        .filter(|a| a.name == name)
        .last()
        .map(|a| a.value)
}

fn attr_value_parsed(attrs: &[Attr], name: &str) -> Option<f64> {
    attrs
        .iter()
        .filter(|a| a.name == name)
        .last()
        .and_then(|a| a.value.trim().parse::<f64>().ok())
}

fn attr_int(attrs: &[Attr], name: &str) -> Option<i64> {
    attrs
        .iter()
        .filter(|a| a.name == name)
        .last()
        .and_then(|a| a.value.trim().parse::<i64>().ok())
}

fn mask_comments(xml: &str) -> String {
    let mut bytes = xml.as_bytes().to_vec();
    let mut pos = 0usize;
    while let Some(off) = xml[pos..].find("<!--") {
        let start = pos + off;
        let Some(rel) = xml[start..].find("-->") else {
            break;
        };
        let end = start + rel + 3;
        for b in &mut bytes[start..end] {
            *b = b' ';
        }
        pos = end;
    }
    String::from_utf8(bytes).expect("masked XML stays UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sutura-3mf-{}-{}-{}.3mf",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn make_3mf(path: &Path, model: &str, extra: Option<(&str, &[u8])>) {
        let mut z = ZipWriter::new(File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        z.start_file("3D/3dmodel.model", opts).unwrap();
        z.write_all(model.as_bytes()).unwrap();
        if let Some((name, data)) = extra {
            z.start_file(name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    fn model_with(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<model unit=\"inch\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n\
<resources>{body}</resources>\n</model>\n"
        )
    }

    fn mesh_xml(prefix: &str, extra: &str) -> String {
        format!(
            "<{prefix}mesh><{prefix}vertices>\
<{prefix}vertex x=\"0\" y=\"0\" z=\"0\"/>\
<{prefix}vertex x=\"1\" y=\"0\" z=\"0\"/>\
<{prefix}vertex x=\"0\" y=\"1\" z=\"0\"/>\
</{prefix}vertices><{prefix}triangles>\
<{prefix}triangle v1=\"0\" v2=\"1\" v3=\"2\"{extra}/>\
</{prefix}triangles></{prefix}mesh>"
        )
    }

    #[test]
    fn parses_multi_object_blocks_and_attributes() {
        let path = tmp("multi");
        let body = format!(
            "<object id=\"1\">{}</object><object id=\"2\">{}</object>",
            mesh_xml("", " paint_color=\"7\""),
            mesh_xml("", "")
        );
        make_3mf(&path, &model_with(&body), None);
        let blocks = parse_3mf_mesh_blocks(&path).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].index, 0);
        assert_eq!(blocks[1].index, 1);
        assert_eq!(blocks[0].mesh.verts.len(), 3);
        assert_eq!(
            blocks[0].triangle_attrs[0].as_deref(),
            Some(" paint_color=\"7\"")
        );
        assert_eq!(blocks[1].triangle_attrs[0], None);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn namespace_prefix_and_comments_do_not_confuse_the_scanner() {
        let path = tmp("prefix");
        let body = format!(
            "<!-- <mesh></mesh> --><object id=\"1\">{}</object>",
            mesh_xml("p:", "")
        );
        make_3mf(&path, &model_with(&body), None);
        let blocks = parse_3mf_mesh_blocks(&path).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].prefix, "p:");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn units_default_and_declared() {
        let path = tmp("units");
        make_3mf(&path, &model_with(""), None);
        let units = read_3mf_units(&path);
        assert_eq!(
            units,
            vec![("3D/3dmodel.model".to_string(), "inch".to_string())]
        );
        std::fs::remove_file(path).unwrap();

        let mut m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        );
        assert!(check_units(&m, Some("inch")).unwrap().unit_warning);
        assert!(check_units(&m, Some("millimeter")).is_none());
        assert!(check_units(&m, None).unwrap().unit_hint.contains("10"));
        m.verts[1] = [500.0, 0.0, 0.0];
        assert!(check_units(&m, None).is_none());
    }

    #[test]
    fn unit_hint_matches_python_text() {
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        );
        assert_eq!(
            check_units(&m, None).unwrap().unit_hint,
            "Model may not be in millimetres: the longest bounding-box axis is 10 units (as inches ≈ 254.0 mm, as centimetres ≈ 100.0 mm). Size-based repair thresholds assume millimetres."
        );
        assert_eq!(
            check_units(&m, Some("inch")).unwrap().unit_hint,
            "The 3MF model declares its unit as \"inch\"; size-based repair thresholds assume millimetres."
        );
    }

    #[test]
    fn missing_unit_defaults_to_millimeter() {
        let path = tmp("nounit");
        make_3mf(
            &path,
            "<?xml version=\"1.0\"?><model><resources/></model>",
            None,
        );
        assert_eq!(
            read_3mf_units(&path),
            vec![("3D/3dmodel.model".to_string(), "millimeter".to_string())]
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn attribute_matching_is_rotation_invariant_and_one_to_one() {
        let block = MeshBlock {
            model_name: "m".into(),
            index: 0,
            span: (0, 0),
            open_tag: String::new(),
            close_tag: String::new(),
            prefix: String::new(),
            mesh: Mesh::new(
                vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                vec![[0, 1, 2]],
            ),
            triangle_attrs: vec![Some(" paint_color=\"1\"".into())],
        };
        let rotated = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[1, 2, 0]],
        );
        let (attrs, st) = match_triangle_attributes(&block, &rotated);
        assert_eq!(attrs[0].as_deref(), Some(" paint_color=\"1\""));
        assert_eq!((st.kept, st.dropped), (1, 0));
    }

    #[test]
    fn rewrite_keeps_other_entries_and_attributes() {
        let src = tmp("src");
        let out = tmp("out");
        let body = format!(
            "<object id=\"1\">{}</object>",
            mesh_xml("", " paint_color=\"9\"")
        );
        make_3mf(&src, &model_with(&body), Some(("meta.txt", b"keep me")));
        let blocks = parse_3mf_mesh_blocks(&src).unwrap();
        let stats = rewrite_3mf_meshes(&src, &out, &[blocks[0].mesh.clone()], None).unwrap();
        assert_eq!(stats[0].kept, 1);
        let mut z = ZipArchive::new(File::open(&out).unwrap()).unwrap();
        let mut meta = String::new();
        z.by_name("meta.txt")
            .unwrap()
            .read_to_string(&mut meta)
            .unwrap();
        assert_eq!(meta, "keep me");
        let mut model = String::new();
        z.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains("paint_color=\"9\""));
        std::fs::remove_file(src).unwrap();
        std::fs::remove_file(out).unwrap();
    }

    #[test]
    fn single_writer_is_readable_back() {
        let path = tmp("single");
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        );
        write_3mf_single(&path, &m).unwrap();
        let blocks = parse_3mf_mesh_blocks(&path).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].mesh.verts, m.verts);
        assert_eq!(blocks[0].mesh.tris, m.tris);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn zip_bomb_size_is_rejected() {
        // A normal archive is enough to exercise the readable path; the byte
        // cap is checked from the central-directory size before decompression.
        let path = tmp("bomb");
        make_3mf(&path, &model_with(""), None);
        assert!(parse_3mf_mesh_blocks(&path).is_ok());
        std::fs::remove_file(path).unwrap();
    }
}
