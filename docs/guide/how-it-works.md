# How Sutura works

[← Back to README](../../README.md) · [Türkçe](tr/how-it-works.md)

## How Sutura repairs a mesh, in plain words

A broken 3D model is like damaged skin. A scanner leaves **holes** where it saw
nothing, triangles get **glued together wrongly** (non-manifold edges), and parts
of the surface **pass through each other** (self-intersections). Slicers either
refuse such a file or print it wrong.

Sutura works like a careful surgeon: it treats the damage and leaves healthy
skin alone. A repair runs as a short chain of steps, and each later step only
acts when the earlier ones left work behind:

```
your file ─► Tamponade ─► Flap ─► Stage 1 clean-up ─► (still open?) Tamponade ─► (still broken?) Graft ─► Dressing/Splint (opt-in) ─► Coaptation (on) ─► fTetWild (if installed) ─► Stage 2 solid ─► repaired copy
             plugs holes  fills holes  fixes edges,                 plugs holes                  rebuilds only     one closed,
             from inside  along the    closes what is               from inside                  the damaged areas printable solid
                          surface      left
```

1. **Tamponade: plugs the holes from the inside, like lava** (on by default).
   It runs first, as a pre-pass on the cleaned-up input before Flap and
   Stage 1, so a genuine crater is sealed at its narrowest point instead of
   getting a flat lid. It fills the part from the inside: picture the inside of
   the part flooded with lava. Where the shell is intact the lava simply
   touches it and stops, so the original surface is not changed. At a hole the
   lava would escape, so it is held back and sealed right at the shell; that
   hardened lava is the plug. Folds trapped inside the part are melted from the
   deepest point outward, one layer at a time; if a layer would create a bad
   edge (the kind that makes the mesh non-manifold) that last layer is undone,
   and a very small spot is welded shut instead. It gets a second chance after
   Flap and Stage 1 for any hole they leave, and is kept only if the result is
   measurably better. The name comes from surgery: a tamponade fills a cavity
   from inside to stop bleeding at its opening. It is on by default; turn it
   off with `--no-tamponade` / `SUTURA_TAMPONADE=0` or the GUI Options
   checkbox. Inside Tamponade, **Stent** is an
   inflate-from-inside plug used when the reference plug is rejected for a
   loop; it stops at the shell and never leaves the solid. Not related to
   method #8 Balloon (native implicit surface reconstruction).
2. **Flap: fills holes the way the surface wants to go** (new in 0.8.0, on by
   default). A classic hole fill puts a flat lid on a hole. Flap continues the
   surrounding surface into the hole instead, like brushing liquid primer or
   epoxy over a gap: the patch leaves the rim at the same slope as its
   neighbours, so a hole in a curved surface gets a curved patch, not a dent or
   a flat spot. Your original triangles are not moved; only the patch is new.
   If Flap cannot fill a hole safely it leaves it to the next step, so a file is
   never worse with Flap than without it. Turn it off with `--no-flap` or the
   *Flap surface hole fill* checkbox.
3. **Stage 1: clean-up (native).** Removes duplicate and broken triangles,
   untangles bad edges, makes every face point the same way and closes any
   hole that is still open.
4. **Graft: only when the damage is serious.** If the model is still open or
   tangled, Graft builds a fresh closed shell over the damaged areas and grafts
   it onto the healthy original surface, like a skin graft. Healthy areas keep
   their original triangles, so fine detail such as engraving or ornament
   survives. Very large scans have a size and time limit so they never hang.
5. **Stage 2: make it one solid (Manifold).** Rebuilds the result as a valid
   closed solid, using the same Manifold kernel Bambu Studio uses.
6. **Optional extras, mostly off by default.** **Dressing** dips the whole model
   in a virtual coat that always comes out closed, but it can soften fine
   detail, so Sutura only *suggests* it when everything else failed and you
   decide; **Splint** is the opt-in exact solid rebuild. **fTetWild** is a
   last-resort rebuild for badly broken files and runs automatically (installed
   by default) when stage 1 still leaves holes; an experimental exact rebuild
   (CDT, `--solid-engine cdt`) can take its place, with fTetWild still used if
   CDT fails or runs out of time.

After every step Sutura counts holes and bad edges again, as if the file had
been re-opened from disk; a step that makes things worse is thrown away. Your
original file is never changed: you get a repaired copy and a report that lists
exactly which steps ran. Small self-intersections that slicers ignore are
reported but no longer treated as failures (`--si-mode repair` restores the
strict behaviour).

