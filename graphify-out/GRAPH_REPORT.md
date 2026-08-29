# Graph Report - sutura  (2026-08-30)

## Corpus Check
- 33 files · ~50,979 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 422 nodes · 728 edges · 31 communities (23 shown, 8 thin omitted)
- Extraction: 96% EXTRACTED · 4% INFERRED · 0% AMBIGUOUS · INFERRED: 27 edges (avg confidence: 0.85)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `7ece1f76`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- MainWindow
- defects.py
- updater.py
- heatmap.py
- classification.py
- MeshViewport
- classify_mesh
- repair.py
- generate_screenshots.py
- RepairWorker
- UpdateCheckWorker
- gui.py
- .__init__
- repair_mesh_from_arrays
- SuturaRepair
- dry_run_mesh_from_arrays
- dry_run_file
- main
- process_file
- AnalyzeWorker
- manifold_bridge.py
- validate_mesh_from_arrays
- UpdateWorker
- verify_before_after_dialog.py
- QThread
- install.sh
- install-macos.sh
- graphify.js
- build_appimage.sh
- open.sh
- uninstall.sh

## God Nodes (most connected - your core abstractions)
1. `MainWindow` - 58 edges
2. `_t()` - 37 edges
3. `MeshViewport` - 21 edges
4. `repair_mesh_from_arrays()` - 14 edges
5. `prepare_render()` - 11 edges
6. `repair_file()` - 11 edges
7. `main()` - 9 edges
8. `check_for_update()` - 9 edges
9. `perform_update()` - 9 edges
10. `_RenderThread` - 8 edges

## Surprising Connections (you probably didn't know these)
- `main()` --calls--> `classify_mesh()`  [INFERRED]
  scripts/calibrate_classifier.py → sutura/mesh_classifier.py
- `main()` --calls--> `render()`  [INFERRED]
  sutura/before_after_render.py → sutura/heatmap.py
- `main()` --calls--> `render()`  [INFERRED]
  sutura/heatmap_render.py → sutura/heatmap.py
- `repair_mesh_from_arrays()` --calls--> `classify_mesh()`  [INFERRED]
  sutura/repair.py → sutura/mesh_classifier.py
- `dry_run_mesh_from_arrays()` --calls--> `estimate_confidence_pre_repair()`  [INFERRED]
  sutura/repair.py → sutura/confidence.py

## Import Cycles
- None detected.

## Communities (31 total, 8 thin omitted)

