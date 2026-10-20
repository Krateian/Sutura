# Splint tier (#18) evaluation — 40-mesh public corpus (2026-10)

Scope: whether the opt-in Splint exact solid-rebuild tier (method #18, Rust
`sutura_geom.splint_cut`) should be flipped to default ON, or preferred over
fTetWild, on the public 40-mesh corpus (`tests/real-world-samples/*.stl`).

## Method

Three arms, one pass over all 40 meshes, each output judged by shape (never by
the tool's own JSON) with the `sutura-verify` metric stack
(`verify_repair_shape`): reload holes / non-manifold edges on the float32 weld,
vertex components and closed shells, OrcaSlicer `number_of_parts` / `manifold`,
two-sided vertex-to-surface distance (max / p95), leak, runtime.

| arm | command | meaning |
|-----|---------|---------|
| A   | (no flags) | main: Graft → Dressing → fTetWild (auto) |
| B   | `--experimental-splint` | Splint in the last-resort slot, fTetWild still available |
| C   | `--experimental-splint --deep-repair full --no-fallback-ftetwild` | Splint replaces fTetWild |

Verdicts are pairwise against arm A (B vs A, C vs A); `SAME` means every shape
metric is within tolerance, `WORSE` when any metric is worse.

Configuration: Splint runs at the fixed `res=128` with adaptive mode `retry`
(hardened pass first; broad rerun only when the hardened cut still has more
than 4 shells). It is adopted only when the candidate reloads strict-watertight.

## Aggregate result

| comparison | SAME | BETTER | WORSE |
|------------|-----:|-------:|------:|
| B vs A     | 39   | 0      | 1     |
| C vs A     | 39   | 0      | 1     |

- Splint ran on **1 of 40** meshes (`thingi10k_100827`) and was **never
  adopted**.
- Total wall clock: A 184.7 s, B 191.7 s, C 181.6 s. The opt-in flag therefore
  costs nothing on the 39 meshes where stage 1 already closes the mesh (the
  tier is a last-resort and does not run).
- The only non-`SAME` mesh is `thingi10k_100827` for both B and C. For B the
  difference is a non-reproducible artifact of fTetWild (a targeted re-run of
  the same mesh returned `SAME`; see determinism below). For C it is the
  deterministic consequence of *removing the fTetWild fallback* — Splint is
  rejected, nothing closes the mesh, and the result keeps 4 holes / 2
  non-manifold edges — not of Splint itself. This is why arm C is a worst-case
  bound and not an argument against enabling Splint alongside fTetWild.

## The single exercised mesh: thingi10k_100827

`thingi10k_100827` is a small (80-face) open scan; stage 1 leaves holes, and it
is the only mesh in the corpus on which the Splint tier is reached.

| arm | time | splint ran | splint output | splint adopted | final (shape) |
|-----|-----:|:----------:|---------------|:--------------:|---------------|
| A | 4.47 s | – | – | – | watertight (fTetWild closed it), 1 part |
| B | 11.83 s | yes (res 128, retry) | 10 parts, 0 holes / 840 non-manifold | no | watertight, 1 part |
| C | 8.51 s | yes (res 128, retry) | 3 parts, 0 holes / 1092 non-manifold | no | 4 holes, 2 non-manifold edges, 2 parts, warning |

Splint at `res=128` over-refines this tiny mesh (≈ 18 632 vertices / 44 672
faces for a 74-face ladder input), and neither the hardened nor the retry pass
yields a strict-watertight candidate, so it is rejected and the ladder's
previous result is kept.

### Candidate measurement (tier fix)

An earlier revision of the tier measured the candidate after
`_repair_welded_topology`, which destroyed the raw cut (840 non-manifold edges
became 1 013 manufactured holes) and reported a misleading rejection. The tier
now measures the raw cut and, when it is not watertight, routes it through the
same Manifold stage-2 rebuild fTetWild's manifold post-process uses
(`run_stage2_arrays`), recording `construct_status` and adopting the rebuild only
when it is no worse.

On `thingi10k_100827` the rebuild constructs cleanly
(`construct_status = Error.NoError`) and its index-space defect count is 0 holes
/ 0 non-manifold, but the result is **not reload-honest**: welding its float32
positions raises 1 hole and 875 non-manifold edges, and the kernel's own
`watertight_check` returns `NotManifold`. The rebuild is therefore no worse than
the candidate only in index space and is correctly rejected on the honest
(welded) metric. A targeted re-run after the fix reproduces this and leaves both
arms unchanged: the fix removes a destructive intermediate step and makes the
reported numbers honest, but does not make Splint adopt on this mesh, because the
Manifold rebuild of this fragmented cut is itself not reload-watertight.

## Determinism

- Arm C (fTetWild disabled) is byte-deterministic: two runs of
  `thingi10k_100827` produced identical outputs.
- Arm B is **not** deterministic: two runs produced different outputs, because
  fTetWild (reached after Splint is rejected) is not byte-deterministic. This
  also explains the 0.0184 mm leak; a targeted re-run of the same mesh (after
  the tier fix) again agreed with A, so the leak is a fTetWild artifact and not
  reproducible.

Splint itself is deterministic (single-threaded core); the arm-B variance comes
entirely from fTetWild.

## Time-budget limitation

`splint_cut` is a single non-interruptible in-process Rust call, so the tier's
wall-clock abort (mirroring the fTetWild timeout) can only discard an
already-returned candidate; it cannot interrupt the rebuild mid-call. The
pre-flight `splint_max_faces` input cap is the only guard that avoids starting
an over-budget rebuild. Both are reported honestly (`skipped_budget` /
`aborted`).

## Decision

The zero-worse standing rule for flipping a tier to default ON is **not met**:

- Splint is never better than the default on any of the 40 meshes (0 BETTER).
- It is exercised on only 1 mesh and adopted on none.
- On the one mesh it runs, replacing fTetWild (arm C) is clearly worse
  (4 holes / 2 non-manifold vs watertight).

Verdict: **Splint stays opt-in** (`SPLINT_DEFAULT_ENABLED=False`,
`--experimental-splint`). There is no evidence to prefer it over fTetWild, and
no fTetWild-only win is recovered by removing fTetWild on this corpus. The
public corpus is dominated by meshes stage 1 already closes, so it cannot
demonstrate Splint's intended strength (heavy, fragmented scans); a private
heavy-scan evaluation would be required before any flip, together with a
resolution policy that adapts `res` to the input scale (the fixed `res=128`
over-refines small meshes).
