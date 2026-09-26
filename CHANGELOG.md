# Changelog

All notable changes to this project are documented here.

## [Unreleased]

### Added

- **Deep-repair ladder (`--deep-repair {off,local,full}`).** The tiers that
  run after the stage-1 chain now have one entry point,
  `repair.deep_repair_ladder`. The mode comes from the flag, then
  `SUTURA_DEEP_REPAIR`, then the `deep_repair` key of
  `~/.config/sutura/config.json` (new `updater.DEFAULT_CONFIG` entry), and
  defaults to `full`. `full` runs the fTetWild tier unchanged (the block was
  moved, not modified; the output is identical to the previous version);
  `--no-fallback-ftetwild` maps to `off` and
  `--experimental-fallback-ftetwild` to `full` when no `--deep-repair` is
  given. `off` runs no tier and reports `deep_repair.available`
  (`holes_remaining`, `nm_remaining`, `tiers`, `estimate_s`) when holes or
  non-manifold edges remain; `repair.estimate_deep_repair_time` gives the
  estimate with placeholder coefficients. `local` is a PyMeshLab-based
  prototype that deletes the damaged region (faces on boundary or
  non-manifold edges plus one vertex ring), closes the openings with refined
  hole filling and smooths only the new interior vertices; it is adopted only
  when holes and non-manifold edges do not get worse, self-intersections do
  not increase and all faces outside the region are unchanged (report key
  `deep_repair.local`). The report gains `deep_repair` (`mode`,
  `holes_before`, `nm_before`, `tiers_run`, `final_tier`, `local`,
  `ftetwild`, `available`); `experimental_ftetwild` is kept.
  `scripts/benchmark_repair_corpus.py` gained `--deep-repair` and the columns
  `final_tier`, `time_total`, `time_local`, `time_ftetwild`, `estimate_s`,
  `actual_s`, `deep_repair_available`, `hausdorff_max_rel` and
  `hausdorff_mean_rel` (one-sided output-to-input, relative to the bounding
  box diagonal). The GUI does not use the new modes yet. New suite
  `tests/test_deep_repair.py`, in CI.
- **fTetWild runs without its quality optimisation.** `ftetwild_bridge`
  calls `pytetwild.tetrahedralize` with `optimize=False` by default
  (`DEFAULT_PARAMS`; `run_bridge(..., params)`, a JSON third argument on the
  bridge command line and `repair.run_ftetwild(..., params)` override it;
  the report records `params`). The optimisation only improves the interior
  tetrahedra, which are discarded. In the macOS parameter sweep
  thingi10k_46012 took 6 s instead of 34 s (Hausdorff 0.09 % of the
  diagonal); the unoptimised boundary can carry non-manifold edges, which
  the manifold3d post-process removes. artec_metal-nut still does not finish
  within the 180 s budget (only `optimize=False` with
  `edge_length_fac=0.1` finished, in 806 s). Output-changing for every mesh
  the fTetWild tier handles.
- **Local tier limited to small, simple damage.** Measured on macOS, the
  local tier made 3 of the 40 real-world samples strictly watertight
  (31 → 34: thingi10k_40886, 46012, 71691) but none of the 115-mesh corpus,
  where it added 69 % run time. It now runs only when there is no
  non-manifold edge, the longest boundary loop has at most 16 edges and the
  damaged region has at most 8 parts and 2,000 faces, and it stops after
  10 s (`LOCAL_MAX_*`; `reject_reason` starts with `scope:` or is `time`).
  Hole refinement is off (`LOCAL_REMESH_REFINE`): it took ~9 s of the
  ~11 s per 90k-face mesh and changed no outcome on the 40 samples. The
  outside-face guard is vectorized. Same three gains on the 40 samples; the
  local tier's time there drops from 24.6 s to 6.0 s (x86_64 VM). The
  report gains `deep_repair.local.max_loop_len`.

### Fixed

- **Closed results with pinched vertices ended as `warning`.** A mesh can
  leave the stage-1 chain with no hole and no non-manifold edge and still not
  be two-manifold, because two parts of the surface meet in one vertex (the
  final hole closing can fan-fill a hole at a vertex shared by two boundary
  loops). Stage 2 only runs on a two-manifold result, so such a strictly
  watertight mesh was reported as a warning (thingi10k_145065, 18 pinched
  vertices; fTetWild never runs there). The pinched vertices are now split by
  repeating `meshing_repair_non_manifold_vertices` (at most 5 passes; three
  on 145065), which only duplicates vertices; the result is kept only when it
  still has no hole and no non-manifold edge. The report carries
  `pinched_vertices_split` (`before`/`after`/`passes`/`adopted`), `--human`
  shows a "Pinched vertices split" line and the GUI repair log a matching
  entry. The output changes only for closed, non-manifold-edge-free meshes
  that are not two-manifold. New suite `tests/test_pinched_vertices.py`
  (synthetic bowtie, clean and open cubes, and thingi10k_145065), in CI.
- **GUI suites aborted on macOS with an existing user config.** With
  `check_for_updates` enabled and a check due in `~/.config/sutura/config.json`,
  constructing `MainWindow` started the update-check thread, which was still
  running when the test subprocess exited (SIGABRT, return code -6). The
  background update check is now skipped on the windowless Qt platforms
  (`offscreen`/`minimal`), as the first-run dialog already was.
- **fTetWild results with a pinched vertex ended as `warning`.** fTetWild's
  boundary can be closed with no non-manifold edge and still not be
  two-manifold (two tetrahedra meeting in one vertex), so stage 2 never ran
  and the file was reported as a warning although it is strictly watertight
  (thingi10k_248395 in some runs; fTetWild is not deterministic). The
  manifold3d post-process of the fTetWild boundary now also runs when the
  boundary is not two-manifold, which lets stage 2 run on the adopted result.
  The output changes only for meshes where fTetWild is adopted and its
  boundary is not two-manifold. New regression tests in
  `tests/test_ftetwild_default.py` (synthetic bowtie, no fTetWild needed).
  Measured on the 40 real-world samples on macOS (fTetWild auto, before and
  after this change): strict watertight 39/40 and category watertight 38/40
  in both runs, total time ~354 s in both, no regression; 224108 and 248395
  were watertight in both runs (the pinched case did not occur there; the
  unit test covers it). thingi10k_1038441, listed as declined in the 0.4.0
  entry, is adopted after the manifold3d post-process (25.8 s, watertight).
  The one remaining strict failure is artec_metal-nut, where fTetWild hits
  its 180 s budget.

## [0.4.2] - 2026-09-25

### Added

- **Evaluated CDT variants for the exact arrangement (developer switches,
  off by default).** `rust/sutura-geom` gained Sloan-style queue flips
  (constraints inserted without split vertices), constraint flags kept
  across flips, and propagation of segment endpoints that lie inside a host
  edge to every triangle sharing that edge (removes the T-junctions the
  reference output leaves there: thingi10k_100045 56 → 0 odd-use edges).
  They are per-thread switches (`cdt2d::experimental`; `SUTURA_CDT_OPTS` in
  `arrangement_digest`, `sutura_geom._set_cdt_experimental`), measured on
  the 40 real-world samples with the new benchmark flags: strict watertight
  stays 31/40 for every variant, propagation costs ~35 % arrangement time,
  so the default output is unchanged (same digests). `arrangement_digest`
  also prints the vertex count and the input/output edge-use histogram.
- `scripts/benchmark_repair_corpus.py`: `--experimental-indirect-autorefine`
  and the developer hook `--cdt-experimental BITS`; per-mesh
  `indirect_adopted`/`indirect_faces_after`/`indirect_error` columns.

### Changed

- **Phase C2: per-host constrained triangulation without linear scans**
  (`rust/sutura-geom/src/cdt2d.rs`, still behind
  `--experimental-indirect-autorefine`). Point location is a stochastic
  visibility walk canonicalised to the lowest-index containing triangle;
  vertex–triangle incidence is maintained so edge lookups and constraint
  marking rotate around one vertex; constraint insertion walks the segment
  corridor, which yields exactly the crossing edge and the on-segment vertex
  the old whole-triangulation scans returned; the normal end of the flip loop
  updates one triangle instead of rebuilding all adjacencies. The exact
  `orient2d` fallback clears denominators and works on `BigInt`, vertex
  interval enclosures are cached, per-host projection constants are computed
  once and each CDT vertex is mapped back to 3D and welded once. Every
  decision is the same as before, so the output is identical (same
  order-independent digest of the exact output triangles, new
  `examples/arrangement_digest.rs`), not only equal in face count. On the
  same x86_64 VM, thingi10k_1038441: full mesh 830 s → 52.5 s, 5000-face
  subset 56.2 s → 13.1 s, 1001-face subset 10.9 s → 3.1 s. On the Apple M2
  used for the C1 numbers the full mesh takes 29.8 s (C1: 462.7 s), with the
  same output digest.
- New `cdt-check` Cargo feature: every accelerated triangulation query is
  asserted against the linear reference scan (used for differential testing;
  the full thingi10k_1038441 mesh passes). CI runs the Rust tests with and
  without it, plus a new randomized accelerated-vs-linear test.
- **Phase C3: classification and exact-arithmetic hot spots**
  (`rust/sutura-geom`, output identical). Orientation tests of a
  line-plane intersection against the edge it was built on return the exact
  zero without evaluation; candidate intersection points are proven
  different by a rigorous interval test before exact rationals are built;
  the exact host frame for touch segments is built once per host; the 3D
  back-projection, the fallback line-line intersection and the segment sort
  key use integer arithmetic with a single normalisation (unit-tested value
  by value against the rational formulas). thingi10k_1038441 on the same
  x86_64 VM: 52.5 s → 28.1 s (classification 19.2 s → 4.6 s); identical
  output digests on that mesh, its subsets and 1038439, 55772, 502009,
  100045, 46012 (90k faces, 165 s → 95 s) and artec_metal-nut (90k faces,
  132 s → 77 s). On the Apple M2 used for the C1/C2 numbers, the full mesh
  takes 16.8 s (C2 on the M2: 29.8 s), same output digest.
- **Phase C4: exact-key hashing and integer implicit constructions**
  (`rust/sutura-geom`, output identical). The output weld map and the
  per-host `(s,t)` vertex index hashed `BigRational` keys through
  `num-rational`'s continued-fraction `Hash` (a chain of `BigInt` floor
  divisions per lookup); a `RatKey` wrapper now hashes and compares the
  reduced `(numer, denom)` pair directly, which is equivalent because every
  rational in the crate is kept reduced. Line-plane and plane-plane-plane
  constructions (`Point3::to_rational`) evaluate their formulas on integers
  over one common power-of-two scale with a single normalisation per
  coordinate (a randomized unit test compares them with the rational
  formulas value by value, including subnormal and widely scaled
  coordinates). thingi10k_1038441 on the same x86_64 VM: 28.2 s → 17.6 s
  (weld and output 6.4 s → 0.06 s, per-host triangulation 21.6 s → 7.4 s,
  implicit construction 5.7 s → 2.3 s); identical output digests on that
  mesh, its four subsets and 100045, 1038439, 55772, 502009, 46012 and
  artec_metal-nut (the two 90k-face meshes gain only 1–3 %, their cost lies
  elsewhere). On the Apple M2, the full mesh takes 10.6 s (C3 on the M2:
  16.8 s), same output digest.
