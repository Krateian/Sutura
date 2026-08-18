# Sutura

<p align="center">
  <img src="assets/icon/sutura-128.png" alt="Sutura" width="128">
</p>

<p align="center">
  <img src="https://github.com/Krateian/Sutura/actions/workflows/ci.yml/badge.svg" alt="CI">
</p>

Two-stage mesh repair for STL and 3MF files, built for Linux with full
macOS support.

Linux has no direct equivalent of Windows' right-click "Fix model" (3D Builder,
Netfabb) or Bambu Studio's broken-on-Linux "Fix model" button. Sutura provides
that workflow: pick a mesh, repair it, keep the original untouched.

Sutura is continuously hardened against real-world inputs — Thingi10K models,
malformed files, adversarial inputs and torture scenarios (huge meshes, thin
walls, multi-part assemblies) — and every change is verified automatically by
CI on each push and pull request.

## Screenshot

![Sutura GUI](assets/screenshot.png)

## Why two stages

* **Stage 1 - PyMeshLab (VCG).** Removes duplicate and degenerate faces,
  repairs non-manifold edges and vertices, orients faces coherently, closes
  holes of *any* size, and drops tiny open debris components. VCG is the
  battle-tested classic for 3D-printing repair.
* **Stage 2 - manifold3d.** Rebuilds the closed mesh as a valid manifold
  solid and merges overlapping shells with a boolean union. This is the same
  library Bambu Studio uses; it guarantees the output is a single closed
  two-manifold.

Each stage fixes what the other cannot: VCG closes big holes but does not
resolve self-intersecting geometry; manifold3d guarantees a watertight result
but its Python binding rejects any input that is not already closed
(`Error.NotManifold`), so stage 1 must finish the mesh first.

On Linux, stage 2 runs in a dedicated python3.11 virtualenv (manifold3d
ships no wheel for Python 3.14). On single-environment installs (macOS/conda,
or any setup where manifold3d is importable from the current Python), stage 2
runs in-process instead. If manifold3d is not available at all, the report
explicitly says `Stage 2 skipped: manifold3d not available in this
environment.` — it is never silently omitted.

The original file is never overwritten. Output is written with a `_fixed`
suffix in the same directory.

## Requirements

Linux:

* `python3` (>= 3.11) with venv support, for the PyMeshLab venv
* `python3.11` specifically, for the manifold3d venv (manifold3d ships
  wheels only up to Python 3.13)
* KDE Plasma for the Dolphin service menu (optional; CLI and GUI work anywhere)

macOS (Apple Silicon / Intel):

* Homebrew and Miniforge (conda). pymeshlab has no PyPI wheel for Apple
  Silicon, so it must come from conda-forge; this is why macOS uses a single
  conda environment (`install-macos.sh`) rather than the Linux pip-only flow.
* Install Python 3.11 via conda (`install-macos.sh` does this automatically).

Linux Python 3.11 install:

* Arch / CachyOS: `sudo pacman -S python311`
* Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
* Fedora: `sudo dnf install python3.11`

On other distros, if your default `python3` *is* 3.11, no extra install is
needed.

## Troubleshooting

* **`python3.11` not found.** Stage 2 (manifold3d) needs Python 3.11 because
  it ships wheels only up to 3.13. Install it per distro:
  * Arch / CachyOS: `sudo pacman -S python311`
  * Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
  * Fedora: `sudo dnf install python3.11`
  Then run `install.sh` again — it reuses the existing virtualenvs.
* **PySide6 install fails.** The GUI needs `PySide6-Essentials`, which is
  installed into the `venv` from PyPI. On distros where `pip install
  PySide6-Essentials` fails (missing build tooling or a blocked PyPI), install
  the system Qt Python bindings instead and point the entry point at them:
  * Debian/Ubuntu: `sudo apt install python3-pyside6`
  * Fedora: `sudo dnf install python3-pyside6`
  * Arch: `sudo pacman -S pyside6` (in the official `extra` repository)
* **The GUI has no KDE file dialog.** The native dialog needs `plasma-integration`
  and a system Qt version that matches PySide6's. If the rubber-band selection
  is missing, Qt falls back to its embedded dialog — Ctrl/Shift+click still
  work for multi-selection.

## Install

### Linux

