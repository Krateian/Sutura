# AGENTS.md

Sutura: two-stage STL/3MF mesh repair for 3D printing. Stage 1 = PyMeshLab
(VCG filter chain), stage 2 = manifold3d solid rebuild. Git repo, branch
`main`, remote `Krateian/Sutura`.

## How it runs

- Real entry point is `sutura/repair.py` (JSON report to stdout; `--human` for a readable report; exit 0 on success or a stage-1-only partial, exit 1 on a hard error like malformed input). `bin/sutura` is only a bash wrapper: `exec $HOME/.local/share/sutura/venv/bin/python .../repair.py "$@"`.
- `repair.py` imports `pymeshlab` *inside* functions, not at module top, so running it with the plain system Python fails. It must run under the `~/.local/share/sutura/venv` interpreter (PyMeshLab), which talks to `venv311` (python3.11) for stage 2 via `sutura/manifold_bridge.py` as a subprocess.
- `install.sh` creates both virtualenvs (`venv` with PyMeshLab from `requirements.txt`, `venv311` with manifold3d from `requirements-311.txt`), copies the source into `~/.local/share/sutura/`, and installs `~/.local/bin/sutura`. Re-running is safe. It bails if no `python3.11` is available (manifold3d ships wheels only up to Python 3.13).
- Multi-object 3MF files are repaired object-by-object in memory (numpy arrays), preserving the original archive structure; per-object stage 2 is deliberately skipped (see the `TODO(stage2)` in `repair_3mf`). Single-mesh files round-trip through OBJ for stage 2.
- The GUI (`sutura/gui.py`, tkinter) just shells out to the installed `sutura` CLI and parses the last stdout JSON line.
- Result classification lives in `sutura/classification.py` (stdlib-only, no numpy/pymeshlab): `classify()` returns `(category, issues, summary_key)` and both the CLI (`repair.py`) and the GUI (`gui.py`) import it so they never diverge. "Watertight" is only claimed when stage 2 actually ran and returned `ok`; a stage-1-closed mesh with stage 2 skipped/errored/never-run (e.g. macOS in-process fallback unavailable) is a warning. CLI `--human`/JSON label issues via this module (English, not localized); the GUI localizes the same codes through its EN/TR dictionary.

## Tests (no framework — plain scripts, need the venvs installed)

- Smoke: `python3 tests/make_broken_stl.py /tmp/broken.stl && sutura /tmp/broken.stl --human` — generates a cube with missing face, inverted winding, duplicate/degenerate/self-intersecting triangles.
- `python3 tests/make_layered_multiobject_3mf.py --check` — layered/folded-vertex 3MF regression.
- `python3 tests/test_adversarial.py` — malformed-input rejection; it expects the **installed** CLI at `~/.local/bin/sutura`, not the repo copy.
- `python3 tests/test_classification.py` — classification stdlib-only rule (importing it must not pull in numpy/pymeshlab) plus the documented category/issue scenarios (watertight, volume warning, stage 2 skipped/error, partial, malformed).
- `tests/real-world-samples/` (Thingi10K, original licenses) are genuinely broken models; see its README for expected results.

## Conventions

- Input files are never overwritten — output always gets a `_fixed` suffix (or `-o`).
- Robustness is a hard requirement: truncated files, wrong-ext-mismatched content, NaN/Inf coordinates, empty/degenerate meshes must return a clear JSON `error` and exit 1, never a crash or a silent "clean" report.
- VCG chain lives in `stage1_chain`; `delete_fallback_chain` is tried only when the main chain leaves non-manifold edges/vertices, and is kept only if it improves them.

## README upkeep

- If a user-visible behaviour is added or changed in this session (CLI flag,
  GUI behaviour, install step, new feature), update `README.md` in the same
  session — without the user asking.

## Cleanup discipline

- At the end of any session that created temporary/debug files (test scripts,
  probe files, generated meshes, log files used only for diagnosis), delete
  them without being asked — both in `/tmp` and in the repo. Before finishing,
  run `git status` and a quick `ls` of any temp working directories, and remove
  anything not meant to persist.
