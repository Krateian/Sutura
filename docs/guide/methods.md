# Repair methods, presets and engines

[← Back to README](../../README.md) · [Türkçe](tr/methods.md)

## Triage Engine intensity presets and named profiles

The effort spent *after* Stage 1 — the deep-repair ladder, the fTetWild
tier's on/off state and its input-size cap, the Graft shell-wrap tier's own
input-size cap, the dense boundary decimation ladder and the Hausdorff sample
count — is grouped into
four read-only presets plus any number of user-named profiles, selected with
`--intensity <name>` (or `SUTURA_INTENSITY`, or the `intensity` key of
`~/.config/sutura/config.json`; the flag wins, then the environment, then the
config). Stage 1 is deliberately **not** affected: the repair mode
(`--mode`/the classifier) and the optional profile (`--profile`) keep exactly
their previous behaviour in every preset. The chosen intensity is reported as
`triage_intensity` and shown as an `Intensity:` line by `--human`;
`--list-intensities` prints the presets and profiles with their effective
values and exits.

- **Quick** — skips the deep-repair ladder and the fTetWild tier entirely
  (`deep_repair=off`, fTetWild off) and has an empty decimation ladder: the
  fastest run, which leaves hard meshes open.
- **Balanced** — the default and the shipped behaviour: every knob is the
  historical `repair.py` constant (fTetWild `auto` when installed, 180 s
  budget, 300,000-face cap, Graft bounded to a 300 s budget and a
  2,000,000-face cap, `(1.5, 3.0)` decimation ladder, 200,000 Hausdorff
  samples). `--intensity balanced` is byte-identical to running without the
  flag.
- **Thorough** — a 600 s fTetWild budget, a 900 s Graft budget, 400,000
  Hausdorff samples and one extra decimation rung, `threshold`
  (= `DENSE_RATIO × max(input, 20,000)`).
- **Extreme** — no input-size cap for fTetWild or Graft, a 1,800 s budget for
  both, 1,000,000 Hausdorff samples, the extra `threshold` rung, and, when
  every rung of a wastefully dense boundary fails, one more fTetWild run with
  the tetrahedron-quality optimisation on before falling back to the
  undecimated boundary.

### Deterministic work budgets

Repair tiers that change the output use **countable work budgets**, not wall
clocks: Flap charges a loop's `m^3` dynamic-programme cost before it starts and
each alternative patch before it is built (the Balanced/Thorough/Extreme
budgets are calibrated to the former 15/30/60-second presets, so a large but
fillable loop is not skipped for cost); Splint counts Delaunay tetrahedra;
Coaptation counts total body faces; Tamponade charges each boundary loop against
the tier work budget and no longer stops a stent search on a per-loop wall-clock
slice; Graft's closing ladder counts grid cells.
The same input and preset therefore produce the same mesh on a fast or a loaded
machine. `seconds` fields remain in the reports as measurements only.

Two documented exceptions remain. **fTetWild** is an external, itself
non-deterministic solver: its `ftetwild_timeout` stays as a per-attempt
watchdog, the deterministic bounds are the input-face cap and the fixed
attempt plan, and an `aborted_time` result is reported explicitly rather than
claimed to be deterministic. **Dressing**'s `within_time_budget` is report-only
(a warning) and never gates adoption. Graft's outer wall-clock abort is likewise
a documented watchdog; its output decision is the deterministic grid-cell work
budget.

The extra `threshold` rung targets the failure mode where a wastefully dense
fTetWild boundary cannot be decimated watertight at the lower `1.5×`/`3×`
targets. On thingi10k_73444 the `optimize=False` boundary returned 469,144
faces; the `20,000` and `23,646` targets both failed the watertight check
(`not_watertight`) while the `threshold` target of 80,000 succeeded, giving a
watertight result (adopted, `shape_changed` flagged at 5 % — the shape guard
is invariant and never a preset). The fTetWild and Hausdorff *guard* itself
(watertight + `FTETWILD_MAX_HAUSDORFF_REL`) is not a knob: it stays on in
every preset and in every profile.

**Named profiles** sit on top of the presets. Each profile is a base preset
plus a sparse set of field overrides (only the fields that were changed are
stored); the four built-in presets stay read-only. Profiles are stored in
`~/.config/sutura/profiles.json` — a separate, schema-versioned file written
atomically (temp file + rename), not a `config.json` key, so a corrupt
profiles blob can never poison the rest of the configuration. A corrupt,
unreadable, wrong-schema or missing file warns on stderr and falls back to
the built-in presets; it never crashes a repair. Names must be non-empty,
unique case-insensitively and must not shadow a built-in preset name. A
profile is selected exactly like a preset (`--intensity my-fork`); when one
is used the report adds `triage_profile_base` and `triage_overrides`
alongside `triage_intensity`. The Hausdorff sample floor (200,000) is
enforced in the resolver too: a hand-edited profile below it is clamped with
a warning. In the GUI (Options → *Repair*) the combo lists the presets, a
separator, then the user profiles, with *New*, *Duplicate*, *Rename* and
*Delete* (confirmed) buttons and an inline *Profile settings* editor for
every field; editing a built-in preset offers *Save as new profile*, and
*Reset to recommended* selects Balanced without deleting any profile.

The original file is never overwritten. Output is written with a `_fixed`
suffix in the same directory.

## Repair method registry and per-object recommendations

