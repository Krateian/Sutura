// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Per-object repair for multi-object containers (3MF), mirroring
//! `sutura_engine.pipeline.service.repair_3mf_objects`.
//!
//! The Python implementation repairs **each `<mesh>` block separately** -- it
//! never merges the objects -- then rewrites them all in one pass with
//! `rewrite_3mf_meshes`. Two properties matter and are reproduced here:
//!
//! * **Object order** follows the archive order of `parse_3mf_mesh_blocks`.
//! * **Byte-identical geometries are repaired once** and their report is
//!   cloned per object, because the report is a pure function of the geometry.
//!   Python keys that cache on `(verts.tobytes(), tris.tobytes())`; the same
//!   bytes are used here as the key so a hit means exactly what it means there.
//!
//! Per-object stage 2 is not a separate step: `repair` already runs it
//! (`stage2.maybe_run_stage2`), so calling `repair` per block gives the same
//! ordering as Python's `repair_mesh_from_arrays` + `maybe_run_stage2` pair.

use std::collections::HashMap;

use serde_json::Value;

use crate::mesh::MeshBlock;
use crate::mesh_io::three_mf::read_3mf_units;
use crate::repair::{repair, CancelToken, ProgressSink, RepairOptions, RepairOutcome};

/// One object's repair, in archive order.
#[derive(Clone, Debug)]
pub struct ObjectRepair {
    /// The `.model` entry the block came from (`3D/Objects/object_1.model`).
    pub model_name: String,
    /// Block index inside that entry.
    pub block_index: usize,
    /// The unit declared by that model part (`millimeter` when absent).
    pub declared_unit: String,
    /// The per-object report; a clone of the cached one on a geometry hit.
    pub report: Value,
    /// The repaired geometry, ready for `rewrite_3mf_meshes`.
    pub mesh: crate::mesh::Mesh,
}

impl ObjectRepair {
    pub fn repaired(&self) -> bool {
        self.report
            .get("error")
            .map(|e| e.is_null())
            .unwrap_or(true)
    }
}

/// The cache key: the object's geometry bytes, exactly as Python keys it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct GeometryKey {
    verts: Vec<u8>,
    tris: Vec<u8>,
}

impl GeometryKey {
    fn of(mesh: &crate::mesh::Mesh) -> Self {
        let mut verts = Vec::with_capacity(mesh.verts.len() * 12);
        for p in &mesh.verts {
            for c in p {
                verts.extend_from_slice(&c.to_le_bytes());
            }
        }
        let mut tris = Vec::with_capacity(mesh.tris.len() * 12);
        for f in &mesh.tris {
            for i in f {
                tris.extend_from_slice(&i.to_le_bytes());
            }
        }
        Self { verts, tris }
    }
}

/// Look up a model part's declared unit, defaulting to millimetres like Python.
fn unit_for(units: &[(String, String)], model_name: &str) -> String {
    units
        .iter()
        .find(|(name, _)| name == model_name)
        .map(|(_, unit)| unit.clone())
        .unwrap_or_else(|| "millimeter".to_string())
}

