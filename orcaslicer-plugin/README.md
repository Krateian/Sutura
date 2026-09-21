# Sutura × OrcaSlicer plugin

Repair the **currently selected model** straight from OrcaSlicer: the mesh is
read through the `orca.host` API (numpy-free `vertex(i)`/`triangle(i)`
accessors — the embedded Python ships only `pip`, no numpy), repaired with the
[separately-installed Sutura CLI](https://github.com/Krateian/Sutura), and the
repaired result is loaded back into the slicer. The plugin does not bundle
numpy/pymeshlab/manifold3d into OrcaSlicer's embedded Python.

## ⚠️ Version requirement — nightly / newer than 2.4.2 REQUIRED

The OrcaSlicer Python plugin system exists **only in nightly builds / releases
newer than 2.4.2**. The stable **2.4.2 release has no "Plugins" menu** — this
plugin will not work there. Use a nightly (or a release newer than 2.4.2)
build.

## ⚠️ EXPERIMENTAL — real-instance verified, early stage

The plugin has been tested end-to-end in a real OrcaSlicer **2.5.0-dev**
install (macOS): it loads, activates, reads the selected model through
`orca.host`, repairs it with the Sutura CLI, and loads the repaired result
back into the scene. The `orca.host` API usage is also stub-tested against a
mock host (`tests/test_orca_plugin.py`), including loading with numpy blocked.
It remains an early-stage feature: treat it as a verified starting point, not
a guaranteed-working product, and report issues (see "Feedback" below).

## Platform: Linux primary, macOS bonus verification

**Primary target is Linux** (Sutura's main platform; the design follows the
Linux `install.sh` layout). The same single file is also verified on macOS as
a bonus real-device layer:

- On **both** Linux and macOS the Sutura CLI is installed at the **same path,
  `~/.local/bin/sutura`** (`install.sh` on Linux, `install-macos.sh` on macOS;
  only the Python interpreter behind the wrapper differs). The old claim that
  "macOS uses a different directory" was incorrect.
- Windows is not supported.
- macOS-specific behaviour: the repaired file is loaded back with the native
  `open -b com.orcaslicer.OrcaSlicer <path>` (bundle-ID matching, skipped when
  more than one OrcaSlicer process is running); Linux uses
  `OrcaSlicer --single-instance <path>`.

## Unique output files — no overwrites

Every run writes a **unique** repaired file
`<stem>_fixed_<timestamp>_<short-uuid>.stl` under OrcaSlicer's `data_dir()`
audit-allowed root. Consecutive runs **never overwrite** a previous result; a
temporary input staging file (`<uuid>.stl`) is written per run and removed
afterwards.

## How it works

- `sutura_repair_linux_x86_64.py` is a single-file OrcaSlicer plugin (PEP 723
  metadata + `@orca.plugin` registration), placed as one entry file in a
  plugin folder.
- On "Run", `execute()` (on the UI thread) reads the selected model in memory
  (`orca.host.model() -> objects() -> volumes() -> mesh()`, using the
  **numpy-free** `vertex(i)` / `triangle(i)` accessors — the embedded Python
  has no numpy), opens a native progress dialog
  (`orca.host.ui.create_progress_dialog`, pulsed while the repair runs) and
  shows the result message when it finishes. On builds without the
  progress-dialog API it falls back to a background thread and reports
  "started" immediately.
- Repair always runs via the **subprocess CLI**
  (`~/.local/bin/sutura <stage> -o <unique_out>`): the embedded interpreter
  ships only `pip` (no numpy/pymeshlab/manifold3d), so in-process repair is
  not possible.
- `register_capabilities()` declares the Sutura CLI path up front with
  `orca.request_permissions(fs_read=[...])`. HONEST SCOPE: this only
  pre-declares filesystem **reads** — the audit API has no declarative form
  for subprocess spawns, and their persisted grant matches the exact command
  line (which contains unique temp paths), so a subprocess permission prompt
  can still appear.
- The repaired file is loaded back via `--single-instance` (Linux) /
  `open -b com.orcaslicer.OrcaSlicer` (macOS), and the result path is reported
  through `orca.host.ui.message(...)`.

## Install (nightly / OrcaSlicer > 2.4.2)

1. Install Sutura first: `./install.sh` (Linux) or `install-macos.sh` (macOS),
   so `~/.local/bin/sutura` exists.
2. Copy the plugin folder into OrcaSlicer's plugin dir:

   ```sh
   mkdir -p ~/.config/OrcaSlicer/orca_plugins/SuturaRepair
   cp sutura_repair_linux_x86_64.py ~/.config/OrcaSlicer/orca_plugins/SuturaRepair/
   ```

   (macOS: `~/Library/Application Support/OrcaSlicer/orca_plugins/`.)
3. Enable it in the OrcaSlicer Plugins dialog, then run it.

## Configuration

- `SUTURA_CLI` env var — override the CLI path (default
  `~/.local/bin/sutura`, the same path on Linux and macOS).
- `ORCA_BIN` env var — override the OrcaSlicer binary used for
  `--single-instance` (default `orca-slicer`).

## Feedback

This is experimental. If you find a bug, a missing feature, or something that
does not work in your OrcaSlicer, please open an issue at
https://github.com/Krateian/Sutura/issues — your report helps make it better.