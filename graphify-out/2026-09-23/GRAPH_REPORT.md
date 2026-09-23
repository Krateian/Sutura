# Graph Report - Sutura  (2026-09-23)

## Corpus Check
- 60 files · ~110,354 words
- Verdict: corpus is large enough that graph structure adds value.
- Unclassified: 9 file(s) not represented in the graph (top: (none) 4, .desktop 2, .icns 1)

## Summary
- 1077 nodes · 1824 edges · 83 communities (73 shown, 10 thin omitted)
- Extraction: 87% EXTRACTED · 13% INFERRED · 0% AMBIGUOUS · INFERRED: 242 edges (avg confidence: 0.93)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `5b88ef1f`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

## Community Hubs (Navigation)
- MainWindow
- main
- updater.py
- heatmap.py
- How it runs
- MeshViewport
- mesh_classifier.py
- repair_3mf
- gui.py
- _t
- .__init__
- AGENTS.md
- autorefine.py
- repair_mesh_from_arrays
- .execute
- Kullanım
- alpha_wrap_prototype.py
- repair.py
- lib.rs
- mesh_classifier_v2.py
- manifold_bridge.py
- history.py
- ._current_path
- Changelog
- repair_score.py
- numpy
- install.sh
- install-macos.sh
- graphify.js
- build_appimage.sh
- open.sh
- uninstall.sh
- pre-push-security-check.sh
- sutura_repair_linux_x86_64.py
- Sutura
- Sutura
- defect_injector.py
- train_classifier_v2.py
- defects.py
- detect
- classify
- Tests (no framework — plain scripts, need the venvs installed)
- Sutura × OrcaSlicer plugin
- benchmark_repair_corpus.py
- ._build_ui
- Fixed
- Security audit (pre-v0.3) — 2026-09-20
- ._worker
- RepairModeDialog
- ._open_before_after
- tuning_applied
- Repair benchmark — strict watertight measurement (115-mesh corpus)
- v0.3.0 readiness assessment
- Mesh type-aware repair
- ftetwild_bridge.py
- calibrate_classifier.py
- fTetWild Fallback Tier — 115-mesh Real-World Corpus Benchmark
- bug_report.md
- Mesh türüne duyarlı onarım
- [0.2.1] - 2026-08-30
- [0.2.3] - 2026-09-19
- Contributing to Sutura
- Feasibility spike: from-scratch autorefine + alpha-wrapping for Sutura's heavy-self-intersection weak spot
- CLI / GUI feature parity notes
- [0.1.0] - unreleased
- Added
- [0.3.0] - 2026-09-20
- sutura-geom
- Security Policy
- ._show_analysis
- _risk
- [0.1.1] - unreleased
- [0.1.4] - 2026-08-19
- [0.1.9] - 2026-08-25
- OrcaSlicer plugin (orcaslicer-plugin/) — version history
- [0.2.5] - 2026-09-19
- Mesh corpus attribution & licenses
- SuturaRepairPlugin
- _write_temp_stl
- macos-quick-action.sh
- fetch_benchmark_corpus.sh
- sutura-geom
- sutura-geom

## God Nodes (most connected - your core abstractions)
1. `MainWindow` - 62 edges
2. `How it runs` - 53 edges
3. `_t()` - 45 edges
4. `detect()` - 27 edges
5. `repair_mesh_from_arrays()` - 25 edges
6. `Changelog` - 23 edges
7. `MeshViewport` - 22 edges
8. `repair_3mf()` - 22 edges
9. `Sutura` - 19 edges
10. `Sutura` - 19 edges

## Surprising Connections (you probably didn't know these)
- `3. Regression / benchmark targets` --references--> `run_one()`  [INFERRED]
  docs/alpha-wrap-feasibility-2026-09.md → scripts/benchmark_repair_corpus.py
- `Added` --references--> `detect()`  [INFERRED]
  CHANGELOG.md → sutura/defects.py
- `Scope` --references--> `detect()`  [INFERRED]
  docs/repair-benchmark-strict-watertight-2026-09.md → sutura/defects.py
- `Defect detail panel` --references--> `detect()`  [INFERRED]
  README.md → sutura/defects.py
- `Kusur detay paneli` --references--> `detect()`  [INFERRED]
  README.tr.md → sutura/defects.py

## Import Cycles
- None detected.

## Communities (83 total, 10 thin omitted)