/// Repair every block of a container, in archive order.
///
/// `units` comes from `read_3mf_units` and is looked up per model part. A
/// block whose repair fails still yields an `ObjectRepair` carrying the error,
/// so the caller can report it without losing the other objects -- the same
/// behaviour as Python, which records the failure in that object's report.
pub fn repair_objects(
    blocks: &[MeshBlock],
    units: &[(String, String)],
    opts: &RepairOptions,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
) -> Vec<ObjectRepair> {
    let mut cache: HashMap<GeometryKey, (crate::mesh::Mesh, Value)> = HashMap::new();
    let mut out = Vec::with_capacity(blocks.len());

    for block in blocks {
        let key = GeometryKey::of(&block.mesh);
        let declared_unit = unit_for(units, &block.model_name);

        let (mesh, report) = if let Some((cached_mesh, cached_report)) = cache.get(&key) {
            // A report is a pure function of the geometry, so a cache hit still
            // gives this object its own copy.
            (cached_mesh.clone(), cached_report.clone())
        } else {
            let v: Vec<[f64; 3]> = block
                .mesh
                .verts
                .iter()
                .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
                .collect();
            let t: Vec<[i64; 3]> = block
                .mesh
                .tris
                .iter()
                .map(|f| [i64::from(f[0]), i64::from(f[1]), i64::from(f[2])])
                .collect();

            let (mesh, report) = match repair(&v, &t, opts, sink, cancel) {
                RepairOutcome::Ok {
                    verts,
                    tris,
                    report,
                } => {
                    let verts32: Vec<[f32; 3]> = verts
                        .iter()
                        .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
                        .collect();
                    let tris32: Vec<[i32; 3]> = tris
                        .iter()
                        .map(|f| [f[0] as i32, f[1] as i32, f[2] as i32])
                        .collect();
                    (crate::mesh::Mesh::new(verts32, tris32), report)
                }
                RepairOutcome::Unavailable { tier, reason } => {
                    let report = serde_json::json!({
                        "error": format!("unavailable tier '{tier}': {reason}"),
                    });
                    (block.mesh.clone(), report)
                }
                RepairOutcome::Error { message } => {
                    let report = serde_json::json!({ "error": message });
                    (block.mesh.clone(), report)
                }
            };
            cache.insert(key, (mesh.clone(), report.clone()));
            (mesh, report)
        };

        // `check_units` runs at the reporting layer, exactly as the single-mesh
        // CLI path does (`repair_cmd.rs` inserts `unit_warning`/`unit_hint`).
        // The declared unit comes from this object's own model part.
        let mut report = report;
        if let Some(unit) = crate::mesh_io::three_mf::check_units(&block.mesh, Some(&declared_unit))
        {
            if let Some(obj) = report.as_object_mut() {
                obj.insert("unit_warning".into(), Value::Bool(true));
                obj.insert("unit_hint".into(), Value::from(unit.unit_hint));
            }
        }

        out.push(ObjectRepair {
            model_name: block.model_name.clone(),
            block_index: block.index,
            declared_unit,
            report,
            mesh,
        });
    }

    out
}

/// Repair a container file end to end and rewrite it with `rewrite_3mf_meshes`.
///
/// `out_path` keeps the 3MF container regardless of its extension: Python's
/// `repair_3mf_objects` always writes the rewritten archive (measured: a
/// `.stl` output name still contained the original 3MF ZIP byte-for-byte).
pub fn repair_3mf_file(
    src: impl AsRef<std::path::Path>,
    out_path: impl AsRef<std::path::Path>,
    opts: &RepairOptions,
    sink: &dyn ProgressSink,
    cancel: &CancelToken,
) -> crate::error::MeshResult<Vec<ObjectRepair>> {
    let src = src.as_ref();
    let out_path = out_path.as_ref();
    let blocks = crate::mesh_io::three_mf::parse_3mf_mesh_blocks(src)?;
    if blocks.is_empty() {
        return Err(crate::error::MeshError::Malformed3mf(
            "no mesh objects found in 3MF".to_string(),
        ));
    }
    let units = read_3mf_units(src);
    let repairs = repair_objects(&blocks, &units, opts, sink, cancel);

    let updates: Vec<crate::mesh::Mesh> = repairs.iter().map(|r| r.mesh.clone()).collect();
    crate::mesh_io::three_mf::rewrite_3mf_meshes(src, out_path, &updates, None)?;
    Ok(repairs)
}

