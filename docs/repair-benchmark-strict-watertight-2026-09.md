# Repair benchmark — strict watertight measurement (115-mesh corpus)

Date: 2026-09-20
Repo state: HEAD `c0f5d26` (v0.2.8 + FAZ 1 `developable_fraction` signal + FAZ 2
40-mesh corpus / retrained classifier head).
Harness: `scripts/benchmark_repair_corpus.py` (added in this session, so the
measurement is reproducible).

## Scope

Re-measure the 115-mesh real-world repair corpus with a **strict** watertight
metric: a `defects.detect()` closed-loop check on the **actual final output
geometry** (the exact arrays saved to the `_fixed` file), instead of trusting
the pipeline's own verdict. This is a measurement/benchmark task — no repair or
classifier code was changed.

Corpus: `/tmp/sutura_corpus_100/` — the same 115 meshes (52 Artec STL scans,
63 Thingi10K) used for the v0.2.3 "manual macOS run" (~90% fully watertight).
Total input size 5.97 GB; largest mesh 477 MB (`artec_dual-clutch-gearbox-hd.stl`).

## Methodology

- Pipeline: repo `sutura/repair.py` under the PyMeshLab environment
  (single conda env; stage 2 runs the manifold3d bridge in-process, the macOS
  layout), `--mode auto`, default classifier engine (`experimental`).
- Each mesh: `scan_bad_coordinates` → load via pymeshlab → input
  `defects.detect()` → `repair_mesh_from_arrays` (auto) →
  `maybe_run_stage2` → `defects.detect()` on the **final** arrays (stage-2
  rebuild when stage 1 closed and stage 2 ran; stage-1 output otherwise) →
  pipeline category via `classification.classify`.
- **Strict watertight** := `defects.detect(final)` reports zero holes AND zero
  non-manifold regions. One file error never kills the run.
- Total wall time ~39 min (sum of per-file `dt` = 2344 s).

### Metric clarification (what "strict" means, and what it does not)

Three different "boundary" numbers exist and are **not** interchangeable:

1. **pymeshlab `get_topological_measures` `boundary_edges`** counts undirected
   boundary **edges**, not holes. Verified empirically: a single triangle
   reports `boundary_edges=3`, a single 4-edge hole reports 4, and a one-huge-
   hole mesh with a 1144-edge rim reports 1144 while the real hole count is 1.
   The historical `//2` shortcut (`boundary_edges // 2`) **undercounts** and is
   not a hole count either. This is the known "1144 vs 1" behaviour — both
   numbers are correct, they measure different quantities.
2. **`repair.boundary_loop_stats`** (numpy) walks the boundary edges into
   connected loops and returns `(n_holes, max_loop_len)`. This is the metric
   behind the pipeline's `stage1.holes_remaining` and is the same loop-walking
   algorithm as `defects.detect_holes`, so the pipeline's own hole metric is
   already loop-based.
3. **`defects.detect()`** = the loop-walking hole count **plus** a non-manifold
   detector (edges shared by more than two faces). **Strict watertight** here
   means running this on the final output arrays: `holes == 0 and
   non_manifold == 0`.

