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
