# Sutura × OrcaSlicer plugin — prototype

**EXPERIMENTAL / UNTESTED IN A REAL OrcaSlicer.** This is a proof-of-concept
OrcaSlicer Script plugin that shells out to the separately-installed Sutura
CLI from a background thread and shows the result in the host UI. It does not
bundle pymeshlab/manifold3d into OrcaSlicer's embedded Python.

> **Platform note (Linux only for now).** This plugin currently works only on
> Linux, because it depends on the Linux `install.sh` CLI path
> `~/.local/bin/sutura`. Sutura has no Windows support at all. On macOS the
> CLI lives elsewhere (`~/.local/share/sutura`, from the `install-macos.sh`
> conda setup), so the current default path would be wrong there. If this is
> ever made cross-platform, the `SUTURA_CLI` env var default should do
> platform-based path detection instead of hardcoding `~/.local/bin/sutura`
> (not implemented now — just a note).

## How it works

- `sutura_repair.py` is a single-file OrcaSlicer plugin (PEP 723 metadata +
  `@orca.plugin` registration), placed as one entry file in a plugin folder.
- On "Run", `execute()` (on the UI thread) returns immediately and spawns a
  daemon `threading.Thread`, so the repair never freezes the slicer.
- The thread calls `~/.local/bin/sutura <file> --human` via `subprocess` and
  reports success/failure plus the `_fixed` output path through
  `orca.host.ui.message(...)`.

## Install (OrcaSlicer 2.4.2+ / nightly)

Copy the plugin folder into OrcaSlicer's plugin dir:

```sh
mkdir -p ~/.config/OrcaSlicer/orca_plugins/SuturaRepair
cp sutura_repair.py ~/.config/OrcaSlicer/orca_plugins/SuturaRepair/
```

Then enable it in the Plugins dialog. Sutura itself must be installed
(`~/.local/bin/sutura`), e.g. via `./install.sh`.

## Configuration

- `SUTURA_CLI` env var — override the CLI path (default
  `~/.local/bin/sutura`).
- `SUTURA_TARGET` env var — target mesh file to repair. Otherwise the
  prototype falls back to a sample path.

## Known limitation / open question

Script `execute()` takes **no arguments** and there is no documented API for
the currently-selected model, so this prototype cannot yet repair "the file I
have selected in the slicer". It repairs a configured target file instead.
Getting the real selection (e.g. via `orca.host` model access or the D-Bus
`AnotherInstance` trick) is Phase 2 and needs a real OrcaSlicer to validate.
