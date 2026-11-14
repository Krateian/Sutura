//! Python-free Sutura core.
//!
//! Phase 1 starts with the mesh boundary types and errors. The I/O readers,
//! pipeline, policy and reporting modules are added in later slices; none of
//! them may depend on pyo3 or numpy.

pub mod error;
pub mod fmt;
pub mod mesh;
pub mod mesh_io;
pub mod p_weld;
pub mod stage2;
pub mod report;
pub mod tiers;

pub use error::{MeshError, MeshResult};
pub use mesh::{Mesh, MeshBlock, SourceFormat};
pub use mesh_io::{
    check_units, load_mesh, load_stl, match_triangle_attributes, obj_has_material_refs,
    parse_3mf_mesh_blocks, read_3mf_units, read_mesh, read_obj, read_ply, read_stl,
    rewrite_3mf_meshes, save_mesh, scan_bad_coordinates, scan_obj_bad_coordinates,
    scan_stl_bad_coordinates, write_3mf_single, write_obj, write_ply, write_stl_binary,
    TriangleAttrStats, UnitWarning,
};
pub use stage2::{should_run_stage2, split_pinched_vertices, PinchResult};
pub use p_weld::{enforce_reload_verdict, p_weld_final, separate_weld_collisions, reload_strict_holes_nm, reload_topology, weld_reload_equivalent, PWeldResult};
pub use report::{
    aggregate_print_reports_value, build_health_report_json, build_print_report_json,
    build_print_report_value, build_record_json, classify_json, classify_value,
    compute_fidelity_json, compute_fidelity_value, compute_scores_json, compute_scores_value,
    detect_defects, estimate_confidence_json, human_line_json, possible_debris,
    removed_small_parts, repair_confidence_json, repair_confidence_value, summary_args_json,
    summary_args_value, summary_text_json, Confidence, DefectReport, Hole, NonManifoldRegion,
    PossibleDebris, PossibleDebrisPart, RemovedSmallParts, SmallPart,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_of_unit_triangle() {
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, -1.0]],
            vec![[0, 1, 2]],
        );
        assert_eq!(m.bbox(), Some(([0.0, 0.0, -1.0], [1.0, 2.0, 0.0])));
    }

    #[test]
    fn empty_mesh_has_no_bbox() {
        assert_eq!(Mesh::default().bbox(), None);
        assert!(Mesh::default().is_empty());
    }

    #[test]
    fn validate_rejects_out_of_range_index() {
        let m = Mesh::new(vec![[0.0, 0.0, 0.0]], vec![[0, 1, 2]]);
        assert!(matches!(
            m.validate(),
            Err(MeshError::IndexOutOfRange { triangle: 0, .. })
        ));
    }

    #[test]
    fn validate_accepts_a_normal_triangle() {
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        );
        assert!(m.validate().is_ok());
    }

    #[test]
    fn format_is_detected_case_insensitively() {
        assert_eq!(
            SourceFormat::from_extension("part.STL").unwrap(),
            SourceFormat::Stl
        );
        assert_eq!(
            SourceFormat::from_extension("a.3mf").unwrap(),
            SourceFormat::ThreeMf
        );
        assert!(SourceFormat::from_extension("a.txt").is_err());
    }
}