- **fTetWild fallback tier on by default when installed.** With the
  optional extra (`requirements-ftetwild.txt`, `SUTURA_WITH_FTETWILD=1`)
  present, the tier now runs without a flag whenever stage 1 still leaves
  holes or non-manifold edges; a closed result that only self-intersects is
  left alone. New opt-out `--no-fallback-ftetwild`;
  `--experimental-fallback-ftetwild` keeps its previous meaning (also runs on
  closed-but-self-intersecting results, explicit skip when not installed).
  GUI: the *fTetWild fallback* checkbox (no longer labelled experimental) is
  checked by default and a second checkbox next to it,
  *+ self-intersections (slow)*, maps to the experimental flag. Without the extra nothing changes (no report
  entry, no time cost). `scripts/benchmark_repair_corpus.py` follows the CLI
  default and gained `--no-fallback-ftetwild`. Measured on the 40 real-world
  samples: strict watertight 31 → 39 (ran on 9 meshes, adopted on 8; the
  remaining open mesh hit the 180 s budget), total time 64 s → 669 s; the
  always-on variant reaches the same 39 in 2,595 s and remeshes 7 already
  closed meshes. fTetWild's output and run time vary between runs; the
  result is re-triangulated. New `tests/test_ftetwild_default.py` (in CI;
  the checks that need fTetWild skip there).
  The default also requires the bridge module, so a stale install that has
  pytetwild but no `ftetwild_bridge.py` stays silent instead of reporting an
  error.

- **GUI: one Options menu instead of a row of checkboxes.** The six
  batch-wide switches (fTetWild fallback, *+ self-intersections (slow)*,
  autorefine, indirect autorefine, join small components, edge-tiebreak
  classifier) had stretched the action row across the window. They now sit
  in one **Options** drop-down next to **Mode**, grouped into *Fallback
  tier* and *Experimental (opt-in)*; the menu stays open while several boxes
  are toggled, and the button label counts the options that differ from
  their defaults (*Options (2)*). The checkbox labels lost their redundant
  "Experimental:" prefix. Behaviour and CLI mapping are unchanged.

### Fixed

- `scripts/benchmark_repair_corpus.py` crashed at import when manifold3d is
  not importable in the running interpreter (the documented behaviour is an
  `n/a` cross-validation column); the import is now optional.
- **GUI construction hung on headless systems.** With no
  `~/.config/sutura/config.json`, `MainWindow` opened the modal first-run
  dialog, which nobody can answer on Qt's `offscreen`/`minimal` platforms, so
  the two suites that build the window (`tests/test_autorefine.py`,
  `tests/test_join_components.py`) hung until their 120 s timeout on the
  ubuntu CI runner. The dialog is now skipped on those platforms without
  writing a config (it is still asked on the first on-screen start), and both
  suites run in CI again.
- **fTetWild left `__tracked_surface.stl` in the working directory.**
  fTetWild writes this ~1 MB debug surface into its current directory, so a
  repair started from a user's folder (terminal, Dolphin/Finder right-click)
  left the file next to the model. The bridge subprocess now runs in the
  repair's private temp directory; a regression check is in
  `tests/test_ftetwild_default.py`.

## [0.4.1] - 2026-09-25

### Fixed

- **OrcaCloud auto-publish reported success on a failed upload.** The
  workflow had lost the official template's status check, so the v0.4.0 run
  got HTTP 400 from the publish API, printed no response body and still
  passed; the v0.4.0 plugin listing was therefore not published. The
  workflow now prints the API response and fails on anything but 201, and
  can be dispatched manually for an existing tag (the OrcaCloud side only
  accepts tokens from the release event, so a dispatch is diagnostic only).
  This release re-publishes the plugin (header version aligned to 0.4.1),
  carrying the OrcaSlicer plugin changes listed under 0.4.0.

## [0.4.0] - 2026-09-25

This release also carries everything recorded under 0.3.1, which was never
tagged: its OrcaSlicer plugin revision ships here.

### Added

