# AGENTS.md

Sutura: two-stage STL/3MF mesh repair for 3D printing. Stage 1 = PyMeshLab
(VCG filter chain), stage 2 = manifold3d solid rebuild. Git repo, branch
`main`, remote `Krateian/Sutura`.

## How it runs

- Real entry point is `sutura/repair.py` (JSON report to stdout; `--human` for a readable report; exit 0 on success or a stage-1-only partial, exit 1 on a hard error like malformed input). `bin/sutura` is only a bash wrapper: `exec $HOME/.local/share/sutura/venv/bin/python .../repair.py "$@"`.
- `repair.py` imports `pymeshlab` *inside* functions, not at module top, so running it with the plain system Python fails. It must run under the `~/.local/share/sutura/venv` interpreter (PyMeshLab), which talks to `venv311` (python3.11) for stage 2 via `sutura/manifold_bridge.py` as a subprocess.
- `install.sh` creates both virtualenvs (`venv` with PyMeshLab from `requirements.txt`, `venv311` with manifold3d from `requirements-311.txt`), copies the source into `~/.local/share/sutura/`, and installs `~/.local/bin/sutura`. Re-running is safe. It bails if no `python3.11` is available (manifold3d ships wheels only up to Python 3.13).
- **AppImage build (alternative distribution):** `scripts/build_appimage.sh` bundles two relocatable python-build-standalone runtimes (Python 3.14 -> `usr/lib/sutura/venv`, Python 3.11 -> `usr/lib/sutura/venv311`) plus the app modules and builds `dist/Sutura-x86_64.AppImage` with appimagetool (which carries its own `mksquashfs`, so no system package needed). The AppRun dispatch exports `SUTURA_DIR`/`SUTURA` so `repair.py` finds its venv311/bridge and `gui.py` finds its CLI without code changes. **AppImage mode detection:** `updater.py` checks the `APPIMAGE` env var (set by the AppImage runtime); in that mode the GUI skips the first-run ask / background checks and the update button shows a "download from releases" message instead of self-updating (self-update cannot write to a read-only squashfs). `updater.py`'s VERSION fallback reads `repair.py` beside itself before the host install dir so the bundled copy is authoritative.
- Multi-object 3MF files are repaired object-by-object in memory (numpy arrays), preserving the original archive structure; per-object stage 2 is deliberately skipped (see the `TODO(stage2)` in `repair_3mf`). Single-mesh files round-trip through OBJ for stage 2.
- The GUI (`sutura/gui.py`, tkinter) just shells out to the installed `sutura` CLI and parses the last stdout JSON line.
- Result classification lives in `sutura/classification.py` (stdlib-only, no numpy/pymeshlab): `classify()` returns `(category, issues, summary_key)` and both the CLI (`repair.py`) and the GUI (`gui.py`) import it so they never diverge. "Watertight" is only claimed when stage 2 actually ran and returned `ok`; a stage-1-closed mesh with stage 2 skipped/errored/never-run (e.g. macOS in-process fallback unavailable) is a warning. CLI `--human`/JSON label issues via this module (English, not localized); the GUI localizes the same codes through its EN/TR dictionary.
- `sutura/defects.py` (stdlib+numpy only) detects the input mesh's holes and non-manifold regions from plain `verts`/`tris` arrays; `repair.py` calls `detect()` on the input and stores it in the report's `defects` key (always in JSON; `--human` only with `--defects`). The GUI renders the selected file's defects in a panel below the log. Keep it free of pymeshlab/trimesh so it stays importable anywhere. `detect(..., with_indices=True)` additionally returns each defect's `verts_idx`/`faces_idx` index lists for the heatmap; the CLI uses the default (False) so its JSON contract is unchanged.
- `sutura/heatmap.py` (numpy + Qt QImage/QPainter only, no pymeshlab) is the defect heatmap rasterizer: it projects the mesh with an auto-fit orthographic isometric camera, depth-sorts faces (painter's algorithm) and fills each face red when it touches a defect vertex, grey otherwise (~250k tris/s on CPU). It is deliberately a **CPU rasterizer, not OpenGL**: offscreen GL draw calls (`glDrawArrays`/`glDrawElements`) segfault on headless systems (verified on an NVIDIA box without a display) and can be unavailable in the AppImage/macOS CI. Do not replace it with GL without re-checking that.
- `sutura/heatmap_render.py` is a CLI entry run as a **subprocess** by the GUI's `HeatmapWorker` to load+detect+rasterise a heatmap into a PNG temp file. This isolation is required: using pymeshlab inside a Qt worker thread while a `QMainWindow` exists corrupts the heap at interpreter shutdown (PySide6 6.11 + Python 3.14). The GUI process therefore never imports pymeshlab. The GUI shows a "Show heatmap" button + clickable thumbnail (on-demand, cached per file; first object for multi-object 3MF) and falls back silently to the text-only panel on any failure.
- `sutura/mesh_classifier.py` (stdlib+numpy only) heuristically classifies a mesh as `mechanical`/`organic`/`unknown` from dihedral-angle geometry. `repair_mesh_from_arrays` calls it and, on non-`unknown`, tunes two Stage 1 thresholds (`mincomponentsize`, `maxholesize`) via the per-type `_type_params` table in `repair.py`. Those per-type values are EXPERIMENTAL/uncalibrated starting points (see the code comment) — treat them as reversible. The result lands in the report as `detected_type`/`detected_confidence` and is shown in the GUI defect-panel header. Keep it free of pymeshlab/trimesh.

## Tests (no framework — plain scripts, need the venvs installed)

- Smoke: `python3 tests/make_broken_stl.py /tmp/broken.stl && sutura /tmp/broken.stl --human` — generates a cube with missing face, inverted winding, duplicate/degenerate/self-intersecting triangles.
- `python3 tests/make_layered_multiobject_3mf.py --check` — layered/folded-vertex 3MF regression.
- `python3 tests/test_adversarial.py` — malformed-input rejection; it expects the **installed** CLI at `~/.local/bin/sutura`, not the repo copy.
- `python3 tests/test_classification.py` — classification stdlib-only rule (importing it must not pull in numpy/pymeshlab) plus the documented category/issue scenarios (watertight, volume warning, stage 2 skipped/error, partial, malformed).
- `python3 tests/test_defects.py` — defects stdlib+numpy-only rule plus hole/non-manifold detection on a clean cube and a cube with a removed face / duplicated face.
- `~/.local/share/sutura/venv/bin/python tests/test_mesh_classifier.py` — mesh classifier stdlib+numpy-only rule (subprocess check) plus mechanical (cube), organic (sphere), and unknown-fallback (cylinder) decisions. Needs the venv because it builds test meshes with trimesh.
- `tests/real-world-samples/` (Thingi10K, original licenses) are genuinely broken models; see its README for expected results.

## Conventions

- Input files are never overwritten — output always gets a `_fixed` suffix (or `-o`).
- Robustness is a hard requirement: truncated files, wrong-ext-mismatched content, NaN/Inf coordinates, empty/degenerate meshes must return a clear JSON `error` and exit 1, never a crash or a silent "clean" report.
- VCG chain lives in `stage1_chain`; `delete_fallback_chain` is tried only when the main chain leaves non-manifold edges/vertices, and is kept only if it improves them.

## README upkeep

- If a user-visible behaviour is added or changed in this session (CLI flag,
  GUI behaviour, install step, new feature), update `README.md` in the same
  session — without the user asking.
- `README.tr.md` is the Turkish translation of `README.md` and must be kept in
  sync with it: any user-visible addition/change to `README.md` also updates
  `README.tr.md` in the same session, without being asked.

## Cleanup discipline

- At the end of any session that created temporary/debug files (test scripts,
  probe files, generated meshes, log files used only for diagnosis), delete
  them without being asked — both in `/tmp` and in the repo. Before finishing,
  run `git status` and a quick `ls` of any temp working directories, and remove
  anything not meant to persist.