### Community 0 - "MainWindow"
Cohesion: 0.06
Nodes (19): QDialog, QMainWindow, BeforeAfterWorker, MainWindow, Modal picker for the batch repair mode. A five-step horizontal slider (Low-…, Small up-arrow icon; teal when an update is available., Ask once (on first run, no config) whether to enable update checks. Skipped for…, Warn that the new version crosses the license boundary and open the releases… (+11 more)

### Community 1 - "defects.py"
Cohesion: 0.06
Nodes (49): main(), CLI entry for the on-demand before/after mesh comparison render. Runs as a…, _boundary_edges(), _boundary_loops(), detect(), detect_holes(), detect_non_manifold(), _edge_keys() (+41 more)

### Community 2 - "updater.py"
Cohesion: 0.09
Nodes (36): backup_install(), check_for_update(), _copy_python_files(), crosses_license_boundary(), download_source(), fetch_latest_release(), _file_hash(), health_check() (+28 more)

### Community 3 - "heatmap.py"
Cohesion: 0.11
Nodes (25): _defect_vertex_set(), _deviation_lut(), deviation_quantile_index(), draw_frame(), _face_normals(), _lighting_shade(), prepare_render(), _project() (+17 more)

### Community 4 - "classification.py"
Cohesion: 0.13
Nodes (18): classify(), is_stage2_skipped(), issue_label(), Batch repair summary - single source of truth for result classification.…, English human-readable label for an issue code (CLI --human)., True if the report says stage 2 was skipped (used for the summary)., Classify one repair result dict into (category, issues, summary_key). category…, Dynamic argument(s) for formatting the summary_key (e.g. hole count). Returns a… (+10 more)

### Community 5 - "MeshViewport"
Cohesion: 0.09
Nodes (10): QLabel, QWidget, _ClickableLabel, MeshViewport, Play-triangle icon, drawn dark so it is visible on the teal Repair button (same…, Magnifying-glass icon in the teal accent, drawn with QPainter., Modal dialog: the static before/after pair (main image + detail close-up,…, QLabel that emits ``clicked`` on a left mouse press. (+2 more)

### Community 6 - "classify_mesh"
Cohesion: 0.19
Nodes (13): _conf_summary(), main(), Mirror of the repair.py confidence gate: whether the tuned Stage 1 thresholds…, tuning_applied(), _class_scores(), classify_mesh(), _dihedral_stats(), Mesh type classification (organic vs mechanical) for repair tuning. stdlib +… (+5 more)

### Community 7 - "repair.py"
Cohesion: 0.22
Nodes (10): Sutura - two-stage STL/3MF mesh repair for Linux., obj_has_material_refs(), Run stage 2. Returns (report, ok); reports a skip, never silence., True when an OBJ file references materials/textures (mtllib/usemtl). The repair…, Repair a single STL/OBJ/3MF file. Returns the report dict., read_obj(), repair_file(), run_stage2() (+2 more)

### Community 8 - "generate_screenshots.py"
Cohesion: 0.29
Nodes (10): load_gui(), main(), make_meshes(), Drive the pre-repair Analyze flow and grab the window once it is done. The…, Drive the before/after dialog and grab it (static view). Repairs the batch,…, Return a path to an executable that runs the repo's own repair.py with the…, _repo_cli_wrapper(), run() (+2 more)

### Community 9 - "RepairWorker"
Cohesion: 0.20
Nodes (6): format_report(), parse_cli_output(), Short per-file result label, using the shared classifier, localized., Repairs files sequentially in a background thread., RepairWorker, summarize()

### Community 10 - "UpdateCheckWorker"
Cohesion: 0.33
Nodes (3): Start a background check if enabled and due. Skipped for AppImage builds: self-…, Background check for a newer release. Emits found((status, tag))., UpdateCheckWorker

### Community 11 - "gui.py"
Cohesion: 0.16
Nodes (11): sutura script, apply_dark_theme(), _dark_palette(), _find_sutura_cmd(), main(), mode_suggestion_keys(), _orbit_basis(), Priority-ordered mode-suggestion keys for an analysis dict `a` (AnalyzeWorker's… (+3 more)

### Community 13 - "repair_mesh_from_arrays"
Cohesion: 0.27
Nodes (10): apply_chain(), delete_fallback_chain(), extreme_extra_passes(), ExtremeRemovedAllError, Extreme-mode extra Stage 1 passes (Stage C). After the main chain has run,…, Repair one mesh given as numpy arrays. Returns (report, verts, tris). ``mode``…, Raised when a repair leaves the mesh with ZERO faces because extreme mode's…, repair_mesh_from_arrays() (+2 more)

### Community 14 - "SuturaRepair"
Cohesion: 0.22
Nodes (4): Run Sutura in a background thread and report via the host UI. Runs entirely off…, SuturaRepair, SuturaRepairPlugin, plugin

### Community 15 - "dry_run_mesh_from_arrays"
Cohesion: 0.25
Nodes (8): boundary_loop_stats(), dry_run_mesh_from_arrays(), Resolve the Stage 1 thresholds for a repair/dry-run run. Single source of truth…, (n_loops, max_loop_len) of the mesh boundary: walks the boundary-edge graph…, Whether the tuned Stage 1 thresholds should be used for a classified mesh,…, Report what a repair WOULD do for one mesh, without doing it. Detects the type,…, resolve_mode_params(), tuning_applied_for()

### Community 16 - "dry_run_file"
Cohesion: 0.29
Nodes (8): dry_run_file(), load_meshes(), Return a description of NaN/Inf coordinates in STL/OBJ files, or None., Load a mesh file as a list of (model_name|None, verts, tris). STL/OBJ give one…, Validate a mesh file without repairing it. Returns the report dict; a hard…, Dry-run one mesh file. Returns the report dict; a hard error is a dict with an…, scan_bad_coordinates(), validate_file()

### Community 17 - "main"
Cohesion: 0.25
Nodes (8): human_defects(), human_dry_run(), human_report(), human_validate(), main(), Render a validate report for --human mode., Render a --dry-run report for --human mode., Render the defect list for --human mode (only with --defects).

### Community 18 - "process_file"
Cohesion: 0.33
Nodes (7): build_mesh_block(), parse_3mf_meshes(), process_file(), Repair one file. Returns (result_dict, category)., Return {model_name: [(verts, tris, (start,end))]} for every <mesh> block., Repair every object mesh in a 3MF archive, preserving structure., repair_3mf()

### Community 20 - "manifold_bridge.py"
Cohesion: 0.47
Nodes (4): main(), Rebuild the closed OBJ at src into a manifold solid at dst; returns the report…, run_bridge(), write_obj()

### Community 21 - "validate_mesh_from_arrays"
Cohesion: 0.33
Nodes (6): Total triangle surface area of a mesh, computed directly from geometry. Works…, Signed volume of a triangle mesh (sum of origin-tetrahedra volumes). The sign…, Analyze one mesh WITHOUT repairing it. Combines defects.detect(), the mesh…, signed_volume(), surface_area(), validate_mesh_from_arrays()

### Community 23 - "verify_before_after_dialog.py"
Cohesion: 0.83
Nodes (3): load_gui(), main(), _repo_cli_wrapper()

### Community 25 - "QThread"
Cohesion: 0.25
Nodes (5): QThread, HeatmapWorker, Renders a mesh heatmap off the GUI thread and off the GUI process. The render…, Builds the interactive-viewer dataset (.npz) in a subprocess. Same isolation…, ViewerDataWorker

### Community 28 - "graphify.js"
Cohesion: 0.60
Nodes (4): firstSegment(), GraphifyPlugin(), isFileDiscovery(), IMPORTANT: keep the reminder string free of backticks and $(...) constructs.

## Knowledge Gaps
- **2 isolated node(s):** `open.sh script`, `uninstall.sh script`
  These have ≤1 connection - possible missing edges or undocumented components.
- **8 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `MainWindow` connect `MainWindow` to `MeshViewport`, `RepairWorker`, `UpdateCheckWorker`, `gui.py`, `.__init__`?**
  _High betweenness centrality (0.180) - this node is a cross-community bridge._
- **Why does `MeshViewport` connect `MeshViewport` to `gui.py`, `.__init__`?**
  _High betweenness centrality (0.078) - this node is a cross-community bridge._
- **Why does `_t()` connect `MainWindow` to `MeshViewport`, `RepairWorker`, `gui.py`, `.__init__`, `QThread`?**
  _High betweenness centrality (0.074) - this node is a cross-community bridge._
- **What connects `open.sh script`, `uninstall.sh script` to the rest of the system?**
  _2 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `MainWindow` be split into smaller, more focused modules?**
  _Cohesion score 0.06169772256728778 - nodes in this community are weakly interconnected._
- **Should `defects.py` be split into smaller, more focused modules?**
  _Cohesion score 0.058001397624039136 - nodes in this community are weakly interconnected._
- **Should `updater.py` be split into smaller, more focused modules?**
  _Cohesion score 0.08961593172119488 - nodes in this community are weakly interconnected._