# Sutura × OrcaSlicer plugin

Repair STL/3MF meshes straight from OrcaSlicer. This plugin shells out to the
[separately-installed Sutura CLI](https://github.com/Krateian/Sutura) in a
background thread and shows the repair result in the slicer — it does not
bundle pymeshlab/manifold3d into OrcaSlicer's embedded Python.

## ⚠️ EXPERIMENTAL — untested in a real OrcaSlicer instance

Please read this before installing. The OrcaSlicer Python plugin system was
introduced in **nightly builds / releases newer than 2.4.2**, which the
developer of this plugin does not have installed yet. As a result this plugin
has **never been run inside a real OrcaSlicer** — only its logic and API usage
have been validated with stub tests against the documented API. It may work,
it may need a small fix or two; if you hit an issue, please report it (see
"Feedback" below). It is offered in good faith as a starting point, not a
guaranteed-working product.

## ⚠️ Does NOT repair the currently selected model

OrcaSlicer's script plugin `execute()` takes **no arguments**, and there is no
documented API to get the currently-selected model. This plugin therefore
repairs a **fixed/configured target file** (set via the `SUTURA_TARGET`
environment variable), not whatever you have selected in the slicer. Real
"repair the selected model" support is future work.

## ⚠️ Linux only

Sutura's default CLI path (`~/.local/bin/sutura`) is the Linux `install.sh`
layout. On macOS the CLI lives elsewhere (a different directory from the
`install-macos.sh` conda setup), and on Windows Sutura is not supported at
all. On Linux, this plugin should work.

## How it works

- `sutura_repair_linux_x86_64.py` is a single-file OrcaSlicer plugin (PEP 723 metadata +
  `@orca.plugin` registration), placed as one entry file in a plugin folder.
- On "Run", `execute()` (on the UI thread) returns immediately and spawns a
  daemon `threading.Thread`, so the repair never freezes the slicer.
- The thread calls `~/.local/bin/sutura <file> --human` via `subprocess` and
  reports success/failure plus the `_fixed` output path through
  `orca.host.ui.message(...)`.

## Install (OrcaSlicer 2.4.2+ / nightly)

1. Install Sutura on Linux first (`./install.sh` from the
   [Sutura repo](https://github.com/Krateian/Sutura)), so `~/.local/bin/sutura`
   exists.
2. Copy the plugin folder into OrcaSlicer's plugin dir:

   ```sh
   mkdir -p ~/.config/OrcaSlicer/orca_plugins/SuturaRepair
   cp sutura_repair_linux_x86_64.py ~/.config/OrcaSlicer/orca_plugins/SuturaRepair/
   ```

3. Enable it in the OrcaSlicer Plugins dialog, then run it.

## Configuration

- `SUTURA_CLI` env var — override the CLI path (default
  `~/.local/bin/sutura`).
- `SUTURA_TARGET` env var — target mesh file to repair. Otherwise the
  prototype falls back to a sample path.

## Feedback

This is experimental. If you find a bug, a missing feature, or something that
does not work in your OrcaSlicer, please open an issue at
https://github.com/Krateian/Sutura/issues — your report helps make it better.
