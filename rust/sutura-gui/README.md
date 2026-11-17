# sutura-gui

Sutura desktop GUI (Phase 1–2): egui/eframe + wgpu mesh preview with a
tiny-skia CPU fallback.

```bash
# build (from rust/)
cargo build -p sutura-gui

# run with a mesh
rust/target/debug/sutura-gui path/to/model.stl

# EN screenshot and exit (verification)
rust/target/debug/sutura-gui --lang en --screenshot shot.png --exit-after \
  tests/real-world-samples/artec_bovine-heart.stl

# Run a repair, then screenshot the filled report panel
SUTURA_PYTHON=/path/to/venv/bin/python \
  rust/target/debug/sutura-gui --lang en --repair --screenshot report.png \
  --exit-after model.stl

# batch-repair every loaded mesh with an intensity preset and the heatmap on
SUTURA_PYTHON=/path/to/venv/bin/python \
  rust/target/debug/sutura-gui --repair-all --intensity thorough --heatmap a.stl b.stl

# force the CPU preview
SUTURA_GUI_CPU=1 rust/target/debug/sutura-gui model.stl
```

The repair button invokes the Python CLI (`sutura/repair.py`) as a subprocess
with `--json --progress-json` until the Rust core API is available. The panel
shows Health / Risk / Fidelity, the localized verdict and status, and offers
Open result / Show in folder. Cancel kills the child's process group. An
existing `_fixed.stl` is never overwritten silently: the app asks first.
`--repair-all` runs a batch queue over every loaded mesh (`--intensity`
quick/balanced/thorough/extreme is passed to the CLI). The heatmap paints faces
touching a boundary or non-manifold edge and reports the fraction. About and
Updates are placeholders. Override the command with `SUTURA_CLI` and the
interpreter with `SUTURA_PYTHON`.

macOS note: if the Command Line Tools SDK lacks libc++ headers, build with a
C++ wrapper that adds
`-isystem /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/c++/v1`.
