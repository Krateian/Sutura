# CLI / GUI feature parity notes

The standing rule (AGENTS.md, FAZ14): **every CLI feature must have a GUI
control and every GUI control a CLI flag** — a feature is added to BOTH by
default; only when one side is technically impossible is it omitted, and that
omission must be documented here. This file records the current parity gaps
(CLI-only / GUI-only) and the exceptions.

## CLI-only (documented gaps)

| Feature | Why GUI doesn't expose it |
|---|---|
| `--classifier-engine classic/experimental` | Engine selection (experimental is the default; classic is a power-user escape hatch). Not in the GUI yet — an accepted gap, engine choice is a deep option. |
| `-o/--output` | GUI always writes the default `_fixed` file in place; a custom output path is a scripting need. |
| `--human`, `--defects`, `--diff` | Text-report presentation flags; the GUI renders the same data graphically (defect panel, repair log, before/after). |
| `export-history` (`--last`/`--clear`/`--summary-only`) | Usage-history is an anonymous CLI/telemetry feature; no GUI viewer yet. |

## GUI-only (allowed exceptions — visual features)

| Feature | Why there is no CLI equivalent |
|---|---|
| Heatmap (2D defect view) | Visual rendering; the CLI reports the same defects in JSON/`--human`. |
| Before/after static + interactive 3D viewer | Visual comparison; the CLI exposes the underlying data (`stage1`/`stage2`/`defects`, `--diff`). |
| Color-coded defect view (red/orange/yellow) | Visual defect-type rendering. |
| "What changed" repair log panel | Visual panel; the CLI prints the same stats in `--human` (`Holes closed`, `Non-manifold edges fixed`, …). |
| Drag & drop, file dialogs | GUI shell affordances. |

## Both (feature parity satisfied)

`--mode` (GUI picker) · `--profile` (GUI dropdown) · repair budgets
`--max-geometry-change`/`--max-risk`/`--force` (GUI budget spin boxes +
declined-file re-run) · `--no-history` (GUI first-run checkbox /
`history_enabled`) · `--dry-run`/`validate` (GUI "Analyze") ·
`--experimental-edge-tiebreak` (GUI checkbox) ·
`--experimental-join-components` (GUI checkbox — gap closed in FAZ14) ·
`--experimental-autorefine` (GUI checkbox) ·
`--no-fallback-ftetwild` (GUI checkbox *fTetWild fallback*, checked by default) ·
`--experimental-fallback-ftetwild` (GUI checkbox *+ self-intersections (slow)* next to it) ·
`--ftetwild-optimize` (GUI checkbox *Optimise tetrahedra (not recommended)* below it) ·
`--experimental-indirect-autorefine` (GUI checkbox — Phase B; all six
batch-wide checkboxes above sit in the GUI's *Options* window) ·
`engines list|check` (GUI *Engines* tab engine list with Reload / *Open
engines folder* / docs link) ·
`ftetwild status|install|uninstall` and `--yes`/`--dry-run` (GUI *Engines*
tab fTetWild section: status line, Install/Remove with a size confirmation
and a cancellable progress dialog; the GUI's confirmation dialog is the
`--yes` equivalent) ·
`--version` (GUI version label).

## History

- **FAZ14:** added the `--experimental-join-components` GUI checkbox (was
  CLI-only), closing the one small parity gap; documented the remaining
  CLI-only / GUI-only items above.- **Deep-repair ladder:** `--deep-repair {off,local,full}` (config key
  `deep_repair`) is CLI-only for now — an open, documented gap. The GUI
  side (first run without deep repair, a pop-up with the
  `deep_repair.available` estimate, an *Options* setting that writes the
  config key) is scheduled separately; until then the GUI keeps its fTetWild
  checkboxes, which map onto `off`/`full`.
- **External engines / fTetWild manager:** the `engines` and `ftetwild`
  subcommands and the GUI *Engines* tab were added together, so the parity
  rule holds. The engine *configuration* (the TOML files) is edited outside
  the app in both cases; the GUI only lists, reloads and opens the folder.