`sutura_engine.methods` (shimmed at `sutura/methods.py`) is an extensible registry
of nineteen repair **methods**, each identified by a stable integer and canonical
slug (`--list-methods` prints num, id, family, availability, and display name).
Methods 1–7 provide fine-grained selectors over the core pipeline (#1 Quick Clean,
#2 Local Mend, #3 Full Mend, #4 Join, #5 Autorefine, #6 Exact Refine, and #7 fTetWild);
#8–#10 are standalone surface-reconstruction tiers (#8 Balloon native implicit
closing and #9 Backplate flat-back closing for single-sided scans, #10 Scaffold
proxy-template reconstruction for heavily damaged meshes); #11 Transplant and #12
Transplant+ provide automated and point-guided repetition healing; #13 Graft is the
morphology shell-wrap envelope that closes large wounds while preserving healthy
surfaces verbatim; #14 Mirror Complete reconstructs symmetric single-sided scans;
#15 Wall Thicken is the opt-in morphological thin-wall thickener that never runs
automatically; #16 Dressing is the opt-in variable-viscosity level-set skin
(its own section below); and #17 Tamponade is the interior plug (on by default)
that fills open boundaries from the inside and melts residual self-intersections
in rolled-back layers (alias `lava`, its own section below); #18 Splint is the
opt-in exact solid rebuild (off by default); and #19 Coaptation is the
float32-ULP seam-weld + exact-union tier (on by default, adopted only when
strictly better; its own section below).
Methods 8–10 run on the original input (like the fTetWild tier) and adopt their
result only when it is strict-watertight and the original surface is still
covered (a one-sided input→output Hausdorff guard), so the estimated back
surface cannot silently move the model; the module's notes (e.g. "back
surface was estimated") are surfaced in the top-level `closing` report key.
Methods 11–12 run after the stage-1 chain on the watertight result: #11 detects
a rotational/translational/helical repeated pattern and transplants a healthy
copy onto each damaged or missing element, #12 transplants the element nearest
the user's `--repeat-source X,Y,Z` point onto the one nearest
`--repeat-target X,Y,Z`. The transplant is adopted only when it stays
watertight, no worse than the baseline, and the untouched geometry did not move
(`hausdorff_outside`); its `repeat` report key carries the pattern type and the
number of positions repaired.

### Repair methods (#1–#19)

The nineteen methods, in the stable integer order printed by `--list-methods`
(canonical IDs shown; legacy slugs remain fully supported as aliases):

| # | Canonical ID | Display Name | Family | What it does | Invents geometry? |
|---|---|---|---|---|---|
| 1 | `quick_clean` | Quick Clean | clean | Stage 1 + Manifold rebuild; no deep repair, no fTetWild. | no |
| 2 | `local_mend` | Local Mend | topology | Deep-repair ladder `local`: re-meshes only the damaged regions. | no |
| 3 | `full_mend` | Full Mend | topology | Deep-repair ladder `full` (the default balanced pipeline). | no |
| 4 | `join` | Join | topology | Moves small disconnected components onto the nearest larger one. | no (moves) |
| 5 | `autorefine` | Autorefine | si | Subdivides intersecting triangles along intersection segments (Lazard & Valque 2025). | no |
| 6 | `exact_refine` | Exact Refine | si | Exact arrangement-lite self-intersection split via the Rust core (`sutura_geom`). | no |
| 7 | `ftetwild` | fTetWild | envelope | Tetrahedralizes the original surface and extracts a watertight, SI-free boundary. | yes (volume remesh) |
| 8 | `balloon` | Balloon | closing | Native implicit (generalized-winding level set) reconstruction closing large openings on single-sided scans. | yes (back surface) |
| 9 | `backplate` | Backplate | closing | Flat back plane and perimeter walls for relief/plate meshes; keeps original face. | yes (back plane) |
| 10 | `scaffold` | Scaffold | template | Coarse watertight proxy hull with re-projection of healthy original regions. | yes (proxy) |
| 11 | `transplant` | Transplant | pattern | Detects rotational/translational patterns and transplants healthy geometry onto defects. | yes (transplant) |
| 12 | `transplant_plus` | Transplant+ | pattern | Interactive point-guided transplant (`--repeat-source`/`--repeat-target`); opt-in. | yes (transplant) |
| 13 | `graft` | Graft | envelope | Morphology shell wrap (dilation -> EDT -> erosion) keeping healthy triangles verbatim. | yes (shell wrap) |
| 14 | `mirror_complete` | Mirror Complete | closing | Reflects a single-sided symmetric scan across its detected symmetry plane. | yes (mirrored back) |
| 15 | `wall_thicken` | Wall Thicken | envelope | Opt-in morphological dilation thickening walls below `--wall-min-thickness`. | yes (offset) |
| 16 | `dressing` | Dressing | envelope | Opt-in variable-viscosity level-set skin; 2-manifold and SI-free by construction. | yes (viscosity coat) |
| 17 | `tamponade` | Tamponade | interior | Interior plug (alias `lava`, on by default): fills each open boundary from the inside, melts residual self-intersections in rolled-back layers; its **Stent** candidate inflates a plug inward from the rim (multi-stent partition for large loops); never deletes an input face. | yes (plug) |
| 18 | `splint` | Splint | solid | Opt-in exact solid rebuild: proxy surface + Delaunay tetrahedralisation, interior cut away (Rust core); deterministic, addon-free. | yes (solid rebuild) |
| 19 | `coaptation` | Coaptation | solid | On by default: closes a folded/overlapping multi-body soup's one-float32-ULP seam (boundary-only tolerance weld) and folds the closed bodies with the exact union; adopted only when strictly better than the ladder result. | yes (union fold) |

The geometry-inventing methods never run unguarded: #7 is bounded by a
one-sided output→input Hausdorff guard; #8, #9, #10, and #14 by an input→output
guard (ensuring the original surface remains fully covered); #11 and #12 are
self-guarded by `hausdorff_outside` (preventing movement of untouched geometry);
#13 Graft is self-guarded by its own healthy original→result fidelity check
(`fidelity_ok`), automatically falling back to fTetWild if healthy detail is lost;
#15 Wall Thicken is guarded input→output and excluded from auto-ranking
(`needs_user_input`) since wall thickening is an explicit design choice; #16
Dressing is self-guarded by its coat→input fidelity gate and likewise excluded
from auto-ranking (opt-in until measured on the corpus); and #17 Tamponade is
self-guarded by its own reload-honest holes/non-manifold/part-count/healthy-face
check; it is on by default in the pipeline but still excluded from auto-ranking
(`needs_user_input`) as an explicit alternative to it. #19 Coaptation is
self-guarded by its own strict adopt check (a reload-watertight union with zero
exact self-intersections and no worse holes, non-manifold edges or component
count); it runs on every full-mode input and returns the ladder result
byte-identical when it cannot improve it.

### Object analysis and recommendations

