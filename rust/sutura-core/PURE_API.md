# Pure Rust API audit (Phase 3 slice 0 + Phase 4 slice 0)

Required by the approved Stage 2 / P-WELD and Tamponade / Flap ports. "core"
means callable without pyo3; the Python symbol surface stays unchanged.

| Need | Pure API | Status |
|---|---|---|
| vertex weld | `topology::weld_vertices_core` | added (P3) |
| STL reload weld | `topology::weld_reload_equivalent_core` | added (P3) |
| duplicate faces | `topology::remove_duplicate_faces_core` | exists |
| unreferenced vertices | `topology::remove_unreferenced_vertices_core` | exists (crate) |
| duplicate vertices | `topology::remove_duplicate_vertices_core` | exists |
| non-manifold edges | `topology::repair_non_manifold_edges_core` | exists |
| non-manifold vertices | `topology::repair_non_manifold_vertices_core` | exists |
| hole closing | `hole::close_holes_core` | exists |
| coherent orientation | `topology::re_orient_faces_coherently_core` | exists |
| topology measures | pyfunction `mesh_topology` only | **missing core**; the tier `is_two_manifold` helper covers Splint |
| Stage 2 rebuild | `manifold::stage2_rebuild_core` | **added** (P4 Splint; pyfunction is a thin wrapper) |
| defect holes/nm | `report::detect_defects` | exists |
| exact SI scan | `self_intersections_core` | **made pub** (P4) |
| SI subset scan | `self_intersections_subset_core` | **made pub** (P4) |
| geometric measures | `topology::mesh_geometric_measures_core` | exists |
| thin-wall diagnosis | `diagnosis::diagnose_core` | exists |
| interior depth | `melt::interior_depth_norm` | exists |
| winding / closest point | `winding::MeshBvh` | exists |
| antipodal winding | `antipodal::AntipodalGwn` | exists |
| Flap fill | `flap::flap_fill` | exists |
| Tamponade plug | `tamponade::tamponade_repair` | exists |
| duplicate faces | `topology::remove_duplicate_faces_core` | **added** (P4 Coaptation/Graft) |
| Manifold union | `manifold::union_pair_core` | **added** (P4 Coaptation) |
| Manifold verdict | `manifold::watertight_check_core` | **added** (P4 Coaptation) |
| Splint cut | `splint_cut_core_opts` | **made pub** (P4) |
| Graft core | `morph_close`, `resample_uniform`, `union_pair` | pyfunction only; needs a pure surface before the Graft slice |

Consequences:

- Tamponade, Flap, Coaptation and the Graft admission gate no longer need
  exact-SI, subset or Manifold wrappers from pyo3; the pure cores are called
  from `sutura-core/src/tiers/`.
- The `manifold` module is now compiled without the `python` feature (the
  pyo3 helpers inside it are individually gated) so `sutura-core` can use the
  Manifold boolean and watertight cores.
- `P-WELD` still needs `mesh_topology_core` for the pinched-vertex count; the
  weld/core subset used by the tiers is covered.
- Stage 2 still needs `stage2_rebuild_core` for a fully Python-free core.
- No `sutura_geom` wheel symbol changes: it still exports 62 Python symbols.
  The new tier entry points live in the `sutura_core_py` bridge
  (`tier_tamponade`, `tier_flap`, `tier_flap_debris`,
  `tier_flap_final`, `tier_flap_off_preferred`, `tier_flap_si_rose`,
  `tier_tamponade_final`, `tier_tamponade_off_preferred`, `tier_coaptation`,
  `tier_graft_decide`, `tier_cdt`, `tier_splint`).
- `cdt3d_solid_core`, `splint_cut_core_opts`, `stage2_rebuild_core` and
  `is_two_manifold` are pure Rust APIs; `local_remesh` remains Python and only
  its wall-clock gate was replaced with a deterministic work budget.
