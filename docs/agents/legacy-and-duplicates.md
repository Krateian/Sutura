# Legacy And Duplicates

Trigger: Backlog, resolved backlog, duplicate/outdated markers.

Source: `origin/main:AGENTS.md` @ 5e28b78, ranges 310-378, 379-414.

## Backlog (v0.3 notes — diagnosed, NOT fixed yet)

- **Pre-v0.3 security audit (2026-09, FAZ12):** `docs/security-audit-2026-09.md`
  records the on-release scan. Result: dependencies (pip-audit), shell/
  subprocess patterns, 3MF XML (XXE — regex parser, no XML lib, so safe by
  construction), ZIP (no extract → no zip-slip), path traversal, and hardcoded
  secrets are all CLEAN. Three LOW recommendations deferred to the maintainer:
  checksum/signature verification for install-script downloads (install.sh
  tarball, build_appimage.sh python-build-standalone/appimagetool), GitHub
  Actions commit-SHA pinning (currently major-version pins), and a 3MF
  zip-bomb decompressed-entry size cap (theoretical memory-DoS, judgment call).

- **Extreme mode can WORSEN heavy-self-intersection scans.** 75-model corpus:
  for scan-derived meshes with thousands of self-intersections (e.g.
  thingi10k_804302 has 14,730 SI; Ephebe 2,491 SI + 327 non-manifold),
  aggressive/extreme escalation never reached watertight, and extreme often
  made things worse (thingi10k_147525: 0 -> 159 holes; Athena: aggressive 56
  holes -> extreme 125 + lost two-manifold) because removing self-
  intersecting faces opens more boundary edges. `mode_suggestion_keys` fires
  `sug_holes_many`/`sug_si_*` on exactly these meshes — the suggestion text
  may over-promise extreme's value on heavy-SI scans. Revisit the suggestion
  wording/conditions for v0.3; do not attempt to fix now.

- **fTetWild output fidelity and determinism.** fTetWild is not
  deterministic (output and run time vary between runs even with
  `num_threads=1`), and its result is re-triangulated. Adopted results can
  change the shape where openings and cavities are closed: one-sided
  output-to-input Hausdorff (benchmark, macOS 2026-09-26) 3.7 % of the
  diagonal on thingi10k_1038441, 1.8 % on 1038439, 1.1 % on 224108, 8 % and
  5 % on two corpus meshes. The earlier "28 % on thingi10k_46012" was a
  measurement artifact: pymeshlab's `get_hausdorff_distance(samplevert=True)`
  also samples unreferenced vertices, and the fTetWild bridge output keeps
  every tetrahedron vertex (interior ones included); when no post-process
  compacts the boundary, those interior points dominate the maximum.
  `repair._hausdorff_rel` (used by the benchmark) drops unreferenced vertices
  first.
- **fTetWild time budget.** fTetWild runs under a 180 s budget. On
  artec_metal-nut (89,999 faces) it times out on macOS even unloaded
  (`ftetwild_error='timeout'`, 2 holes left, `warning`; the only strict
  failure of the 40 real-world samples), and on a loaded machine it can time
  out on thingi10k_248395 as well (15 holes left). The budget/timeout
  behaviour (budget size, fTetWild parameters such as the target edge
  length, input reduction, reporting) needs its own treatment.
