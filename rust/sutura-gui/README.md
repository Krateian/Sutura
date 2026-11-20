# sutura-gui

Sutura desktop GUI: egui/eframe + wgpu mesh preview with a tiny-skia CPU
fallback, and the repair itself running **in-process** through `sutura-core`.

```bash
# build (from rust/)
cargo build -p sutura-gui

# run with a mesh
rust/target/release/sutura-gui path/to/model.stl

# EN screenshot and exit (verification)
rust/target/release/sutura-gui --lang en --screenshot shot.png --exit-after \
  tests/real-world-samples/artec_bovine-heart.stl

# Run a repair, then screenshot the report panel
rust/target/release/sutura-gui --lang en --repair --screenshot report.png \
  --exit-after model.stl

# batch-repair every loaded mesh with an intensity preset and the heatmap on
rust/target/release/sutura-gui --repair-all --intensity thorough --heatmap a.stl b.stl

# repair with a specific mode / profile (same values as the CLI)
rust/target/release/sutura-gui --mode aggressive --profile scan model.stl

# force the CPU preview
SUTURA_GUI_CPU=1 rust/target/release/sutura-gui model.stl
```

The Repair button calls `sutura_core::repair` directly — there is no Python
process and no subprocess at all. Settings go through the same
`repair_settings::options_from` the CLI uses, so both front ends agree by
construction; the GUI cannot drift from `--mode`, `--profile` and the rest.

The panel shows Health / Risk / Fidelity, the localized verdict and status, the
defect analysis (detected type and confidence, components, boundary loops,
non-manifold edges, self-intersections, open area), the objects inside a 3MF
archive, the methods the ranking recommends for each of them, and Open result /
Show in folder. Cancelling sets a cooperative token the repair checks; there is
no child process to signal. An existing `_fixed.<ext>` is never overwritten
silently: the app asks first, and the output keeps the input's extension
(`.3mf` stays `.3mf`, because a 3MF is a multi-object archive and writing it as
STL loses every object but one).

`--repair-all` runs a batch queue over every loaded mesh. Intensity, the mode
ladder, the profile preset and every control in the settings panel are saved to
`~/.config/sutura/gui.json` (beside the CLI's `config.json`, in a separate file
so neither clobbers the other) and restored on launch; an explicit
`--intensity`/`--lang` still wins over the saved value.

Hidden verification flags, not in `--help`: `--headless-analyze`,
`--headless-objects`, `--headless-report`, `--headless-heatmap`,
`--headless-scores`, `--headless-pick`. These print the *values* the panels are
built from, so the Python-vs-Rust parity gate can compare them without pixels.
`SUTURA_GUI_SCROLL=<px>` starts the settings panel at a scroll offset — the panel
is taller than any window, so without it nothing below the fold can be captured
in a screenshot.

macOS note: if the Command Line Tools SDK lacks libc++ headers, build with a
C++ wrapper that adds
`-isystem /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/c++/v1`.
