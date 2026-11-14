# Pure Rust API audit (Phase 3, slice 0)

Required by the approved Stage 2 / P-WELD port. "core" means callable without
pyo3; the Python symbol surface stays unchanged.

| Need | Pure API | Status |
|---|---|---|
| vertex weld | `topology::weld_vertices_core` | added |
| STL reload weld | `topology::weld_reload_equivalent_core` | added |
| duplicate faces | `topology::remove_duplicate_faces_core` | exists |
| unreferenced vertices | `topology::remove_unreferenced_vertices_core` | exists (crate) |
| duplicate vertices | `topology::remove_duplicate_vertices_core` | exists |
| non-manifold edges | `topology::repair_non_manifold_edges_core` | exists |
| non-manifold vertices | `topology::repair_non_manifold_vertices_core` | exists |
| hole closing | `hole::close_holes_core` | exists |
| coherent orientation | `topology::re_orient_faces_coherently_core` | exists |
| topology measures | pyfunction `mesh_topology` only | **missing core** |
| Stage 2 rebuild | pyfunction `manifold::stage2_rebuild` only | **missing core** |
| defect holes/nm | `report::detect_defects` | exists |

Consequences:

- P-WELD needs `mesh_topology_core` (pinched-vertex count) and the weld cores
  above; the weld cores are now pure.
- Stage 2 needs `stage2_rebuild_core` (Manifold body + report struct). The
  pyfunction currently mixes geometry and PyDict assembly; extracting it is a
  separate, larger slice.
- No wheel symbol changes: `sutura_geom` still exports 62 Python symbols.
