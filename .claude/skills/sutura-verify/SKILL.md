---
name: sutura-verify
description: Use to independently verify ANY Sutura repair change before trusting it — a new or tuned tier, adoption gate, threshold, Graft/Flap/Tamponade/Dressing behaviour, or a Rust-kernel change. Shape-based default-vs-opt-out comparison (holes, non-manifold, components/closed shells, Orca parts+manifold, two-sided distance, leak, runtime), never the tool's own JSON. Includes the reproducible harness under scripts/.
---

# Sutura verification rules

Why: a repair must be judged by the SHAPE it produces, not by byte equality or
by the report the tool prints about itself. A change that closes a hole but
moves healthy skin, adds a floating shell, leaks outside the input, or makes
the part non-manifold is a regression even when the JSON says "watertight".
This skill is how an agent that did NOT write the change checks the claim.

## The metric set (all recomputed from the saved file)
- reload holes, non-manifold edges — `defects.detect` on the exact float32 weld
  (the same reload-equivalent weld P-HONEST uses, so a claim that survives only
  in memory is caught).
- vertex-connected components and fully closed shells.
- OrcaSlicer `number_of_parts` / `manifold` from headless `--info` (when the
  binary is found; skip cleanly when not — see "Portability").
- two-sided vertex-to-surface distance to the input (max / p95), via
  `sutura_geom.closest_points`.
- outside-the-input leak: added output vertices with generalized winding < 0.5
  farther than the tolerance.
- tolerance = `clamp(bbox_diag / 10000, 0.0125, 0.05)` mm (the Tamponade plug
  gate; keep the same formula so numbers are comparable across tiers).
- wall-clock runtime of each run.

## Rules
- Default (feature ON) must be no worse than the opt-out on EVERY mesh. A
  single WORSE mesh blocks the change; "better on average" is not enough.
- Judge shape, never bytes: byte-identical output is a strong hint but not
  proof, and a tiny metric shift can be a real regression.
- Iterate on a small representative set; run the full corpus only at gates
  (never a 115-mesh benchmark while the desktop PC is away). Always include
  `thingi10k_100827` (the canonical small, stubborn open mesh).
- Independent verification: the verifier must not be the author of the change.
- Never name a private sample in any tracked file. Pass private inputs by path
  on the command line and write their outputs to scratch. This skill and its
  scripts must stay free of private names and absolute paths.

## How to run
The measurement half (input vs one output):
```
<repair-venv>/bin/python .claude/skills/sutura-verify/scripts/verify_repair_shape.py \
    INPUT.stl OUTPUT_fixed.stl [--repo <repo>] [--render --out-dir DIR]
```
The comparison half (runs the repair twice per mesh, default vs opt-out):
```
<repair-venv>/bin/python .claude/skills/sutura-verify/scripts/verify_default_vs_optout.py \
    [MESH ...] --repo <repo> --python <repair-venv>/bin/python \
    --opt-out-flag --no-tamponade --out-dir /tmp/<scratch>
```
With no MESH it uses the public corpus `tests/real-world-samples/`; `--limit N`
caps the count. The runner exits non-zero when any mesh is WORSE and writes
`summary.json` + `report.md`. `--python` must be an interpreter with pymeshlab
(the repair venv), not the plain system Python.

## Portability (Linux + macOS)
- `--repo` resolves by walking up to `sutura/repair.py`; never hard-code it.
- `sutura_geom` and PySide6 are optional: without them distance/leak/SI and
  rendering degrade to zeros/None rather than failing. Read the printed
  `si_output_note` / `render_error` to see which parts actually ran.
- OrcaSlicer is found via `SUTURA_ORCA`, the macOS app bundle, or `PATH`; when
  absent the Orca columns are `?`/`unknown` and the verdict falls back to the
  components count.
- Keep all outputs under `/tmp` (or a caller scratch dir); never commit them.

## What "done" looks like
A verified change: every mesh in the representative set is SAME or BETTER,
`thingi10k_100827` included, no new leak, no rise in parts/shells, two-sided
distance within tolerance, and the runtime delta is explained.
