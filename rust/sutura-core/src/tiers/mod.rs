//! Repair-tier orchestration ported from the Python tier modules.
//!
//! The geometry kernels stay in `sutura-geom`; these modules own the
//! acceptance gates, budgets and report fields.  The Python tier files keep
//! the same public entry points and delegate here when `SUTURA_TIER_ENGINE`
//! (or a per-tier override) is `rust`.

pub mod cdt;
pub mod closing;
pub mod coaptation;
pub mod common;
pub mod flap;
pub mod graft;
pub mod splint;
pub mod tamponade;

#[cfg(test)]
mod tests {
    use super::common;

    #[test]
    fn component_labels_are_ordered() {
        let tris = [[0, 1, 2], [3, 4, 5], [2, 3, 6]];
        let (n, labels) = common::component_labels(7, &tris);
        assert_eq!(n, 1);
        assert!(labels.iter().all(|l| *l == 0));
        let (n, labels) = common::component_labels(9, &tris);
        assert_eq!(n, 3);
        assert_eq!(labels[0], 0);
        assert_eq!(labels[7], 1);
        assert_eq!(labels[8], 2);
    }

    #[test]
    fn canonical_face_keys_ignore_winding() {
        let v = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let a = common::canonical_face_keys(&v, &[[0, 1, 2]]);
        let b = common::canonical_face_keys(&v, &[[2, 0, 1]]);
        assert_eq!(a, b);
    }

    #[test]
    fn median_edge_length_of_a_unit_right_triangle() {
        let v = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let m = common::median_edge_length(&v, &[[0, 1, 2]]);
        assert!((m - 1.0).abs() < 1e-12);
    }

    #[test]
    fn empty_tiers_are_noops() {
        let v: Vec<[f64; 3]> = Vec::new();
        let t: Vec<[i64; 3]> = Vec::new();
        let params = serde_json::json!({});
        let (ov, ot, rec) = super::tamponade::run(&v, &t, &params);
        assert!(ov.is_empty() && ot.is_empty());
        assert_eq!(rec.get("adopted").and_then(|x| x.as_bool()), Some(false));
        let (ov, ot, rec) = super::flap::run(&v, &t, &params);
        assert!(ov.is_empty() && ot.is_empty());
        assert_eq!(rec.get("adopted").and_then(|x| x.as_bool()), Some(false));
    }
}
