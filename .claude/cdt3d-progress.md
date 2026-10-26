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

## 2026-10-26 — P3 segment recovery implemented and gated

What was done:
- New `rust/sutura-geom/src/cdt3d/segment.rs`:
  - exact PLC edge extraction and deterministic per-edge split chains;
  - Si's acute-vertex classification (two incident PLC segments forming an
    angle strictly below 90 degrees), computed exactly with an interval filter
    and rational fallback;
  - the three split cases: both endpoints acute -> midpoint; one acute endpoint
    -> sphere centred at the remembered acute vertex plus the
    close-to-reference/shift/midpoint rule; no acute endpoint -> maximum
    circumradius reference and the smallest touching-sphere intersection;
  - LNC Steiner points parameterised on the *original* PLC edge (the P1 `Lnc`
    type only carries explicit endpoints), with the split parameter rounded to
    `f64`;
  - exact Bowyer--Watson insertion: whole-star seeding on faces/edges, exact
    `insphere_sign_perturbed` conflict tests, positive new tets, local
    neighbour stitching, and an incremental PLC edge-count index;
  - exact final validity: positive tets, duplicate/degenerate tets, neighbour
    reciprocity, empty sphere with `insphere_sign`, box/ghost hull check,
    unused vertices, and `missing_after = 0`.
- `cdt3d/mod.rs` exports `recover_segments`/`SegmentMesh`/`SegmentOptions`/
  `SegmentReport` and shares the corpus test loader between P2 and P3.
- `cdt3d/dt.rs` only exposes `sorted_face` and `build_neighbours` to the crate
  and reuses the shared test loader; no P2 behaviour change.
- Tests: category-1 midpoint and smallest-sphere rules, category-3 midpoint,
  exact acute classification, positive face apex, a 5-run 100827 test and the
  ignored 8-mesh gate.

Key finding during gating: using only the first-category reference rule on all
segments (no acute classification) ballooned `plate_a_dent` past 16k splits and
was still not converged.  The correct acute classification is mandatory: that
fixture contains a 320-edge high-valence dent vertex (`3436`), so almost all of
its incident PLC edges are category 2/3.  Applying the paper's acute-vertex
rules reduced the final count to 16,771 Steiner points and made the run finish.

Performance work (needed for the plate gate):
- incremental edge-incidence counts instead of rebuilding an edge set per split;
- local Bowyer--Watson neighbour stitching instead of rebuilding all neighbours
  (the dominant cost before: 10 ms/split at 45k tets);
- `f64` prefilter for the reference/circumradius scan, with exact rational
  fallback for collinearity; exact squared comparisons for the category-1
  branch and exact predicates for insertion/validation;
- a deterministic seed hint (first new tet) for point location, which cut the
  `plate_a_dent` gate from 163 s to ~68 s per run.

Gate (`cargo test --release cdt3d::segment::tests::segment_public_corpus_gate
-- --ignored`), 5 runs per mesh in one process; the gate was also run in a
second process and all eight hashes matched.  Every row has `empty = off_box =
ngb = unused = 0`, `missing_after = 0`, and `valid = true`.

| mesh | PLC E | missing before | Steiner | splits | cat1/2/3 | acute V | subseg | tets | secs/run | hash |
|---|---|---|---|---|---|---|---|---|---|---|
| thingi10k_100827 | 132 | 26 | 68 | 68 | 0/0/132 | 47 | 200 | 634 | 0.052 | de6e07edac20fa5c |
| case1_open_cup | 320 | 52 | 128 | 128 | 0/0/320 | 130 | 448 | 1377 | 0.100 | e4069fe7322c4664 |
| case2_pipe | 128 | 16 | 64 | 64 | 0/0/128 | 64 | 192 | 606 | 0.071 | fc14028427e2aed3 |
| case3_broken_cup | 509 | 110 | 753 | 753 | 0/0/509 | 194 | 1262 | 6214 | 1.141 | 5f86082e79ae7123 |
| case4_cracked_sphere | 1687 | 42 | 158 | 158 | 0/0/1687 | 568 | 1845 | 3895 | 0.137 | 708bcd9b13ff8a2e |
| case5_box_missing_face | 17 | 2 | 2 | 2 | 0/0/17 | 8 | 19 | 58 | 0.004 | d265d998222d34bb |
| case6_thin_cup | 160 | 16 | 64 | 64 | 0/0/160 | 65 | 224 | 613 | 0.064 | f51a2e5042170b77 |
| plate_a_dent | 20610 | 454 | 16771 | 16771 | 0/0/20610 | 6876 | 37381 | 122595 | 67.7 | 8ce442e77e5af205 |

