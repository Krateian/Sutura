# sutura-geom

Foundation crate for Sutura's future indirect-predicates geometry core.

**Phase A scope (this crate, currently):** a thin PyO3 wrapper exposing the
explicit-point robust predicates `orient3d` and `insphere` from the `robust`
crate (Shewchuk adaptive-precision floating-point arithmetic) to Python as the
module `sutura_geom`.

Indirect/implicit predicates, expansion arithmetic, 2D CDT, triangle-triangle
intersection and any repair-pipeline integration are **out of scope for Phase A**
and will be layered on top of this crate in later phases (see
`docs/alpha-wrap-feasibility-2026-09.md` and the Phase A plan in
`/tmp/rust-core-research-plan.md`).

## Dependencies and licenses

All dependencies are permissive (MIT / Apache-2.0 / BSD) and verified against
`cargo tree` before shipping. Any dependency resolving to GPL/LGPL/AGPL is
rejected.

| Crate | Version | License |
|---|---|---|
| `robust` | `=1.2.0` | MIT OR Apache-2.0 |
| `pyo3` | `=0.29.2` | MIT OR Apache-2.0 |
| `numpy` (rust-numpy) | `=0.29.0` | BSD-2-Clause |

`maturin` (currently `==1.15.0`) is the build tool; it is **not** a Cargo
dependency. Install it into the target venv:

```bash
<venv>/bin/pip install "maturin==1.15.0"
```

## Building and wiring into the venv

```bash
cd rust/sutura-geom
cargo build                 # plain build (rlib)
cargo test                  # Rust unit tests
maturin develop --interpreter <venv>/bin/python
```

`maturin develop` installs the compiled `sutura_geom` extension module into the
given venv (the `extension-module` feature is applied automatically via
`pyproject.toml`). The extension is built with `abi3-py311`, so it runs on
Python 3.11+ without per-version rebuilds.

## Python API

```python
import sutura_geom

# Each point is a length-3 sequence (tuple, list, or numpy 1-D array).
sutura_geom.orient3d((0,0,0), (1,0,0), (0,1,0), (0,0,1))   # > 0
sutura_geom.orient3d((0,0,0), (1,0,0), (0,1,0), (0,0,-1))  # < 0
sutura_geom.orient3d((0,0,0), (1,0,0), (0,1,0), (0,0,0))   # == 0

sutura_geom.insphere((0,0,0), (1,0,0), (0,1,0), (0,0,1), (0.25,0.25,0.25))  # > 0
```

Smoke test: `tests/test_sutura_geom.py` (run with the venv python that has the
extension installed).

## Phase C0 profile (thingi10k_1038441)

Run with `cargo run --release --example bench_arrangement --features profile --
/tmp/thingi10k_1038441.obj`.

| Input | Wall time | cdt_per_host | Key count |
|---|---|---|---|
| 100 faces | 0.63 s | 29 % | 98 host triangles with segments |
| 500 faces | 2.21 s | 20 % | 501 host triangles with segments |
| 1001 faces | 52.98 s | **84.9 %** | 515,326 `orient2d` calls |
| 5000 faces | 180 s (timeout) | **82.3 %** | one host triangle CDT >158 s |
| 10418 faces (full) | 600 s (timeout) | **77.1 %** | only 19 host triangles completed |

**Verdict:** the bottleneck is an algorithmic blow-up inside the per-host 2D
CDT/arrangement on host triangles that carry many intersection segments.
Predicate filters (C1) and expansion arithmetic (C2) reduce the cost per
predicate but do not address the per-host complexity; the next chunk must
target the arrangement directly.

The full report is at `/tmp/phase-c0-profile.md`.

## Phase C1 result (thingi10k_1038441, same harness, Apple M2)

C1 did two things: every crossing is constructed from the original input
planes/lines (PPI of host plane + the two intersecting triangles' planes, LPI
against a host edge) instead of chaining previously constructed 2D points,
and a rigorous interval filter (`src/interval.rs`, one-ulp outward rounding,
NaN/overflow undecidable) sits in front of the exact `BigRational`
`orient3d` (implicit points), `orient2d` and `incircle`. A sampling profile
showed that most of the C0 time was `BigRational` normalisation (gcd) inside
those predicates, so the filter paid off more than the C0 verdict above
expected.

