# Changelog

All notable changes to this project are documented here.

## [Unreleased]

## [0.1.8-beta.1] - 2026-08-23

First **beta (pre-release)** build, published so willing users can try two new
features on GitHub while regular auto-update users never see it: `/releases/latest`
skips pre-releases, so the update checker stays on the stable channel.

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
- **Zoom / balloon detail (before/after GUI).** The before/after dialog now
  renders a second, smaller close-up of the WORST original defect region
  (the defect with the largest physical bounding-box diagonal, holes and
  non-manifold regions compared on the same metric). The close-up uses the
  same zoomed camera frame for both views (`heatmap.focus_frame`), so the
  original vs repaired comparison is apples-to-apples. When the original has
  no defects the detail view simply mirrors the main view.
- **New before/after colour scheme.** The before view now shows the original
  defects in the historical red `(235,60,70)` (previously the before image
  was unmarked); the repaired view is rendered in the brand teal `#14b8a6`
  (fixed/healthy) with any REMAINING holes / non-manifold regions in red.
  Same scheme in the close-up.

### Changed

- **`updater.py` semver hardening.** `parse_version` now understands semver
  pre-release tags: a stable release sorts after any pre-release of the same
  version, so a beta tester is offered the eventual stable release instead of
  being stuck on the beta. The `/tags` fallback also skips dashed (pre-release)
  tags so it can never surface a beta to a normal user.

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
  nightly builds / releases newer than 2.4.2, which we have not run, so it has
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

### Fixed

- `install.sh` was not copying `classification.py` (and `updater.py`) into
  the installed `~/.local/share/sutura/` directory, so the installed CLI and
  GUI failed with `ModuleNotFoundError` right after install. Both modules are
  now copied with the rest of the application files.
- **Mesh classifier regression.** The `mechanical` class initially lowered
  Stage 1's `mincomponentsize` to 4, which let small/degenerate meshes (e.g.
  the 2-triangle case in `tests/test_adversarial.py`) survive the debris
  cutoff and be "repaired" instead of rejected — a CI regression (the
  `degenerate` adversarial scenario). `mincomponentsize` is now kept at the
  default 8 for `mechanical`; the type still tunes `maxholesize` (300).
- `install-macos.sh` had the same gap (it only copied `repair.py`,
  `manifold_bridge.py`, `gui.py`, `__init__.py`), so a macOS install was also
  missing `classification.py` and `updater.py`. Both are now copied in the
  flat layout and the importable package layout.
- **Dolphin service menu submenu (`X-KDE-Submenu=Sutura`)** prevented the
  action from appearing for `.stl` files on some KDE/Plasma versions; the
  submenu was removed and the action now shows at the top level for both
  `.stl` and `.3mf`.

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