### Community 0 - "MainWindow"
Cohesion: 0.21
Nodes (3): QMainWindow, MainWindow, Batch-wide repair profile selection (None = auto/classifier).

### Community 1 - "main"
Cohesion: 0.12
Nodes (19): main(), render(), Render a mesh heatmap to a QImage. Equivalent to…, load_mesh(), main(), Load an input mesh as plain (verts, tris) numpy arrays. STL/OBJ load via…, defect_diagonal(), initial_view_camera() (+11 more)

### Community 2 - "updater.py"
Cohesion: 0.07
Nodes (41): importlib_util, shutil, backup_install(), check_for_update(), _copy_python_files(), crosses_license_boundary(), download_source(), fetch_latest_release() (+33 more)

### Community 3 - "heatmap.py"
Cohesion: 0.14
Nodes (19): _defect_vertex_set(), _deviation_lut(), deviation_quantile_index(), _face_normals(), _lighting_shade(), prepare_render(), Offscreen defect heatmap renderer (numpy + Qt raster paint engine). Projects…, Unit face normals via vectorized cross products (degenerate -> +z). (+11 more)

### Community 4 - "How it runs"
Cohesion: 0.10
Nodes (30): How it runs, _clamp(), estimate_confidence_pre_repair(), _label(), Repair confidence scoring for Sutura reports. stdlib-only by design (same rule…, Pre-repair confidence estimate for validate / --dry-run. Uses ONLY signals…, Post-repair confidence score for a full repair report dict. Anchored on the…, repair_confidence() (+22 more)

### Community 5 - "MeshViewport"
Cohesion: 0.07
Nodes (16): QLabel, QWidget, _ClickableLabel, MeshViewport, prep(), _orbit_basis(), QLabel that emits ``clicked`` on a left mouse press., Rotate a 3x3 camera basis (rows = right/up/forward) by yaw (around world +Y)… (+8 more)

### Community 6 - "mesh_classifier.py"
Cohesion: 0.27
Nodes (9): _class_scores(), classify_mesh(), _dihedral_stats(), Mesh type classification (organic vs mechanical) for repair tuning. stdlib +…, Smooth class memberships (mechanical, organic) in [0,1] from the three…, Return {'type', 'confidence', 'metrics'}. type: 'mechanical' | 'organic' |…, Stable logistic sigmoid., Compute (near90_pct, flat_pct, gentle_pct) from face normals' dihedrals. The… (+1 more)

### Community 7 - "repair_3mf"
Cohesion: 0.14
Nodes (27): 3. 3MF XML (XXE) + ZIP (zip-slip), B.1 Current architecture (facts from the code), B.2 Feasibility: MEDIUM, B.3 What is already preserved (verified, no work needed), B.4 Technical blockers / gaps (ordered by severity), B.5 Risks, B.6 Estimated complexity, B.7 Step-by-step implementation plan (+19 more)

### Community 8 - "gui.py"
Cohesion: 0.05
Nodes (47): sutura script, math, pyside6_qtcore, pyside6_qtgui, pyside6_qtwidgets, load_gui(), main(), make_meshes() (+39 more)

### Community 9 - "_t"
Cohesion: 0.19
Nodes (5): Start a repair batch on ``files`` (optionally forced past the repair budget).…, Run the read-only pre-repair analysis (validate + --dry-run) on every file.…, Offer to re-run the budget-declined files with --force. The CLI already…, Build the batch summary strip: counts + clickable issue detail., _t()

### Community 10 - ".__init__"
Cohesion: 0.08
Nodes (15): QThread, BeforeAfterWorker, HeatmapWorker, Builds the interactive-viewer dataset (.npz) in a subprocess. Same isolation…, Ask once (on first run, no config) about update checks and the anonymous usage…, Start a background check if enabled and due. Skipped for AppImage builds: self-…, Command for a Python entry-point script. Inside a PyInstaller bundle this is…, Background check for a newer release. Emits found((status, tag)). (+7 more)

### Community 11 - "AGENTS.md"
Cohesion: 0.22
Nodes (7): Backlog (v0.3 notes — diagnosed, NOT fixed yet), Cleanup discipline, Process discipline, README upkeep, Release checklist, mode_suggestion_keys(), Priority-ordered mode-suggestion keys for an analysis dict `a` (AnalyzeWorker's…

### Community 12 - "autorefine.py"
Cohesion: 0.06
Nodes (42): 5. Licensing red flags, pyrobust_predicates, _aabbs(), autorefine(), broad_phase_candidates(), _clip_segment_2d(), detect_pairs_with_segments(), _eliminate_degenerate() (+34 more)

### Community 13 - "repair_mesh_from_arrays"
Cohesion: 0.13
Nodes (21): Conventions, Fixed, 1. Where self-intersections are handled today (`repair.py`), A.1 The bug, A.2 Exact filter that zeroes the faces (per-step trace), A.3 Root cause (verified experimentally, NOT fixed), A.4 Build-config comparison (conda-forge vs PyPI), A.5 Implication for Stage 2 (why this matters) (+13 more)

### Community 14 - ".execute"
Cohesion: 0.18
Nodes (9): [0.1.6] - 2026-08-20, Added, Added, CI, Dependencies, Security, How it works, Show the outcome recorded by a finished _worker via the host UI. (+1 more)

### Community 15 - "Kullanım"
Cohesion: 0.14
Nodes (16): Defect detail panel, OrcaSlicer plugin (experimental), Pre-repair analysis, Kullanım, Kusur detay paneli, Onarım öncesi analiz, OrcaSlicer eklentisi (deneysel), Usage (+8 more)

### Community 16 - "alpha_wrap_prototype.py"
Cohesion: 0.07
Nodes (25): heapq, scipy_spatial, _cell_intersects_input(), _cell_key(), _circumradius3(), _detect_two_sided(), DistanceOracle, _project_to_offset() (+17 more)

### Community 17 - "repair.py"
Cohesion: 0.07
Nodes (36): collections, re, issue_label(), Batch repair summary - single source of truth for result classification.…, Dynamic argument(s) for formatting the summary_key (e.g. hole count). Returns a…, English human-readable label for an issue code (CLI --human)., summary_args(), Sutura - two-stage STL/3MF mesh repair for Linux. (+28 more)

### Community 18 - "lib.rs"
Cohesion: 0.10
Nodes (20): AllowTypeChange, Bound, prelude, PyArrayLike1, PyModule, PyResult, pyvalueerror, robust (+12 more)

### Community 19 - "mesh_classifier_v2.py"
Cohesion: 0.11
Nodes (25): _class_scores(), _classic_decision(), _classic_verdict(), classify_mesh(), _curvature_developable_fraction(), _dihedral_stats(), _edge_extra_features(), _face_areas() (+17 more)

### Community 20 - "manifold_bridge.py"
Cohesion: 0.22
Nodes (9): manifold3d, main(), Stage 2 bridge: rebuild a closed mesh as a valid manifold3d solid. Runs under…, Rebuild the closed OBJ at src into a manifold solid at dst; returns the report…, Independent manifold3d watertight/manifold verdict for a triangle mesh. Cross-…, run_bridge(), watertight_check(), write_obj() (+1 more)

### Community 21 - "history.py"
Cohesion: 0.12
Nodes (21): datetime, hashlib, append_record(), build_record(), export_history(), load_records(), mesh_fingerprint(), _mesh_format() (+13 more)

### Community 22 - "._current_path"
Cohesion: 0.14
Nodes (4): Render the selected file's input defects into the defect panel., Fill the 'what changed' repair-log panel from a repair result dict., Enable/disable the heatmap button and show any cached thumbnail., Enable the button only when the selected file has a repaired output.

### Community 23 - "Changelog"
Cohesion: 0.10
Nodes (20): [0.1.2] - 2026-08-18, [0.1.3] - 2026-08-19, [0.1.5] - 2026-08-19, [0.2.2] - 2026-09-18, [0.2.6] - 2026-09-19, [0.2.7] - 2026-09-19, [0.2.8] - 2026-09-19, [0.3.1] - 2026-09-20 (+12 more)

### Community 24 - "repair_score.py"
Cohesion: 0.16
Nodes (16): json, compute_scores(), _deep_merge(), _health(), _health_factors(), load_config(), Repair Health / Repair Risk scoring for the repaired mesh. A NEW, SEPARATE…, Extract the health factors' boolean "is this ok?" values. Returns a dict of… (+8 more)

### Community 25 - "numpy"
Cohesion: 0.22
Nodes (11): numpy, os, pymeshlab, load_stl(), main(), Compare the classic vs experimental (v2) classifier on the real-world corpus.…, CLI entry for the on-demand before/after mesh comparison render. Runs as a…, CLI entry for the on-demand defect heatmap render. Runs as a SEPARATE PROCESS… (+3 more)

### Community 28 - "graphify.js"
Cohesion: 0.40
Nodes (3): IMPORTANT: keep the reminder string free of backticks and $(...) constructs., ref_fs, ref_path

### Community 33 - "sutura_repair_linux_x86_64.py"
Cohesion: 0.17
Nodes (15): orca, _cross(), _model_to_mesh(), Sutura Repair — OrcaSlicer script plugin. Repairs the currently SELECTED model:…, Read the model's first readable volume as (name, verts, tris).…, Normalise a vertex to [float, float, float] from a list/tuple/np array., Normalise a triangle to [int, int, int] from a list/tuple/np array., 3D cross product of two 3-vectors (pure Python). (+7 more)

### Community 34 - "Sutura"
Cohesion: 0.12
Nodes (16): Contributing, Install, Known limitations, Libraries, License, Linux, macOS, Requirements (+8 more)

### Community 35 - "Sutura"
Cohesion: 0.12
Nodes (16): Bilinen sınırlamalar, Destekçiler, Ekran Görüntüsü, Gereksinimler, Katkı, Kullanım geçmişi (anonim, isteğe bağlı kapatılabilir), Kurulum, Kütüphaneler (+8 more)

### Community 36 - "defect_injector.py"
Cohesion: 0.15
Nodes (15): _diag(), inject_degenerate(), inject_flipped_normal(), inject_hole(), inject_non_manifold(), inject_self_intersect(), load_mesh(), main() (+7 more)

### Community 37 - "train_classifier_v2.py"
Cohesion: 0.23
Nodes (15): build_dataset(), confusion(), features_of(), fit_logistic(), leave_one_out(), load_stl(), main(), predict_logistic() (+7 more)

### Community 38 - "defects.py"
Cohesion: 0.19
Nodes (14): _boundary_edges(), _boundary_loops(), detect_holes(), detect_non_manifold(), find(), union(), _edge_keys(), _loop_geometry() (+6 more)

### Community 39 - "detect"
Cohesion: 0.19
Nodes (13): Fixed, Benchmark corpus (115-mesh), Developer tooling — synthetic defect injection, Feature status, Test, Benchmark corpus'u (115 mesh), Geliştirici aracı — sentetik defekt enjeksiyonu, Test (+5 more)

### Community 40 - "classify"
Cohesion: 0.17
Nodes (13): 4. Staged implementation plan, 4a. Autorefine + snap-rounding (Lazard & Valque 2025) — smaller, nearer-term, 4b. Alpha wrapping (Portaneri et al. 2022) — experimental Stage 2 alternative, Methodology, Metric clarification (what "strict" means, and what it does not), classify(), _classify_objects(), _obj_closed() (+5 more)

### Community 41 - "Tests (no framework — plain scripts, need the venvs installed)"
Cohesion: 0.20
Nodes (12): Tests (no framework — plain scripts, need the venvs installed), focus_frame(), A (center, scale) camera frame that fits ALL meshes in ``verts_list`` with the…, A (center, scale) camera frame zoomed in on a defect region. ``verts_idx``…, shared_frame(), ExtremeRemovedAllError, join_small_components(), find() (+4 more)

### Community 42 - "Sutura × OrcaSlicer plugin"
Cohesion: 0.18
Nodes (8): Configuration, ⚠️ EXPERIMENTAL — real-instance verified, early stage, Feedback, Install (nightly / OrcaSlicer > 2.4.2), Platform: Linux primary, macOS bonus verification, Sutura × OrcaSlicer plugin, Unique output files — no overwrites, ⚠️ Version requirement — nightly / newer than 2.4.2 REQUIRED

### Community 43 - "benchmark_repair_corpus.py"
Cohesion: 0.24
Nodes (10): gc, count_self_intersections(), load_input(), main(), Repair-corpus benchmark with a STRICT watertight metric. Runs the current…, Count self-intersecting faces of a mesh (pymeshlab's per-face SI selection).…, Repair one mesh and return (entry, error)., run_one() (+2 more)

### Community 44 - "._build_ui"
Cohesion: 0.18
Nodes (4): Small up-arrow icon; teal when an update is available., Play-triangle icon, drawn dark so it is visible on the teal Repair button (same…, Magnifying-glass icon in the teal accent, drawn with QPainter., Warn that the new version crosses the license boundary and open the releases…

### Community 45 - "Fixed"
Cohesion: 0.20
Nodes (10): Backlog resolved, [0.2.0] - 2026-08-26, Added, Changed, Documentation, Fixed, The class-specific tuning gates, sourced from repair.py at call time. repair.py…, _tuning_gates() (+2 more)

### Community 46 - "Security audit (pre-v0.3) — 2026-09-20"
Cohesion: 0.20
Nodes (9): 1. Dependency security (`pip-audit`), 2. Shell / subprocess injection, 4. Path traversal / output writes, 5. Install scripts — checksum verification (LOW, recommendation), 6. Hardcoded secrets, 7. GitHub Actions (LOW, recommendation), Decisions, Security audit (pre-v0.3) — 2026-09-20 (+1 more)

### Community 47 - "._worker"
Cohesion: 0.20
Nodes (9): _load_back(), Run the separately-installed Sutura CLI on the staged file. Returns (True, '')…, Number of distinct OrcaSlicer processes running (pgrep -f). Returns None when…, Best-effort: reload the repaired file into the slicer. Linux: OrcaSlicer…, Repair off the UI thread; store the outcome in `result` (plain dict).…, A UNIQUE repaired-output filename (never a fixed name, so consecutive runs…, _run_subprocess(), _running_orca_processes() (+1 more)

### Community 48 - "RepairModeDialog"
Cohesion: 0.22
Nodes (5): QDialog, Modal picker for the batch repair mode. A five-step horizontal slider (Low-…, Return (max_geom_change, max_risk); 0/None both mean no limit., Open the repair-mode dialog (mode + repair budgets); apply to the next batch., RepairModeDialog

### Community 49 - "._open_before_after"
Cohesion: 0.33
Nodes (9): _apply_inter_state(), _on_rendering_stage(), _on_viewer_done(), _on_viewer_failed(), show(), _show_mode(), _thumb(), toggle() (+1 more)

### Community 50 - "tuning_applied"
Cohesion: 0.25
Nodes (9): [0.1.7] - 2026-08-22, Added, Added, Changed, Changed, Changed, Fixed, Mirror of the repair.py confidence gate: whether the tuned Stage 1 thresholds… (+1 more)

### Community 51 - "Repair benchmark — strict watertight measurement (115-mesh corpus)"
Cohesion: 0.22
Nodes (8): Addendum (FAZ 4) — metal-nut regression fixed, corpus back to 103/115, `artec_metal-nut.stl` — the only regression vs the previous run, Comparison with the previous run, Conclusion, Repair benchmark — strict watertight measurement (115-mesh corpus), Results, Scope, The 13 warning meshes, in detail

### Community 52 - "v0.3.0 readiness assessment"
Cohesion: 0.25
Nodes (7): `--experimental-join-components`: promote to the main chain?, Is this enough for a v0.3.0 minor release?, Other open items / watch-outs, Repo size (~97 MB, corpus STLs), Summary, Tonight's changes (5 commits since v0.2.8, all local, no push), v0.3.0 readiness assessment

### Community 53 - "Mesh type-aware repair"
Cohesion: 0.25
Nodes (8): Classifier engines (`--classifier-engine`), Classifier methodology, Known limitation of the classifier, Local roughness / geometrical / statistical features as a tie-breaker (NOT integrated), Mesh type-aware repair, MeshCNN edge features as fusion and tie-breaker (NOT integrated), Opt-in edge-tiebreak head (`--experimental-edge-tiebreak`, NOT the default), Retrospective single-feature scan (FAZ 10)

### Community 54 - "ftetwild_bridge.py"
Cohesion: 0.36
Nodes (7): extract_boundary(), main(), fTetWild fallback bridge: tetrahedralize a broken surface and extract the…, Boundary triangles of a tet mesh: faces incident to exactly one tet, wound…, Tetrahedralize the surface mesh at src and write the boundary surface to dst.…, run_bridge(), write_obj()

### Community 55 - "calibrate_classifier.py"
Cohesion: 0.38
Nodes (6): argparse, make_classifier_set, _conf_summary(), _get_engine(), main(), Calibration harness for the mesh classifier. Runs the current default mesh…

### Community 56 - "fTetWild Fallback Tier — 115-mesh Real-World Corpus Benchmark"
Cohesion: 0.29
Nodes (6): Conclusion, fTetWild Fallback Tier — 115-mesh Real-World Corpus Benchmark, fTetWild tier stats, Headline result, Known limitation, Timeout meshes (30, all dense real-world scans)

### Community 57 - "bug_report.md"
Cohesion: 0.29
Nodes (6): Attach the mesh (if possible), Command used, Environment, Expected vs. actual behaviour, Mesh source, Output

### Community 58 - "Mesh türüne duyarlı onarım"
Cohesion: 0.29
Nodes (7): Geriye dönük tek-özellik taraması (FAZ 10), Lokal roughness / geometrik / istatistiksel özellikler tie-breaker olarak (ENTEGRE EDİLMEDİ), Mesh türüne duyarlı onarım, MeshCNN kenar özellikleri fusion ve tie-breaker olarak (ENTEGRE EDİLMEDİ), Sınıflandırıcı metodolojisi, Sınıflandırıcı motorları (`--classifier-engine`), Sınıflandırıcının bilinen sınırlaması

### Community 59 - "[0.2.1] - 2026-08-30"
Cohesion: 0.33
Nodes (6): [0.2.1] - 2026-08-30, Added, Fixed, Performance, is_stage2_skipped(), True if the report says stage 2 was skipped (used for the summary).

### Community 60 - "[0.2.3] - 2026-09-19"
Cohesion: 0.33
Nodes (6): [0.2.3] - 2026-09-19, Added, Changed, Dependency, Locate manifold_bridge.py. Prefers a copy bundled next to the executable in a…, _resolve_bridge()

### Community 61 - "Contributing to Sutura"
Cohesion: 0.33
Nodes (5): 1. License and contribution terms, 2. License compatibility — copyleft dependency policy, 3. Development workflow, 4. Pull request checklist, Contributing to Sutura

### Community 62 - "Feasibility spike: from-scratch autorefine + alpha-wrapping for Sutura's heavy-self-intersection weak spot"
Cohesion: 0.33
Nodes (5): 2. Prototyping environment (what's actually available), 3. Regression / benchmark targets, 6. Outcome — alpha wrapping evaluated, NOT integrated (2026-09-22), Feasibility spike: from-scratch autorefine + alpha-wrapping for Sutura's heavy-self-intersection weak spot, Recommended next step

### Community 63 - "CLI / GUI feature parity notes"
Cohesion: 0.33
Nodes (5): Both (feature parity satisfied), CLI / GUI feature parity notes, CLI-only (documented gaps), GUI-only (allowed exceptions — visual features), History

### Community 64 - "[0.1.0] - unreleased"
Cohesion: 0.40
Nodes (5): [0.1.0] - unreleased, Added, Changed, Fixed, Known platform note

### Community 65 - "Added"
Cohesion: 0.40
Nodes (5): [0.1.8] - 2026-08-24, Added, Changed, defect_camera(), Build a defect-facing camera basis for the before/after comparison. ``center``…

### Community 66 - "[0.3.0] - 2026-09-20"
Cohesion: 0.40
Nodes (5): [0.3.0] - 2026-09-20, Added, Changed, Experimental evaluations (documented, NOT integrated), Security

### Community 67 - "sutura-geom"
Cohesion: 0.40
Nodes (4): Building and wiring into the venv, Dependencies and licenses, Python API, sutura-geom

### Community 68 - "Security Policy"
Cohesion: 0.40
Nodes (4): Reporting a Vulnerability, Security Policy, Supported Versions, What we consider a security issue

### Community 70 - "_risk"
Cohesion: 0.40
Nodes (4): _pct_delta(), Percentage change (signed). before<=0 or missing -> None., Weighted risk score. Returns (score, factors_dict)., _risk()

### Community 71 - "[0.1.1] - unreleased"
Cohesion: 0.50
Nodes (4): [0.1.1] - unreleased, Added, Changed, Fixed

### Community 72 - "[0.1.4] - 2026-08-19"
Cohesion: 0.50
Nodes (4): [0.1.4] - 2026-08-19, Added, Changed, Fixed

### Community 73 - "[0.1.9] - 2026-08-25"
Cohesion: 0.50
Nodes (4): [0.1.9] - 2026-08-25, Added, Documentation, Fixed

### Community 74 - "OrcaSlicer plugin (orcaslicer-plugin/) — version history"
Cohesion: 0.50
Nodes (4): [0.2.2] - 2026-09-21, [0.2.3] - 2026-09-21, Fixed, OrcaSlicer plugin (orcaslicer-plugin/) — version history

### Community 75 - "[0.2.5] - 2026-09-19"
Cohesion: 0.50
Nodes (4): [0.2.5] - 2026-09-19, Added, Changed, Fixed

### Community 76 - "Mesh corpus attribution & licenses"
Cohesion: 0.50
Nodes (3): Benchmark corpus release (115 meshes), Mesh corpus attribution & licenses, Third-party library dependencies

### Community 77 - "SuturaRepairPlugin"
Cohesion: 0.50
Nodes (3): plugin, $schema, SuturaRepairPlugin

### Community 78 - "_write_temp_stl"
Cohesion: 0.50
Nodes (4): The audit-hook allowed root: orca.host.data_dir(), else a fallback., Write a unique transient STL under data_dir()/sutura_repair/<uuid>.stl. Returns…, _temp_dir(), _write_temp_stl()

### Community 79 - "macos-quick-action.sh"
Cohesion: 0.83
Nodes (3): log(), notify(), macos-quick-action.sh script

## Knowledge Gaps
- **162 isolated node(s):** `$schema`, `sutura-geom`, `sutura-geom`, `ORIGIN`, `X` (+157 more)
  These have ≤1 connection - possible missing edges or undocumented components. (Counts symbols only; 501 node(s) total have ≤1 connection when file, concept and rationale nodes are included.)
- **10 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `How it runs` connect `How it runs` to `main`, `heatmap.py`, `MeshViewport`, `repair_3mf`, `gui.py`, `.__init__`, `AGENTS.md`, `autorefine.py`, `repair_mesh_from_arrays`, `Kullanım`, `alpha_wrap_prototype.py`, `repair.py`, `mesh_classifier_v2.py`, `history.py`, `._current_path`, `detect`, `classify`, `Tests (no framework — plain scripts, need the venvs installed)`, `RepairModeDialog`, `._open_before_after`, `tuning_applied`?**
  _High betweenness centrality (0.183) - this node is a cross-community bridge._
- **Why does `detect()` connect `detect` to `Added`, `[0.3.0] - 2026-09-20`, `Sutura`, `How it runs`, `Sutura`, `main`, `defects.py`, `classify`, `Tests (no framework — plain scripts, need the venvs installed)`, `Kullanım`, `alpha_wrap_prototype.py`, `Repair benchmark — strict watertight measurement (115-mesh corpus)`?**
  _High betweenness centrality (0.116) - this node is a cross-community bridge._
- **Why does `Changelog` connect `Changelog` to `[0.1.0] - unreleased`, `Added`, `[0.3.0] - 2026-09-20`, `[0.1.1] - unreleased`, `[0.1.4] - 2026-08-19`, `[0.1.9] - 2026-08-25`, `Sutura × OrcaSlicer plugin`, `[0.2.5] - 2026-09-19`, `OrcaSlicer plugin (orcaslicer-plugin/) — version history`, `Fixed`, `.execute`, `tuning_applied`, `[0.2.1] - 2026-08-30`, `[0.2.3] - 2026-09-19`?**
  _High betweenness centrality (0.090) - this node is a cross-community bridge._
- **Are the 52 inferred relationships involving `How it runs` (e.g. with `tuning_applied()` and `.distance()`) actually correct?**
  _`How it runs` has 52 INFERRED edges - model-reasoned connections that need verification._
- **Are the 23 inferred relationships involving `detect()` (e.g. with `How it runs` and `Tests (no framework — plain scripts, need the venvs installed)`) actually correct?**
  _`detect()` has 23 INFERRED edges - model-reasoned connections that need verification._
- **Are the 6 inferred relationships involving `repair_mesh_from_arrays()` (e.g. with `How it runs` and `Tests (no framework — plain scripts, need the venvs installed)`) actually correct?**
  _`repair_mesh_from_arrays()` has 6 INFERRED edges - model-reasoned connections that need verification._
- **What connects `$schema`, `sutura-geom`, `sutura-geom` to the rest of the system?**
  _162 weakly-connected nodes found - possible documentation gaps or missing edges._