### The helpers behind the steps

The numbered steps are the visible repair. Around them sit a few helpers that
decide what to do and check the result:

- **Diagnosis** looks at the model before any repair and works out what kind
  of part it is — a machined mechanical shape, a smooth organic shape, a
  scanned surface, a lattice — so the later steps can pick sensible settings.
  It runs first.
- **Triage** turns that diagnosis and the kind of damage into a plan: which
  repair methods to try, in what order, and how long to spend on them. It runs
  between the steps and drops any method that would take too long.
- **Chart** is a cache of earlier work, keyed by the shape of the input. It is
  checked before the heavy steps, so repairing the same geometry again (or the
  objects inside one file) is fast.
- **X-Ray** is the honest ending check. Instead of trusting the counts held in
  memory, it re-opens the file that was just written and counts holes and bad
  edges the way a slicer would. It runs at the end of a repair.
- **Stitch** runs just before that check. A file can look closed in memory but
  reload with a small gap once its coordinates are rounded on write; Stitch
  heals those seams without moving any face.
- **Hull** extracts the outer surface. When the model is an assembly of parts
  that touch or overlap, it keeps only the outside, so internal faces do not
  turn into defects. It runs as a pre-step when Diagnosis recognises an
  assembly.
- **Cast** is the link to Sutura's own fast geometry engine (a small Rust
  helper). It is loaded only while a step that needs exact measurements runs,
  such as the exact self-intersection split or the Graft and Dressing shells.

The repair tiers themselves — Flap, Tamponade, Graft, Dressing, Splint and
fTetWild — are described in the steps above.

## Repeatable results