- **Phase C1: exact self-intersection arrangement made practical on dense
  scans** (`rust/sutura-geom`, still behind `--experimental-indirect-autorefine`).
  - Segment-segment crossings inside a host triangle are constructed from the
    original input geometry (PPI of host plane and the two intersecting
    triangles' planes, LPI against a host edge) instead of being chained from
    previously constructed 2D points, which bounds coordinate bit size. Every
    CDT vertex carries its provenance; when no original-data construction
    exists the exact 2D construction is the fallback, so a crossing is never
    dropped and no implicit point is rounded to build a new one.
  - A rigorous interval-arithmetic filter (`src/interval.rs`: outward
    rounding by one ulp per operation, NaN/overflow undecidable) now decides
    `orient3d` on implicit points and the CDT's `orient2d`/`incircle` whenever
    the enclosure excludes zero; otherwise the exact `BigRational` path runs,
    so results are unchanged by construction. Differential tests compare the
    filter against the exact evaluation on random and deliberately degenerate
    inputs.
  - Measured on thingi10k_1038441 (Apple M2, release build, identical output
    face counts before/after on every subset): 1001-face subset 53.0 s ->
    6.0 s, 5000-face subset timeout at 180 s -> 31.1 s, full mesh (10,418
    faces, 4,443 proper SI pairs) did not finish in 30+ min -> 462.7 s. The
    remaining time is dominated by the per-host constrained triangulation;
    the path stays opt-in and developer-built (`maturin develop` in
    `rust/sutura-geom`).
  - C0 profiling harness `rust/sutura-geom/examples/bench_arrangement.rs`
    (feature `profile`, per-phase timers and predicate counters, spatially
    local subsets, per-subset time boxes) and a `cdt-diag` feature for
    per-host CDT diagnostics.
- **Geogram evaluation (not integrated).** `docs/geogram-spike-2026-09-24.md`
  records a measured spike of Geogram's `MeshSurfaceIntersection` +
  `remove_internal_shells` as an external self-union solver: fast, but its
  output is not accepted by the manifold3d rebuild on any tested mesh and the
  community edition aborts in `RadialSort` on thingi10k_1038441 (the library
  points to the proprietary geogramplus kernel). The path is closed.
- **OrcaSlicer plugin revision** (recorded under the never-tagged 0.3.1).
  The plugin now repairs the **currently selected model** by reading it in
  memory through the `orca.host` API (`model() -> objects() -> volumes() ->
  mesh()`, using the numpy-free `vertex(i)`/`triangle(i)` accessors — the
  embedded interpreter ships only `pip`, no numpy) instead of a fixed
  configured file. Repair runs via the subprocess CLI (no numpy/pymeshlab in
  the embedded interpreter); the repaired result is written with a **unique
  per-run filename** (`<stem>_fixed_<timestamp>_<uuid>`) so consecutive runs
  never overwrite a previous output, and loaded back via `--single-instance`
  (Linux) / `open -a OrcaSlicer` (macOS). The OrcaSlicer plugin system
  requires nightly / releases newer than 2.4.2 (stable 2.4.2 has no Plugins
  menu). Primary target is Linux; the same file is also verified on macOS (the
  CLI is at the same `~/.local/bin/sutura` path on both platforms). Stub-
  tested against a mock `orca.host` (`tests/test_orca_plugin.py`), including
  loading with numpy blocked; the real-instance GUI run is performed
  separately (user / computer-use).
- **Phase B indirect-predicates prototype (`--experimental-indirect-autorefine`).**
  The `rust/sutura-geom` crate now implements the full Phase B chain: indirect
  `orient3d`/`orient2d`/`incircle` predicates on explicit and LPI/PPI implicit
  points (`predicates2d.rs`, `predicates3d.rs`), an exact triangle–triangle
  intersection classifier (`triangle_intersection.rs`), a 2D constrained
  Delaunay triangulation for projected implicit points (`cdt2d.rs`), an
  `arrangement_lite` PyO3 binding that splits a self-intersecting triangle
  soup, and `--experimental-indirect-autorefine` wiring in `sutura/repair.py`.
  The incremental single-segment CDT insertion was replaced by a planar
  arrangement step that computes all segment-segment intersections up front
  before enforcing constraints, fixing the "constrained edge blocks segment
  insertion" panic seen on thingi10k_1038441-style dense self-intersections.
  Regression tests: 38 Rust unit tests + 9 Python smoke tests pass, plus two
  new CDT tests for multiple crossing segments in one host triangle. Known
  limitation: the subdivision phase is exact-rational only, so dense
  self-intersection scans do not finish in practical time; a filtered
  f64/interval/exact evaluation layer is the next step before the path can be
  benchmarked against the full 115-mesh corpus.
- **Experimental fTetWild fallback tier for residual self-intersections**
  (`sutura/ftetwild_bridge.py`, `--experimental-fallback-ftetwild`, GUI
  checkbox — FAZ17). A guaranteed-correct last-resort solidifier: when the
  stage-1 chain (autorefine included) still leaves self-intersections, holes,
  or non-manifold edges, the ORIGINAL input surface is tetrahedralized with
  fTetWild (via the `pytetwild` wrapper, MPL-2.0) and its boundary is
  extracted as a watertight, SI-free triangle mesh. The bridge follows the
  same dual-mode dispatch as `manifold_bridge.py` (venv311 subprocess on
  Linux, in-process in single-environment installs) and the result is adopted
  only when it is no worse on the same holes+non-manifold metric used by the
  autorefine guard. The report carries `experimental_ftetwild`
  (`ran`/`time`/`output_faces`/`output_holes`/`output_non_manifold`/
  `adopted`/`error`). Measured on the heavy-SI corpus (auto mode):
  thingi10k_100281 goes 3677→0 SI faces (adopted, ~55s), thingi10k_100827
  11→0 (adopted, ~0.5s); thingi10k_1038441 is correctly declined by the guard
  (its raw fTetWild boundary is non-manifold, 531 edges). `pytetwild` +
  `pyvista` were added to `requirements-311.txt`. Usage history records
  `ftetwild_applied`.
- **Iterative snap-rounding pass in the autorefine prototype**
  (`sutura/autorefine.py` — FAZ16 refinement). The base Lazard & Valque loop
  gained the CGAL 6.1 June-2025 second-half fix: after each snap the
  degenerate/sliver elements created are eliminated and still-intersecting
  vertices are re-snapped + re-resolved iteratively (`MAX_SNAP_ROUNDS`,
  strict-decrease guard). The grid is kept at the finest float-exact level —
  coarsening it was measured and rejected on the heavy-SI corpus (100281:
  906→97 pairs on the fine grid vs 906→74289 coarsened). Report additions:
  `snap_rounds_per_iteration` and `grid_scales_used`. Standalone SI on the
  heavy-SI corpus improves vs the previous autorefine (100281 3677→4849→3787,
  1038439 411→97→69, 46012 454→466→214).
- **Experimental autorefine self-intersection resolution**
  (`sutura/autorefine.py`, `--experimental-autorefine`, GUI checkbox — FAZ16).
  A from-scratch reimplementation of the published Lazard & Valque 2025 loop
  ("Resolving self-intersections in 3D meshes while preserving floating-point
  coordinates", CGF 44(5)): identify properly-intersecting triangle pairs,
  snap-round the involved vertices onto a float-exact grid, subdivide each
  triangle along its intersection segments (so each segment becomes a shared
  edge), and iterate until no proper intersections remain. Unlike the extreme
  mode's delete-and-reclose approach it **never deletes an input face** — every
  input triangle is either kept whole or split — which directly targets the
  documented "extreme mode can worsen heavy-SI scans" failure mode. Exact
  predicates come from `pyrobust-predicates` (Unlicense / public domain);
  pure numpy + predicates, no pymeshlab/scipy/trimesh, no CGAL source. In the
  pipeline the flag runs the same stage-1 chain on the autorefine-preprocessed
  input and **adopts it only when its final output has no more holes and
  non-manifold edges than the default chain** (adopt/fallback guard); the
  report carries `experimental_autorefine` (SI pairs before/after, iterations,
  converged, adopted). Measured on the real-world corpus: a clear SI reduction
  on moderate-SI meshes (e.g. thingi10k_1038439 411→97 SI faces standalone,
  integrated 532→123 vs chain-only), but the float64 construction is limited
  on dense-SI scans (thousands of intersecting faces in one region) where the
  subdivision opens holes the chain cannot fully re-close — that documented
  limitation is why it stays behind a flag. See
  `docs/alpha-wrap-feasibility-2026-09.md` (feasibility report) for the
  full before/after numbers and the licensing analysis. Regression-tested by
  `tests/test_autorefine.py` (never-delete property, SI-pair resolution on a
  crossing pair, surface-area stability on two interpenetrating spheres,
  adopt/fallback guard, CLI flag, GUI checkbox).
- **Benchmark harness records input/output self-intersections.**
  `scripts/benchmark_repair_corpus.py` now measures SI on the input and final
  output arrays (`input_self_intersections` / `output_self_intersections` /
  `stage1_si_remaining`), prints an `si=in->out` column and a before/after
  summary. The strict watertight metric itself still deliberately excludes SI
  (documented); SI stays a separate tracked signal.
- **Feasibility report for alpha wrapping.** `docs/alpha-wrap-feasibility-2026-09.md`
  records the research spike for a from-scratch alpha-wrapping Stage-2
  alternative (Portaneri et al. 2022) and the Lazard & Valque autorefine
  approach: where SI is handled today, the available prototyping stack, the
  measured corpus targets, the staged implementation plan with the two-sided
  wrap guard rails, and the licensing picture (CGAL GPL avoided by
  from-scratch reimplementation; pyrobust-predicates Unlicense chosen over the
  2D-only `robust`/`shewchuk` alternatives).

### Changed

- **fTetWild fallback dependencies are now an optional extra.** `pytetwild`
  and `pyvista` moved from `requirements-311.txt` to
  `requirements-ftetwild.txt`: pyvista pulls in VTK, roughly 1.1 GB installed
  (VTK alone ~520 MB), which the default install, the AppImage and the .dmg
  no longer carry for an opt-in experimental tier. Enable it with
  `SUTURA_WITH_FTETWILD=1 ./install.sh` (Linux) or
  `SUTURA_WITH_FTETWILD=1 ./install-macos.sh` (macOS); without it
  `--experimental-fallback-ftetwild` / the GUI checkbox reports an explicit
  skip with that hint.

### Fixed

- **`build-macos.yml` was invalid YAML** since the fTetWild commit (a comment
  line at column 0 inside a `run: |` block), so the macOS .dmg workflow failed
  instantly; fixed and validated.
- **`--experimental-autorefine` could crash a repair** when
  `pyrobust-predicates` was missing (the module import sat outside the
  guard); it is now imported inside the guard and reported as an error.
  `pyrobust-predicates` (pure Python) is added to `install-macos.sh` and the
  .dmg build, which previously did not install it.
- **`Point3::canonical_key` (sutura-geom)** panicked on any PPI key
  (9-element copy into a 3-element array) and sorted flattened coordinate
  scalars, which could give two different points the same dedup key; keys
  now sort whole vertices, normalise -0.0, and never index NaN placeholders.
- **Touch-segment host-edge classification** mapped the `s = 0` and
  `s + t = 1` edges the wrong way round under `p = a + s(b-a) + t(c-a)`.

## OrcaSlicer plugin (orcaslicer-plugin/) — version history

The plugin keeps its own version number for the Orca Cloud listing, separate
from the main project's release train. The changelog sections above describe
the main project; the plugin's own versions are recorded here.

From Sutura 0.4.0 on, the Orca Cloud listing is published automatically on
every GitHub Release whose plugin file changed, and its version follows the
Sutura release tag; the plugin header was aligned to `0.4.0` accordingly. The
0.4.0 listing carries the 0.2.3 changes below.

### [0.2.3] - 2026-09-21

### Added

- **Native progress dialog during repair.** `execute()` now opens a host-owned
  progress dialog (`orca.host.ui.create_progress_dialog`, indeterminate
  `pulse()`) for the duration of the repair and only then shows the result
  message, instead of returning immediately with a "Sutura Repair started."
  message while the worker ran silently in the background. The result/error
  messages moved out of the worker into `execute()`. On builds without the
  progress-dialog API the old background behaviour is kept as a fallback.
- **Upfront filesystem-read permission declaration.** `register_capabilities()`
  now calls `orca.request_permissions(fs_read=[SUTURA_CLI])` to pre-declare
  the external Sutura CLI binary path. HONEST SCOPE: the OrcaSlicer audit API
  only has a declarative form for `fs_read` — `process`/subprocess spawns and
  network access stay reactive (one dialog "Yes" per target) and their
  persisted grants are keyed to the exact command line (which contains unique
  temp paths). This change therefore does NOT reduce the per-run subprocess
  permission prompt; it only covers reads of the CLI path up front.

### [0.2.2] - 2026-09-21

Retroactively recorded: all three fixes shipped in the Orca Cloud listing
under 0.2.2 (commits up to `fcfb4a2`) but never entered this changelog.

### Fixed

- **numpy dependency removed.** The embedded OrcaSlicer Python ships only `pip`
  in site-packages (no numpy), which broke plugin loading outright. Mesh
  access and binary STL writing are now pure-stdlib (`struct`-based), and
  repair always runs via the subprocess Sutura CLI.
- **`orca.ExecutionResult.failure()` signature fixed.** The host requires a
  leading `orca.PluginResult` status argument; the plugin called it with a bare
  message string. All `failure()` call sites now pass
  `PluginResult.RecoverableError`.
- **macOS bundle-ID reopen.** The repaired file is reloaded with
  `open -b com.orcaslicer.OrcaSlicer <path>` (bundle-ID matching) instead of
  name-based `open -a`, and the reopen is skipped when more than one OrcaSlicer
  process is running (ambiguous). Verified end-to-end on real OrcaSlicer
  2.5.0-dev (macOS).

## [0.3.0] - 2026-09-20

### Added

- **Curved-but-mechanical classifier signal (`developable_fraction`).** The
  experimental `mesh_classifier_v2` engine gained a curvature-developability
  signal: the area fraction whose per-face normals lie on a common great
  circle (the geometric signature of a developable surface — a cylinder,
  pipe, fillet or other ruled curved part). The plane-only RANSAC could not
  see cylinders/fillets/pipes; this signal is what lets the head classify
  curved-but-mechanical parts that classic reads as `unknown`. The synthetic
  calibration set grew from 28 to 35 meshes.
- **Real-world classifier corpus grown 16 → 40 meshes.** `tests/real-world-samples/`
  gained 24 Thingi10K meshes (CC0/CC-BY; per-file attribution in
  `docs/ATTRIBUTION.md`), and the v2 head was retrained on it. On the labeled
  set experimental scores 29/36 vs classic 16/36 (mechanical recall 16/20 vs
  2/20); LOO-CV on the 71-mesh labeled set 0.845 vs classic 0.648.
- **115-mesh strict-watertight repair benchmark + harness.**
  `scripts/benchmark_repair_corpus.py` measures the final output geometry
  with a strict `defects.detect()` closed-loop check; result 103/115 (~90%)
  strictly watertight, 0 crashes, every pipeline claim confirmed 1:1. Report:
  `docs/repair-benchmark-strict-watertight-2026-09.md`.
- **Repair benchmark corpus published as a release asset.** The ~6 GB 115-mesh
  corpus is now downloadable via `scripts/fetch_benchmark_corpus.sh` (split
  `.tar.gz` on the `benchmark-corpus-v1` GitHub release), so a fresh machine
  can reproduce the benchmark without re-scraping Thingi10K.
- **Optional manifold3d cross-validation layer.** The benchmark harness
  independently re-checks each repaired mesh with manifold3d next to
  `defects.detect()` (115/115 agreement on the corpus); the verdict is `n/a`
  when manifold3d is unavailable. `tests/test_manifold3d_watertight.py`.
- **GUI repair preview/UX.** A color-coded defect view (red = non-manifold,
  orange = flipped winding, yellow = degenerate face) in the interactive
  before/after viewer, and a "What changed" repair-log panel (holes closed,
  non-manifold edges fixed, faces removed, components, stage 2).
- **Opt-in experimental options.** `--experimental-edge-tiebreak` (11-feature
  classifier head — base features + the five strongest scan signals) and
  `--experimental-join-components` (moves small components onto the nearest
  larger one instead of deleting them) are available from the CLI and the GUI
  (batch-wide checkboxes). Both are opt-in; the default behaviour is
  unchanged.
- **Synthetic defect injection tool.** `scripts/defect_injector.py` corrupts a
  mesh with selectable defect types (`--hole`, `--non-manifold`,
  `--self-intersect`, `--flipped-normal`, `--degenerate`) and writes a broken
  copy; validated by `tests/test_defect_injector.py`.

### Changed

- The v2 (experimental) classifier is the default engine, and the per-type
  Stage 1 tuning now uses a **near-boundary classic-agreement confidence
  fallback**: a coin-flip head decision with a strong agreeing classic
  inherits classic's confidence so the tuning gate is not silently disabled.

### Fixed

- **`artec_metal-nut.stl` scan-corpus regression (103 → 102 → 103).** The
  retrained head collapsed this organic scan's confidence to 0.022 (a
  coin-flip below `ORG_TUNE_GATE`), disabling organic tuning; the fallback
  restored it to watertight (103/115, zero new regressions).
- **`defect_type_colors` false positives on clean meshes.** Perpendicular
  neighbours (e.g. a cube) were wrongly flagged as flipped; the test is now a
  normalized cosine against self-consistent neighbours, so a consistently
  wound closed mesh has zero coloured faces.

### Security

- **3MF zip-bomb guard.** Every 3MF archive entry is read through a bounded
  helper that checks the zip header's declared uncompressed size (cap: 1 GiB)
  before reading, so a crafted archive fails with a controlled error instead
  of exhausting memory.
- **python-build-standalone downloads now SHA-256-verified** in
  `scripts/build_appimage.sh`. See `docs/security-audit-2026-09.md` for the
  full scan and the remaining low recommendations.

### Experimental evaluations (documented, NOT integrated)

Feature experiments that measured negatively are documented in the README
"Classifier methodology" section and the corresponding docs — Weinmann et al.
(2015) eigenvalue descriptors, local roughness/geometrical/statistical
tie-breakers, and MeshCNN edge features — the default classifier is unchanged
by them.

## [0.2.8] - 2026-09-19

### Changed

- **`mesh_classifier_v2` (experimental) is now the default classifier engine**, replacing `classic`.
  Measured on the 16-mesh real-world corpus: experimental scores 11/12 correct vs classic's 7/12
  (mechanical recall 2/7 -> 6/7, organic 5/5, no regression; leave-one-out CV 37/40 vs 35/40).
  `--classifier-engine classic` (or `SUTURA_CLASSIFIER_ENGINE=classic`) still forces the old engine;
  an invalid engine value or any exception/invalid result from the experimental engine still falls
  back to classic silently, as before -- only the default direction changed.
- Install scripts (`install.sh`, `install-macos.sh` flat and package layouts, `scripts/build_appimage.sh`)
  now ship `mesh_classifier_v2.py`, which was previously missing from all three -- without this fix the
  default flip would have been a silent no-op on installed copies.
- `scripts/calibrate_classifier.py`'s own default engine flipped to match; its synthetic 28-mesh
  calibration set shows no regression either way (28/28 both engines).

## [0.2.7] - 2026-09-19

### Added

- **Unit-mismatch detection.** Warns when a mesh's scale suggests non-millimeter units (inch/cm) or
  a degenerate/empty mesh, with 3MF `<model unit="...">` declared-unit precedence over the heuristic.
  Surfaced as a `WARNING:` line in `--human` output and in the GUI report.
- **Repair budget.** `--max-geometry-change` (%) and `--max-risk` (0-100 score) gate whether a repair
  is saved: exceeding either threshold produces a `budget_declined` status instead of silently saving,
  with an interactive TTY confirmation prompt or `--force` to save anyway. The worst object drives each
  metric on multi-object 3MF. GUI adds budget spinboxes to the repair-mode dialog and a re-run-with-force
  prompt for declined files in the batch summary.
- **Per-object Stage 2 for multi-object 3MF.** Every closed object in a multi-object 3MF now gets its
  own manifold3d watertight pass, not just object 0. New aggregates `objects_watertight` /
  `objects_stage2_ok`; classification now considers every object's outcome, not just object 0's;
  `--human` and the GUI report per-object Stage 2 verdicts.

### Fixed

- **Layered/duplicated-vertex 3MF objects (e.g. Bambu/Orca exports) no longer get destroyed by Stage 1
  on macOS.** `stage1_chain` and `delete_fallback_chain` now run a second `meshing_remove_duplicate_faces`
  pass immediately after `meshing_remove_duplicate_vertices`: vertex dedup on a layered mesh is what
  *creates* the duplicate faces, and without the second pass `meshing_repair_non_manifold_edges` saw a
  per-edge face soup and deleted the mesh entirely (`"all faces are degenerate"`). See
  `docs/stage2-3mf-per-object.md` (Investigation A) for the full root-cause trace.

## [0.2.6] - 2026-09-19

### Added

- **Repair Health / Repair Risk scoring (`sutura/repair_score.py`).** A new,
  additive scoring system independent of the classic confidence score.
  `repair_health` (0–100) measures final-mesh soundness (watertight + no
  non-manifold edges + no self-intersections + no remaining holes) and
  `repair_risk` (0–100) measures how much the repair altered the mesh
  (face/vertex/component/volume deltas). A two-axis status label
  (`safe` / `review` / `caution` / `failed` / `unavailable`) is derived from
  the Health/Risk tier combination via a config lookup table, so the two axes
  never collapse to a single threshold. Weights and thresholds live in
  `sutura/repair_score_config.json` (shipped; optional user override via the
  same mechanism as `updater.py`'s config). Scoring is fail-silent — any
  missing/invalid metric is skipped (weight redistributed) and scoring never
  breaks a repair. Reported as `repair_health` / `repair_risk` /
  `repair_status` (+ `*_factors`) in JSON, a `Health:/Risk:/Status:` line in
  `--human`, and localized (EN/TR) in the GUI defect-panel header. Volume
  delta reuses the existing stage-1 `volume_change_percent` (no new
  computation); `components_before` and final self-intersection count were
  added to the stage-1 report. Self-intersections are measured on the stage-1
  output by reusing the existing `ms` MeshSet (no reload); stage-2 output is
  assumed self-intersection-free by construction (documented in code).
  Regression-tested (`tests/test_repair_score.py`); the module is shipped by
  `install.sh`, `install-macos.sh`, `scripts/build_appimage.sh` and the
  self-update path (`updater.py`).
- **macOS Finder Quick Action (Dolphin ServiceMenu eşdeğeri).**
  `install-macos.sh` now installs a **"Sutura — Repair"** Quick Action
  (`~/Library/Services/Sutura Quick Action.workflow`) that appears under
  Finder's right-click → *Quick Actions* for selected STL/3MF files. It calls
  the **bundled `sutura-cli`** inside a PyInstaller `Sutura.app` (found in
  `/Applications` or `~/Applications`), so it works independently of the conda
  dev environment. Feedback is a native macOS notification (no windows): a
  single file shows `Health: X/100  Risk: Y/100  Status: <label>` (the
  Repair-Health/Risk fields above), a batch shows one summary
  (`N/M repaired, K failed — see log`). Per-run logs go to
  `~/Library/Logs/Sutura/sutura-<timestamp>.log`. Non-STL/3MF files are
  skipped and reported. A Gatekeeper guard checks `com.apple.quarantine` on
  the `.app` bundle root and the binary; if present it tells the user to
  right-click → Open `Sutura.app` once first (it never strips the attribute
  itself). The workflow is registered with `pbs -update` — `lsregister`
  refuses workflow bundles (`kLSNotAnApplicationErr`). Idempotent (re-runs
  re-copy the workflow and rescan). Note: there is still **no macOS
  uninstall script** (the Linux `uninstall.sh` covers only KDE/Linux); the
  README documents the manual removal steps, including
  `~/Library/Services/Sutura Quick Action.workflow`.

## [0.2.5] - 2026-09-19

### Added

- **Repair profiles (`--profile` / GUI Profile dropdown).** Named Stage 1
  threshold presets — `mechanical`, `organic`, `scan`, `miniature`, `fast` —
  opt-in via the CLI flag or a batch-wide GUI dropdown. Only effective while
  the mode is `auto`; an explicit fixed mode wins. Default behaviour
  (`auto`, no profile) is byte-identical to before. Reported as
  `repair_profile` (JSON) and `Profile:` (`--human`).
- **macOS native launch (Spotlight).** `install-macos.sh` now creates a
  `~/Applications/Sutura.app` wrapper (launch the GUI from Spotlight with
  `Cmd+Space` → *Sutura*, no terminal), and the `Build macOS .app/.dmg`
  workflow builds the app as **`Sutura.app`** with a teal icon and proper
  `CFBundleName`/`CFBundleDisplayName`/`CFBundleIconFile` so Spotlight finds
  it by name. Idempotent.
- **Real-world calibration corpus.** `tests/real-world-samples/` grew from 3
  to 16 meshes (8 decimated Artec scans + 8 Thingi10K) with an
  `ATTRIBUTION.md` (Artec CC BY 4.0, Thingi10K varying licenses) and a
  crash-regression harness `tests/test_real_world_corpus.py` (16 meshes,
  0 crashes).

### Fixed

- **macOS .dmg was 70 MB bigger than necessary.** The CI build bundled
  `scipy` (~34 MB × 4 tools) although nothing imports it — removed from the
  workflow's pip install and the PyInstaller hidden imports.
- **`Build macOS .app/.dmg` Info.plist step would fail.** `Add
  :CFBundleIconFile` errored ("Entry Already Exists") because PyInstaller's
  `--icon` already sets it — now a `Set`.

### Changed

- **Mesh type-aware repair — classifier bias investigated (no fix).** A
  4th "planar_fraction" signal was measured on the corpus but does **not**
  separate scanned mechanical parts from organic scans (organic statues
  carry large flat bases → higher planar fraction than the mechanical parts;
  the dihedral `flat` band overlaps and a threshold would regress low-poly
  synthetic organics). Documented as a hard geometry-only limitation; the
  16-mesh corpus keeps the behaviour visible for future classifier work.
- **SuturaGUI → Sutura naming.** Current docs/UI refer to the app as
  `Sutura` (the v0.2.3 release historically shipped `SuturaGUI.app`).

## [0.2.3] - 2026-09-19

### Added

- **Bundle-aware path resolution for a standalone macOS `.app`.** The GUI
  now resolves the CLI and the heatmap/before-after/viewer renderers to
  sibling executables in a PyInstaller bundle (`gui.py` `_bundle_tool` /
  `_script_cmd`, active only under `sys.frozen`), and `repair.py` resolves
  `manifold_bridge.py` next to the bundled CLI first so **stage 2
  (manifold3d) runs from a self-contained app** with no system install
  (`_resolve_bridge`). Outside bundles the historical behaviour is
  unchanged. Proven with a frozen-GUI spike under an empty `HOME`/`SUTURA`:
  `stage2_bridge_available=True`, watertight repair, confidence 93/100.
- **macOS `.dmg` packaging (unsigned).** A `Build macOS .app/.dmg` workflow
  builds a self-contained `SuturaGUI.app` and an UDZO
  `Sutura-vX.Y.Z.dmg` on each `v*` tag. The .dmg is **unsigned** (no Apple
  Developer Program / notarization yet): first open shows macOS's
  *"unidentified developer"* warning — use right-click → Open or
  `xattr -dr com.apple.quarantine SuturaGUI.app`.

### Changed

- **Mesh type-aware repair — scan-corpus validation + documented
  limitation.** Validated on a 115-mesh real-world scan corpus (52 Artec
  STL scans, 60 Thingi10K, 3 repo samples): 0 crashes, ~90% fully
  watertight. The README now documents that scanned **mechanical** parts
  (screws, gears, crankshafts) are frequently read as **organic** with
  high confidence because scan noise reads as gentle curvature — a known
  heuristic limitation, not a defect.

### Dependency

- manifold3d 3.5.3, trimesh 5.1.0 (Dependabot PRs #6/#5, already merged to
  main; the v0.2.2 pins were 3.5.2 / 5.0.0).

## [0.2.2] - 2026-09-18

### Fixed

- **Pre-push security scan now actually scans on macOS.** The hook's
  `SEARCH_PATTERNS` used `BEGIN (RSA |OPENSSH |EC |DSA |)PRIVATE KEY`, whose
  trailing empty alternation branch (`|)`) is rejected by BSD grep
  ("empty (sub)expression"), so on macOS every grep invocation failed and
  the secret-scan step silently reported "clean" without scanning anything.
  The group is now optional (`BEGIN (RSA |OPENSSH |EC |DSA )?PRIVATE KEY`),
  a valid POSIX ERE that matches identically on both BSD and GNU grep.
- **Mode suggestions now cover simple hole defects.** The gentle
  "a step up can be tried if unsatisfied" tip (`sug_holes_few`) fired only
  for 3+ holes, so a classic broken cube with a single missing face
  (1-2 holes) got **no** suggestions at all after Analyze. It now fires
  for any input with at least one hole. Regression-covered in
  `tests/test_suggestions.py`.

## [0.2.1] - 2026-08-30

A maintenance release that hardens the repair pipeline and the community
loop. The two dominant measured bottlenecks on large meshes are gone
(mesh classifier ~24x, bad-coordinate scan ~160x — a 325k-face mesh now
repairs in ~5.2s instead of ~9.7s), the engine records an anonymous usage
history for community-driven tuning, the graphify knowledge graph is wired
in for AI-assisted development, and the `-o` output flag — documented but
previously dead — now actually works.

### Performance

- **Mesh classifier vectorized (~24x).** `_dihedral_stats` (edge→face
  pairing + per-edge scalar numpy) is now a single argsort-based bulk
  numpy pass; bit-identical results verified on an 18-scenario regression.
- **Bad-coordinate scan vectorized (~160x).** Binary STL records are read
  in one `np.fromfile` bulk read instead of a per-record Python loop; all
  error messages and the ASCII/OBJ/3MF branches are unchanged.
- Combined on a 325k-face scan mesh: total repair 9.72s → 5.24s (~46%).

### Added

- **Anonymous usage history (opt-out).** `sutura/history.py` records purely
  technical repair data (mesh sizes, defect counts, classifier outcome,
  mode, timing, geometry-only fingerprint) to
  `~/.local/share/sutura/history.jsonl`. No file names, paths, user data.
  CLI `--no-history`, GUI first-run checkbox, `sutura export-history`
  (summary + full JSON in one command).
- **Graphify knowledge graph integration.** OpenCode plugin nudges toward
  the graph (`graphify-out/`), post-commit/post-checkout git hooks keep it
  fresh, plus a graph.json merge driver.
- **Contributing section + maintainer note** in the README (EN/TR).

### Fixed

- **`-o/--output` flag now works.** It was parsed but never passed to the
  repair path, so output always went to `<input>_fixed`; documented README
  behavior now matches reality (including multi-object 3MF → the given path).
- **Dolphin multi-file history consistency.** `open.sh` now honors the GUI's
  `history_enabled` config for its direct CLI path.
- **Dead-code cleanup** (no behavior change): duplicate `faces_after` key,
  unused `tri_idx`, unused imports (`is_stage2_skipped`, `numpy`, `QAction`,
  `QImage`).

## [0.2.0] - 2026-08-26

The **interactive viewer** release: the biggest feature step since 0.1.0.
A CPU-rendered interactive 3D view (rotate/zoom + surface-deviation diff)
joins the static before/after comparison, the mesh classifier gets a
correctness fix with recalibrated thresholds, `.obj` files become
first-class citizens (Dolphin + CLI) with an explicit material-loss
warning, the stage-1 repair pipeline gets a measured hole-closing
overhaul, auto-update learns to respect the new license, and the project
switches to the **PolyForm Noncommercial 1.0.0** license.

### Added

- **Interactive 3D viewer (v0.2).** The before/after dialog now has a
  **Static / Interactive** switch: **drag** rotates the mesh, the **mouse
  wheel** zooms, a low-poly LOD is rasterized per frame while dragging
  (background thread, latest frame wins, no pymeshlab in the GUI process)
  and a full-resolution frame lands ~300 ms after you stop. The interactive
  view opens at the same defect-facing camera as the static comparison. A
  second toggle switches the repaired side between the **Repair status**
  colour map and a **Surface deviation** map: each face is coloured by the
  distance of the repaired surface to the original surface (quantile-scaled
  navy → cyan → yellow → red ramp), still-broken defects stay drawn on top
  in orange, and the global Hausdorff max is shown in the dialog. The
  dataset is built lazily by a new subprocess
  (`sutura/viewer_data_render.py`) on first use and cached for the dialog's
  lifetime; it decimates both meshes to a ~3.5k-triangle interactive LOD
  (`LOD_TARGET=3500`, tuned against the 75-model corpus: median ~71 FPS,
  no model under 30 FPS at 720×540) and computes the per-vertex distance
  with pymeshlab's nearest-surface-point filter — no new dependencies (no
  scipy/rtree). `sutura/viewer_common.py` (pure numpy+Qt) shares the
  camera/defect helpers between the static renderer and the viewer.
  Regression-tested (`tests/test_viewer_data.py`, `tests/test_heatmap.py`).
- **`.obj` support.** The Dolphin right-click menu (`model/obj` MIME) and
  the CLI now accept `.obj` files. Because the repair rebuilds the mesh
  (vertices + triangles only), material/texture references (`mtllib` /
  `usemtl`) are not preserved: instead of dropping them silently the report
  carries `material_discarded` (JSON) and a `Material:` line (`--human`) —
  a cosmetic field that never changes the category. WRL/PLY/OFF/DAE are
  deliberately NOT added (low real demand; PLY/OFF have no MIME
  registration on this system and need a format-specific
  `scan_bad_coordinates` fix; DAE loses its triangles in pymeshlab).
  Tests: `tests/make_broken_obj.py`, `tests/test_obj_repair.py`.
- **Auto-update license boundary.** Auto-update no longer silently crosses
  the v0.2.0 license boundary: `crosses_license_boundary()` stops a v0.1.x
  install from jumping to a license-changed release, shows the new terms in
  an informational dialog instead of installing, and offers the releases
  page (shown once for background checks, every time on a manual click).
  Covered by `tests/test_updater.py`.
- **Sponsor section.** `README.md`/`README.tr.md` gain a permanent Sponsors
  section and the repo gets a `.github/FUNDING.yml`.

### Fixed

- **mesh_classifier edge-pairing bug + recalibration.** The dihedral-angle
  edge→face pairing used `i // 3` but the concatenated edge array is
  block-stacked per index, so the wrong faces were paired and produced
  garbage near90/coplanar signals. Fixed to `i % F` (verified 7680/7680
  against trimesh) and split the old `coplanar` band into a true `flat`
  (<1°) and a `gentle` (1–15°) curvature band so smooth high-poly organics
  never read mechanical. Thresholds recalibrated on the labeled set
  (accuracy 0.893 → 1.000, organic recall 0.812 → 1.000, unknown-rate
  0.107 → 0); `ORG_TUNE_GATE` moved 0.55 → 0.70 (the old "organic
  confidence tops out at ~0.62" claim was a bug artifact).
- **confidence.py tuning-gate divergence.** The hardcoded
  `_MECH_GATE`/`_ORG_GATE` mirrors in `sutura/confidence.py` were replaced
  with a call-time import from `repair.py` (`_tuning_gates()`), so the
  confidence score can never drift from the real repair thresholds again.
  This produced a real inconsistency before: for organic confidence in
  `[0.55, 0.70)` validate/`--dry-run` applied a `below_confidence_gate`
  penalty while a real repair reported `tuning_applied: true`.
- **Distribution gaps found in the pre-release audit.** `LICENSE` was not
  being shipped by any installer (`install.sh`, `install-macos.sh`,
  `updater.py`, `scripts/build_appimage.sh`) — now distributed everywhere,
  satisfying PolyForm's terms-or-URL requirement. The AppImage bundle was
  missing `confidence.py` and `before_after_render.py` (a latent bug that
  would have broken the CLI entirely since `repair.py` imports
  `confidence` at top level); all new modules are now in every module list.
  `updater.py`'s confidence.py gap could have left a stale copy of the
  gate-sync fix behind on update.
- **Mode suggestions are now mode- and size-aware.** The suggestion logic
  moved into a pure `mode_suggestion_keys()` (was `_analysis_suggestions`):
  the low-confidence step-up tip only fires while the mode is still
  low/medium/auto (no point suggesting a step up on aggressive/extreme),
  and the extreme caveat only fires when extreme is suggested AND the mesh
  is small enough (< 20 faces) for extreme to actually delete it. Tested by
  `tests/test_suggestions.py`.
- **Interactive viewer never rendered (crash fix).** `heatmap._defect_vertex_set`
  used `verts_idx or []`, which raises ValueError on a multi-element numpy
  array. gui.py passes `np.asarray` defect indices into `prepare_render`,
  so `set_data` aborted silently in the viewer and the viewport stayed
  black for every repaired mesh. Now safe for plain lists AND numpy arrays
  in `heatmap.py` + `viewer_common.py` (regression test in
  `tests/test_heatmap.py`).
- **Stage 1 chain reordering.** `meshing_repair_non_manifold_vertices` now
  runs BEFORE hole closing (closing a hole on a mesh with non-manifold
  vertices fan-fills it and can create new non-manifold edges), and a final
  `close_holes` pass re-closes anything the debris removal re-opened. Applied
  to both `stage1_chain` and `delete_fallback_chain`.
- **Mesh-sensitive `maxholesize`.** The fixed value (1000) skipped any input
  boundary loop longer than that (VCG counts each hole edge twice), leaving
  large scan holes open. The effective value is now
  `max(mode/type base, 2 × longest input boundary loop)` — shared by the
  real repair and `--dry-run` (the plan reports the effective value). Validated
  on the 75-model corpus: 0 regressions, every partial case's residual loops
  dropped (e.g. thingi10k_117959 2757→2, Goethe_Lifemask 1059→1, Athena
  683→3), one case (penelope) gained two-manifold.
- **Interactive-viewer FPS + LOD overshoot.** The interactive LOD target
  was lowered 8000 → 3500 and the clustering threshold steps refined based
  on a measured 75-model corpus run: median 71 FPS, minimum 43.8 FPS, no
  model under 30 FPS at 720×540 (a 2.05M-face scan was ~20 FPS before).
  A QPainterPath color-group batching experiment was tried and reverted —
  it measured ~1.8× slower than the per-face path everywhere.
  `_decimate_lod` now picks the result at-or-below target that is closest
  to it (never above target, never over-collapsed when a step jumps far
  below); 17 corpus models that previously landed at 4.5–5.5k tris now land
  at 1.6–3.4k.

### Changed

- **`holes_remaining` now reports the real boundary-loop count** (was
  `boundary_edges // 2`, half the total boundary edges — a single long hole
  was inflated into hundreds/thousands of "holes"). The report's Stage 1
  `holes_remaining`/`holes_closed` (and the `--human` lines) now count actual
  loops, so a mesh with one 2,119-edge hole reports `1` instead of `1059`.
  JSON consumers comparing against previous versions will see smaller,
  correct numbers.
- **License changed to PolyForm Noncommercial 1.0.0** starting with v0.2.0
  (personal, non-commercial use stays free). The `LICENSE` file now carries
  the official PolyForm text with a note that released v0.1.0–v0.1.9
  versions remain permanently licensed under Apache License 2.0 (see the
  LICENSE file at each of those git tags). The Apache-required `NOTICE` file
  was removed — PolyForm Noncommercial does not require it, and the v0.1.x
  tags keep their own copy. The auto-update license-boundary dialog now
  names the new license explicitly.

### Documentation

- README.md / README.tr.md: interactive viewer usage, auto-update boundary
  note, classifier narrative rewritten for the 3-band model, Feature Status
  before/after row re-scored (~60% → ~75%), torture-test description
  corrected (five scenarios, including the extreme-mode self-intersecting
  pair). `.obj` support and the material-loss note added; the layered-3MF
  "13 and 26 micro-holes" example updated to the current behavior (0 and 0,
  fully closed but `stage2_skipped` for multi-object 3MF).

## [0.1.9] - 2026-08-25

Repair Confidence Score, the GUI Analyze button with mode suggestions, a GUI
drag-and-drop selection fix, and closing the v0.1.8 README documentation
debt.

### Added

- **Repair Confidence Score.** Every repair report now carries a single
  0–100 `repair_confidence` value with a High/Medium/Low label that combines
  existing repair signals (stage 2 outcome, remaining holes, classifier
  confidence, tuning status, repair mode, self-intersections, volume change)
  into one honest figure; `--human` shows it as a `Confidence: X/100 (Label)`
  line and the GUI defect-panel header adds a localized confidence segment.
  validate / `--dry-run` report an estimate instead (`estimated_confidence`),
  explicitly labelled with "actual result may differ after repair", because
  the post-repair signals do not exist yet. New stdlib-only
  `sutura/confidence.py` (`repair_confidence()` /
  `estimate_confidence_pre_repair()`), shared by the CLI and the GUI.
- **GUI Analyze button + analysis pane.** A new **Analyze** action runs the
  same read-only checks as `validate` + `--dry-run` on the selected files
  (nothing is written) and shows them in a dedicated pane above the input
  defects: detected type / mode / tuning status, hole / self-intersection /
  non-manifold / debris counts, a watertight pre-verdict, and the estimated
  confidence with its "result may differ" warning.
- **Mode suggestions.** The analysis pane lists up to three priority-ordered
  mode suggestions (e.g. an aggressive/extreme step for scan-derived holes,
  an assembly-type caution, an extreme caveat for parts under 20 faces).
  Informational only — they never change the mode automatically.
- **Button regrouping + icons.** The toolbar is now grouped into three
  logical rows (file management / analyze+mode+repair+stop /
  heatmap+before-after), with Qt standard icons for common actions, custom
  QPainter icons for Analyze/Repair, and EN/TR tooltips on every button.

### Fixed

- **GUI: drag-and-drop files are now auto-selected.** A file added by drag &
  drop (or any add path) is selected immediately, so the analysis and defect
  panels update to it right away instead of showing the previous file's
  content.

### Documentation

- Closed the v0.1.8 README documentation debt: the Feature Status table was
  re-scored against the actual codebase (Repair modes row added, before/after
  ~60%, validate ~55%, dry-run ~50%, mesh type-aware ~75%, test coverage
  ~88%, GUI ~87%) and the extreme-mode `extreme_removed_object` distinct
  error is documented.
- README.md / README.tr.md now document the Repair Confidence Score and the
  Analyze flow; screenshots regenerated (including a new analyze-panel.png).

## [0.1.8] - 2026-08-24

First **stable** 0.1.8 release. This is the sum of the two 0.1.8 beta
pre-releases: the read-only analysis tooling, the before/after comparison
(with its camera-directing and colour-scheme refinements), and the
extreme-mode reporting fix — all together for the first time. Auto-update
users are now offered this release on the stable channel.

### Added

- **`validate` subcommand.** `sutura validate model.stl` analyzes a mesh
  WITHOUT repairing or writing anything: `defects.detect()` holes /
  non-manifold regions, the mesh classifier (`detected_type` /
  `detected_confidence`), self-intersecting face count, connected components,
  signed volume (winding orientation), surface area and a `watertight`
  verdict. JSON report; `--human` for a readable one (`--defects` lists each
  defect region). Multi-object 3MF files report per-object. Exit 0 on a
  successful analysis (even a broken mesh), exit 1 on a hard error (missing /
  malformed input).
- **`--dry-run`.** `sutura model.stl --dry-run` reports what a repair WOULD
  do — detected type, the resolved mode and Stage 1 thresholds
  (`mincomponentsize`/`maxholesize`), `tuning_applied`, found holes / largest
  hole diameter / non-manifold regions / self-intersections / removable
  debris faces, and whether stage 2 would run — and writes NO output file at
  all (no `_fixed`, no temp residue). Threshold resolution is shared with the
  real repair via `resolve_mode_params`, so dry-run and repair can never
  diverge (same single-source-of-truth rule as `classification.py`).
- **Before/after comparison (introduced in beta.1, refined in beta.2).** The
  repaired view uses a **tri-state colour scheme**: **grey** where the mesh
  was never broken, a vivid **green** `(46,204,113)` where an original defect
  used to be and is now healthy, and **orange** `(255,140,60)` where a defect
  remains (the original view keeps its red `(235,60,70)` defects). Because
  repair changes the mesh topology (before/after vertex indices don't
  correspond), the green classification is **spatial**:
  `before_after_render.healed_face_mask` measures each repaired face against
  the original defect centroids' real extent (max |v - centroid| from
  `verts_idx`, × 1.5 halo — not the bbox diameter) and only counts a face as
  healed when the repaired mesh's own detect() reports no defect there. Only
  the `cap` (256) largest defects drive the green highlight, so a scan mesh
  with thousands of micro-cracks stays fast; the orange "still broken" signal
  is uncapped and always full-resolution.
- **Worst-defect zoom / balloon detail (before/after GUI).** The before/after
  dialog renders a second, smaller close-up of the WORST original defect
  region (the defect with the largest physical bounding-box diagonal, holes
  and non-manifold regions compared on the same metric). The close-up uses
  the same zoomed camera frame for both views (`heatmap.focus_frame`), so the
  original vs repaired comparison is apples-to-apples. When the original has
  no defects the detail view simply mirrors the main view.
- **The before/after camera now aims at the worst defect (defect_camera).**
  The shared camera is no longer a fixed isometric view: it is automatically
  directed toward the worst ORIGINAL defect's centroid
  (`heatmap.defect_camera`), so the defect never hides behind the mesh and
  stays visible in both the main and the detail views. The fixed
  `_ISOMETRIC` camera is used only as a fallback when the mesh is clean or
  the defect sits at the bounding-box centre. Known limitation: on extremely
  asymmetric meshes (long/thin, dumbbell-like) the defect-facing angle can
  collapse the view-space bounding box and render the mesh small — deferred
  to the v0.2 interactive 3D viewer.

### Changed

- **`updater.py` semver hardening.** `parse_version` now understands semver
  pre-release tags: a stable release sorts after any pre-release of the same
  version, so a beta tester is offered the eventual stable release instead of
  being stuck on the beta. The `/tags` fallback also skips dashed (pre-release)
  tags so it can never surface a beta to a normal user.
- **Extreme mode reports whole-object deletion distinctly.** When extreme
  mode's `mincomponentsize=20` deletes a small connected component and the
  mesh ends up with zero faces, the repair is no longer misreported as the
  generic "all faces are degenerate" / `malformed` error. It now carries
  `category=error` with the issue `extreme_removed_object` and the clear
  message "Extreme mode removed all geometry (small connected component below
  the size threshold); try a less aggressive mode (e.g. Auto)" — in `--human`
  and, localized EN/TR, in the GUI. Genuine malformed inputs keep their
  existing behaviour.