`--analyze <file>` performs a cheap per-object analysis (faces, components,
boundary loops, open-area ratio, single-side/relief scores, repetition score,
non-manifold edges, self-intersecting faces, mesh type) and prints ranked
method recommendations; third-party engines are
listed in a **separate** section and are never mixed into the ranking. The
self-intersection probe is capped at 200,000 faces — above that it samples and
flags the value as an estimate. `--methods 2,3,5` tags the methods to try, in
order. Without a flag the repair is **auto**: today's default pipeline runs
first and, when its result is already strict-watertight, the output is
byte-identical to before. Only when that baseline fails are ranked methods
tried (at most three extras; a geometry-inventing method such as fTetWild only
when its recommendation score is high enough). The fallback ladder is
**count-budgeted**, not time-budgeted: at most three ranked candidate methods
are tried, each with its own deterministic tier budget. When the count is
exhausted the best candidate is kept and the result is marked with a
"fallback attempts exhausted" note. A very high wall-clock watchdog (3600 s)
exists only as a batch safety net; if it ever trips it is reported explicitly
as `fallback_watchdog`, never silently treated as a normal ladder stop, and it
is not expected to fire on an unloaded machine. An explicit
`--deep-repair`/`--no-fallback-ftetwild` keeps today's exact behaviour instead
of escalating, and explicitly tagged methods (`--methods`) are never budgeted.
Before trying the ranked methods, the automatic path can skip a method that the
detected damage makes pointless: duplicate faces skip Join (#4), and interior
faces or an interior region skip Transplant (#11) and Wall Thicken (#15). This
only affects the automatic fallback order; the baseline repair is untouched.
`SUTURA_TRIAGE_PRUNE=0` restores the previous order.

For multi-object 3MF files, auto escalation operates per object: objects that
reload watertight after the baseline repair remain untouched, while failing
objects escalate through ranked fallback methods independently (reporting
per-object `method_used`). The report adds `method_used`, `methods_tried`,
`method_reached_watertight` and, when escalation ran, `analysis` and
`recommendations`.

**Learning triage (local, bounded).** When the anonymous usage history is
enabled, each repair also records the per-`(object type, method)` outcome
(accepted/rejected) and elapsed time into `~/.local/share/sutura/triage_learning.json`
(only template ids and method slugs — the same privacy rule as the history).
`rank_methods` then adds a small Bayesian bonus: a Beta(2, 2) posterior success
mean scaled to `[0, 0.05]`, damped by a speed factor, and exactly zero without
data. The bonus never lifts a method the geometric analysis did not already
favour. It is toggled with the `learning_triage` config key /
`SUTURA_LEARNING_TRIAGE` / `--no-learning-triage`, and reset from the CLI
(`sutura clear-learning`) or the GUI **Options → General → Reset learning**
button, without touching the usage history. No network is used.

### Per-file method tagging in the GUI (right-click menu)

The same registry is exposed in the GUI. The file list has a **Method**
column that shows *Auto*, *Auto (rec. #3 … 82%)* after an analysis, or the
tag chain the user chose. Right-clicking one or several files opens a menu:
*Analyze* runs the per-object analysis (`--analyze`) in the background,
*Recommended methods* lists the ranked top methods with a score bar and the
reason in the tooltip, *Use method* lists all nineteen methods (#1–#19) as a
checkable, order-preserving popup (the order in which they are checked is the
try-order, e.g. `#2 → #3 → #5`); picking #12 opens a small picker that
CPU-rasterises the object (no OpenGL) so the user can click the healthy source
element and then the damaged target element, and *External engines* lists the
configured third-party engines (kept separate from the ranking). *Clear tags* returns a
file to Auto. Tagged files run exactly their methods, in order; untagged files
keep the auto behaviour. Engines tagged per file are selected with the CLI
`--engines NAME[,NAME]` (`None` runs every enabled engine as configured).

![File-list right-click method menu](../../assets/method-menu.png)

### Repeated-element repair (auto/manual picker)

`--methods 11` (auto) and `--methods 12` (manual) target models that repeat a
congruent feature — gear teeth, knurls, thread crests, a regular tile/bump
grid, a 3D lattice of identical parts — where one or a few copies are broken,
dented or protruding. Detection is **segmentation-free**: it samples the
surface, builds the principal inertia axes and tests rotational orders 2–64
(plus high knurl counts 72/80/90/100/120) and translational steps — in 2D and
in a full 3D lattice — with a multi-step verification that rejects a
fundamental-order aliasing (`n=8` vs `n=24` on a 24-tooth gear). Alignment
supports uniform **similarity** (scale) and **mirror** as well as rigid
transforms. A damaged element is replaced by a healthy one through a
volumetric Manifold CSG transplant; missing material is detected by the
forward point-to-surface deviation, an extraneous bump by the reverse
distance, and the cavity cut is shrunk by 0.1 % so the donor and the cavity
slightly overlap (removing the coplanar boundary that otherwise collapses
into non-manifold edges on a float32 STL write). For multi-component
assemblies `extract_outer_shell` dilates the parts by a relative epsilon and
boolean-unions them into a single outer shell before repair. The transplant is
adopted only when it stays watertight, no worse than the baseline, and the
untouched geometry did not move (`hausdorff_outside`).

On the owner's `patterned samples` set (the #11 auto path):

| model | faces | repetition score | pattern | damaged element | outside Hausdorff | reload verdict |
|---|---|---|---|---|---|---|
| `gear 24teeth 2x.stl` | 30,144 | 0.649 | rotational C₂₄ | 1.5 mm dent on tooth #18 | 0.000000 mm | watertight (0 holes, 0 NM, genus 1) |
| `PET Bottle Cap.stl` | 29,792 | 0.669 | rotational C₁₂₀ | 0.51 mm knurl dent | 0.0849 mm (mean 0.00006) | watertight (0 holes, 0 NM) |
| `rubics-cubestl.stl` | 118,956 → 64,380 (outer shell) | 0.616 | 3D lattice (3×3×3) | 3.5 mm dent on a corner cubie | 0.000000 mm | watertight (0 holes, 0 NM, genus 0) |

**Honest non-results.** Not every detected symmetry is repairable, and the
tier reports the reason rather than forcing a transplant. On the pistachio
bowl (369,732 faces, score 0.749) and the Sea Shell Vase (359,910 faces,
score 0.763) the detected order is a *continuous* spiral shell / a scaling
fan of ribs, not a discrete congruent array; a rigid CSG transplant would tear
the 1.5 mm shell, so `positions_repaired` is 0. On `large-shell.stl`
(1,318,202 faces) the score is 0.000 — genuine organic morphology with
variable-size growth, no congruent repetition. All three are written
unmodified (stage-1 output), never silently reshaped.

### Single-side scan closing

Methods #8 (`poisson_close`) and #9 (`flat_back_close`) in
`sutura/closing.py` handle a mesh that is a **single-sided** scan or relief
with one dominant opening — the "back" is missing, so no repair can be
verified against real geometry. #8 extracts the native generalized-winding signed
level set over the open boundary; #9 keeps the measured surface exactly and closes the
opening with a flat back plane and side walls (the cap follows the
boundary-loop orientation, so clockwise loops no longer build an inverted
cap). Both run on the **original** input, not the cleaned arrays (stage 1
would already have flat-capped the loop), and are adopted only when the
result is strict-watertight and the original surface is still covered by a
one-sided input→output Hausdorff guard. The module's notes (e.g. "back
surface was estimated") land in the `closing` report key.

### Mirror completion

Method #14 (`mirror_complete`, `sutura/mirror_repair.py`) is a symmetry-aware
alternative to the implicit Balloon close for a single-sided scan of an (approximately)
bilateral object. `detect_mirror_plane` fits the dominant boundary loop's plane
and scores how planar and dominant that loop is; when the confidence clears the
threshold, `mirror_close` reflects the visible surface across the plane (with
flipped winding, so the shared boundary edges cancel and the seam closes),
welds the seam, and — for overlapping halves — dissolves them into one outer
shell with `extract_outer_shell`. When the symmetry confidence is low or the
mirrored stitch is not a valid solid it falls back to the native implicit close (#8); the `closing` report's `mode` field says which path ran
(`mirror` vs `poisson_fallback`) and `mirror_score` records the confidence.

### Thin-wall analysis and thicken

`sutura/wall_thickness.py` (behind method #15, `wall_thicken`) estimates the
local wall thickness of a mesh from a signed-distance-field grid: area-weighted
surface samples with a nearest-normal sign, and an inward ray march from each
face centroid to the first zero crossing (with a nearest opposite-facing-sample
fallback). `estimate_wall_thickness` returns a **heatmap-ready per-vertex**
array plus `min`/`median`/`mean` and the count of vertices below a target.
`thicken_to_min` offsets thin walls by a morphological dilation of the solid
(`delta = (target - min) / 2`, bounded to 25 % of the bbox diagonal) and
re-extracts the surface with a dependency-free marching-tetrahedra pass. #15 is
opt-in by design (`needs_user_input`, so it is excluded from the ranking and
never auto-escalated) and is selected explicitly — `--methods 15`, optionally
with `--wall-min-thickness T` (default 1 % of the bbox diagonal).

### Proxy template repair

Method #10 (`proxy_template`, `sutura/proxy_repair.py`) targets a heavily
broken but otherwise healthy mesh: it builds a coarse watertight proxy from a
native implicit (generalized-winding level-set) reconstruction, then **re-projects the healthy original
regions** onto it so the detailed surface is preserved and only the missing
parts come from the proxy. It is bounded by a face budget and, like the
closing tiers, adopted only when strict-watertight and the original surface
stays covered (input→output guard).

### Flap (surface hole fill)

Flap (`sutura_engine.flap`) is a surface-based hole filler that covers a
boundary loop by continuing the neighbouring surface instead of laying a flat
lid over it. It runs as a **Stage-1 pre-pass on the raw input**, before the
native stage-1 chain and before the Graft/fTetWild tiers, so those tiers see the flap-closed
surface. Each loop is triangulated with a minimum-area dynamic program, refined
to the surrounding edge length, and faired with a discrete biharmonic
(thin-plate) solve that carries the rim's position and slope into the patch. The
original triangles are kept verbatim -- only the new patch is added -- and any
loop Flap skips or rejects is left to the ordinary Stage-1 flat hole fill, so
every hole still gets a lid.

It is **on by default**. `--no-flap` (or `SUTURA_FLAP=0`) turns it off, `--flap`
(or `SUTURA_FLAP=1`) forces it explicitly, and the GUI exposes the same switch
as the Options → Repair checkbox **Flap surface hole fill**. A candidate
is adopted only when the reload-honest `holes + non-manifold` count does not
worsen; a flatness-based cleanup then drops zero-thickness debris sheets and
never a legitimate solid part. A pre-chain exact-SI rise is **flagged, not
rejected**: Stage-1/Stage-2 can lower it (or raise it further), so a final
safety net measures both the Flap-ON and Flap-OFF finals and keeps the better
one, scored damage → parts → exact SI. A lower SI never buys a part-count
change. Above the exact classifier's 150k-face cap the SI is unknown and the
comparison falls back to damage/parts; `--si-mode off` skips the report/deep-
repair SI policy but the Flap guard still measures it internally for this
decision. When the thin-plate patch is rejected (a coplanar
fold), the loop tries the membrane, reference-fairing and unfaired refined
candidates in turn and commits the first that passes the same guards within the
deterministic Flap work budget (`FLAP_WORK_BY_INTENSITY`; a loop's `m^3` DP cost
is charged before it starts, then each alternative before it is built). The
Balanced, Thorough and Extreme budgets match the former 15, 30 and 60-second
presets, so a large but fillable loop is not skipped for cost. The
wall clock is recorded for the report only and never decides adoption, so the
same input and preset produce the same patch on any machine or load; set
`SUTURA_FLAP_SELECT=0` to disable this per-loop selection and keep the
thin-plate-only behaviour.

The fill core is the Rust implementation (`sutura_geom.flap_fill`, part of the
`sutura_geom` extension), which is a hard requirement of the runtime. The
pure-Python oracle that previously served as a fallback has been removed, so
Flap always uses the Rust core; the topology and report fields are unchanged
(faired vertices agree with the former oracle to about 1e-8).

The report carries a `flap` key, present only when the switch is on: `ran`,
`adopted`, `reason`, `baseline_holes`, `baseline_non_manifold`,
`candidate_holes`, `candidate_non_manifold`, `loops_found`, `loops_filled`,
`loops_skipped`, `loops_skipped_by_reason`, `patch_faces`, `new_vertices`,
`time_s`, `max_orig_vertex_move_mm`, and `debris_cleanup` when the cleanup
dropped anything.

Measured on the 41-mesh regression: Flap was adopted on 13 meshes, and the run
ended reload-watertight on all 41. Across the corpus it reduced the total
Stage-1 self-intersection count from 10,051 to 8,880; on ornate-frame it
reduced the Stage-1 self-intersections from 4,016 to 3,005; and the maximum surface deviation on thingi10k_1038441
dropped from 4.11 mm to 0.60 mm. The Rust engine keeps the whole 41-mesh corpus
Flap step at 7.2 s.

### Tamponade (interior hole plug, #17)

Method #17 Tamponade (`sutura_engine.tamponade`, Rust `sutura_geom.tamponade`)
closes open boundaries from the inside. Instead of laying a patch over the
opening, it walks each boundary loop and builds a plug that meets the shell at
the narrowest point, without touching the surrounding healthy triangles; the
input faces are carried through verbatim (`healthy_faces_kept`). Residual
self-intersections are then melted in thin layers from the deepest point
outward, and a layer that would not reduce the crossing count is rolled back, so
the tier never ends up worse. It is the interior counterpart of Flap and Graft.
The boundary walk and the reference-surface ring use a canonical vertex order,
so the loop decomposition and the selected plug do not depend on the order the
input triangles are stored in. It runs as a **pre-pass on the cleaned input,
before Flap and Stage 1**, and
gets a **second chance after the Stage-1/Flap closing ladder, before the
Graft/fTetWild tiers**.

It is **on by default**: disable it with `--no-tamponade` (or
`SUTURA_TAMPONADE=0`), or the Options → Repair checkbox **Tamponade (fill holes
from inside)**. A plug is rejected only when one of these holds, so the tier only
changes the output when it strictly helps:

- it **leaks outside the solid** — a plug sample lies in empty space beyond
  `clamp(diagonal/10000, 0.0125, 0.05)` mm from the original surface and not over
  the hole aperture;
- it **creates holes or non-manifold edges** on the reload-honest (welded) mesh;
- it **overlaps an existing surface with the opposite orientation** (a double
  wall) over more than 20 % of its own area — opposite coincident faces can
  cancel material in an even-odd slicer, whereas a same-orientation overlap is
  unioned by a positive-fill slicer and is left alone;
- it **would add a separate part or a closed shell** of its own (a plug must be
  edge-connected to the main shell through its boundary loop);
- or fewer than 99 % of the pre-tier faces are kept.

Inside the tier, a loop whose reference plug is rejected gets an
inflate-from-inside candidate, **Stent**: the plug is grown inward and clamped so
it can never rise past the rim (a flat through-hole cannot inflate into a dome,
and a Stent seal is always built on the original rim, never on a reference
overshoot trim). A loop a single Stent cannot seal is partitioned into simple
sub-loops at its pinch points and across the shortest valid interior chord, and
one Stent is inflated per sub-loop; adjacent sub-plugs share the chord edge
exactly, so the seam stays manifold, and the group is judged all-or-nothing. The
name is deliberately **not** *balloon*, to avoid confusion with method #8 Balloon
(native implicit reconstruction). Stent runs only when Tamponade runs; its only
switch is the env opt-out `SUTURA_TAMP_STENT=0` (default on, a geometry no-op).

Because the adoption gate is topological, a plug that is clean but changes the shape or drops the part count could still pass; a final safety net therefore compares the post-tier result against the pre-tier result on the reload-honest mesh and re-runs with Tamponade off whenever the tier did not strictly help, so the tier can never make the final model worse than `--no-tamponade`.

Self-intersections are **report-only** (a slicer unions overlapping volumes), so
they never block adoption. The Rust extension must provide
`sutura_geom.tamponade` (there is no numpy fallback); otherwise the tier reports
itself unavailable and is skipped. The deterministic work budget follows the
intensity preset in cut-grid cells (`TAMPONADE_WORK_BY_INTENSITY`; `quick`
disables the tier); a loop's `throat_cells` cost is charged before its min-cut,
and the melt pass is bounded by its layer count, not a clock. The prepass
override `SUTURA_TAMPONADE_PREPASS_BUDGET` is a count in the same units (the
old seconds meaning is retired). The report carries
a `tamponade` key with
`ran`, `adopted`, `reason`, `plugs`, `plug_faces`,
`melt_layers_tried`/`_accepted`/`_rolled_back`, `micro_welds_ok`/`_failed`,
`healthy_faces_kept`, `input_faces`, `output_faces` and `seconds`.

### Splint (exact solid rebuild, #18)

Method #18 Splint (`sutura_engine.pipeline.tiers.splint`, Rust core
`sutura_geom.splint_cut`) is the **last-resort** tier of the deep-repair ladder:
it builds an unsigned-distance proxy of the input surface, Delaunay-
tetrahedralises it and cuts the interior away, producing a solid boundary that
is watertight by construction. It is deterministic and addon-free (no fTetWild
extra), and is tried **after Dressing and before fTetWild**.

Splint is **opt-in and off by default** (`--experimental-splint` /
`SUTURA_SPLINT`, or `--no-splint` / `SUTURA_SPLINT=0` to force it off; the
library default keeps `deep_repair=None` byte-identical). A candidate is adopted
only when it reloads strict-watertight; when the raw cut is closed but not
two-manifold, it is first rebuilt through the Stage-2 Manifold bridge and the
rebuild is kept only when it is no worse on holes and non-manifold edges. Its
cost is bounded by a resolution cap (`SPLINT_RES=128`), an input-face cap and a
deterministic work budget in Delaunay tetrahedra (`SPLINT_WORK_BY_INTENSITY`);
the Rust cut itself is a single, non-interruptible call, so when the budget is
exhausted the whole candidate is dropped. The
report carries a `splint` key with `ran`,
`adopted`, `reject_reason`, `input_faces`, `output_faces`, `output_parts`,
`out_manifold`, `cut_shells`, `construct_status`, `manifold_postprocessed`,
`res`, `adaptive*` and `seconds`.

### Coaptation (seam-weld union, #19)

Method #19 Coaptation (`sutura_engine.pipeline.tiers.coaptation`, Rust core
`sutura_geom.close_holes` + `union_pair`) targets a folded/overlapping
multi-body soup — for example a thin sheet the stage-1 chain closes as two
overlapping halves. Such a result reloads as several edge-connected bodies
whose seams are separated by roughly one float32 ULP and is left closed but
riddled with self-intersections where the bodies cross, so Graft / Dressing /
Splint / fTetWild never run. Coaptation runs on the **original input arrays**
(like Graft/fTetWild/Splint), welds the seam (only boundary-edge vertices
within `COAPTATION_SEAM_ULP_K=1` float32 ULP are merged — interior vertices are
never merged, so a genuine thin wall cannot collapse), closes each body and
folds them with the exact union.

It is **on by default** (`--coaptation` to force, `--no-coaptation` /
`SUTURA_COAPTATION=0` to disable; the ON switch is
`COAPTATION_DEFAULT_ENABLED`/`SUTURA_COAPTATION_DEFAULT`) and sits **after
Splint and before fTetWild**. A candidate is adopted only when it is
**strictly better** than the result the ladder currently holds: reload
watertight, exact self-intersections 0 and below the baseline's measurable
count, and no worse on holes, non-manifold edges or component count. Otherwise
the ladder result is returned byte-identical, so a mesh that cannot benefit is
unchanged. Its cost is bounded by an input-face cap (`COAPTATION_MAX_FACES`,
`skipped_budget`/`reject_reason='too_large'`) and a deterministic work budget
in total body faces (`COAPTATION_WORK_BY_INTENSITY`) checked before the exact
union. The report carries a `coaptation` key
with `ran`, `adopted`, `gate`, `ulp_k`, `tol`, `bodies_in`, `bodies_after_weld`,
`merged`, `baseline`, `candidate`, `input_faces`, `output_faces`,
`skipped_budget`, `aborted`, `reject_reason`, `reason`, `warnings` and
`seconds`.

### Graft (morphology shell wrap, #13)

Method #13 Graft (`sutura_engine.graft`, legacy `sutura/shell_wrap.py`) is a high-fidelity
morphology shell-wrap tier tried automatically before fTetWild whenever a mesh
contains severe damage or complex topological defects.

> **Note:** Graft (#13) requires the `sutura_geom` Rust extension (installed automatically
> by `install.sh`, `install-macos.sh`, and bundled in AppImage/.dmg distributions). The
> extension is mandatory: without it the installer fails and the CLI refuses to start with
> a named error, so there is no degraded Graft/fTetWild fallback path.

1. **Generalized-Winding Signed Distance**: Using the Rust `sutura_geom` core, a
   signed-distance field is constructed on an adaptive voxel grid tuned to the
   median edge length of healthy triangles.
2. **Morphological Closing**: The surface undergoes a morphological dilation followed
   by a true Euclidean Distance Transform (EDT) re-distancing and erosion. The
   closing radius begins at ~1.5 voxels and expands dynamically only until the
   extracted dual-contouring envelope is reload-watertight.
3. **Verbatim Hybridization**: The envelope is refined and projected back onto the
   original vertices. Triangles in healthy regions are preserved byte-identical from
   the input mesh, while the envelope bridges and seals damaged or missing regions.
4. **Fidelity Guard & Fallback**: The result is adopted only if reload-watertight. If
   the healthy surface deviation exceeds threshold (`fidelity_ok == False`), auto-triage
   evaluates fTetWild in parallel and adopts whichever candidate achieves lower
   healthy-surface deviation. Detail loss is flagged in the report (`graft.warnings`).
5. **Deterministic resolution**: The grid budget is a pure function of the explicit
   intensity preset (Quick/Balanced 1.5M cells, Thorough 10M, Extreme 25M), so the same
   input plus the same options always produce the same envelope on any machine. Free RAM
   never selects an arbitrary resolution: if the preset budget would not fit, Graft falls
   back once to the Balanced grid (reported in the output) and only a case where even that
   does not fit skips Graft with a clear `low_memory` message.
6. **Deterministic closing ladder**: The closing-radius ladder stops on a deterministic
   work budget derived from the input grid and the preset, not on a wall clock, so the
   same input and preset give the same mesh on a fast or a loaded machine. The wall-clock
   budget is only a documented outer watchdog/safety abort: if it trips, Graft is
   abandoned, the stage-1 result is kept, and both the report and the GUI say so
   plainly. It is not a normal budget and does not participate in the deterministic
   work decision.
7. **Input-coverage guard**: an envelope is accepted only if it covers at least half of the
   input's real extent (`graft.GRAFT_MIN_INPUT_COVERAGE = 0.5`; the report carries
   `input_coverage` and `coverage_ok`), so a collapsed result that happens to be accurate
   where it remains is rejected and the stage-1 result is kept.
8. **Self-intersection policy**: a Graft candidate that reduces holes or
   non-manifold edges but raises the exact self-intersection count is accepted
   and the rise is flagged (`si_flagged`) instead of rejecting the candidate,
   matching fTetWild. Without that topology gain the strict non-rise rule
   applies. The face/vertex blow-up ceiling is unchanged.

### Dressing (variable-viscosity volumetric skin, #16)

Method #16 Dressing (`sutura_engine.dressing`) is the surgical sibling of Graft:
instead of bridging the damaged region, it dips the whole part in a spatially
varying "liquid" and extracts the level set `F(x) = s(x) - r(x)`, where `s` is
the generalized-winding signed distance and `r` a viscosity radius that stays
thin over detailed healthy surface and grows thicker over damage. The
isosurface of a scalar field is 2-manifold and self-intersection-free by
construction, which is what makes Dressing the tool for whole-shell topological
folds where local patch excision and hybrid splicing fail.

1. **Feature-anchored resolution**: the voxel size follows the mesh's
   characteristic feature size (median edge length), clipped to the intensity
   preset's diagonal band, so a coarse open fragment is not oversampled (the
   earlier `diag/200` rule produced a 1 M-face coat and 7 % deviation on a
   71-face mesh).
2. **Variable viscosity**: the base radius is thin (fidelity comes from a thin
   coat, not surface snapping — projecting the coat back onto the input
   re-introduced thousands of self-intersections and is permanently rejected),
   the bridging radius is capped by the preset and by 45 % of the largest gap.
3. **Numpy prototype backend, Rust-ready**: the field build and marching
   tetrahedra run in numpy today; the Rust `sutura_geom.dressing_coat`
   narrow-band core is feature-detected and takes over automatically when
   available. Decimation is a native quadric edge-collapse (meshoptimizer) that
   rolls back atomically if it would open a hole,
   create a non-manifold edge or introduce an exact self-intersection.
4. **Drain (healthy-region erode-back)**: the coat sits ~`r_base` outside the
   original, so a *drain* term is added **inside the level set**
   (`F = s - r + delta_r`) — never by vertex projection, which re-introduced
   7,850 self-intersections. Full drain cuts healthy growth ~40 % (median
   +0.33 → +0.20 mm) with a harmless ~4 µm median inward dip while defects stay
   covered. `--dressing-drain MODE|mm` / `SUTURA_DRESSING_DRAIN` / the GUI
   *Drain* combo override the preset default (Quick off, Balanced/Thorough
   full, Extreme deep).
5. **Auto-ladder fallback behind a single default switch, shipped OFF.** Method
   #13 Graft (verbatim hybrid) remains the default automated repair pass after
   Stage 1 (it keeps healthy original triangles verbatim where it can). With the
   switch on (`DRESSING_DEFAULT_ENABLED` / `SUTURA_DRESSING_DEFAULT=1`) the Auto
   ladder treats Dressing as an opt-in fallback after Graft and before fTetWild;
   under `--si-mode repair`, a positive exact self-intersection count also
   triggers the tier (under default `--si-mode report`, only remaining holes and
   non-manifold edges trigger fallbacks). `--experimental-dressing` /
   `SUTURA_DRESSING=1` / the GUI checkbox *Dressing (viscosity coat)* **force**
   it to run (warning: *watertight, but the shape may deform / fine detail may be lost*);
   `--no-dressing` / `SUTURA_DRESSING=0` disable it. Adoption requires
   strict-watertight, exact-SI-free, the preset's coat→input fidelity gate, and
   the shape-preservation gates below. **The switch is OFF by default**: on a
   fine-detail relief (ornate-frame) the voxel staircase visibly flattens
   fluting even though the mean deviation stays ~0.08 mm, so Dressing must not
   replace the verbatim Graft/fTetWild result on such parts until that quality
   issue is fixed.
6. **Shape-preservation gates.** Besides fidelity, a candidate is adopted only
   when, relative to the result the ladder held before Dressing: the signed
   volume changes by at most `DRESSING_MAX_VOLUME_DELTA` (10 %), the
   connected-component count grows by at most `DRESSING_MAX_PARTS_SLACK` (1),
   and the P95 coat-vs-input **vertex-normal** angle over healthy surface stays
   below `DRESSING_MAX_NORMAL_ANGLE_P95` (30°, a placeholder — measured coat P95
   is 12–36° on the accepted meshes and 82–88° on the detail lost frames).
   The normal metric (nearest input vertex via a KD-tree, healthy = not a defect
   vertex) catches the staircase that the position-only Hausdorff gate does not;
   the volume/parts gates catch a coat that absorbs a cavity or shatters into
   debris. The geometric CAD/machined-vs-organic score (`cad_likeness`) is
   recorded on every Auto run for the future calibrated CAD guard; the ladder
   already runs Graft first for all inputs, so a CAD-like part gets the verbatim
   Graft hybrid before Dressing and Dressing only runs when damage remains.
7. **Defect-set and viscosity overrides**: `--dressing-defects {all,holes_nm}`
   (`SUTURA_DRESSING_DEFECTS` / the GUI *Defects* combo) selects which input
   defects drive the viscosity mask — `all` (the default) is holes,
   non-manifold edges and self-intersections, `holes_nm` drops the
   self-intersection term so a scan whose only damage is holes/non-manifold
   edges is not thickened by spurious SI. `--dressing-rmax-scale F` /
   `--dressing-sigma-scale F` (`SUTURA_DRESSING_RMAX_SCALE` /
   `SUTURA_DRESSING_SIGMA_SCALE` / the GUI *r_max* and *sigma* combos) multiply
    the resolved `r_max` / `sigma`; the default (unset = 1.0) is byte-identical,
    and non-positive or unparseable values are ignored.
8. **Component cleanup**: before decimation the extracted triangle soup is split
   into vertex-connected components and the extraction debris — sub-voxel shells
   below a face-count, voxel-volume and relative-volume floor — is dropped. A
   surviving inverted shell is kept only when it is nested inside a kept material
   shell *and* the input's generalized winding number at its centre is ~0, i.e.
   the input is genuinely empty there (a true cavity). On the 13-mesh
   `dressing_tuned` set this collapsed `thingi10k_145065` from 669 to 1
   component and `thingi10k_63785` from 25 to 1 (both `number_of_parts = 1` in
   OrcaSlicer afterwards) while keeping the three genuine material lumps of
   `thingi10k_1038439`; the removed counts and volume are reported in
   `report['dressing']['cleanup']`.
9. **Opt-in suggestion and `--dressing-force-adopt`.** When a plain Auto
   repair still leaves the geometry open or non-manifold (under default
   `--si-mode report`), the report carries a `suggestions` list (`method = dressing`,
   separate from the defect `issues`, so the category is unchanged). The `--human`
   report prints a marked hint (`⚠ Not watertight…`) that points at
   `--experimental-dressing` when Dressing never ran:
   `  ⚠ Not watertight. You can try Dressing: --experimental-dressing (watertight, but the shape may deform / fine detail may be lost).`
   or at `--dressing-force-adopt` when Dressing ran and its coat was rejected by
   the shape-preservation gates (with the failed volume / normal numbers in
   parentheses):
   `  ⚠ Not watertight. Dressing ran but its result was rejected by the quality gate (...).`
   `    Force it with --dressing-force-adopt if you accept the deformation (the shape may deform).`
   `--dressing-force-adopt` implies `--experimental-dressing` and
   skips the volume / components / normal / fidelity gates while STILL
   requiring a strict-watertight, self-intersection-free candidate; the record's
   `forced` field marks when the gates were bypassed. The GUI exposes the same
   flag as a batch-wide Options → Experimental checkbox *Dressing: force adopt (may deform)*
   and, after a batch in which any result offered the hint, a non-blocking button
   *⚠ Try Dressing (makes it watertight, but the part may deform)* (or
   *⚠ Try Dressing on %d file(s) (watertight, but the part may deform)*)
   that re-runs exactly those files with the flag (and sets the checkbox).

### Honest watertight verdict (verified after save/reload)

STL stores no vertex sharing, so a mesh that is watertight on the in-memory
index topology can reload with non-manifold edges once the loader re-welds
exactly-coincident `float32` positions — a class of false "watertight" claims
that surfaced on 7 of the 40 real-world samples (e.g. `thingi10k_1038439`).
Sutura now judges the **saved** mesh, not the in-memory arrays:

- **P-HONEST** (`repair.enforce_reload_verdict`) re-welds the final mesh in the
  save/reload-equivalent form (positions cast to `float32`, exact duplicates
  merged) and only lets a report claim watertight when that form is
  strict-watertight. A false claim is downgraded (`reload_watertight = false`,
  `stage1.reload_holes` / `reload_non_manifold`,
  `stage2.watertight_after_reload = false`), and the method registry applies
  the same rule, so such a baseline counts as failed and the auto path
  escalates. A genuinely watertight mesh keeps its report and output
  byte-identical.
- **P-WELD** (`repair.p_weld_final`) is the reload-safe final pass that fixes
  the seam instead of only reporting it. When the in-memory topology is
  already a clean 2-manifold, it splits the `float32`-coincidence vertex
  groups with a sub-ULP nudge (face count preserved, geometry moved by less
  than one part in 10⁶); when the welded mesh itself is repairable it falls
  back to topology repair plus small-hole capping. A candidate is adopted only
  when the saved mesh reloads strict-watertight, so the pass can never make
  the output worse. `report['p_weld']` records before/after holes and
  non-manifold counts, the method and the number of nudged vertices;
  `--human` prints a *Reload-safe final pass* line.

With P-WELD, all seven previously-`warning` samples now reload strictly
watertight with unchanged face counts — `thingi10k_100281`, `1038439`,
`1038441`, `145065`, `224108`, `248395`, `71691` — while already-watertight
meshes are left untouched (`p_weld` absent).

## External repair engines and the fTetWild manager

Sutura can call user-installed third-party repair **engines** and manage the
fTetWild fallback tier from the same interface (CLI and GUI). The engines are
opt-in and inert until configured.

**External engines.** An engine is a small TOML file in
`${XDG_CONFIG_HOME:-~/.config}/sutura/engines/*.toml` naming an executable and
its arguments (with `{input}`, `{output}` and optional `{tmpdir}`
placeholders). Engines are slotted into the pipeline at a `placement` —
`before_stage1`, `after_stage1`, `after_stage2`, `replace_ftetwild` or
`final_fallback` — or ordered explicitly by a `chain.toml` listing the
built-in stages (`stage1`, `stage2`, `deep_repair`, `ftetwild`) and engine
names; an unknown name rejects the whole chain and falls back to the default.
Commands run without a shell, in their own process group, killed on timeout.
**Every engine result passes the same guard as fTetWild** — adopted only when
its holes and non-manifold edges are no worse than the current result, and an
adopted result far from the input is flagged `shape_changed` (Hausdorff >
`FTETWILD_MAX_HAUSDORFF_REL`) rather than rejected; an engine can never bypass
or disable the guard. Each run adds a per-engine report entry (`name`,
`placement`, `rc`, `time`, `adopted`, `reject_reason`, stdout/stderr tails)
and broken configs only warn in `engine_warnings`, never fail a repair. With
no engines configured the repair is byte-identical to the pre-engines
behaviour. `sutura engines list` shows the resolved chain and each engine's
placement, enabled state and whether its binary was found; `sutura engines
check` additionally exits non-zero when a configured binary is missing (it
never runs an engine).

**fTetWild manager.** `sutura ftetwild status|install|uninstall` inspects,
installs or cleanly removes the fTetWild tier (`pytetwild` only) in the Sutura
environment only (the Linux main `venv`, or its `venv311` fallback, or the
macOS conda `sutura-env`), showing the download and on-disk sizes first and
requiring `--yes` (or an interactive confirmation) for install/uninstall;
`--dry-run` prints the plan and never touches the environment. It refuses
unsupported layouts (AppImage, frozen `.app`/`.dmg`, system Python,
read-only environments) with an explicit reason, and uninstall prunes only
the packages the extra added, protecting the base environment. In the GUI
this is the **Engines** tab of the Options window: a status line
(installed/version/size or the unsupported reason), Install/Remove with the
size shown in the confirmation and a streamed, cancellable progress dialog,
plus the configured engine list with Reload, *Open engines folder* and a link
to `docs/external-engines.md`. The threads are interruptible and joined when
the window closes.

**Last-resort solid engine (experimental).** Beyond the fTetWild tier, the
**Solid engine** setting chooses what rebuilds the last resort: `ftetwild`
(default) keeps the existing fTetWild tier, while `cdt` runs an experimental
exact constrained tetrahedralization that keeps the original surface exactly
where it can and extracts a watertight solid from it. It runs after Coaptation
and before fTetWild; if its result fails its checks (watertight, one closed
outward-facing solid) or runs out of time, the ladder falls through to fTetWild,
which then runs with its own time limit, so the worst case takes about twice as
long. Set it with `--solid-engine cdt|ftetwild`,
`SUTURA_SOLID_ENGINE`, the Options → Repair control, or the `solid_engine` key in
the Sutura settings file (the GUI value is saved there and inherited by the CLI,
Quick Action, Dolphin and the OrcaSlicer plugin). The choice is recorded under
`report['solid_engine']` (`selected` / `produced` / `reason`). Known limitation:
for now CDT is slow — on many models, even ones with a few thousand faces, it
does not finish within its time limit, and Sutura then falls back to fTetWild
automatically. Use it on small or simple models, or when you want the original
surface kept exactly.