All initial PLC vertices are classified acute on these eight meshes, so every
initial segment starts in category 3 and is midpoint-split; the halves then
remember one acute endpoint and use the category-2 rule.  Category 1 is
therefore covered by the synthetic tests, not by the corpus.

`cargo test --release`: **377 passed, 0 failed, 4 ignored** (the P0 gate, the P2
gate, the P3 gate and the pre-existing ignored test).

Default path: P3 is not called from `arrangement`, `Splint`, the CLI or the
GUI; the only edits outside the new module are test sharing and crate-internal
visibility in `dt.rs`.  The full default test suite is green.

Next step: commit P3 locally, run `graphify update .`, then start P4 (face
recovery: missing-face detection, cavity split, local Delaunay, expansion,
gift-wrapping fallback, SOS).

### Self-review — P3 caveats and uncertainties

- The reference/circumradius scan and the category-2 acute rule use `f64` for
  candidate selection and the sphere/point comparisons.  The chosen split
  parameter is rounded anyway, and every insertion/validation predicate is
  exact, but the split *heuristic* is not a fully exact decision like P1's
  predicates.  The category-1 branch decision uses exact squared distances.
- Category 3 always takes the midpoint and does not first reuse an existing
  vertex lying exactly on the subsegment.  The corpus never exercised the
  collinear-reuse path (`reused = 0` everywhere); it is covered only by the
  generic chain logic.
- `plate_a_dent` is a pathological gate input: a single 320-edge acute dent
  vertex produces 16,771 Steiner points and dominates the gate at ~68 s/run.
  The five-run gate is ~6 minutes; further runtime work would be needed before
  P6 if this fixture is in the normal test loop.
- All eight test PLCs classify every vertex as acute.  The category-1 path and
  the no-acute reference selection are tested synthetically but not by a real
  corpus mesh.
- The incremental edge index and local neighbour stitching are checked only by
  the final exact validity report (orientation, reciprocity, empty sphere); no
  separate debug build cross-checks the incremental structures against a full
  rebuild after every insertion.
- P3 still has no Python binding or CLI/GUI flag; that remains P6 work, or
  earlier only if P5 needs it.

## 2026-10-26 — P3 closed / handoff

