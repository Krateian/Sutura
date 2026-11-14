//! Measurement and reporting primitives (Python-free).

pub mod classify;
pub mod debris;
pub mod defects;
pub mod score;

pub use classify::{
    classify_json, classify_value, estimate_confidence_json, repair_confidence_json,
    repair_confidence_value, summary_args_json, summary_args_value, Confidence,
};
pub use debris::{
    possible_debris, removed_small_parts, PossibleDebris, PossibleDebrisPart, RemovedSmallParts,
    SmallPart,
};
pub use defects::{detect_defects, DefectReport, Hole, NonManifoldRegion};
pub use score::{compute_scores_json, compute_scores_value};

pub(crate) fn round_to(value: f64, decimals: u32) -> f64 {
    let scale = 10f64.powi(decimals as i32);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> (Vec<[f32; 3]>, Vec<[i32; 3]>) {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let t = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        (v, t)
    }

    #[test]
    fn detects_one_hole_with_four_rim_vertices() {
        let (v, mut t) = cube();
        t.pop();
        t.pop();
        let r = detect_defects(&v, &t, true);
        assert_eq!(r.holes.len(), 1);
        assert_eq!(r.holes[0].vertices, 4);
        assert_eq!(r.holes[0].verts_idx.as_ref().unwrap().len(), 4);
        assert!(r.non_manifold.is_empty());
    }

    #[test]
    fn detects_a_non_manifold_region() {
        let v = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, -1.0, 0.0],
        ];
        let t = vec![[0, 1, 2], [1, 0, 3], [0, 1, 4]];
        let r = detect_defects(&v, &t, false);
        assert_eq!(r.non_manifold.len(), 1);
        assert_eq!(r.non_manifold[0].faces, 3);
    }

    #[test]
    fn reports_a_small_component() {
        let (v, mut t) = cube();
        let n = v.len() as i32;
        let mut v2 = v.clone();
        v2.extend([[3.0, 0.0, 0.0], [4.0, 0.0, 0.0], [3.0, 1.0, 0.0]]);
        t.push([n, n + 1, n + 2]);
        let v2f: Vec<[f64; 3]> = v2
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let r = removed_small_parts(&v2f, &t, 8).unwrap();
        assert_eq!(r.count, 1);
        assert_eq!(r.parts[0].faces, 1);
    }
}