## [0.1.7] - 2026-08-22

### Added

- **Repair mode (`--mode`).** A five-step Stage 1 aggressiveness ladder,
  defaulting to `auto`: `low {8,200}`, `medium {8,1000}` (the historical
  default thresholds), `auto` (the shipped classifier + confidence-gate
  behaviour — the default, so existing users/CI see no change), `aggressive
  {12,3000}`, `extreme {20,10000}`. Fixed modes bypass the classifier for
  parameter selection (it still runs for the informative
  `detected_type`/`detected_confidence` fields, and `tuning_applied` is
  false); `auto` is byte-identical to running without `--mode`. The mode is
  reported as `repair_mode` in the JSON, a `Mode:` line in `--human`, and
  per-object for multi-object 3MF. Invalid values are rejected by argparse.
  `mincomponentsize` stays >= 8 in every mode (never lower, CI regression
  risk); note that `extreme` (mincomponentsize=20) will delete a whole object
  whose connected part has fewer than 20 faces — intended but aggressive.
- **Repair mode picker (GUI).** A small **Mode: Auto** button next to the
  heatmap/before-after buttons opens `RepairModeDialog`: a five-step
  horizontal slider (Low–Medium–Auto–Aggressive–Extreme) with a live,
  localized one-line description per step (the Extreme step honestly warns it
  can delete objects smaller than 20 faces) and OK/Cancel. The mode is stored
  **batch-wide** on the main window (`self._repair_mode`, default `auto`), not
  per file, and `RepairWorker` passes it to the CLI as `--mode <mode>`, so the
  chosen mode appears in the JSON `repair_mode` of every report in the batch.
  Fully localized EN/TR. Screenshots regenerated (English UI).
