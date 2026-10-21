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
| `--self-check` | The GUI runs the same availability check at startup and shows a dismissible banner when a method is unavailable, but it does not expose the full fixture-exercising command. An accepted gap until the banner grows a "run full check" action. |
| `--self-repair` | Installation-integrity maintenance for deployed installs (verify, back up, replace, roll back). The GUI never modifies installed files, so this stays a CLI/installer action. |
| `--diagnostics ZIP_FILE` | Full parity: the GUI health banner's export button runs the same `sutura.diagnostics.create_diagnostics_zip` and writes the identical archive to a user-chosen path (success/failure shown in the GUI). |
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
`--dressing-defects {all,holes_nm}`, `--dressing-rmax-scale F`, `--dressing-sigma-scale F` (GUI Options → Experimental combos *Defects*, *Bridge radius*, *Influence width*) ·
`--dressing-force-adopt` (GUI checkbox *Dressing: force adopt (may deform)* in Options → Experimental, and post-batch *⚠ Try Dressing (makes it watertight, but the part may deform)* button; implies `--experimental-dressing`) ·
`--si-mode {repair,report,off}` (GUI *Self-intersections* combo in Options → Repair: Report only / Repair / Do not measure; the worker adds `--si-mode` only when it differs from the default `report`; SI no longer blocks watertight verdict) ·
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

- **Self-intersection policy:** `--si-mode {repair,report,off}` (env
  `SUTURA_SI_MODE`, default `report`) and the GUI *Self-intersections* combo
  (Options → Repair: *Report only* / *Repair* / *Do not measure*) were added
  together, so the parity rule holds. Under `report`, self-intersections no
  longer block the `watertight` classification verdict on closed 2-manifold
  surfaces. The combo stores a batch-wide value and the worker passes
  `--si-mode` only when it is not the default, keeping baseline commands
  unchanged.