One line (fetches the latest `main` and installs):

```sh
curl -fsSL https://raw.githubusercontent.com/Krateian/Sutura/main/install.sh | bash
```

Or from a clone:

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install.sh
```

This creates two virtualenvs under `~/.local/share/sutura`, installs the CLI
wrapper at `~/.local/bin/sutura`, installs the hicolor app icons, and
registers the Dolphin service menu. Re-running is safe.

Installation uses pip inside isolated virtualenvs — no AUR, no yay/paru
required, nothing touches your system package manager. The GUI needs
PySide6 (~79 MB download, part of the `venv`); total installed size for
the two virtualenvs is roughly 800 MB.

On Arch, if `python311` is not installed, install it first (see above).

### macOS

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install-macos.sh
```

`install-macos.sh` checks for Homebrew, installs Miniforge via Homebrew if
conda is missing, creates a `sutura-env` conda environment (Python 3.11),
installs pymeshlab from conda-forge and manifold3d/trimesh/PySide6 from pip,
verifies the imports, copies the app files to `~/.local/share/sutura/`, and
creates `~/.local/bin/sutura` (CLI) and `~/.local/bin/sutura-gui` launchers.
It is macOS-only and re-runnable.

Note: conda can be initialized non-interactively; if the script asks you to
restart the terminal for `conda init` to take effect, do so and re-run it.

## Usage

Runs fully offline after installation — no telemetry, no network calls
during repair, works without internet once installed.

CLI:

```sh
sutura model.stl            # writes model_fixed.stl
sutura model.3mf -o fixed.3mf
sutura model.stl --human    # human-readable report
sutura a.stl b.3mf c.stl    # batch: each file gets a _fixed output
```

With multiple files, every input is repaired in turn and a summary is
printed (`N fully repaired, M with warnings, K failed`); the exit code is
non-zero if any file failed. `-o` is only valid with a single file.

Multi-object 3MF files are handled natively: every object mesh is repaired
independently and written back into the archive, so no object is lost. The
report lists the result per object (holes remaining, two-manifold).

GUI:

```sh
~/.local/share/sutura/gui.py
```

The GUI supports batch repair: add any number of files, press **Repair**,
and each one is processed in turn with its result listed per file. Files
can be added with **Add files…** (native multi-select, rubber-band included),
**Add folder…** (every `.stl`/`.3mf` in the folder, one level deep), or by
dragging files or folders onto the window. **Stop** terminates the running
repair and marks the remaining files as cancelled. Drag & drop works on
native Wayland sessions (the GUI is a Qt application, not XWayland).

Dolphin: right-click an STL/3MF file -> **Repair with Sutura**. With a single
selection the GUI opens with the file loaded; with multiple selections each
file is repaired headlessly and a summary dialog is shown.

After installing or removing the service menu, run `kbuildsycoca6` (the
installer does this automatically) or restart Dolphin.

## Test

Synthetic broken mesh:

```sh
python3 tests/make_broken_stl.py /tmp/broken.stl
sutura /tmp/broken.stl --human
```

The generator produces a cube with a missing face, an inverted winding, a
duplicated face, a fin triangle and a self-intersecting triangle.

Regression suites:

```sh
python3 tests/make_layered_multiobject_3mf.py --check   # layered multi-object 3MF
python3 tests/test_adversarial.py                       # malformed-input handling
```

Real-world samples in `tests/real-world-samples/` come from the
[Thingi10K](https://ten-thousand-models.appspot.com/) dataset (Zhou &
Jacobson): three genuinely broken models — one non-manifold, one
self-intersecting, one both. They retain their original licenses from the
Thingi10K metadata; see `tests/real-world-samples/README.md` for details
and the repair result expected from each.

Torture tests cover hard-but-printable geometry:

```sh
python3 tests/torture_tests.py
```

This runs four scenarios and reports the before/after for each: a 5M-triangle
sphere (repair time), a 0.05 mm thin slab (feature-loss risk — it must survive
intact), a multi-part assembly (the 8-face debris-removal threshold must not
delete legitimate parts), and a rough scan-style mesh with many micro-cracks
(residual-holes expectation).

## Robustness

Malformed or hostile inputs are rejected with a clear error and a non-zero
exit code, never a crash or a silently wrong result:

| Input | Behaviour |
|---|---|
| Truncated / cut-off binary STL | rejected: "Unable to open file ... Malformed file" |
| Header claims more triangles than the file holds | rejected: "Malformed file" |
| NaN/Infinity vertex coordinates | rejected: "input mesh contains NaN or infinite coordinates" |
| Empty mesh (0 triangles) | rejected: "input mesh is empty (no triangles)" |
| Fully degenerate mesh (only zero-area faces) | rejected: "all faces are degenerate; nothing to repair" |
| Wrong extension (OBJ content in `.stl`, or the reverse) | rejected: "Unable to open file" |

Any of these returns exit code 1, so scripts can reliably detect failure.

## Libraries

| Library | Role | Why |
|---|---|---|
| PyMeshLab | stage 1 filter chain | VCG-based, proven for print repair, fills holes of any size, Python 3.14 wheel available |
| manifold3d | stage 2 solid rebuild | watertight guarantee, robust boolean, same engine as Bambu Studio |
| trimesh | stage 2 IO | OBJ/mesh loading in the manifold venv |

Pinned in `requirements.txt` and `requirements-311.txt`.

## Known limitations

* **macOS has no right-click / Finder integration yet.** On macOS only the
  CLI and the GUI are available; there is no equivalent of Linux's Dolphin
  service menu ("Repair with Sutura"). macOS users run `sutura-gui` or
  `sutura <file>` from a terminal.
* **Native KDE file dialog.** The GUI sets `QT_QPA_PLATFORMTHEME=kde` and
  points `QT_PLUGIN_PATH` at `/usr/lib/qt6/plugins` so QFileDialog uses the
  native KDE dialog (rubber-band rectangle selection included). This assumes
  the system Qt version matches the bundled PySide6 Qt; on other distros
  where that differs, Qt falls back to its embedded dialog — rectangle
  selection may be unavailable, but Ctrl/Shift+click always works.
* **Self-intersections within one connected shell.** manifold3d rebuilds the
  mesh as a solid, which resolves interior/overlapping geometry, but the
  rebuild can slightly reshape features in pathological cases. Always check
  the result in a slicer.
* **Large hole patches.** VCG fills holes with flat triangulated patches; for
  very large holes the fill is a simple patch, not a smart reconstruction.
  It closes the mesh, but the patch quality is average and may need smoothing.
* **Tiny disconnected debris.** Components with fewer than 8 faces are
  removed. A small legitimate part that is not connected to the main body
  will be removed too.
* **Inverted whole models.** If the repaired volume comes out negative, the
  whole mesh is flipped; a model that was consistently wound "inside out"
  will be corrected automatically.
* **manifold3d Python binding.** It rejects any input with open edges; if
  stage 1 cannot close a hole, stage 2 is skipped and the stage 1 result is
  used as-is (the report says so).
* **Layered/duplicated-vertex 3MF exports.** Some slicers (Bambu Studio
  included) write 3MFs whose objects repeat every vertex position ~15x as
  separate vertex entries, and whose surfaces are folded (several faces
  coincident on one edge). VCG can turn such meshes into valid 2-manifolds,
  but a few sub-millimetre cracks may remain that `close_holes` refuses to
  fill (the fill patch would be degenerate). The result is two-manifold but
  not always fully watertight; most slicers auto-heal cracks this small on
  import. Example from development: a 2-object Bambu export ended with 13
  and 26 remaining micro-holes per object after the best possible VCG pass.
* **All objects are preserved.** Multi-object 3MFs are repaired object by
  object and written back, so no object is lost. The per-object result is
  reported in the CLI output and the GUI.

## Contributing

Missing a feature? Found a mesh that won't repair? Open an issue. A good
bug report is worth far more than a bare "it doesn't work", so when you
report a mesh please include:

* the `sutura <file> --human` output (or the JSON report),
* the command you ran,
* and, if you know it, how the mesh was produced — slicer, scanner, CAD
  export, etc.

This makes the root cause much easier to pin down. Structured reports are
encouraged: see `.github/ISSUE_TEMPLATE/bug_report.md`.

## License

Apache License 2.0. See `LICENSE`.

This project also includes a `NOTICE` file (Apache 2.0 §4d) preserving
attribution; if you redistribute or build on Sutura, please keep it intact.