# Development handoff

Working notes for resuming development in a new session (local or cloud).
Updated at the end of each working session; the git history is the record
of what changed, this file is the record of where work stands.

## State (2026-09-25)

- `main` contains Phase C2 and C3 of `rust/sutura-geom`, the headless GUI
  fix, the fTetWild fallback tier on by default when installed, refreshed
  screenshots and the M2 timings. CI (test 3.11, test 3.14, rust-geom,
  CodeQL) is green.
- No version was tagged for these changes. The last release is v0.4.1.
  The fTetWild default is a user-visible behaviour change, so the next
  release is a candidate for v0.5.0 (see the release checklist in
  AGENTS.md).

## Performance status of the exact arrangement (thingi10k_1038441)

| Build | Apple M2 | x86_64 VM (2 vCPU) |
|---|---|---|
| C1 (v0.4.0) | 462.7 s | 830 s |
| C2 | 29.8 s | 52.5 s |
| C3 | not measured yet | 28.1 s |

The output is identical across C1, C2 and C3: the order-independent digest
from `examples/arrangement_digest.rs` is `7b05a6fc3b785f42` for this mesh
(OBJ converted from `tests/real-world-samples/thingi10k_1038441.stl` with
trimesh). Remaining cost on the VM after C3: per-host triangulation 21.5 s,
weld and output 6.0 s (hashing of large `BigRational` keys), implicit point
construction 5.9 s, classification 4.6 s.

Measuring on macOS (conda env `sutura-env`):

```sh
cd rust/sutura-geom
PYO3_PYTHON=/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python \
  cargo build --release --example arrangement_digest
DYLD_LIBRARY_PATH=/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/lib \
  ./target/release/examples/arrangement_digest /tmp/thingi10k_1038441.obj
```

`DYLD_LIBRARY_PATH` must not be exported while cargo itself runs (libiconv
conflict), and without `PYO3_PYTHON` the example links against a different
libpython than the one on `DYLD_LIBRARY_PATH`.

## Rules for performance work on the Rust core

- A performance change keeps the output identical. Verify with
  `arrangement_digest` before/after on 1038441, its subsets (from
  `examples/bench_arrangement.rs`) and at least 1038439, 55772, 502009,
  46012 and artec_metal-nut.
- `cargo test --release --features cdt-check` asserts every accelerated
  triangulation query against the linear reference scan; CI runs it.
- Changes that alter the output (for example Sloan-style flipping instead
  of Steiner splits for non-convex quadrilaterals) are a separate, explicit
  decision and are measured on the real-world corpus.

## Known findings

- fTetWild is not deterministic: output and run time vary between runs even
  with `num_threads=1`. Its result is re-triangulated; the one-sided
  output-to-input Hausdorff distance reaches 28 % of the bounding-box
  diagonal on thingi10k_46012 where openings and cavities are closed.
- thingi10k_224108 and thingi10k_248395 are strictly watertight after the
  fTetWild fallback, but stage 2 is skipped, so the category is `warning`
  (not investigated).
- The strict watertight benchmark of the 40 real-world samples:
  31/40 without fTetWild, 39/40 with the default fallback (669 s total on
  the VM).

## Candidate next steps

1. Measure C3 on the M2 and add the number to the table above and to
   `rust/sutura-geom/README.md`.
2. Weld and output / implicit construction cost (identical output).
3. Sloan-style flipping (output-changing; needs its own decision).
4. Release v0.5.0 following the AGENTS.md checklist.