/// Aggregate the per-object counters the multi-object report carries.
///
/// Measured against Python (`service.repair_3mf_objects`): both
/// `objects_watertight` and `objects_stage2_ok` are **counts**, not booleans,
/// and `objects_watertight` requires three things per object -- two-manifold,
/// no holes left, and a successful stage-2 rebuild. Returning a bool here made
/// the report read `true` where Python read `3`.
pub fn aggregate_flags(repairs: &[ObjectRepair]) -> (usize, usize, usize) {
    let objects = repairs.len();
    let watertight = repairs
        .iter()
        .filter(|r| {
            let s1 = r.report.get("stage1");
            let two_manifold = s1
                .and_then(|s| s.get("two_manifold"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let holes = s1
                .and_then(|s| s.get("holes_remaining"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let stage2_ok = r
                .report
                .get("stage2")
                .and_then(|s| s.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            two_manifold && holes == 0 && stage2_ok
        })
        .count();
    let stage2_ok = repairs
        .iter()
        .filter(|r| {
            r.report
                .get("stage2")
                .and_then(|s| s.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    (objects, watertight, stage2_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str, idx: usize, tris: usize) -> MeshBlock {
        let mut verts = Vec::new();
        let mut faces = Vec::new();
        for f in 0..tris {
            let base = (f * 3) as i32;
            for k in 0..3 {
                let x = ((base + k) % 7) as f32;
                verts.push([x, 0.0, 0.0]);
            }
            faces.push([base, base + 1, base + 2]);
        }
        MeshBlock {
            model_name: name.to_string(),
            index: idx,
            span: (0, 0),
            open_tag: String::new(),
            close_tag: String::new(),
            prefix: String::new(),
            mesh: crate::mesh::Mesh::new(verts, faces),
            triangle_attrs: Vec::new(),
        }
    }

    #[test]
    fn geometry_key_is_the_geometry_not_the_name() {
        let a = block("3D/Objects/a.model", 0, 4);
        let b = block("3D/Objects/b.model", 0, 4);
        // Same triangles, different entry name: the key must match, because
        // Python keys the cache on `(verts.tobytes(), tris.tobytes())`.
        assert_eq!(
            GeometryKey::of(&a.mesh),
            GeometryKey::of(&b.mesh),
            "identical geometry under different names must share a cache key"
        );

        let c = block("3D/Objects/c.model", 0, 5);
        assert_ne!(
            GeometryKey::of(&a.mesh),
            GeometryKey::of(&c.mesh),
            "different geometry must not collide"
        );
    }

    #[test]
    fn unit_lookup_defaults_to_millimetres() {
        let units = vec![
            ("3D/Objects/object_1.model".to_string(), "inch".to_string()),
            (
                "3D/Objects/object_2.model".to_string(),
                "centimeter".to_string(),
            ),
        ];
        assert_eq!(unit_for(&units, "3D/Objects/object_1.model"), "inch");
        assert_eq!(unit_for(&units, "3D/Objects/object_2.model"), "centimeter");
        // Absent model parts fall back the way Python's `units.get(..., 
        // 'millimeter')` does.
        assert_eq!(unit_for(&units, "3D/Objects/missing.model"), "millimeter");
    }

    #[test]
    fn aggregate_flags_are_counts_over_every_object() {
        let ok = serde_json::json!({
            "stage1": {"two_manifold": true, "holes_remaining": 0},
            "stage2": {"ok": true},
        });
        // Two-manifold with holes still open is NOT counted as watertight,
        // even though stage 2 succeeded.
        let open = serde_json::json!({
            "stage1": {"two_manifold": true, "holes_remaining": 2},
            "stage2": {"ok": true},
        });
        let no_stage2 = serde_json::json!({
            "stage1": {"two_manifold": true, "holes_remaining": 0},
            "stage2": {"ok": false},
        });
        let make = |report: Value| ObjectRepair {
            model_name: "m".into(),
            block_index: 0,
            declared_unit: "millimeter".into(),
            report,
            mesh: crate::mesh::Mesh::default(),
        };

        // Python returns counts: 3 objects, 3 watertight, 3 stage-2 ok.
        let (n, w, s) = aggregate_flags(&[make(ok.clone()), make(ok.clone()), make(ok.clone())]);
        assert_eq!((n, w, s), (3, 3, 3));

        // An open object drops the watertight count but not the stage-2 count.
        let (n, w, s) = aggregate_flags(&[make(ok.clone()), make(open)]);
        assert_eq!((n, w, s), (2, 1, 2));

        // A failed stage-2 rebuild drops both.
        let (n, w, s) = aggregate_flags(&[make(ok), make(no_stage2)]);
        assert_eq!((n, w, s), (2, 1, 1));

        assert_eq!(aggregate_flags(&[]), (0, 0, 0));
    }
}
