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
| Ray-stabbing vote (`raystab=` / `SUTURA_RAYSTAB`, library/Graft only) | Experimental inside/outside disambiguation; no CLI flag or GUI control yet by design. Deliberately deferred until it is measured on the real-world corpus, at which point it gets both a CLI flag and a GUI control. |

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
`--no-graft` / `--experimental-graft` (GUI right-click method menu #13 Graft + auto fallback ladder) ·
`--no-dressing` / `--experimental-dressing` (GUI checkbox *Dressing (viscosity coat)* in Options → Experimental; off by default; #16 is also selectable via the right-click *Use method* menu) ·
`--dressing-drain MODE|mm` (GUI *Drain* combo next to the Dressing checkbox: Preset default / Off / Half / Full / Deep) ·
`--no-cache` / `clear-cache` (GUI Options → General *Enable Sutura Chart cache* checkbox + *Clear Cache* button) ·
`--experimental-indirect-autorefine` (GUI checkbox — Phase B; all seven
batch-wide checkboxes above sit in the GUI's *Options* window) ·
`--methods 11/12` and `--repeat-source`/`--repeat-target` (GUI *Use method*
lists #11 *Transplant* and #12 *Transplant+*; picking #12 opens
a CPU-rasterised picker dialog where the user clicks the healthy source and the
damaged target element; the two 3D points are stored with the file's tags and
passed as the CLI flags) ·
`--list-methods` / `--analyze` / `--methods` (GUI right-click menu: *Analyze*
runs `--analyze`; *Recommended methods* and *Use method* tag methods in order;
the new **Method** column shows *Auto* / the top recommendation / the tag
chain) · `--engines` (GUI *External engines* submenu, tagged the same way;
separate from the method ranking) ·
`engines list|check` (GUI *Engines* tab engine list with Reload / *Open
engines folder* / docs link) ·
`ftetwild status|install|uninstall` and `--yes`/`--dry-run` (GUI *Engines*
tab fTetWild section: status line, Install/Remove with a size confirmation
and a cancellable progress dialog; the GUI's confirmation dialog is the
`--yes` equivalent) ·
`--version` (GUI version label).

## History

- **Dressing (#16):** `--no-dressing` / `--experimental-dressing` and the
  batch-wide GUI checkbox (Options → Experimental) were added together, so the
  parity rule holds. Dressing is opt-in (default off) until it is measured on
  the real-world corpus.
- **FAZ14:** added the `--experimental-join-components` GUI checkbox (was
  CLI-only), closing the one small parity gap; documented the remaining
  CLI-only / GUI-only items above.
- **Deep-repair ladder:** `--deep-repair {off,local,full}` (config key
  `deep_repair`) is CLI-only for now — an open, documented gap. The GUI
  side (first run without deep repair, a pop-up with the
  `deep_repair.available` estimate, an *Options* setting that writes the
  config key) is scheduled separately; until then the GUI keeps its fTetWild
  checkboxes, which map onto `off`/`full`.
- **External engines / fTetWild manager:** the `engines` and `ftetwild`
  subcommands and the GUI *Engines* tab were added together, so the parity
  rule holds. The engine *configuration* (the TOML files) is edited outside
  the app in both cases; the GUI only lists, reloads and opens the folder.
- **P0 (method registry):** `--list-methods`, `--analyze` and `--methods`
  were CLI-only initially; the GUI controls were the explicit P3 follow-up.
- **P-REP (repeated-element repair, methods 11/12):** the standalone
  `repeat_repair.py` was integrated into the registry (11 `repeat_auto`,
  12 `repeat_manual`) with `--repeat-source X,Y,Z` / `--repeat-target X,Y,Z`
  for the manual method; the GUI *Use method* entry for #12 opens a picker
  dialog (kept off the GUI's pymeshlab-free process by loading the mesh in the
  `repeat_picker_render.py` subprocess) so both sides are covered.
- **P3 (GUI method tagging):** closed that gap. The GUI file list gained a
  third **Method** column and a right-click menu (*Analyze*, *Recommended
  methods*, *Use method* with a checkable keep-open popup, *External engines*,
  *Clear tags*). `--engines NAME[,NAME]` was added with the GUI *External
  engines* submenu in the same change (per-file engine selection; default
  `None` runs every enabled engine as before), so the parity rule holds.
- **Closing / proxy-template methods (#8–#10):** the standalone
  `closing.py` / `proxy_repair.py` tiers are reachable from the CLI
  (`--methods 8/9/10`) and from the GUI *Use method* list (all fifteen methods);
  no separate flag exists, so parity holds without a dedicated control.
- **Method #14 Mirror Complete / #15 Wall Thicken (v0.6.1):** both are in the
  shared method registry, so the GUI *Use method* menu lists and tags them with
  no extra control (their EN/TR tooltips are localized). The only CLI-only
  surface is the numeric `--wall-min-thickness T` (the GUI runs #15 with the
  automatic default, 1 % of the bbox diagonal) — documented here as an accepted
  gap; a numeric entry can be added to the Options window later.
- **Learning triage (v0.6.1):** CLI `--no-learning-triage` / `clear-learning`
  map onto the GUI Options → General *Learning triage* checkbox and *Reset
  learning* button, so the parity rule holds.
- **P-FINAL (reload-honest verdict + P-WELD + csg_bridge):** P-HONEST and
  P-WELD are automatic pipeline behaviour, not user-facing options, so they
  apply identically to CLI and GUI repairs and need no parity control. The
  repeated-element CSG bridge (`csg_bridge.py`) is an implementation detail
  behind method #11/#12; it is added to every packaging list in the same
  change.
- **Method #13 Graft / shell wrap (v0.7.0):** CLI `--no-graft` / `--experimental-graft`
  and env `SUTURA_GRAFT` control the morphology shell-wrap tier; the GUI right-click
  menu lists Graft (#13) under *Use method* and *Recommended methods* with full EN/TR
  localization, and surfaces detail-loss warnings directly in the repair log.
- **Sutura Chart cache (v0.7.0):** CLI `--no-cache` and `sutura clear-cache` correspond
  to GUI Options → General *Enable Sutura Chart cache* checkbox and *Clear Cache* button,
  preserving parity across CLI and GUI workflows.
- **Graft sign-field Pass 0 + intensity-scaled grid budget (unreleased):** Pass 0 is
  OFF by default and is an opt-in evaluation switch (`SUTURA_GRAFT_SIGN_FIELD=1`), like the
  ray-stab vote — env-only, no CLI flag or GUI control by design (it is not proven; on
  framebaroque it fails both gates and costs ~326 s). The grid budget is derived
  automatically from the Triage intensity preset, which both the CLI (`--intensity`) and
  the GUI (batch-wide *Intensity* picker) already set, so parity is automatic.
- **Ray-stabbing (+ sign-field disambiguation) CLI/GUI control:** both votes remain
  environment/library-only until a real-world-corpus measurement; the follow-up adds a CLI
  flag and a GUI control together, per the parity rule.
- **Guarded SI excise + refined Stage-1 hole fill (unreleased):** the Full-Mend
  self-intersection excise is automatic pipeline behaviour (both CLI and GUI get it, so
  parity holds) with an env opt-out only (`SUTURA_SI_EXCISE=0`), like the ray-stab vote.
  The refined Stage-1 hole fill (`SUTURA_REFINE_HOLE=1`) is an unproven evaluation switch,
  so it is env-only too; when it is proven it gets a CLI flag and a GUI control together.
- **Localized exact self-union (`sutura/local_exact.py`, unreleased):** the tier that
  replaces a residual Stage-2 self-intersecting cluster with the exact outer-hull
  triangulation of a small patch. It is OPT-IN and OFF by default for every install
  (experimental; measured on the 40-mesh corpus it removes SI on only an isolated mesh and
  the framebaroque fold is global, not local — see the class docstring and
  `docs/local-exact-notes.md`). It is forced with `SUTURA_LOCAL_EXACT=1` (values
  `1/true/yes/on`) and disabled with `SUTURA_LOCAL_EXACT=0`; there is deliberately NO CLI
  flag or GUI control, so CLI and GUI behave identically (parity by construction, like the
  ray-stab vote).