| Input | C0 | C1 (crossings from original data) | C1 + interval filter | Output faces |
|---|---|---|---|---|
| 100 faces | 0.63 s | 0.50 s | **0.16 s** | 158 (unchanged) |
| 500 faces | 2.21 s | 2.12 s | **0.68 s** | 607 (unchanged) |
| 1001 faces | 52.98 s | 53.17 s | **5.99 s** | 2,510 (unchanged) |
| 5000 faces | timeout (180 s) | timeout (180 s) | **31.1 s** | 10,731 |
| 10418 faces (full) | timeout (600 s) | — | **462.7 s** | 37,325 (4,443 proper SI pairs) |

Full-mesh phase split with the filter: classify pairs 10.9 s, constrained
triangulation per host 150.6 s (83 % of instrumented time, 59.6 M `orient2d`
calls). Next: cut the per-host CDT cost (walking point location instead of
linear scans, fewer non-convex fallback splits) before corpus-wide
benchmarking.

## Phase C2 result (per-host CDT without linear scans)

C2 removes every whole-triangulation scan from the per-host constrained
triangulation (`src/cdt2d.rs`) while keeping each decision the old code made,
so the output is identical, not merely equal in face count:

- **Point location** is a stochastic visibility walk (it terminates on any
  triangulation, Delaunay or not) from the last located triangle. When the
  point lies on an edge or a vertex, the answer is canonicalised to the
  lowest-index containing triangle, which is what the old linear scan
  returned; the scan remains as the fallback if a walk cannot conclude.
- **Vertex–triangle incidence** is maintained on every triangle write, so
  edge lookups and constraint marking rotate around one vertex instead of
  scanning all triangles.
- **Constraint insertion** walks the segment corridor from `a` to `b`. Only
  corridor triangles can own an edge that properly crosses the segment, so the
  lowest `(triangle, edge)` among them equals the old scan's first hit; the
  walk also yields every vertex on the open segment (the lowest index is
  used, as before).
- The **normal end of the flip loop** (the constraint has become an edge) no
  longer rebuilds the adjacency of the whole triangulation; the rewrite is
  reproduced locally with the same vertex rotation and flags.
- Cached interval enclosures per vertex feed the filtered `orient2d`; the
  exact fallback clears denominators and works on `BigInt` instead of
  normalising a `BigRational` after every operation (same sign). Per-host
  projection constants are computed once, a point is projected once per
  insertion, host vertices get their known `(0,0)/(1,0)/(0,1)` coordinates,
  segment vertices are sorted with cached keys, and each CDT vertex is mapped
  back to 3D and welded once.
- Batch pre-filters (segment pairs, vertex-on-segment) use the conservative
  interval boxes; the exact test still decides every pair that can intersect.

Non-convex quadrilaterals still fall back to a Steiner split, as before;
switching to Sloan-style queue flipping would change the output (fewer split
vertices) and is left for a separate, output-changing step.

Verification: `cargo test --release --features cdt-check` runs every
accelerated query against the linear reference scan inside the library and
panics on any disagreement; the full thingi10k_1038441 mesh was run this way
without a mismatch. `examples/arrangement_digest.rs` prints an
order-independent digest of the exact output geometry; the pre-C2 and C2
builds produce the same digest on every input below.

Measured on the same machine (x86_64 cloud VM, 2 vCPU; slower than the M2
used for C1, so compare within the table only). The OBJ was converted with
trimesh 5.x, whose vertex welding makes the subsets differ slightly from the
C1 table above.

| Input | C1 | C2 | Output faces | Digest |
|---|---|---|---|---|
| 100 faces | 0.28 s | 0.18 s | 158 | identical |
| 501 faces | 1.11 s | 0.85 s | 607 | identical |
| 1001 faces | 10.85 s | 3.07 s | 2,494 | identical |
| 5000 faces | 56.2 s | 13.1 s | 10,683 | identical |
| 10418 faces (full) | 830 s | **52.5 s** | 37,323 (4,445 proper SI pairs) | identical |

Full-mesh phase split after C2: classify pairs 19.2 s, per-host constrained
triangulation 26.9 s (was ~83 % of the time), weld and output 6.2 s;
`orient2d` calls on the full mesh dropped to 1.1 M. The remaining cost is
spread over the triangle-pair classification and the exact constructions of
implicit points rather than concentrated in the triangulation.