Self-intersections are deliberately **out of scope** of this strict closed-loop
metric: `defects.detect()` does not measure them (they remain a separate signal
in the pipeline's validate report), and a manifold3d stage-2 rebuild eliminates
them by construction on the meshes where it runs.

## Results

| Metric | Count | % |
|---|---|---|
| Meshes run | 115 | — |
| Crashes / hard errors | 0 | 0% |
| **Strict watertight** | **102** | **88.7%** |
| Pipeline category `watertight` | 102 | 88.7% |
| Pipeline category `warning` | 13 | 11.3% |
| Pipeline category `error` | 0 | 0% |

**Claim-vs-check agreement: perfect.** Every pipeline `watertight` claim is
confirmed by the strict check, and no `warning` mesh is secretly strict-
watertight (0 discrepancies in both directions).

**Input → output defect closure** (same detector, before/after):

| Defect | Input total | Final output total |
|---|---|---|
| Holes (boundary loops) | 244 | 39 |
| Non-manifold regions | 47 | 0 |

205 of 244 input holes were closed (~84%); every input non-manifold region was
fixed. The 39 remaining holes are concentrated in the 13 warning meshes below.

## Comparison with the previous run

The previous measurement (`/tmp/sutura_corpus_results.json`, installed v0.2.8
copy, pre-FAZ 1/2 classifier) reported **103 watertight / 12 warning / 0 error**
on the same corpus. The current run reports **102 / 13 / 0**:

- **Regressed to warning (1): `artec_metal-nut.stl`** — caused by the FAZ 2
  retrained classifier head, **not** by the strict metric (see below).
- **Recovered to watertight (0):** none.

The remaining 12 warning meshes are the same set as the previous run.

## The 13 warning meshes, in detail

`in_*` = input `defects.detect()`, `s1_holes` = `stage1.holes_remaining`,
`s1_2manifold` = `stage1.two_manifold`. Stage 2 never ran on any of these
(`s2=None`), because stage 1 did not close them — the pipeline's rule, unchanged.

| mesh | in holes | in nm | s1 holes | s1 2-manifold | type / conf | notes |
|---|---|---|---|---|---|---|
| artec_airplane-without-texture | 2 | 0 | 1 | yes | mech 0.932 | 1 stubborn hole |
| artec_car-body | 1 | 0 | 1 | yes | mech 0.898 | 1 stubborn hole |
| artec_church-facade | 22 | 0 | 7 | yes | org 0.127 | heavy scan holes; sub-gate conf → defaults |
| artec_fountain-basin | 5 | 0 | 1 | yes | mech 0.629 | sub-gate conf → defaults |
| artec_metal-nut | 12 | 0 | 1 | yes | org 0.022 | **FAZ 2 regression**, see below |
| artec_michel-rodange-monument | 1 | 0 | 1 | yes | mech 0.725 | sub-gate conf → defaults; 1 stubborn hole |
| artec_smart-car | 146 | 0 | 12 | yes | org 0.584 | heaviest scan-hole count; VCG limit |
| thingi10k_100827 | 5 | 0 | **9** | **no** | mech 0.948 | hole count **grew** during the chain; output not even 2-manifold |
| thingi10k_1017012 | 0 | 6 | 1 | yes | mech 0.982 | all nm fixed, 1 hole opened |
| thingi10k_1038439 | 10 | 0 | 3 | yes | mech 0.978 | heavy scan holes |
| thingi10k_1038441 | 5 | 13 | **6** | yes | mech 0.194 | all nm fixed but hole count grew 5→6; sub-gate conf → defaults |
| thingi10k_1038444 | 0 | 2 | 1 | yes | mech 0.982 | all nm fixed, 1 hole opened |
| thingi10k_224108 | 1 | 4 | 1 | yes | org 0.904 | all nm fixed, 1 stubborn hole |

Root-cause groups:

- **Heavy scan-hole sets that VCG cannot fully close** (`artec_smart-car`
  146→12, `artec_church-facade` 22→7, `thingi10k_1038439` 10→3). The
  mesh-sensitive `maxholesize` covers the largest input loop, but remaining
  loops stay open (self-intersecting boundaries, or holes re-opened by the
  debris / non-manifold passes after the final `close_holes`).
- **Hole count grew during the chain** (`thingi10k_100827` 5→9 and ends
  non-two-manifold; `thingi10k_1038441` 5→6). The duplicate/non-manifold/debris
  passes re-open edges. `thingi10k_100827` is the worst case — the output is
  not even 2-manifold.
- **Sub-gate classifier confidence → tuned thresholds disabled.** Several
  warning meshes fall below their class gate (mech < 0.75, org < 0.70), so the
  conservative defaults are used and less scan debris is dropped:
  `fountain-basin` 0.629, `michel-rodange` 0.725, `church-facade` 0.127,
  `1038441` 0.194, and `metal-nut` 0.022 (below).
- **1–2 genuinely stubborn holes** on otherwise-clean outputs (car-body,
  airplane, michel-rodange, 1017012, 1038444, 224108) — the documented VCG
  limit.

### `artec_metal-nut.stl` — the only regression vs the previous run

Previous run (installed v0.2.8): organic, confidence **0.987** → tuned
(`mincomponentsize=12`) → **watertight**. Current run (FAZ 2 head): organic,
confidence **0.022** → below `ORG_TUNE_GATE` (0.70) → tuning disabled → default
`mincomponentsize=8` keeps the small open debris component → 1 hole remaining →
**warning**, stage 2 not attempted.

This is a **classifier confidence collapse**, not a strict-metric artefact: the
same file still repairs watertight with `--classifier-engine classic`
(organic 0.987, tuning applied, 0 holes, stage 2 ok). The FAZ 2 retrain on the
grown 40-mesh corpus moved this organic scan's confidence from 0.987 to 0.022.
Because auto-mode repair gates tuning on the classifier confidence, one mesh
silently dropped out of the watertight set. Flagged for the classifier work;
not fixed here (this session is measurement-only).

## Conclusion

- The pipeline's watertight claims hold up under a strict, independent
  closed-loop check: **102/115 (88.7%)** of the corpus output is genuinely
  watertight, with **zero** claim-vs-check discrepancies.
- On the same corpus the previous run reported 103/115; the single difference
  is `artec_metal-nut.stl`, regressed by the FAZ 2 classifier head, not by any
  repair change.
- The remaining 13 warnings are genuine VCG limits on scan meshes (heavy hole
  sets, holes re-opened by the repair chain) and low-confidence classifier
  gates — consistent with the README's existing "~90% fully watertight, not
  100%" claim, which does not need to change.

## Addendum (FAZ 4) — metal-nut regression fixed, corpus back to 103/115

The `artec_metal-nut.stl` regression was fixed with a **near-boundary
classic-agreement confidence fallback** in `mesh_classifier_v2.py` (see the
README "Mesh type-aware repair" row): when the head is a coin-flip
(`|p_mech − 0.5|·2 < 0.15`) and classic strongly agrees with the barely-chosen
class (class-score ≥ 0.70), the head keeps its class but reports classic's
confidence, so the tuning gate sees the strong agreeing signal. Confident head
decisions are never overridden.

Re-run of the full 115-mesh corpus with the fix (same harness, same auto
mode): **103/115 (89.6%) strict watertight**, 0 crashes, 0 claim-vs-check
discrepancies — the warning set is now **identical** to the original pre-FAZ 2
run (the same 12 meshes; `artec_metal-nut` restored, **zero new regressions**).
The fallback fires on exactly one mesh (metal-nut, head confidence restored
0.022 → 0.987 organic); the three near-boundary DISAGREEMENT cases
(`crankshaft`, `motorcycle-engine-cover-hd`, `plaster-cast-teeth`) keep their
low confidence → no tuning → conservative defaults, the intended behaviour.