- **Extreme extra passes (Stage C).** In `--mode extreme` only, the repair now
  runs two extra Stage 1 steps after the main chain: it selects and removes
  self-intersecting faces (`compute_selection_by_self_intersections_per_face`
  → `meshing_remove_selected_faces` → `meshing_remove_unreferenced_vertices`)
  and then runs the main chain one more time with the same thresholds to close
  the holes / drop the debris that removal exposed. When the mesh has no
  self-intersections the extra passes are skipped harmlessly. The report gains
  `extreme_passes_applied` (true/false, every mode) and, when the passes ran,
  `self_intersections_found`/`self_intersections_removed`; `--human` shows an
  "Extreme passes" line. Deliberately **not** a full remesh —
  `meshing_isotropic_explicit_remeshing` is out of scope because it can
  unpredictably change topology. The other four modes are untouched (verified
  by the auto-equality regression test). Torture harness gained a
  self-intersecting-pair scenario run in extreme mode (self-intersections
  78 → 0, `extreme_passes_applied=True`).

### Changed

- **Confidence gate for mesh-type-aware tuning (Aşama 3).** A classified
  mesh only gets its tuned Stage 1 thresholds (`mincomponentsize`/
  `maxholesize`) when the classifier is reasonably sure. The gate is
  class-specific — mechanical `MECH_TUNE_GATE=0.75`, organic
  `ORG_TUNE_GATE=0.55` (named constants in `repair.py`) — because the
  organic confidence is structurally capped at ~0.62 (the sigmoid over the
  coplanar metric cannot exceed it), so an organic gate cannot be as high as
  a mechanical one. Below the gate the detected type is still reported, but
  the conservative default thresholds (`mincomponentsize=8`,
  `maxholesize=1000`) are used. The report gains `tuning_applied` (true/
  false) in the JSON and a "- tuned / - default thresholds" note in
  `--human`; the GUI defect panel shows the same status, localized EN/TR.
  Measured with the calibration harness on the 28-mesh labeled set: 20/28
  meshes stay tuned, 5 classified meshes fall back to defaults (3 borderline
  mechanical at ~0.72 — lattice/damaged box — and 2 noisiest organic blobs
  at ~0.50–0.52), matching the Aşama-3 analysis.
