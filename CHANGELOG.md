# Changelog

All notable changes to this project are documented here.

## [1.0.0] - unreleased

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
