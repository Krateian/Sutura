# Changelog

All notable changes to this project are documented here.

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