- **The exact arrangement is SI-free only as exact rationals (float64
  hand-off evaluated, not adopted).** `indirect_bridge.py` rounds the exact
  output to `float32`, and the `si_after: 0` in its report holds only for
  the exact rationals. Measured (2026-09-25, 8 SI meshes, pymeshlab
  self-intersecting faces before/after the stage-1 chain and after the
  final float32 write): self-intersections come back with ANY floating
  representation, not because of float32 in particular. Re-scanning the
  rounded output with the crate's own exact classifier, 1038441 still has
  4,220 proper SI pairs in float64 and 4,068 in float32 (input 4,436);
  small meshes go to 0 (100045, 55772 in float32). float64 is mostly WORSE
  after the chain (1038441 16,402 vs 7,252 SI faces, 1038439 625 vs 221,
  46012 2,040 vs 1,224; metal-nut about equal), because float32 rounding
  merges near-coincident vertices (1,322 on 1038441, 153 on 1038439) so
  those faces share vertices and no longer count as intersecting. The
  arrangement also leaves touch points and coplanar overlaps unresolved by
  design (re-scan of 1038441: ~365k touch points, ~13k coplanar overlaps).
  A float64 hand-off is therefore NOT a fix; the real fix is snap rounding
  inside the Rust core (round the exact output to a float grid without
  creating new intersections, as CGAL's autorefine does), together with
  resolving touch points and coplanar overlaps — a large, output-changing
  project. The CDT variant bits 6 (edge-point propagation + Sloan) lower the
  post-chain SI clearly on dense scans (1038441 7,252 -> 4,036, 46012 1,224
  -> 786, 100281 3,832 -> 3,760) but raise it slightly on 100045 (0 -> 3)
  and 1038439 (221 -> 258); not a default recommendation. Measurement
  scripts were throwaway (/tmp).

## Backlog resolved

- **Closed but not two-manifold after stage 1, without fTetWild — FIXED.**
  thingi10k_145065 left the chain strictly watertight (0 holes, 0
  non-manifold edges, manifold3d_watertight=True) but with 18 pinched
  vertices, so `two_manifold=False`, stage 2 was skipped and the category
  was `warning`; fTetWild never runs there. `repair._split_pinched_vertices`
  (called right before the final stage-1 measurement, so it also covers an
  adopted fTetWild boundary) repeats `meshing_repair_non_manifold_vertices`
  up to `PINCH_SPLIT_MAX_PASSES=5` times (145065: 18 -> 2 -> 1 -> 0) and keeps
  the result only when holes and non-manifold edges stay 0; report key
  `pinched_vertices_split`. `classification.py` and the stage-2 gate are
  unchanged. Note that STL carries no topology: the split vertices share
  coordinates and weld again when the file is reloaded; the pinch is a
  property of the shape and slicers handle it — the fix corrects the stage-2
  flow and the report. Regression: `tests/test_pinched_vertices.py`.

- **Strict-watertight but `warning` after fTetWild — FIXED.** Diagnosed on
  macOS (224108: 3/3 watertight; 248395: 7/10 watertight, 2 warning, 1
  fTetWild timeout): the warning runs had `holes_remaining=0`,
  `non_manifold_edges_remaining=0`, `two_manifold=false`,
  `manifold_postprocessed=false` and no stage 2, i.e. a pinched vertex on the
  fTetWild boundary; every run with `manifold_postprocessed=true` ended
  watertight. The manifold3d post-process now also triggers on a closed but
  not two-manifold boundary (see the fTetWild bullet in How it runs).

- **LOD overshoot on ~17/74 corpus models — FIXED.** `_decimate_lod` used a
  "closest to target" selection that could pick an ABOVE-target step when
  the next step overshot below by a similar margin (measured up to 5.5k tris
  vs target 3500). It now picks the result AT OR BELOW the target that is
  closest to it (never above, never over-collapsing when a step jumps far
  below), with a finer ascending step list `(1.0, 1.5, 2.0, 3.0, 5.0, 8.0)`.
  Verified on all 17 overshoot models (now 1,616-3,368 tris, all <= target)
  and 5 of them re-timed (FPS 44-46 -> 64-98, all >= 64 FPS). Small meshes
  at or below the target are returned unchanged.


## Flagged duplicates / outdated (do not delete; review separately)

These items are preserved in the topic files (or in the cited file) and are
flagged here for a later, separate review. This pass only marks them.

| Source | Tag | Note |
|---|---|---|
| `AGENTS.md` 1-14 -> `workflow.md` | OUTDATED? | The "two-stage" overview may predate the current native-core flow; compare with `pipeline-stages.md` before changing the wording. |
| `AGENTS.md` 476-558 -> `workflow.md` | DUP | Process discipline is also canonical in `.claude/skills/sutura-dev/SKILL.md`; the skill remains authoritative. |
| `AGENTS.md` 585-674 -> `workflow.md` | DUP | Release checklist is also canonical in `.claude/skills/sutura-release/SKILL.md`; the skill remains authoritative. |
| `AGENTS.md` 415-436 -> `verification-bench.md` | DUP | Verification tiers are also canonical in `.claude/skills/sutura-verify/SKILL.md`. |
| `AGENTS.md` 437-466 -> `verification-bench.md` | OUTDATED? | Test commands use `~/.local/share/sutura/venv` and PyMeshLab-era wording; compare with the current native `sutura_geom` runtime before changing anything. |
| `AGENTS.md` 39-40 -> `packaging-updater-selfrepair.md` | REVIEW | `venv311` / PyMeshLab-era install narrative must be checked against the current native core; no deletion here. |
| `AGENTS.md` 211-218 -> `pipeline-stages.md` | DUP | Measure-helper differences overlap `docs/agents/diagnosis-classifier.md` and `docs/agents/verification-bench.md`; keep one canonical explanation later. |
| `tests/test_updater.py:116` | CODE-COMMENT | Mentions AGENTS.md in a code comment; this docs-only task does not edit code. |
