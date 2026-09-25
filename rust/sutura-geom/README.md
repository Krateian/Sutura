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

Same harness on the Apple M2 used for the C1 table (same OBJ, same digest
`7b05a6fc3b785f42` as on the VM): 100 faces 0.11 s, 501 faces 0.50 s,
1001 faces 1.77 s, 5000 faces 7.55 s, **full mesh 29.8 s** (C1 on the M2:
462.7 s); classify pairs 10.8 s, per-host triangulation 16.2 s.

Full-mesh phase split after C2 (VM): classify pairs 19.2 s, per-host constrained
triangulation 26.9 s (was ~83 % of the time), weld and output 6.2 s;
`orient2d` calls on the full mesh dropped to 1.1 M. The remaining cost is
spread over the triangle-pair classification and the exact constructions of
implicit points rather than concentrated in the triangulation.

## Phase C3 result (classification and exact-arithmetic hot spots)

After C2 the remaining time was spread over the triangle-pair classification
and a few exact constructions. C3 keeps the output identical again and
removes those hot spots:

- `point_inside_triangle_3d`: when the tested point is a line-plane
  intersection built on the very edge being tested, the orientation is
  exactly zero by construction and is returned without evaluation (this was
  the case the interval filter could never decide, so it always fell back to
  exact rationals).
- `points_are_equal`: a rigorous interval test on the homogeneous
  coordinates (`λp·dq − λq·dp`) proves most candidate pairs different before
  any rational coordinate is built.
- The exact host frame used to classify touch segments is built once per
  host triangle instead of once per segment.
- `HostFrame::point3d`, the fallback `line_line_intersection` and the
  segment sort key are evaluated on integers over a common denominator with
  one normalisation at the end (the sort key is the numerator of the old
  parameter; its positive denominator is common to all entries, so order and
  ties are unchanged). A unit test checks each rewrite against the textbook
  `BigRational` formula value by value.

Differential check (same digest before and after): thingi10k_1038441 and
its subsets, 100045, 1038439, 55772, 502009, 46012 (90k faces) and
artec_metal-nut (90k faces).

| thingi10k_1038441, x86_64 VM | C2 | C3 |
|---|---|---|
| full mesh | 52.5 s | **28.1 s** |
| classify pairs | 19.2 s | 4.6 s |
| per-host triangulation | 26.9 s | 21.5 s |

Other meshes on the VM (C2 → C3): 1038439 8.5 s → 4.8 s, 55772 7.0 s →
3.9 s, 502009 23.0 s → 9.7 s, 46012 165 s → 95 s, artec_metal-nut 132 s →
77 s (the last two measured with a second job running).

## Phase C4 result (exact-key hashing and integer implicit constructions)

The C3 profile still spent 6.0 s in weld and output and 5.9 s in implicit
point construction. Both had one cause: normalised `BigRational` work that
does not affect the result.

- `num-rational` hashes a ratio through its continued-fraction expansion (a
  chain of `BigInt` floor divisions) and compares two ratios through `cmp`,
  so that non-reduced ratios agree with `Eq`. The output weld map and the
  per-host `(s,t)` vertex index now use a `RatKey` wrapper that hashes and
  compares the `(numer, denom)` pair directly. Every rational in the crate is
  built with `Ratio::new`, `from_f64` or ratio arithmetic, which keep it
  reduced with a positive denominator, so for these values the two
  equalities coincide (a `debug_assert` checks the invariant on every key).
  The maps are only probed and filled, never iterated, so the welded vertex
  order is unchanged as well.
- `Point3::to_rational` writes every input coordinate as `X * 2^e` with one
  common exponent, evaluates the line-plane and plane-plane-plane formulas
  on the integers `X` and reduces each coordinate once. The value, and
  therefore the normalised rational, is the one the rational formulas give;
  `integer_constructions_match_rational` compares both value by value on
  4,000 random constructions (zero, subnormal and widely scaled
  coordinates, parallel/degenerate cases included).

Differential check (same digest before and after; debug build with the
`RatKey` assertion active on the 1001-face subset and 100045): the four
subsets of thingi10k_1038441, the full mesh, 100045, 1038439, 55772,
502009, 46012 and artec_metal-nut.

| x86_64 VM (2 vCPU) | C3 | C4 |
|---|---|---|
| thingi10k_1038441, full mesh | 28.2 s | **17.6 s** |
| weld and output | 6.4 s | 0.06 s |
| per-host triangulation | 21.6 s | 7.4 s |
| implicit construction | 5.7 s | 2.3 s |
| classify pairs | 4.8 s | 4.6 s |

Phase times come from `bench_arrangement --features profile` (the phases
overlap, and profiling adds overhead); the full-mesh wall times from
`arrangement_digest`. Other meshes (C3 → C4): 1001-face subset 1.56 s →
1.14 s, 5000-face subset 7.25 s → 5.36 s, 100045 0.39 s → 0.31 s, 1038439
5.1 s → 3.9 s, 55772 3.6 s → 3.4 s, 502009 10.0 s → 7.9 s, 46012 95.3 s →
92.2 s, artec_metal-nut 78.5 s → 77.7 s. On the two 90k-face meshes the
remaining cost lies outside the phases changed here.
