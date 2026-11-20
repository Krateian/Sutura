// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! Post-analysis suggestions: "what should I try next".
//!
//! Ported from `sutura/gui.py:3639` (`mode_suggestion_keys`). The rules are the
//! same, including the cap of three and the extreme caveat, because a suggestion
//! is advice and advice that drifts from the CLI's behaviour is worse than none.
//!
//! A suggestion never changes anything by itself: each one names a mode, and the
//! user applies it. That is what `Apply` does -- it sets the mode, nothing else.

use sutura_core::analysis::Analysis;

/// One suggestion: the mode it recommends, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// `quick` / `balanced` / `thorough` / `extreme`, or `None` for advice that
    /// is not a mode change (the extreme caveat).
    pub mode: Option<&'static str>,
    /// The i18n key for the reason.
    pub reason_key: &'static str,
}

/// The mode a suggestion would apply, for the Apply button's label.
impl Suggestion {
    pub fn is_actionable(&self) -> bool {
        self.mode.is_some()
    }
}

/// The suggestion rules, in priority order, capped at three.
///
/// Mirrors `mode_suggestion_keys` rule for rule. The thresholds are the ones the
/// Python GUI used, and they are deliberately conservative: a suggestion that
/// fires on every mesh is noise.
pub fn suggestions(a: &Analysis) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let mut extreme_fired = false;

    let si = a.self_intersections;
    let holes = a.boundary_loops;

    if si > 10 {
        out.push(Suggestion {
            mode: Some("extreme"),
            reason_key: "app.sug_si_many",
        });
        extreme_fired = true;
    } else if si >= 1 {
        out.push(Suggestion {
            mode: Some("thorough"),
            reason_key: "app.sug_si_few",
        });
        extreme_fired = true;
    }
    if holes > 10 {
        out.push(Suggestion {
            mode: Some("aggressive"),
            reason_key: "app.sug_holes_many",
        });
        extreme_fired = true;
    } else if holes >= 1 {
        out.push(Suggestion {
            mode: Some("medium"),
            reason_key: "app.sug_holes_few",
        });
    }
    if a.non_manifold_edges > 3 {
        out.push(Suggestion {
            mode: Some("thorough"),
            reason_key: "app.sug_nm",
        });
    }
    if a.components > 1 {
        out.push(Suggestion {
            mode: Some("medium"),
            reason_key: "app.sug_cc",
        });
    }
    if a.mesh_type == "unknown" {
        out.push(Suggestion {
            mode: Some("balanced"),
            reason_key: "app.sug_unknown",
        });
    }
    if a.boundary_loops == 0 && a.non_manifold_edges == 0 && a.self_intersections == 0 {
        out.push(Suggestion {
            mode: Some("quick"),
            reason_key: "app.sug_watertight",
        });
    }
    if (a.type_confidence * 100.0).round() < 40.0 {
        out.push(Suggestion {
            mode: Some("thorough"),
            reason_key: "app.sug_low_conf",
        });
    }

    out.truncate(3);
    // Extreme can delete a whole object below 20 faces; say so when that is the
    // advice. Informational, so it is appended rather than counted in the three.
    if extreme_fired && a.faces < 20 {
        out.push(Suggestion {
            mode: None,
            reason_key: "app.sug_extreme_caveat",
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(faces: usize, loops: usize, nm: usize, si: usize, comps: usize) -> Analysis {
        Analysis {
            mesh_type: "mechanical".to_string(),
            type_confidence: 0.9,
            faces,
            vertices: faces,
            components: comps,
            boundary_loops: loops,
            largest_loop_len: loops,
            largest_loop_ratio: 0.0,
            non_manifold_edges: nm,
            self_intersections: si,
            open_area_ratio: 0.0,
            repetition_score: 0.0,
            single_side_score: 0.0,
            relief_score: 0.0,
            bbox_diagonal: 0.0,
            surface_area: 0.0,
            model: None,
            classifier_engine: "experimental".to_string(),
            possible_debris: None,
            diagnosis_v2: None,
        }
    }

    #[test]
    fn a_watertight_mesh_is_told_low_mode_is_enough() {
        let s = suggestions(&analysis(5000, 0, 0, 0, 1));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].mode, Some("quick"));
        assert_eq!(s[0].reason_key, "app.sug_watertight");
    }

    #[test]
    fn many_self_intersections_recommend_extreme() {
        let s = suggestions(&analysis(5000, 0, 0, 25, 1));
        assert_eq!(s[0].mode, Some("extreme"));
        assert_eq!(s[0].reason_key, "app.sug_si_many");
    }

    #[test]
    fn the_list_is_capped_at_three_plus_the_caveat() {
        // Everything fires at once: two problems plus components plus unknown.
        let mut a = analysis(10, 40, 9, 40, 4);
        a.mesh_type = "unknown".to_string();
        let s = suggestions(&a);
        assert_eq!(
            s.iter().filter(|x| x.is_actionable()).count(),
            3,
            "at most three actionable suggestions"
        );
        // The mesh has 10 faces, so the extreme caveat applies and is last.
        assert_eq!(s.last().unwrap().mode, None);
        assert_eq!(s.last().unwrap().reason_key, "app.sug_extreme_caveat");
    }

    #[test]
    fn the_caveat_needs_a_small_mesh() {
        // Same advice, but 5000 faces: no caveat.
        let s = suggestions(&analysis(5000, 0, 0, 25, 1));
        assert!(s.iter().all(|x| x.mode.is_some()));
    }

    #[test]
    fn low_confidence_is_its_own_reason() {
        let mut a = analysis(5000, 0, 0, 0, 1);
        a.type_confidence = 0.1;
        let s = suggestions(&a);
        assert!(
            s.iter().any(|x| x.reason_key == "app.sug_low_conf"),
            "a low type confidence must produce its own suggestion"
        );
    }
}
