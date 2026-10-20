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
- The only non-`SAME` mesh is `thingi10k_100827` for both B and C.

## The single exercised mesh: thingi10k_100827

`thingi10k_100827` is a small (80-face) open scan; stage 1 leaves 7 holes.

| arm | time | splint ran | splint adopted | splint output | final (shape) |
|-----|-----:|:----------:|:--------------:|---------------|---------------|
| A | 4.47 s | – | – | – | watertight (fTetWild closed it), 1 part |
| B | 11.83 s | yes (res 128, retry) | no — 10 parts, 1013 holes | rejected | watertight, 1 part, but a 0.0184 mm (1 pt) leak |
| C | 8.51 s | yes (res 128, retry) | no — 3 parts, 1094 holes | rejected | 4 holes, 2 non-manifold edges, 2 parts, warning |

Splint at `res=128` over-refines this tiny mesh (proxy ≈ 48 999 vertices for 80
input faces), and neither the hardened nor the retry pass yields a
strict-watertight candidate, so it is rejected and the ladder's previous result
is kept. The B-vs-A `WORSE` is a 0.0184 mm leak introduced by the
non-Splint tiers (see determinism below), not by an adopted Splint.

## Determinism

- Arm C (fTetWild disabled) is byte-deterministic: two runs of
  `thingi10k_100827` produced identical outputs.
- Arm B is **not** deterministic: two runs produced different outputs, because
  fTetWild (reached after Splint is rejected) is not byte-deterministic. This
  also explains the 0.0184 mm leak; an earlier single run of the same mesh had
  agreed with A.

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
