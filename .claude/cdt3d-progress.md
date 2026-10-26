# CDT3D progress log (append-only)

Branch: `spike/cdt3d` (from `spike/splint-v2`). Design:
`.claude/cdt3d-design.md`. No push, no main, no version bump.

## 2026-10-26 — P0 start

Approved decisions: clean-room Rust only; geogram BSD reuse allowed; caps are a
separate reported class; positive-area coplanar overlaps are resolved, other
contacts reported; O9/O10 agreed.

P0 scope: PLC validator + arrangement completion (canonical input,
T-junction/touching/coplanar resolution, boundary loops, report, f64 handoff).
Gate: `plc_valid=true` on `thingi10k_100827` and the seven Rust fixtures;
5-run and 2-process hashes identical; default path byte-identical; `cargo test`
green.

Plan:
1. Add an internal `arrangement_core` with opt-in extra edge/face points; keep
   `arrangement_lite_core` behaviour unchanged.
2. Add `cdt3d/plc.rs`: canonical input, all-vertex edge/face extras,
   positive-area coplanar detection + deterministic 1-ulp perturbation,
   iterative conforming arrangement, canonical output, validator/report/hash.
3. Add a Python binding `cdt3d_plc` (opt-in; default pipeline untouched).
4. Add synthetic unit tests, a 100827 5-run test, and an ignored public-corpus
   gate test for the seven fixtures.
5. Run `cargo test --release`, run the gate twice in separate processes, commit
   locally, `graphify update .`.

Next step: implement step 1.

## 2026-10-26 — P0 implemented and gated

What was done:
- `arrangement.rs`: added `ExtraConstraints` and an internal `arrangement_core`;
  `arrangement_lite_core` now calls it with an empty extra set (default
  behaviour structurally unchanged).  The core can force segment-endpoint
  edge propagation and accept per-edge/per-face conformance points.
- New `cdt3d/plc.rs`: exact input weld/dedup, all-vertex edge/face extras,
  exact positive-area coplanar detection (host-frame clipping), deterministic
  one-ulp perturbation of positive-area coplanar pairs, iterative conforming
  arrangement, canonical exact output, validator and FNV digest.
- New `cdt3d_plc` Python binding (opt-in; default pipeline untouched).
- Synthetic tests (crossing, T-junction, coplanar overlap, duplicates) and a
  5-run 100827 test; ignored public-corpus gate for 100827 + seven fixtures.

Key fix found during gating: `HostFrame` projection is a least-squares
projection and does not itself test coplanarity.  The conformance pass now
requires exact `orient3d == 0` before treating a vertex as on a host edge or
inside a host triangle.  Without that, near-coincident derived points were
counted as 9 false touch points on 100827 and the loop never converged.

Gate results (`cargo test --release plc_public_corpus_gate -- --ignored`,
5 runs per mesh, two separate processes; hashes identical):
| mesh | valid | outV/outF | passes | si | tj | touch | cop | bnd | nm | comps | area_ratio | secs/run | hash |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| thingi10k_100827 | true | 47/83 | 1 | 0 | 0 | 0 | 0 | 15 | 0 | 1 | 1.000000 | 0.031 | 6072b9f4276b666d |
| case1_open_cup | true | 130/192 | 1 | 0 | 0 | 0 | 0 | 64 | 0 | 2 | 1.000000 | 0.269 | b91c8f081e10ee9f |
| case2_pipe | true | 64/64 | 1 | 0 | 0 | 0 | 0 | 64 | 0 | 1 | 1.000000 | 0.036 | 36dd8415916d74c3 |
| case3_broken_cup | true | 194/316 | 1 | 0 | 0 | 0 | 0 | 70 | 0 | 2 | 1.000000 | 0.393 | abacf6bc6957b69b |
| case4_cracked_sphere | true | 568/1120 | 1 | 0 | 0 | 0 | 0 | 14 | 0 | 1 | 1.000000 | 1.073 | 19e11187374eb92d |
| case5_box_missing_face | true | 8/10 | 1 | 0 | 0 | 0 | 0 | 4 | 0 | 1 | 1.000000 | 0.002 | 3cb6ee81a5a7944b |
| case6_thin_cup | true | 65/96 | 1 | 0 | 0 | 0 | 0 | 32 | 0 | 1 | 1.000000 | 0.129 | 775d1408f0ebac0b |
| plate_a_dent | true | 6876/13734 | 1 | 0 | 0 | 0 | 0 | 18 | 0 | 1 | 1.000000 | 8.306 | 9b95f497043f5279 |