- **Dressing (#16):** `--no-dressing` / `--experimental-dressing` and the
  batch-wide GUI checkbox (Options → Experimental) were added together, so the
  parity rule holds. The new single default switch
  (`DRESSING_DEFAULT_ENABLED` / `SUTURA_DRESSING_DEFAULT`, which makes Dressing
  an Auto-ladder fallback) is an env/constant rollout knob, not a user control;
  the GUI checkbox still maps to the forcing `--experimental-dressing`, and
  `--no-dressing` / `SUTURA_DRESSING=0` remain the disable path on both sides.
  The switch ships OFF until the fine-detail (voxel staircase) loss is fixed.
- **Dressing opt-in UX:** `--dressing-force-adopt` (implies
  `--experimental-dressing`) and the batch-wide GUI checkbox *Dressing: force adopt (may deform)*
  (Options → Experimental) plus the post-batch button *⚠ Try Dressing (makes it watertight, but the part may deform)*
  (or *⚠ Try Dressing on %d file(s) (watertight, but the part may deform)*) were added
  together, so the parity rule holds. The report's `suggestions` list is a
  separate output field (not an issue code); the GUI reads the same key to
  decide whether to show the button, so both sides consume the identical
  payload. The CLI `--human` output emits the exact suggestion strings:
  `  ⚠ Not watertight. You can try Dressing: --experimental-dressing (watertight, but the shape may deform / fine detail may be lost).`
  when not run, or
  `  ⚠ Not watertight. Dressing ran but its result was rejected by the quality gate (...).\n    Force it with --dressing-force-adopt if you accept the deformation (the shape may deform).`
  when gate-rejected.
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
  dialog (kept off the GUI process by loading the mesh in the
  `repeat_picker_render.py` subprocess) so both sides are covered.
- **Proxy resample + alpha wrap native swap (Phase 4 W3c/W3e):** the proxy-tier
  uniform resampler and hole closure moved from PyMeshLab
  (`generate_resampled_uniform_mesh` + `meshing_close_holes`) to the native
  `sutura_geom.resample_uniform` + `topology.close_holes`, and the alpha-wrap
  fallback moved to `sutura_geom.morph_close`. This is an implementation detail
  behind methods #9/#10, so CLI and GUI use it identically and no control is
  added. The former isolation switches `SUTURA_PROXY_RESAMPLE_TIMEOUT` /
  `SUTURA_PROXY_RESAMPLE_MAX_RSS_MB` (env-only) were removed with the VCG
  subprocess guard they configured.
- **P3 (GUI method tagging):** closed that gap. The GUI file list gained a
  third **Method** column and a right-click menu (*Analyze*, *Recommended
  methods*, *Use method* with a checkable keep-open popup, *External engines*,
  *Clear tags*). `--engines NAME[,NAME]` was added with the GUI *External
  engines* submenu in the same change (per-file engine selection; default
  `None` runs every enabled engine as before), so the parity rule holds.
- **Closing / proxy-template methods (#8–#10):** the standalone
  `closing.py` / `proxy_repair.py` tiers are reachable from the CLI
  (`--methods 8/9/10`) and from the GUI *Use method* list (all sixteen methods);
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
  ornate-frame it fails both gates and costs ~326 s). The grid budget is derived
  automatically from the Triage intensity preset alone (never from free RAM, so the
  grid is deterministic), which both the CLI (`--intensity`) and the GUI (batch-wide
  *Intensity* picker) already set, so parity is automatic. Free RAM is a discrete
  fallback guard: a preset budget that would not fit falls back once to the Balanced grid
  (`grid_fallback`, with a clear message), and only a budget where even Balanced does not
  fit is refused (`reject_reason='low_memory'`); the resolution is never silently coarsened.
  It is automatic pipeline behaviour for both CLI and GUI, so it needs no separate control.
  The closing ladder also stops on a deterministic work budget (input grid + preset), not a
  wall clock; the wall-clock budget is only an outer all-or-nothing abort whose message is
  printed by `--human` and shown by the GUI repair log (`repair_log_graft_aborted`), so the
  two surfaces stay identical.
- **Ray-stabbing (+ sign-field disambiguation) CLI/GUI control:** both votes remain
  environment/library-only until a real-world-corpus measurement; the follow-up adds a CLI
  flag and a GUI control together, per the parity rule.
- **Guarded SI excise + Stage-1 hole fill (unreleased):** the Full-Mend
  self-intersection excise is automatic pipeline behaviour (both CLI and GUI get it, so
  parity holds) with an env opt-out only (`SUTURA_SI_EXCISE=0`), like the ray-stab vote.
  The experimental refined Stage-1 hole fill (`SUTURA_REFINE_HOLE=1`, a Liepa-style
  `refinehole=True`) was REMOVED in Phase 4 W3d: it was OFF by default and measured to worsen
  an open mesh (thingi10k_100827: 9 -> 23 holes, SI 29 -> 77), so the plain native
  ear-clipping closer is always used and there is no CLI flag or GUI control to add. The SI
  excise re-cap likewise no longer requests refinement (it changed no corpus outcome).
- **Flap per-loop candidate selection (unreleased):** on by default, the selector tries the
  membrane / reference-fairing / unfaired refined alternatives only when a loop's thin-plate
  patch is rejected (coplanar fold), charged against the Flap wall-clock budget, so both the
  CLI and the GUI get it and parity is automatic. It is disabled with the env-only
  `SUTURA_FLAP_SELECT=0` (like the ray-stab vote, no CLI flag or GUI control).
- **Localized exact self-union (`sutura/local_exact.py`, unreleased):** the tier that
  replaces a residual Stage-2 self-intersecting cluster with the exact outer-hull
  triangulation of a small patch. It is OPT-IN and OFF by default for every install
  (experimental; measured on the 40-mesh corpus it removes SI on only an isolated mesh and
  the ornate-frame fold is global, not local — see the class docstring and
  `docs/local-exact-notes.md`). It is forced with `SUTURA_LOCAL_EXACT=1` (values
  `1/true/yes/on`) and disabled with `SUTURA_LOCAL_EXACT=0`; there is deliberately NO CLI
  flag or GUI control, so CLI and GUI behave identically (parity by construction, like the
  ray-stab vote).
- **Tamponade (#17, interior hole plug + SI melt, v0.9.0):** CLI
  `--tamponade` / `--no-tamponade` and env `SUTURA_TAMPONADE` /
  `SUTURA_TAMPONADE_DEFAULT` control the on-by-default interior-plug tier; the GUI
  exposes the same as Options → Repair *Tamponade (fill holes from inside)*
  (EN/TR), and the repair-log panel renders the adopted / not-adopted verdict.
  The library default (`TAMPONADE_DEFAULT_ENABLED = True`) runs the tier on every
  path; `--no-tamponade` / `SUTURA_TAMPONADE=0` / the GUI checkbox disable it.
- **Stent (Tamponade's inflate-from-inside plug, v0.9.0):** a per-loop plug
  candidate inside Tamponade that runs when the reference plug is rejected for
  that loop. It rides INSIDE the Tamponade tier (it only runs when Tamponade runs
  — on by default; disable Tamponade itself with `--no-tamponade` /
  `SUTURA_TAMPONADE=0` / the GUI checkbox), so there is deliberately NO separate
  CLI flag or GUI control. Its only independent switch is the env opt-out
  `SUTURA_TAMP_STENT=0` (default
  ON whenever Tamponade runs), which is an exact geometry no-op; CLI and GUI
  behave identically because neither exposes it. The tier report carries
  `stent_used` / `stent_rejected` and the standard per-plug `plug_metrics`
  array; the GUI's tamponade log renders the adopted verdict unchanged. The
  name is deliberately NOT "balloon" to avoid confusion with method #8 Balloon
  (native implicit surface reconstruction).
- **Splint (#18, exact solid rebuild, unreleased):** CLI `--experimental-splint`
  / `--no-splint` and env `SUTURA_SPLINT` (`0/1/true/false/...`) control the
  off-by-default last-resort solid-rebuild tier; the GUI exposes the same as
  Options → Experimental *Splint (exact solid rebuild)* (EN/TR) and #18 is also
  selectable via the right-click *Use method* menu, with an EN/TR repair-log
  line rendering the adopted / not-adopted / aborted / budget verdict. The
  library default (`splint=False`, `SPLINT_DEFAULT_ENABLED = False`) keeps a
  direct `repair_mesh_from_arrays(deep_repair=None)` call byte-identical;
  `--experimental-splint` / `SUTURA_SPLINT=1` / the GUI checkbox enable it, and
  `--no-splint` / `SUTURA_SPLINT=0` are the disable path on both sides. The
  resolution, hash and wall-clock rules live in the shared
  `pipeline.presets.resolve_splint`, so CLI and GUI cannot diverge.
- **Native self-intersection detection (always ON):** the Rust-counted exact
  self-intersection classifier is the only path for every SI consumer; the
  PyMeshLab opt-out was removed. It has deliberately NO CLI flag or GUI
  control, so the CLI and the GUI behave identically (parity by construction,
  like the ray-stab vote).
- **fTetWild deterministic retry + time abort (v0.9.1):** the retry after a boundary
  that fails the holes/non-manifold guard is a pure function of that verdict (no
  wall-clock gate), and `FTETWILD_TIMEOUT` is only an outer all-or-nothing abort that
  keeps the previous stage result. Both are automatic pipeline behaviour (the CLI
  `--human` line and the GUI repair log show the abort plainly, the GUI via
  `repair_log_ftetwild_aborted`), so there is no separate flag or control and parity is
  automatic. The proposed P1 (content-addressed result cache) and P4 (deterministic
  replacement tier) are NOT implemented.
- **User-data directory (`SUTURA_DATA_DIR`, unreleased):** the runtime data path
  (`~/.local/share/sutura/data/`, holding `history.jsonl` and
  `triage_learning.json`) is overridden with an environment variable only. It is a
  process-level path setting (like `SUTURA_DIR`, `HOME` or `XDG_*`), not a repair
  option, so it needs no CLI flag or GUI control and the CLI and GUI resolve the
  same directory through the shared `sutura_engine/paths.py`. The uninstaller's
  `--purge` (which removes that directory) is an installer-script flag with no
  in-app counterpart, matching the other install/uninstall-time options.
- **Method health + `--self-check` / `--diagnostics` / `--self-repair` (P0, unreleased):**
  the per-method error isolation and the `method_health` / `method_errors` report
  keys are automatic pipeline behaviour (both CLI and GUI consume them, so parity
  is automatic; the `--human` `Methods: N/17 OK` line and the GUI repair-log
  `repair_log_methods` string render the same data). The three new commands are
  CLI-only surfaces and are documented as such in the table above: the GUI shows
  the startup availability banner (the `--self-check` availability half) but not
  the full fixture run or the self-repair/integrity actions. `--diagnostics` has
  GUI parity: the health banner's export button calls the same
  `sutura.diagnostics.create_diagnostics_zip(out_path)` and reports the outcome
  in the GUI, so it is listed as a covered surface in the table above.

- **Splint core (`sutura_geom.splint_cut`, experimental):** a `sutura_geom`
  PyO3 test hook for steps 1–8 of the Splint spike (Geogram Delaunay + winding
  cut + fix-A island cleanup), with `adaptive=None|'hardened'|'retry'` (default
  `None`; the retry uses the self-contained `cut_shells > 4` rule). It has
  deliberately NO CLI flag, GUI control or method-registry entry; step 9 dual
  contouring is out of this core, and the raw step-8 cut is generally not
  manifold. It is a research surface, not a user feature, so there is no parity
  gap to close yet.
