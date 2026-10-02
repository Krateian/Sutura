# Localized exact self-union — experiment notes (unreleased)

`sutura/local_exact.py` (plus the Rust `PyMeshBvh` winding binding in
`rust/sutura-geom`) is an experimental, opt-in tier that resolves a residual
self-intersection cluster after Stage 2 and P-WELD by cutting a small patch
around the cluster, computing the exact planar arrangement of the patch and
replacing it with the triangulated **outer hull** of the arrangement.  It is
forced with `SUTURA_LOCAL_EXACT=1` and is OFF for every install.

## What it guarantees

Four properties were treated as non-negotiable and are covered by
`tests/test_local_exact.py`:

1. The outer-hull winding filter uses one BVH over the **whole** mesh, never
   the patch alone (an open patch has no meaningful inside).
2. The patch boundary is preserved bit-for-bit: each original boundary vertex
   must map to an arrangement vertex at exactly the same `float64` coordinate,
   and the directed boundary edge set must be identical; otherwise the cluster
   is skipped (`boundary_split` / `boundary_mismatch`).
3. A cluster is committed only when, after stitching, the reload-equivalent
   strict `(holes, non_manifold)` count is no worse than before.  Otherwise
   the pre-cluster arrays are restored byte-for-byte.
4. With no self-intersections, or with `time_budget <= 0`, the function is a
   byte-identical no-op.

## Where it works: synthetic local folds

On closed cubes whose only defect is a **local** fold (two opposite vertices
swapped, so the edge-incidence topology is unchanged), the tier resolves the
fold exactly: `SI 2 -> 0`, reload-honest, and the guard correctly rolls back
the variant that would introduce a non-manifold edge.

## Where it does not: real-world residual SI

Measured on the 40-mesh `tests/real-world-samples` corpus, run twice on the
same branch and machine with the tier forced OFF then ON (Balanced intensity,
`deep_repair=full`, `graft=auto`):

| mesh | SI off | SI on | tier result |
|------|-------:|------:|-------------|
| artec_crocodile-statue | 319 | 319 | ran, 0/99 clusters accepted (3.1 s) |
| artec_metal-nut | 474 | 474 | ran, 0/85 accepted (2.7 s) |
| artec_pipe-bend | 34 | 34 | ran, 0/9 accepted (0.8 s) |
| thingi10k_100281 | 3724 | 3724 | ran, 0/1181 accepted (10.1 s) |
| thingi10k_1038439 | 85 | 85 | ran, 0/9 accepted (0.6 s) |
| thingi10k_1038441 | 390 | 390 | ran, 0/29 accepted (0.9 s) |
| thingi10k_145065 | 240 | 240 | ran, 0/65 accepted (3.7 s) |
| thingi10k_248395 | 6 | 6 | ran, 0/2 accepted (0.4 s) |
| thingi10k_39507 | 24 | 24 | ran, 0/6 accepted (0.6 s) |
| thingi10k_40886 | 168 | 168 | ran, 0/36 accepted (2.3 s) |
| thingi10k_46012 | 90 | 90 | ran, 0/38 accepted (2.6 s) |
| thingi10k_63785 | 135 | **125** | 1/54 accepted, 81 -> 75 in the Stage-2 mesh (5.4 s) |

On 11 of the 12 meshes not a single cluster is accepted.  On the one mesh
that changes (`thingi10k_63785`) the tier removes 6 of the 81 Stage-2
self-intersections; the mesh still leaves 125.  No mesh is made
non-watertight and no `(holes, non_manifold)` count changes.  Total time over
the 12 meshes rose 124.3 s -> 166.8 s (+42.5 s), essentially all of it
arrangement work that is then rejected.

A 2026-09 run over the full 40-mesh corpus with the tier forced on reached the
same conclusion (one mesh changed, `thingi10k_63785` 135 -> 125).

### The framebaroque fold is global, not local

The private `framebaroque` Full-Mend output carries 6014 residual
self-intersections concentrated in a heavily folded main shell (376,556
faces).  With a 90 s budget the tier accepted **1 of 80** clusters and moved
SI 6014 -> 6010.  The rejection histogram is dominated by `boundary_split`
(71; `boundary_mismatch` 7, `no_outer_hull` 1), the failure mode where the
patch's boundary vertices cannot be reproduced by the exact arrangement.  This
is not a size problem: growing a single-face seed to a 43,000-face patch
(k = 34 rings) still reports `boundary_split`, because the fold is a
whole-shell, global self-intersection rather than a locally separable
overlap.  The plan's central locality assumption does not hold for this
model.

## Decision

The tier is kept in the tree (algorithm, Rust binding, tests, integration) and
ships **OFF by default**, opt-in through `SUTURA_LOCAL_EXACT=1`, because the
real-world corpus shows no mesh on which it removes residual self-intersections
for a small time cost.  The genuine fix for the residual SI of densely folded
scans is global snap-rounding inside the Rust core (see the "exact arrangement
is SI-free only as exact rationals" backlog entry), not local patching.