Repair decisions that change the output use counted work, not elapsed time. The
same file with the same repair settings therefore produces the same result on a
fast machine, a slow machine or a busy one; the time in the report is only a
measurement. A few external or report-only limits keep a very high safety
timeout (the fTetWild solver, Graft's outer abort) or a warning
(Dressing's soft budget); those are documented exceptions and are called out in
the report when they matter. The fTetWild tier (used only for meshes stage 1
cannot close) is one more exception: on two runs it can split the surface into
slightly different triangles, but the shape and printability stay the same.

## Why a staged pipeline

* **Stage 1 - native engine (Rust `sutura_geom` + `sutura_engine`).** Removes
  duplicate and degenerate faces, repairs non-manifold edges and vertices,
  orients faces coherently, closes holes of *any* size, and drops tiny open
  debris components. The filters mirror the battle-tested VCG `meshing_*`
  semantics Sutura was built on, reimplemented natively (no PyMeshLab at
  runtime), so the pipeline stays dependency-light and permissively licensed.
* **Stage 2 - Manifold (in the Rust core).** Rebuilds the closed mesh as a
  valid manifold solid and merges overlapping shells with a boolean union.
  This is the same C++ Manifold kernel Bambu Studio uses, compiled into the
  mandatory `sutura_geom` extension; it guarantees the output is a single
  closed two-manifold.

Each stage fixes what the other cannot: stage 1 closes big holes but does not
resolve self-intersecting geometry; Manifold guarantees a watertight result
but it rejects any input that is not already closed (`NotManifold`), so
stage 1 must finish the mesh first.

Stage 2 runs in-process in the mandatory `sutura_geom` Rust extension on
every platform. There is no Python 3.11 venv for Stage 2 and no subprocess
bridge: the extension statically links the Manifold C++ kernel. A failed rebuild is
reported explicitly, never silently omitted.

**Optional tiers.** Around the two core stages sit tiers for self-intersecting
and badly broken meshes. The experimental ones are off by default and enabled
by their own CLI flag or a checkbox in the GUI's **Options** window; the
fTetWild fallback is installed and on by default:

* *Before stage 1:* `--experimental-autorefine` (splits intersecting
  triangles along their intersection segments, never deletes a face) and
  `--experimental-indirect-autorefine` (the same idea with exact predicates in
  the Rust `sutura-geom` core, evaluation-only). In `auto` mode, when a model fits
  the `mechanical` or `dense_scan_heavy_si` templates and contains multiple
  closed components that touch or overlap (e.g. multi-part assemblies like
  Rubik-style models), an **outer-shell extraction** pre-step dissolves internal
  interfaces via a slight dilation (`5e-4 * bbox_diagonal`, ~0.05 mm for a
  100 mm object) and boolean union, eliminating internal faces and degenerate
  self-intersections before repair passes.
* *After stage 1, as a last resort:* the fTetWild fallback (tetrahedralizes
  the original input with fTetWild and extracts a watertight boundary;
  installed by default, ~6 MB, see Install). It runs by default, but only when
  stage 1 still leaves holes or non-manifold edges;
  `--no-fallback-ftetwild` (GUI: *Options* → *Repair* tab → uncheck *fTetWild fallback*) turns it off,
  `--experimental-fallback-ftetwild` (GUI: *Options* → *Repair* tab → *+ self-intersections
  (slow)*, below it) also runs it on a closed result that still
  self-intersects. fTetWild's own tetrahedron-quality optimisation is off
  (only the boundary is used, and it mostly costs time);
  `--ftetwild-optimize` (GUI: *Optimise tetrahedra*, not recommended)
  turns it on. Alternatively, the experimental exact rebuild **CDT**
  (`--solid-engine cdt`; GUI: *Options* → *Repair* → *Solid engine*) takes the
  place of this last resort and falls back to fTetWild when it fails or runs
  out of time.
* *In Full Mend, after stage 1:* a **guarded self-intersection excise + re-cap**
  removes residual self-intersecting faces (the faces plus a one-ring band;
  if that round reduces but cannot close the pre-existing boundary, the same
  round is retried once with a two-ring band, and the retry is rejected if it
  adds a connected component), closes the opened loops with the native hole
  fill (`selfintersection=True` — which *prevents* new crossing caps) and
  commits a round only when the self-intersection count strictly drops,
  non-manifold edges do not increase, and the mesh closes completely (no
  boundary edges); otherwise the pre-round mesh is restored byte-for-byte.
  Self-intersection detection uses the exact Rust classifier. No connected
  component is ever dropped, at most three rounds run (the round count and a
  30 % face-fraction brake bound the work; the time shown in the report is only
  a measurement), and the step is a no-op on meshes without self-intersections.
  Disable it with `SUTURA_SI_EXCISE=0`.

These after-stage-1 tiers form the **deep-repair ladder**, selected with
`--deep-repair {off,local,full}` (or `SUTURA_DEEP_REPAIR`, or the
`deep_repair` key of `~/.config/sutura/config.json`; the flag wins, then the
environment, then the config). `full` is the default for the CLI and the
file-manager menu and runs the fTetWild tier exactly as described above. `off`
runs no tier; when holes or non-manifold edges remain, the JSON report
carries `deep_repair.available` (remaining holes and non-manifold edges, the
tiers that could still run and a rough time estimate `estimate_s`; the
estimate coefficients are placeholders until they are calibrated on the
corpora) and `--human` shows a "Deep repair available" line. `local` is an
experimental prototype that re-meshes only the damaged regions: faces on a
boundary or non-manifold edge and their one-vertex ring are deleted, the
openings are closed with the native refined hole filling and the new
interior vertices are smoothed with the boundary fixed; it is adopted only
when holes and non-manifold edges do not get worse, self-intersections do not
increase and every face outside the deleted region is unchanged. It only
runs on small, simple damage (no non-manifold edge, boundary loops of at most
16 edges, at most 8 damaged regions and 2,000 deleted faces, 10 s); hole
refinement is off because it cost ~9 s per 90k-face mesh without changing a
result. Measured on the 40 real-world samples it made 3 more meshes strictly
watertight (31 → 34); on the 115-mesh corpus it gained none, which is why it
is not part of `full`.

Before the ranked fallback methods are tried, the automatic path can skip a
method that the detected damage makes pointless: duplicate faces skip Join,
and interior faces or an interior region skip Transplant and Wall Thicken. It
only changes the fallback order, never the baseline repair; set
`SUTURA_TRIAGE_PRUNE=0` to restore the previous order.

The **self-intersection policy** is selected with `--si-mode
{repair,report,off}` (or `SUTURA_SI_MODE`, or the GUI Options → Repair combo
*Self-intersections* [Report only / Repair / Do not measure]).
`report` (the default) measures and reports the residual
self-intersection count, but self-intersections no longer block the
`watertight` classification verdict on a closed 2-manifold surface, nor do
they escalate the deep-repair ladder on their own — only holes and
non-manifold edges drive the automatic tiers and the verdict; the count stays
visible in `stage1.self_intersections_remaining`. A mesh with zero holes and
zero non-manifold edges is classified as `watertight` under `report` even when
residual self-intersections remain. `repair` restores the
earlier behaviour where a positive count counts as damage and can trigger the
automatic Dressing fallback. `off` skips the exact classifier and reports the
count as `null` ("not measured"). Explicit force flags
(`--experimental-dressing`, `--experimental-fallback-ftetwild`) override the
policy and still act on residual self-intersections. The guarded
`si_excise_recap` pass keeps its own independent count in every mode. The
chosen mode lands in the report as `si_mode` (and `deep_repair.si_mode`).
On automatic runs the final-output check is recorded in `si_escalation`:
`triggered`, `outcome`, `reason`, the adopted `method`/`num`/`name`, the
`candidates` list (each with its verdict, work estimate, elapsed time, metrics
and SHA-256), the `baseline` metrics and `tolerance_mm`. When it does not run,
`triggered` is false and `reason` says why. Explicit `--methods` runs do not
auto-escalate.

Every tier is adopted only when its result is no worse than the plain
staged result on holes and non-manifold edges, so the core staged path always
remains the safety net. An adopted fTetWild result whose one-sided
output-to-input Hausdorff distance exceeds 1 % of the bounding-box diagonal
(provisional threshold) is kept but flagged: the report carries
`shape_changed: true` and the issue `shape_changed` ("Shape changed by the
fTetWild fallback"), without downgrading the category. Measured on macOS
this concerns thingi10k_1038441 (3.7 %), 1038439 (1.7 %) and 224108 (1.1 %)
of the 40 samples. When the default fTetWild boundary (run without the
quality optimisation) fails the holes and non-manifold guard even after the
Manifold pass, the mesh is tetrahedralized once more with the
optimisation on, within what remains of the 180 s budget; the report lists
both runs (`attempts`) and names the adopted one (`adopted_attempt`).
Inputs above 300,000 faces skip the fTetWild tier (`reject_reason:
too_large`): the slowest completed run on the 40 samples took ~55 s at
90,000 faces, so larger scans cannot finish within the budget, and the
dense Artec scans of the 115-mesh corpus timed out on every run.
With the fTetWild tier skipped (`SUTURA_WITH_FTETWILD=0`) and with no flag set,
the behaviour is exactly the Stage-1 + Stage-2 core above.

Measured on the 40 real-world samples (`scripts/benchmark_repair_corpus.py`,
2-vCPU x86_64 VM): strict watertight 31/40 without the fallback, **39/40**
with the default fallback (the tier ran on 9 meshes, adopted on 8; the one
left open, `artec_metal-nut`, hit the 180 s per-mesh budget). Total run time
64 s → 669 s; the extra time is spent only on meshes that would otherwise stay
open. Running it also on closed-but-self-intersecting results
(`--experimental-fallback-ftetwild`) gives the same 39/40 in 2,595 s and
remeshes 7 already-closed meshes, which is why that stays opt-in. Caveats:
fTetWild's output and run time are not reproducible run to run (e.g.
thingi10k_248395 took 30–160 s, occasionally beyond the budget, even with one
thread); the result is re-triangulated (face counts change, often coarser);
every input point stays close to the output (0.2–1.4 % of the bounding-box
diagonal on the checked meshes) while the output can add surface away from
the input where openings or cavities are closed (one-sided output-to-input
Hausdorff distance 1–4 % on thingi10k_224108, 1038439 and 1038441, 5–8 % on
two corpus meshes; an earlier figure of 28 % on thingi10k_46012 came from a
measurement that also sampled interior tetrahedron vertices). fTetWild now runs without its mesh-quality
optimisation (`optimize=False`; the optimisation only improves the discarded
interior tetrahedra): in a parameter sweep on macOS it cut thingi10k_46012
from 34 s to 6 s with a 0.09 % Hausdorff distance, while artec_metal-nut
still exceeded the budget. The run times and counts above were measured
with the optimisation on.

Since v0.9.1 the retry after a boundary that fails the holes/non-manifold check
is deterministic (it no longer depends on the remaining wall clock), and the
time budget is only an outer all-or-nothing abort that keeps the previous stage
result. The output is still not byte-identical run to run.
