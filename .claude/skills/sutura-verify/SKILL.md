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

## Verification modes

The comparison script has three modes:

- **`--feature` (default, existing):** one repo, default (feature ON) vs
  opt-out (feature OFF). This is the original behaviour and the tier commands
  below keep working unchanged.
- **`--byte` (refactors):** two builds, baseline (`--baseline-repo`,
  `--baseline-python`) vs candidate (`--repo`, `--python`), same arguments;
  outputs must be byte-identical. Add `--deterministic` to append
  `--no-graft --no-fallback-ftetwild --deep-repair off`, and/or
  `--exclude-nondeterministic` to report fTetWild-adopting meshes as
  `SKIPPED-NONDET` instead of failures.
- **`--quality` (behaviour changes):** baseline vs candidate; no mesh may be
  worse on watertight (holes/NM), SI-after, two-sided Hausdorff (existing
  tolerance), MC-GWN union volume (0.1 % of input) or parts. Every changed mesh
  is listed. Missing SI is a hard failure. `--exclude-nondeterministic` also
  works here, because fTetWild output is not deterministic.

`--jobs N` runs meshes in parallel (`--jobs 4` for gates). Baseline outputs are
cached under `--cache-dir` (default `~/.cache/sutura-verify`) keyed by the input
SHA-256, the exact flags and the baseline HEAD, so a repeated T1/T2 reuses the
baseline half. Candidate outputs are never cached.

## Verification tiers

| Tier | When | Scope | Command | Gate | Measured (this M2) |
|---|---|---|---|---|---|
| T0 | every commit | unit/parity, no corpus | `cargo test` + the relevant Python unit/parity suites | pass | seconds |
| T1 | output-affecting change | 3 heavy meshes: `thingi10k_100827`, `thingi10k_1038441`, `artec_metal-nut` | `--quality --exclude-nondeterministic --jobs 4` (or `--byte --deterministic`) | no worse / identical | **138.7 s cold, 73.5 s warm** |
| T1+ | organic/scan-touching change | T1 + `thingi10k_100281`, `thingi10k_224108`, `thingi10k_46012` | same | same | ~2x T1 cold |
| T2 | before a repair-path PR merge | all **40 tracked** meshes (explicit `git ls-files`, not the 48-file dir) | same | no worse / identical | **338.2 s cold, 169.2 s warm** (cache+4-way) |
| T3 | before a release | T2 + local hard set + local reference bench | T2 + the owner's bench command | no worse; bench explained | local only |

- T0 is the only tier in CI. T1/T1+/T2/T3 are developer/lead gates; keep
  outputs under `/tmp`.
- Rust-only **unwired** changes (no `sutura/` call site, registry, CLI or GUI)
  need T0 only; once wired, T1/T2.
- T3 inputs are private/local; pass paths on the command line and never name
  them in tracked files. The docs only state that T3 exists.
- Use the explicit tracked mesh list for T2; `resolve_meshes` on the directory
  also picks up ignored `*_fixed.stl` artifacts.

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
`summary.json` + `report.md`. `--python` must be the repair venv interpreter (numpy + the mandatory
`sutura_geom` extension), not the plain system Python.

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