- **README.tr.md çeviri kalitesi gözden geçirildi.** (Translation quality
  pass on the Turkish README: natural phrasing instead of literal
  machine-translation, consistent technical terminology — e.g. "mesh" /
  "su geçirmez" / "boolean" kept consistently, feature-status table and
  recent additions (diff/version flags, before/after) verified against the
  English source; markdown structure unchanged.)

### Added

- **Before/after mesh comparison (GUI).** A **Show before/after** button next
  to the heatmap button renders the selected file's original and repaired
  meshes with the SAME shared isometric camera frame — identical framing and
  scale, so the toggle is a true comparison — and opens a dialog with a single
  image area plus a toggle button that flips between "Original" and
  "Repaired". On-demand only (never auto-rendered), cached per file, with the
  same loading state as the heatmap button. Deliberately a static CPU
  rasterizer pair, not an interactive 3D slider: offscreen GL draw calls
  segfault on headless systems (the same constraint that made the heatmap a
  CPU renderer), so the comparison is a click-toggle between two rasterised
  PNGs. The render runs in a subprocess (`before_after_render.py`, same
  isolation rule as `heatmap_render.py`): pymeshlab never touches the GUI
  process. Renders the first object for multi-object 3MF, matching the defect
  panel. `heatmap.py` gained an optional shared `frame` (camera) so two meshes
  render identically; the existing heatmap render path is unchanged.