- Committed P3 locally as `0f92065` ("cdt3d: add exact segment recovery with
  LNC splits (P3)") on `spike/cdt3d`; not pushed.
- `graphify update .` run after the commit: 9622 nodes, 22075 edges, 348
  communities; only the pre-existing Geogram C++ syntax warnings remain.
- `cargo test --release`: 377 passed, 0 failed, 4 ignored.
- The 8-mesh gate was run in three separate processes (including the final
  code); all eight hashes matched every time, and the in-process five-run
  assertions passed for every mesh.

Status: P0, P1, P2 and P3 complete and gated.  P4 (face recovery: missing-face
detection, cavity split, local Delaunay, expansion, gift-wrapping fallback,
SOS) is next.  Handoff point: any fresh session can resume from commit
`0f92065` and this file.  P3 remains internal and opt-in; no CLI/GUI flag or
Python binding exists yet (P6).

## 2026-10-26 — P4.0 missing-face detection

What was done:
- New `rust/sutura-geom/src/cdt3d/face.rs` scaffold: `FaceOptions`,
  `FaceReport`, `FaceMesh`, `recover_faces`, and exact
  `detect_missing_faces`.
- A PLC face is missing exactly when a tet edge properly pierces its interior
  (strict plane-side crossing plus the three interior edge-plane signs, all
  `orient3d`).  Candidate faces are found through a deterministic uniform grid
  over PLC face AABBs; tet edges are deduplicated in a `BTreeSet`, candidate
  face lists are sorted, and no hash iteration drives a decision.
- `recover_faces` currently copies the P3 mesh and reports the missing count;
  retetrahedralization is not implemented yet (expected at P4.1).
- Synthetic crossing/coplanar tests, a 5-run 100827 detection test, and an
  ignored 8-mesh detection gate.

Detection gate (`cargo test --release
cdt3d::face::tests::face_detection_public_corpus_gate -- --ignored`):

| mesh | PLC faces | tets | missing before | detect secs |
|---|---|---:|---:|---:|
| thingi10k_100827 | 83 | 634 | 11 | 0.001 |
| case1_open_cup | 192 | 1377 | 12 | 0.005 |
| case2_pipe | 64 | 606 | 6 | 0.001 |
| case3_broken_cup | 316 | 6214 | 79 | 0.010 |
| case4_cracked_sphere | 1120 | 3895 | 7 | 0.012 |
| case5_box_missing_face | 10 | 58 | 0 | 0.000 |
| case6_thin_cup | 96 | 613 | 6 | 0.002 |
| plate_a_dent | 13734 | 122595 | 8 | 2.712 |

`case5_box_missing_face` already has every surviving PLC face represented; its
input hole is a boundary loop, not a missing interior face.  The detection grid
makes `plate_a_dent` cheap (2.7 s); the ~82 s gate wall time is still dominated
by P3 segment recovery (~68 s for plate).

`cargo test --release`: **380 passed, 0 failed, 5 ignored** (P0/P2/P3 gates, the
new P4 detection gate, and the pre-existing ignored test).

Next step: P4.1 cavity construction, plane split, local Delaunay, and exact
boundary/face verification.  If local Delaunay cannot represent a boundary
triangle, expand the cavity; log each failure.

## 2026-10-26 — P4.1 cavity split and local Delaunay (partial)

What was done:
- `recover_faces` now iterates missing faces, collects the tets incident to
  piercing edges, splits each face-connected cavity by the face plane into
  `V1` (on/above) and `V2` (on/below), computes a local Geogram Delaunay of
  each vertex set, keeps only tets whose exact point-in-tet test places the
  centroid inside the removed cavity, and replaces the cavity.
- Replacement is verified exactly: every original cavity boundary triangle must
  appear once with the same orientation, no extra new boundary face is allowed,
  no new tet edge may still pierce the face, and cavity volume must be
  preserved.  Failures are logged behind `CDT3D_DEBUG_FACE` and skipped.
- No expansion or gift-wrapping yet; those are P4.2/P4.3.  No Steiner points are
  added (the P3 count is unchanged).

P4.1 numbers (release; five runs not yet asserted for P4):

| mesh | missing before | missing after | cavities tried | retetra | tets | structural valid |
|---|---:|---:|---:|---:|---:|---|
| thingi10k_100827 | 11 | 9 | 14 | 2 | 632 | true |
| case1_open_cup | 12 | 12 | 12 | 0 | 1377 | true |
| case2_pipe | 6 | 6 | 6 | 0 | 606 | true |
| case3_broken_cup | 79 | 79 | 172 | 0 | 6214 | true |
| case4_cracked_sphere | 7 | 7 | 7 | 0 | 3895 | true |
| case5_box_missing_face | 0 | 0 | 0 | 0 | 58 | true |
| case6_thin_cup | 6 | 6 | 6 | 0 | 613 | true |
| plate_a_dent | 8 | 8 | 10 | 0 | 122595 | true |

Wall time of the face stage alone: 100827 0.051 s; case3 8.1 s; plate 23.1 s
(on top of P3).  Structural checks (`orientation`, `duplicate`, `non-manifold`,
`neighbour`, `off-box hull`, `unused`) are zero everywhere.

Why so few cavities succeed (recorded for P4.2): the local Delaunay of `V1` and
`V2` frequently (a) leaves a cavity boundary triangle unrecovered, (b) has a
cavity boundary triangle whose vertices straddle the face plane, or (c) still
leaves a piercing edge because the two half-triangulations do not agree on the
face.  These are exactly the cases the Si–Gärtner expansion and the
gift-wrapping fallback must handle.  `case3_broken_cup` also shows the retry
loop is costly (172 cavity attempts, 8.1 s) before hard-failing.

`cargo test --release`: **381 passed, 0 failed, 6 ignored**.

Next step: P4.2 — expand a cavity when a boundary triangle is missing or when
its boundary crosses the face plane; add the opposite tet, recompute, and
record expansions.  Then P4.3 gift-wrapping for the remaining cases.