Observations:
- 100827: 71 input faces -> 83 pieces; 47 vertices, area preserved; 15
  boundary edges remain (open surface), 0 non-manifold edges.
- The six small fixtures are unchanged in face count; `plate_a_dent` gains 2
  faces (T-junction/edge conforming) and takes ~8-9 s per run in release.
- `cargo test --release`: **357 passed, 0 failed, 2 ignored** (the new
  public-corpus gate plus the pre-existing ignored test).
- Default path: `arrangement_lite_core` passes an empty `ExtraConstraints`
  and does not force propagation; the only added work on that path is empty
  map/vector lookups.  Existing arrangement tests pass.

Next step: commit P0 locally, run `graphify update .`, then start P1 (exact
LNC point + `orient3d`/`inSphere` for implicit points, rational fallback,
symbolic perturbation) behind the same opt-in module.

## 2026-10-26 — P0 closed

- Committed P0 locally as `2ed5fbd` ("cdt3d: add opt-in PLC completion and
  validator (P0)") on `spike/cdt3d`; not pushed.
- Rebuilt the extension with `maturin develop --release` and verified the
  opt-in binding on `thingi10k_100827`: 47/83 output, `plc_valid=True`,
  `proper_si=0`, `t_junctions=0`, `touch_points=0`,
  `coplanar_positive_after=0`, `boundary_edges=15`, `components=1`,
  hash `0x6072b9f4276b666d` (same as the Rust gate).
- `cargo test --release`: 357 passed, 0 failed, 2 ignored.
- `graphify update .` run after the commit; graph clean.

Status: P0 complete and gated. P1 (exact LNC point, implicit
`orient3d`/`inSphere`, rational fallback, symbolic perturbation) is next.
Handoff point: any fresh session can resume from this file and commit
`2ed5fbd`.

## 2026-10-26 — P1 exact kernel implemented and gated

What was done:
- `point.rs`: added `Point3::Lnc { a, b, t }` (`t*a + (1-t)*b`) with exact
  `to_rational`, rational reference, homogeneous `(lambda,d)=(p,1)`, interval
  enclosure, and approximate `to_f64`.  The exact path uses the rational
  `1-t`, so the point is exactly on the segment.
- `triangle_intersection.rs`: canonical LNC key (tag 3; endpoint order is
  normalised by swapping `a/b` and replacing `t` with `1-t`).
- `interval.rs`: added rigorous `det5` (cofactor expansion via `det4`).
- `predicates3d.rs`: added `insphere_sign`:
  - all-explicit -> `robust::insphere`, normalised to +/-1/0;
  - implicit -> homogeneous 5x5 interval filter, then exact rational 5x5
    determinant by deterministic exact Gaussian elimination;
  - added `insphere_exact_sign` (test/reference) and
    `insphere_sign_perturbed` implementing the published index-sort +
    orient3d symbolic-perturbation rule.
- Calibrated the homogeneous determinant against `robust` on 500 random
  explicit 5-tuples: signs agree exactly.  The 5x5 row form
  `[d*lambda, |lambda|^2, d^2]` is positive-inside for a positively oriented
  tet (verified on the regular tetrahedron).
- Differential tests:
  - 800 random mixed explicit/LNC in-sphere tuples: filter never contradicts
    the exact rational sign; the full predicate matches the exact sign.
  - 3000 random mixed explicit/LPI/PPI/LNC orient3d tuples: the existing
    filter/exact test now includes LNC and passes.
  - near-degenerate cospherical five-tuple: unperturbed exact/robust zero,
    one-ulp inward move classified identically by filter/exact/robust.
  - symbolic perturbation: deterministic, even permutation of the first four
    points preserves sign, odd permutation flips it.
  - `det5` interval test against an exact rational determinant (100 random
    5x5 matrices).
- `cargo test --release`: **364 passed, 0 failed, 2 ignored**.
- Default path unchanged: the new enum variant, predicates and `det5` are not
  called by the existing arrangement/Splint paths.

Next step: P2 (initial Delaunay + tet/neighbour structure + canonicalisation,
box/ghost handling, exact validity checks).  Handoff point: P0 + P1 complete.

## 2026-10-26 — P2 initial Delaunay implemented and gated

What was done:
- New `rust/sutura-geom/src/cdt3d/dt.rs`:
  - padded box around the PLC bounding box (pad = 5% of the diagonal, advanced
    by `next_down`/`next_up` until strictly outside), with the eight corners
    appended after the PLC vertices as hard sink terminals;
  - sequential Geogram `BDEL` on the PLC vertices plus the eight box corners,
    reusing the existing crate-internal `splint::delaunay` wrapper (no new
    FFI and no default-path change);
  - oriented canonicalisation: sort each tet's ids, use the exact `orient3d`
    to force positive orientation, then sort the tet list; rebuild
    `neighbours[t][lv]` from face incidences and verify reciprocity; deterministic
    FNV digest of canonical vertices + tets;
  - exact validity report: positive orientation, duplicate/degenerate tets,
    non-manifold face incidence, neighbour reciprocity, unused vertices, hull
    faces on the box (ghost/sink boundary), and a local empty-sphere test on
    every interior face with the P1 `insphere_sign`.
- `cdt3d/mod.rs` exports `build_dt`, `Dt`, `DtOptions`, `DtReport`,
  `BoundingBox3`; `splint/mod.rs` makes the existing `delaunay` module
  `pub(crate)` (visibility only, no behaviour change).
- Tests: box strictness, tetrahedron + box, duplicate input rejection, flat-tet
  rejection, a deliberately non-Delaunay pair (proves the empty-sphere check
  fires), a 5-run 100827 test, and the ignored 8-mesh gate.
- No Python binding yet: P2 is internal opt-in code, not on the default path.

Gate (`cargo test --release cdt3d::dt::tests::dt_public_corpus_gate -- --ignored`),
5 runs per mesh in one process; the whole gate was run in three separate
processes.  All eight hashes were identical in every process.  Every row has
`nonpositive = degenerate = duplicate = non-manifold = inconsistent = off-box
= unused = empty-sphere = 0` and `plc_valid = dt_valid = true`.

| mesh | plcV | totalV | tets | hull/ghost | interior | off-box | ngb | empty | cosph | unused | secs/run | hash |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| thingi10k_100827 | 47 | 55 | 252 | 12/12 | 498 | 0 | 0 | 0 | 16 | 0 | 0.001 | afa089cef908b956 |
| case1_open_cup | 130 | 138 | 691 | 12/12 | 1376 | 0 | 0 | 0 | 244 | 0 | 0.003 | 848159b61c6e7b24 |
| case2_pipe | 64 | 72 | 274 | 12/12 | 542 | 0 | 0 | 0 | 236 | 0 | 0.003 | f63676a58f9cefbb |
| case3_broken_cup | 194 | 202 | 977 | 12/12 | 1948 | 0 | 0 | 0 | 520 | 0 | 0.004 | d8cf4252c0f46c1e |
| case4_cracked_sphere | 568 | 576 | 3176 | 12/12 | 6346 | 0 | 0 | 0 | 532 | 0 | 0.009 | 0bd91e3ae2bc37d9 |
| case5_box_missing_face | 8 | 16 | 42 | 12/12 | 78 | 0 | 0 | 0 | 36 | 0 | 0.001 | 658066f7f57d28b5 |
| case6_thin_cup | 65 | 73 | 279 | 12/12 | 552 | 0 | 0 | 0 | 252 | 0 | 0.002 | 6b594616c5c0b145 |
| plate_a_dent | 6876 | 6884 | 34838 | 12/12 | 69670 | 0 | 0 | 0 | 35544 | 0 | 0.142 | 9910b986e28e356d |

`cosph` is the number of directed interior-face tests whose exact in-sphere
value is zero (cospherical, accepted; not a violation).  `ngb` is the number
of inconsistent neighbour links.  The local face test is equivalent to a
global empty-sphere test on the convex box and avoids the O(V*T) sweep.

`cargo test --release`: **370 passed, 0 failed, 3 ignored** (the P0 gate, the
new P2 gate and the pre-existing ignored test).

Default path: the only non-additive change is `mod delaunay` ->
`pub(crate) mod delaunay`; the new module is not called from `arrangement`,
`Splint`, the CLI or the GUI.  The full default test suite is green.

Next step: commit P2 locally, run `graphify update .`, then start P3 (segment
recovery: missing-edge detection, encroaching reference, acute classification,
LNC split, insertion, exact tests).

### Self-review — P0/P1 items that remain uncertain

P0:
- The positive-area coplanar policy moves one non-shared vertex by one ulp.
  It is reported and preserves area on the gate meshes, but its shape safety
  for very thin stacked coplanar shells is not independently proven.
- `build_plc` conformance is O(n^2) per pass; `plate_a_dent` costs ~8-9 s per
  run in release.  That is acceptable for a gate but may be too slow for the
  final pipeline.
- `area_ratio` is computed from projected `f64` triangle areas, not an exact
  rational sum; it is a diagnostic, not a certificate.
- `plc_valid` deliberately accepts open boundaries (15 boundary edges on
  100827).  P2/P3 must keep treating an open PLC as a valid input.

P1:
- `insphere_sign_perturbed` implements the published index-sort/orient3d
  parity rule and passes even/odd permutation tests, but it has not been
  compared against an independent SoS oracle on general five-point cospherical
  configurations.
- The homogeneous 5x5 in-sphere convention is calibrated against `robust` on
  random explicit tuples and the regular tetrahedron.  Negative homogeneous
  denominators and implicit-point edge cases rely on the interval filter and
  the exact rational fallback; coverage is differential, not a proof.
- The exact 5x5 rational solve uses deterministic Gaussian elimination without
  an explicit pivot strategy proof; randomized tests pass, but adversarial
  zero-pivot inputs are not exhaustively characterised.
- `Lnc::to_f64` is the approximate construction; exact predicates use
  `to_rational`.  Endpoint aliasing (`t = 0/1`) and the canonical key are tested
  indirectly, not as a dedicated property.
- The rational fallback is correctness-first.  Near-cospherical/near-coplanar
  inputs can be slow until the paper's indirect polynomial filters land.

## 2026-10-26 — P2 closed

- Committed P2 locally as `8339fce` ("cdt3d: add initial Delaunay and
  tet/neighbour validation (P2)") on `spike/cdt3d`; not pushed.
- `graphify update .` run after the commit: 9545 nodes, 21819 edges, 356
  communities; only the pre-existing Geogram C++ syntax warnings remain.
- `cargo test --release`: 370 passed, 0 failed, 3 ignored.
- Gate hashes were reproduced in three separate processes.

Status: P0, P1 and P2 complete and gated.  P3 (segment recovery) is next.
Handoff point: any fresh session can resume from commit `8339fce` and this
file.  P2 is opt-in and absent from the default path; no CLI/GUI flag or
Python binding exists yet (expected at P6, or earlier if P5 needs it).