### Changed

- **Mesh classifier confidence is now a signed-margin score
  (`sutura/mesh_classifier.py`).** Instead of two hard per-metric thresholds
  with a confidence that measured distance to a single boundary (and a flat 0
  for `unknown`), each metric (near-90° dihedral fraction, coplanar fraction)
  is mapped through a smooth sigmoid and the two signals are combined
  (mechanical = OR of the two, organic = AND). The decision is taken on the
  margin between the two memberships (`mechanical >= 0.7`, `organic >= 0.5`),
  which removes the hard `[55,60]` near90 discontinuity — barely-over-60
  organic meshes (low-poly spheres/tori) now fall to `unknown` instead of
  being wrongly tuned as mechanical. `unknown` no longer returns a flat 0: it
  carries the proximity to the nearer class plus `metrics['leaning']`. The
  public return shape `{'type', 'confidence', 'metrics'}` is unchanged, so
  `repair.py` / `gui.py` / the CLI JSON contract are untouched. Measured on a
  new labeled synthetic set (28 meshes): mechanical precision went 0.857 →
  1.000 (the two false positives became `unknown`), recall stayed 1.000, and
  mean confidence on correct predictions went 0.203 → 0.739 while there are
  no wrong non-`unknown` predictions left.
- **Classifier calibration harness (`scripts/calibrate_classifier.py`) + labeled
  synthetic set (`tests/make_classifier_set.py`).** A deterministic set of 28
  labeled meshes (boxes/gears/lattices/extruded profiles vs UV/ico spheres,
  torus, capsule, noisy blobs — several LODs each plus damaged variants) and a
  harness that reports per-class precision/recall, unknown rate and the
  confidence distribution on correct/wrong/unknown predictions. Used to
  baseline the classifier and to verify the signed-margin change above; the
  per-type repair thresholds (`mincomponentsize`/`maxholesize`) were **not**
  changed by this work.
- **Self-contained dark theme (Fusion + QPalette).** The GUI no longer
  depends on the system Qt platform theme for its look. Right after the
  `QApplication` is created it applies Qt's bundled `Fusion` style plus a
  dark `QPalette` (`_dark_palette()` in `gui.py`), so it renders the same on
  every platform and Qt version — including when the system Qt differs from
  the bundled PySide6 Qt and the native KDE/Breeze theme is unavailable.
  The accent (highlight/link/button) is **teal `#14b8a6`**: it matches the
  teal already used for the Repair button, the progress bar and the update
  arrow, reads as "repaired / healthy" for a mesh-repair tool, and keeps a
  single consistent brand colour across the whole UI instead of introducing a
  second one. The status-row version label was bumped to a lighter dimmed
  grey (`#9aa4ae`, a dimmed WindowText variant) so it stays subtle but reads
  clearly on the dark background. The screenshot generator applies the same
  theme so `assets/*.png` always match the real GUI. The Qt version-check for
  the native file dialog is untouched — this theme is a separate, parallel
  layer.

### Fixed

- **GUI would not start when the system Qt version drifted from the bundled
  PySide6 Qt.** The GUI mixes the system plugin directory
  (`/usr/lib/qt6/plugins`) into `QT_PLUGIN_PATH` for the native KDE file
  dialog, but the system platform plugins (libqwayland.so/libqxcb.so) are
  built against the system Qt's private API. After a system Qt upgrade that
  no longer matches the bundled PySide6 Qt (e.g. system 6.11.2 vs bundled
  6.11.1), loading them aborted startup with `undefined symbol
  ... Qt_6_PRIVATE_API` — "Could not load the Qt platform plugin". The GUI
  now compares the system Qt version (via `qmake`) with the bundled
  PySide6 `qVersion()` and only mixes in the system plugins on an exact
  match; on a mismatch it keeps Qt on its own bundled plugins, so the GUI
  opens with Qt's embedded file dialog instead of failing hard.

### Changed

- **README audit + Feature Status section.** Both `README.md` and
  `README.tr.md` now carry an honest "Feature status" table before the
  Requirements section, giving a maturity percentage per major area (STL
  repair, 3MF multi-object, GUI, CLI, batch, defect detection, heatmap, mesh
  classification, cross-platform, auto-update, Dolphin, OrcaSlicer plugin,
  tests) with a one-line reason per figure and the known limitation that
  explains why it is not 100%. OrcaSlicer is explicitly marked experimental
  (~35%). The READMEs were also corrected where they had drifted from the
  code: the mesh-classifier table now shows the actual `mechanical`
  `mincomponentsize` of 8 (was stale at 4), and the CLI/geometry-diff docs now
  cover the previously-undocumented `--diff` and `--version` flags plus the
  before/after geometry fields and the GUI diff line.
- **Heatmap now uses a three-point lighting model.** The CPU rasterizer
  (`heatmap.py`) previously filled every non-defect face with a single flat
  grey, which made the red defect regions hard to read and gave the mesh no
  visual depth. Faces are now shaded with vectorized Lambertian diffuse from
  three fixed lights — a bright key (camera direction), a low fill (camera
  left) and a rim/back light (silhouette edges) — plus an ambient term, so
  surface curvature and edge lines read clearly. Defect (red) faces get a
  38% lighting modulation over the base red, keeping the "hot" region
  clearly red from every angle while still shading it. All normal/light math
  is numpy-vectorized (no per-face Python loop). Benchmark on the torture
  scan mesh (1.24M tris): ~224k tris/s vs ~244k tris/s before — about 8%
  slower, well within tolerance. Pure numpy/QPainter, platform-independent
  (macOS included). Screenshots regenerated.

## [0.1.6] - 2026-08-20

### Added

- **Before/after geometry diff in reports.** Each repair now reports the
  actual before/after geometry numbers alongside the existing volume warning:
  `volume_change_percent` (now signed, indicating direction), plus
  `surface_area_before`/`after` and `surface_area_change_percent`
  (computed directly from the triangles, so it is meaningful on open meshes
  too), and `vertices_before`/`after`, `faces_before`/`after`. These are
  always in the CLI JSON (`stage1`) and shown per object for multi-object
  3MF files; `--human --diff` also prints them. The GUI defect panel shows a
  one-line summary ("Volume: +0.12% · Surface: -2.37% · Vertex: 12→9") above
  the defect list, localized (EN/TR).
- **`--diff` CLI flag.** With `--human`, also print the before/after geometry
  diff. JSON always includes the fields.
- **Defect heatmap (GUI).** A "Show heatmap" button under the defect panel
  renders the selected mesh with hole/non-manifold regions highlighted red on
  a neutral grey mesh, shown as a clickable thumbnail that opens a larger
  zoom dialog. Rendering is on-demand (never automatic) and cached per file.
  For multi-object 3MF files it renders the first object, matching the defect
  panel's existing first-object behaviour.
  - `defects.detect(..., with_indices=True)` now also returns each defect's
    `verts_idx`/`faces_idx` index lists for highlighting; the CLI JSON
    contract is unchanged (default is `with_indices=False`).
  - **Rendering is a CPU rasterizer, not GPU OpenGL.** An offscreen
    `QOpenGLContext` + FBO pipeline was prototyped first, but raw GL draw
    calls (`glDrawArrays`/`glDrawElements`) segfault on headless systems
    (verified on an NVIDIA box without a display) and can be unavailable in
    the AppImage/macOS CI. The shipped `heatmap.py` depth-sorts and fills
    faces with Qt's raster paint engine (~250k tris/s on CPU), which works
    everywhere and never crashes the GUI.
  - **The render runs in a subprocess** (`heatmap_render.py`), not a GUI
    thread: using pymeshlab inside a Qt worker thread while a `QMainWindow`
    exists corrupts the heap at interpreter shutdown (PySide6 6.11 + Python
    3.14). The subprocess isolates pymeshlab entirely and keeps the GUI
    responsive and crash-free. Failure falls back silently to the text-only
    defect panel.
- **AppImage packaging.** `scripts/build_appimage.sh` bundles two relocatable
  python-build-standalone runtimes (Python 3.14 for stage 1 + GUI, 3.11 for
  stage 2) plus the app modules and builds `dist/Sutura-x86_64.AppImage` with
  appimagetool (which carries its own `mksquashfs`, so no system package is
  needed). The AppRun dispatch exports `SUTURA_DIR`/`SUTURA` so the bundled
  copy finds its venv311/bridge/CLI without code changes. In AppImage mode
  (detected via the `APPIMAGE` env var) the GUI skips first-run/background
  update checks and the update button shows a "download from releases"
  message instead of self-updating, since a read-only squashfs cannot be
  written to. Also fixes an updater crash where the VERSION fallback read a
  host install dir that does not exist on an install.sh-free AppImage setup.
- **Muted version number in the GUI status row.** A small `vX.Y.Z` label (from
  the shared `VERSION` constant, not hardcoded) now sits at the right end of
  the status row, per KDE HIG status-bar conventions. The screenshot generator
  now points at the repo's own `gui.py` instead of an installed copy.
- **Experimental OrcaSlicer plugin.** `orcaslicer-plugin/` adds a
  self-contained OrcaSlicer script plugin that repairs a file straight from
  the slicer by shelling out to the installed Sutura CLI in a background
  thread. It is offered as a starting point and is **untested in a real
  OrcaSlicer**: the Python plugin system it targets exists only in OrcaSlicer
  nightly builds / releases newer than 2.4.2, which the project has not run, so it has
  only been stub-tested against the documented API. It repairs a configured
  target file (not the selected model — `execute()` takes no selection) and is
  Linux-only (relies on the `install.sh` CLI path `~/.local/bin/sutura`).
- **Dependabot config** for pip and GitHub Actions so dependencies and
  workflow actions are kept up to date automatically.

### CI

- **AppImage build & publish workflow.** A new workflow builds
  `dist/Sutura-x86_64.AppImage` on every `v*.*.*` tag push (and via
  `workflow_dispatch` to backfill older tags) and uploads it as an asset to
  the corresponding GitHub Release.
- **Dynamic GitHub status badges.** READMEs now show live badges backed by
  real endpoints (CI and AppImage-build workflow status, latest release,
  license, downloads, contributors, top language, repo size, commit activity).
- **CodeQL code-scanning workflow.** Standard CodeQL Action for Python runs on
  push to `main`, pull requests and a weekly schedule, uploading SARIF results
  to code scanning; adds the CodeQL workflow-status badge to both READMEs.

### Dependencies

- Bump `actions/checkout` from 4 to 7.
- Bump `actions/setup-python` from 5 to 7.

### Security

- **Restricted CI token permissions.** `.github/workflows/ci.yml` now sets
  `permissions: contents: read` at the workflow level. The CI jobs only check
  out the repo and run tests, so they no longer receive the default
  broad-scope GITHUB_TOKEN (fixes GitHub CodeQL
  `actions/missing-workflow-permissions` warning).

## [0.1.5] - 2026-08-19

### Added

- **SECURITY.md** with a vulnerability reporting policy.

### Fixed

- **Dolphin service menu submenu (`X-KDE-Submenu=Sutura`)** prevented the
  action from appearing for `.stl` files on some KDE/Plasma versions; the
  submenu was removed and the action now shows at the top level for both
  `.stl` and `.3mf`.
- **Mesh classifier regression.** The `mechanical` class initially lowered
  Stage 1's `mincomponentsize` to 4, which let small/degenerate meshes (e.g.
  the 2-triangle case in `tests/test_adversarial.py`) survive the debris
  cutoff and be "repaired" instead of rejected — a CI regression (the
  `degenerate` adversarial scenario). `mincomponentsize` is now kept at the
  default 8 for `mechanical`; the type still tunes `maxholesize` (300).

## [0.1.4] - 2026-08-19

### Added

- **Batch repair summary.** When more than one file is repaired in one run,
  the CLI and GUI now show a breakdown of how many files came out watertight,
  with warnings, or failed, and which kinds of warnings/errors occurred
  (volume change, Stage 2 skipped, partial repair, malformed input).
- CLI `--human` prints the per-issue counts under the batch summary; JSON mode
  adds `summary.issue_counts` and a `category`/`issues` field on each file.
- GUI shows a summary strip when a batch finishes (watertight / warnings /
  failed counts) with a clickable "show issues" detail in the log.
- `sutura/classification.py`: stdlib-only single source of truth for result
  classification, shared by the CLI and the GUI (no numpy/pymeshlab import).
- **Defect detection (`sutura/defects.py`).** The input mesh's holes and
  non-manifold regions are now reported per defect (centroid, hole diameter,
  size) instead of just aggregate counts. The defect list is always included
  in the CLI JSON output; `--human` shows it only with `--defects` to avoid
  noise. The GUI shows the selected file's defects in a dedicated panel below
  the log. `defects.py` is stdlib+numpy only (no pymeshlab/trimesh).
- **Mesh type-aware repair (`sutura/mesh_classifier.py`).** Heuristically
  guesses whether an input is mechanical or organic from dihedral-angle
  geometry (numpy, not ML) and, on high-confidence cases, tunes two Stage 1
  thresholds: `mincomponentsize` (debris cutoff) and `maxholesize` (hole
  fill). Ambiguous meshes report `unknown` and keep the historical default
  parameters (safe fallback). The detected type is shown in the GUI defect
  panel header, as a `Type:` line in `--human`, and as
  `detected_type`/`detected_confidence` in the JSON report.
  **Experimental:** the per-type threshold values (mechanical 4/300, organic
  12/1000) are uncalibrated starting points, deliberately conservative and
  reversible. Curved-but-mechanical parts (cylinders, fillets) are not
  classified and keep defaults.
- `README.tr.md`: Turkish translation of the README (new, kept in sync with
  README.md).

### Changed

- **Breaking:** a mesh that stage 1 closes but stage 2 does not confirm (Stage
  2 skipped, Stage 2 error, or Stage 2 never ran - e.g. the macOS/conda
  in-process fallback being unavailable) is now reported as a **warning**, not
  watertight. "Watertight" is only claimed when stage 2 actually validated the
  closed solid. Stage 2 processing errors (a genuine manifold3d/bridge
  failure, not a skip) are also now classified as **warning** rather than
  error - the stage 1 output is still written, so a stage 2 error should not
  hard-fail a batch. `stage2_skipped` and `stage2_error` remain distinct issue
  codes and are counted separately in `summary.issue_counts`.

### Fixed

- `install.sh` was not copying `classification.py` (and `updater.py`) into
  the installed `~/.local/share/sutura/` directory, so the installed CLI and
  GUI failed with `ModuleNotFoundError` right after install. Both modules are
  now copied with the rest of the application files.
- `install-macos.sh` had the same gap (it only copied `repair.py`,
  `manifold_bridge.py`, `gui.py`, `__init__.py`), so a macOS install was also
  missing `classification.py` and `updater.py`. Both are now copied in the
  flat layout and the importable package layout.

## [0.1.3] - 2026-08-19

### Added

- **Opt-in update checker.** Sutura checks GitHub releases for newer versions
  once a week when enabled (one request to GitHub, no other data sent); the
  GUI shows an update arrow when a newer release exists.
- **Self-update with automatic backup and rollback.** Updating backs up the
  current installation first and provides a rollback guarantee if the update
  fails.

## [0.1.2] - 2026-08-18

### Changed

- License changed from MIT to Apache License 2.0; added a `NOTICE` file
  (Apache 2.0 §4d) preserving attribution.

## [0.1.1] - unreleased

### Added

- macOS support (Apple Silicon / Intel) via a single conda environment.
- `install-macos.sh`: automated setup (Homebrew + Miniforge + `sutura-env`
  conda env with pymeshlab/manifold3d/trimesh/PySide6, CLI and GUI
  launchers), macOS-only.
- Stage 2 runs in-process when manifold3d is importable from the current
  interpreter (single-environment installs such as macOS/conda), instead of
  only through the fixed Linux `venv311` subprocess.

### Changed

- GUI resolves the `sutura` CLI flexibly (`$SUTURA` env, the Linux wrapper,
  or the bundled `repair.py` run with the current interpreter).

### Fixed

- The GUI no longer hangs on "Repairing…" if the CLI is missing: subprocess
  launch failures are caught and reported as a clear per-file error, and the
  worker always finishes (`all_done` emitted).

## [0.1.0] - unreleased

Initial public release. Two-stage mesh repair for STL/3MF files on Linux.

### Added

- **Two-stage repair engine.** Stage 1 (PyMeshLab/VCG) removes duplicates and
  degenerates, repairs non-manifold edges and vertices, orients faces, closes
  holes and drops tiny debris. Stage 2 (manifold3d) rebuilds the closed mesh
  as a watertight solid and merges overlapping shells.
- **Multi-object 3MF support.** Every object mesh is repaired independently
  and written back into the archive, so no object is lost; per-object results
  are reported.
- **Batch repair (CLI and GUI).** Repair many files in one run with a
  summary (fully repaired / warnings / failed) and a non-zero exit code when
  any file fails.
- **Robustness.** Truncated files, NaN/Inf coordinates, empty/degenerate
  meshes and wrong-extension inputs are rejected with a clear error and
  exit code 1, never a crash or a silently wrong result.
- **Volume-change guard.** A warning is added when repair changes the volume
  by more than 15% (flags thin-feature loss or over-aggressive repair).
- **PySide6 GUI.** Native Wayland Qt application with drag & drop, batch
  repair, a Stop button, per-file status, and a monospace report log. Native
  KDE file dialog (rubber-band multi-select) when the system Qt matches.
- **One-line install.** `curl -fsSL .../install.sh | bash` fetches the source
  and installs everything; no AUR, pip + venv only.
- **Dolphin service menu** ("Repair with Sutura"), application menu entry,
  hicolor app icons, and GUI screenshot.
- **Tests.** Synthetic broken-mesh smoke test, layered multi-object 3MF
  regression, adversarial-input regression, and torture tests (5M-triangle
  sphere, thin walls, multi-part, scan-style mesh).
- **Real-world samples.** Three broken Thingi10K models for manual testing.

### Changed

- CLI arguments parsed with argparse.
- CLI accepts multiple input files; `-o` is single-file only.

### Fixed

- Multi-object 3MF handling that dropped all but the first object.
- STL round-trip corrupting layered/duplicated-vertex meshes into triangle
  soup (repaired in memory as numpy arrays).
- Truncated/wrong-count STLs, NaN/Inf coordinates and empty meshes being
  silently mishandled or returning exit 0 on failure.
- kdialog progress updates in the service menu.
- Binary STL files with a truncated or inconsistent triangle count could
  hang indefinitely during repair instead of failing cleanly (found via CI
  hardening).

### Known platform note

- Python 3.12/3.13 are not covered by CI. `pymeshlab` imports on Python 3.13
  but the VCG repair pipeline segfaults there; the root cause has not been
  investigated. CI runs on 3.11 (the manifold3d / stage 2 target) and 3.14
  (the current Python).
