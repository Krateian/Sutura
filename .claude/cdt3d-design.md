# CDT3D: exact constrained tetrahedralization for Splint

Status: research and design only. Branch `spike/cdt3d` (from `spike/splint-v2`).
No source changes, no commit, no push, no version bump. The target is an
opt-in Splint path that replaces the dual-contour/input-lattice tet basis with
an exact constrained tetrahedralization, so every selected output face is a
piece of an input triangle. The long-term goal is to replace the
non-deterministic fTetWild fallback with a deterministic engine at
fTetWild-level shape fidelity (two-sided Hausdorff at the T-tolerance
`clamp(bbox_diag/10000, 0.0125, 0.05)` mm; 0.0125 mm on
`tests/real-world-samples/thingi10k_100827.stl`).

The design is written against the existing Rust kernel in
`rust/sutura-geom`: `arrangement.rs` (exact 2D-per-host arrangement),
`cdt2d.rs` (exact 2D CDT), `predicates3d.rs`/`interval.rs` (exact predicates),
`splint/{delaunay,cut,label,emit}.rs` (Geogram Delaunay, winding/min-cut,
extraction), and `mincut.rs` (Dinic s-t cut).

---

## Objections

The following points are the reasons this path is not a small extension of the
current H7/H7b work. They are ordered by the risk they impose on the plan; each
one must be either resolved or explicitly accepted by the owner before code is
written.

### O1. The legal boundary excludes the fastest implementations

Sutura ships under PolyForm Noncommercial 1.0.0 (`LICENSE`), while
`rust/sutura-geom/Cargo.toml` and the crate headers declare MIT OR Apache-2.0.
The distributed product is the PolyForm work. The reference CDT code
(`github.com/MarcoAttene/CDT`) is GPL-3.0 or LGPL-3.0 depending on the build;
TetGen is AGPL-3.0-or-later; CGAL's `Constrained_triangulation_3` package is
GPL; Attene's `Indirect_Predicates`, `NFG` and `Delaunay3D` are LGPL. None of
the GPL/AGPL code can be copied, translated or statically linked into the
PolyForm product. The only clean options are an independent implementation
from the published algorithm (with citation), a dynamically linked LGPL
library with relinking/source obligations, or MPL file-level reuse. The
recommended choice is the independent Rust implementation; the LGPL CDT tool
is useful only as a non-shipped oracle. This is an engineering-legal
conclusion, not legal advice; a license audit is required before distribution.

### O2. The input is not a valid PLC, and `arrangement.rs` is not a PLC builder

A CDT algorithm assumes a valid piecewise-linear complex: the boundary of
every element is an element of the complex, and the intersection of any two
elements is empty or a union of elements. `arrangement_lite_core` only splits
*proper* triangle intersections with a per-host 2D CDT. It does not build a
full 3D arrangement, does not resolve coplanar overlaps by default, counts
touch points without splitting the host, and leaves T-junctions unless the
experimental `PROPAGATE_EDGE_POINTS` switch is on.

Measurements on the target and the seven Rust fixtures (this session, public
samples):

| mesh | V | F | edge-components | odd-use edges | vertex-on-edge T-junctions | positive-area coplanar overlaps |
|---|---:|---:|---:|---:|---:|---:|
| `thingi10k_100827` | 47 | 71 | 1 | 51 | 34 | 2 (areas 3.18e-7 and 6.36e-7 mm²) |
| `case1_open_cup` | 130 | 192 | 2 | 64 | 0 | 0 |
| `case2_pipe` | 64 | 64 | 1 | 64 | 0 | 0 |
| `case3_broken_cup` | 194 | 316 | 2 | 70 | 0 | 0 |
| `case4_cracked_sphere` | 568 | 1120 | 1 | 14 | 0 | 0 |
| `case5_box_missing_face` | 8 | 10 | 1 | 4 | 0 | 0 |
| `case6_thin_cup` | 65 | 96 | 1 | 32 | 0 | 0 |
| `plate_a_dent` | 6876 | 13732 | 1 | 18 | not exhaustively checked | not exhaustively checked |

`arrangement_lite` on `thingi10k_100827` produces 83 triangles, 0 proper
self-intersections, but still 15 odd-use edges and unresolved coplanar
contacts. Feeding this directly to a "valid PLC" CDT is unsound. Phase A must
therefore deliver a validator and a sanitizer, not just the existing splitter;
the six fixtures that were exhaustively checked are valid PLCs apart from open
boundaries (`plate_a_dent` was outside the diagnostic time box), but the target
is not yet one.

### O3. Genuinely open inputs cannot become closed solids made only of input triangles

A closed output surface cannot be a subset of an open input surface. The
target and several fixtures have odd-use edges; `case2_pipe` and
`case5_box_missing_face` are genuinely open. Closing them requires cap faces
(or a remesh) that are not input triangles. `thingi10k_100827` is special: an
independent check of the fTetWild arm-A output (696 vertices, 1388 faces)
found a closed surface with 0 odd-use and 0 non-manifold edges, every new
vertex within 1.2e-6 mm of the input surface, and every face centroid within
8.0e-7 mm. In other words, fTetWild closed a geometrically closed but
combinatorially non-conforming surface by remeshing it on the same surface.
The exact path may achieve the same on `100827` by resolving T-junctions,
non-manifold boundary vertices and the two tiny coplanar overlaps; it cannot
do so for `case5_box_missing_face` without adding the missing face.

Consequence: the "surface is made of input triangle pieces" guarantee must be
defined as "every selected non-cap face is a piece of an input triangle".
Caps must be a separate, reported constraint class. If the owner forbids caps
entirely, the exact tier cannot meet the 0-hole gate on the open fixtures and
must be scoped to closed/combinatorially-closable inputs.

### O4. Exact embedding removes the fidelity limit, not the labelling problem

The overnight results show that the tet basis, not the min-cut, was the
fidelity limit: H7b best parthood was 2 parts at Hmax 7.73 mm, and H7c
(prune 0.01 + bridge) reached 1 part at Hmax 7.727 mm, dominated by
input→output error on the folded flap. Embedding the input faces removes the
"vertices live on a ~5 mm lattice" cause: the cut can run on the input surface.
It does not remove the open-flap ambiguity, the need to choose one side of each
input face, the possible non-manifold selection, or the coverage requirement
(every input vertex must lie on the selected surface). The labelling stage must
be reworked so label changes are only allowed across constraint faces; simply
replacing the point set while keeping the current smoothness graph can still
emit arbitrary tet faces and still lose input vertices. The min-cut must be
contracted over non-constraint dual edges, and the result must be measured
two-sided, not inferred from the tet basis.

### O5. The face-recovery stage is research-grade work, not a port of a short module

The published algorithm has four stages: initial Delaunay, segment recovery,
face recovery, and inside/outside marking. The segment stage is a known
Si–Gärtner procedure with a new LNC Steiner point type. The face stage is the
hard part: cavity retriangulation, boundary-matching, cavity expansion, and a
fallback modified gift-wrapping algorithm with symbolic perturbation. The
reference C++ implementation is about 3.9k lines in `include/` plus three
LGPL libraries; a clean-room Rust version with exact predicates, tests and
determinism is a multi-month engineering effort. A plan that assumes the
existing arrangement and a weekend Delaunay wrapper will reach the target is
not credible.

### O6. Determinism is a property of the whole pipeline, not of the arithmetic alone

The spike already found that Geogram's BDEL returns the same cell set in a
different order on every call, which polluted every index-ordered downstream
pass until `canonicalize_tets` was added. A CDT core adds many more
order-sensitive decisions: cavity expansion order, apex selection, symbolic
perturbation, Steiner point insertion, and cell union-find order. The core must
be single-threaded and canonical; `HashMap` iteration, unordered floating-point
reductions, thread scheduling, and platform-dependent `f64` contraction are
all defects. A five-run in-process hash is not sufficient; two separate
processes and a second platform are required. The fTetWild reference cannot be
a determinism oracle because it is itself non-deterministic; it may only be a
shape baseline.

### O7. Coplanar overlaps and slivers need an explicit policy

The target has two positive-area coplanar overlaps with areas 3.18e-7 and
6.36e-7 mm², both between near-coincident sheets with opposite orientation.
Exact splitting turns them into coincident slivers; a valid PLC cannot contain
two triangles covering the same area. Dropping both leaves a tiny hole; keeping
one changes the winding; perturbing one by an infinitesimal changes the
"exact input" claim but is below f32 output resolution. This is a real policy
decision, not a numerical detail. The recommended policy is: exact split,
cancel opposite-oriented double coverage, and if a positive-area double cover
remains, apply a deterministic one-ulp normal perturbation or drop it with an
explicit `sliver_dropped` report. The result is "exact on the sanitized PLC",
not "bit-identical to the raw input PLC". The alternative is to refuse the
target as an invalid PLC, which defeats the purpose.

### O8. "Input triangle pieces" means arrangement pieces, not whole input triangles

When the input properly self-intersects, no tetrahedralization can contain both
original triangles as faces; the arrangement splits them. The guarantee is that
every output face is a subset of one input triangle (possibly after splitting
at an intersection, a T-junction or a segment Steiner point). For
`thingi10k_100827` there are no proper intersections, so the pieces are whole
input triangles apart from T-junction splits. This distinction must appear in
the report and in the shape gate; otherwise a reviewer may read "exact input
triangle" as "the original 71-triangle soup is a subset of the output", which
is false for general inputs.

### O9. The reference metric must be measured independently

The notes validated that the fTetWild arm-A output is a shape-preserving
remesh, not a one-sided artefact. The independent check above confirms that the
output is closed and lies on the input surface to about 1e-6 mm, but it is a
remesh with new vertices, not a reuse of all input vertices. The practical bar
is the T-tolerance 0.0125 mm, two-sided, measured with the independent harness
in `.claude/skills/sutura-verify/SKILL.md`, not the tool's own JSON. A result
that reports "Hmax 0" from a one-sided or vertex-only query is not acceptable.

### O10. Rollout must remain opt-in and reversible

The default Splint path and the fTetWild fallback must remain byte-identical
until the exact path passes all gates on the target and the seven fixtures, and
then the larger corpus. The new path needs a `SplintOpts` flag (default false),
a CLI flag, a GUI control, report fields, and the same T0/T1/T2/T3 process as
any method. A failure at any stage must fall back, not degrade the default
output. No version bump or release is implied by this design.

---

## 1. Prior art and licenses

### 1.1 Prior art

| Work | What it provides | License | Relevance |
|---|---|---|---|
| Diazzi, Panozzo, Vaxman, Attene, *Constrained Delaunay Tetrahedrization: A Robust and Practical Approach*, ACM TOG 42(6), 2023 (arXiv:2309.09805, DOI 10.1145/3618352) | Exact CDT of a valid PLC; segment recovery with LNC Steiner points; face recovery with cavity retriangulation and a gift-wrapping fallback; symbolic perturbation; 100% success on 4408 valid Thingi10k PLCs | Paper: ACM copyright; no code license | Primary algorithm reference; independent implementation |
| `github.com/MarcoAttene/CDT` | Reference C++ implementation of the above | GPL-3.0, or LGPL-3.0 with `-DLGPL=ON` (LGPL build uses NFG, Indirect_Predicates and Delaunay3D; the default GPL path uses modified `hxt_SeqDel`) | Oracle only, or dynamic LGPL dependency; no copy/translation into the product |
| TetGen (Si, ACM TOMS 2015) | Practical CDT/quality tetrahedral meshing; the theoretical base of the 2023 paper | AGPL-3.0-or-later (v1.5+), commercial license available; v1.4.3 is "MIT with noncommercial clause" | Algorithm reference only for v1.5+; v1.4.3 is old and has the same cavity-expansion gap; not recommended |
| CGAL `Constrained_triangulation_3` (6.1+) | Conforming 3D constrained Delaunay triangulation, Steiner points allowed | GPL-3.0+ (the package is in CGAL's GPL list); commercial license available | Not usable in the product; possible external oracle only |
| Geogram (`BrunoLevy/geogram`) | BSD-3-Clause Delaunay/PSM; the vendored `geogram_delaunay_psm` is already used for the sequential BDEL 3D Delaunay | BSD-3-Clause | Clean to reuse for the initial unconstrained Delaunay and for validation; it has 2D CDT but no 3D constraints |
| fTetWild (Hu et al., ACM TOG 2020) | Envelope-based triangle insertion into a floating-point tet mesh; fast and robust; output within an epsilon envelope | MPL-2.0 | Shape baseline and fallback; not a CDT and not deterministic; file-level MPL reuse is legally possible but not proposed for the core |
| TetWild (Hu et al., ACM TOG 2018) | Rational exact envelope insertion | MPL-2.0 | Same role as fTetWild; slower; baseline only |
| Attene, *Indirect Predicates for Geometric Constructions*, CAD 2020 (DOI 10.1016/j.cad.2020.102856) | Indirect orient/inSphere predicates for LPI/TPI/PPI and LNC points | `Indirect_Predicates` is LGPL (upstream text: 3-or-later; repository metadata: 2.1); `NFG` is LGPL | Reimplement the published formulas in Rust; do not copy headers; dynamic linkage only if ever used |
| `Delaunay3D` (`MarcoAttene/Delaunay3D`) | Templated 3D Delaunay data structure used by the CDT code | GPL-3.0 or LGPL-3.0 | Not usable except as a non-shipped oracle |
| Shewchuk adaptive predicates; `robust` crate | Exact orientation/insphere for explicit `f64` points | Public domain (Shewchuk); MIT (`robust` crate) | Clean to use; already a dependency |
| `num-bigint`, `num-rational` | Exact integer/rational arithmetic | MIT OR Apache-2.0 | Clean to use for the exact fallback |

### 1.2 Legal decision per option

**Option A — independent Rust implementation from the paper (recommended).**
Algorithmic ideas and mathematical formulas are not copyrightable; a
clean-room implementation written from the paper, the Si–Gärtner procedure and
standard symbolic-perturbation literature is clean. The implementation must
not copy identifiers, comments, source structure or tables from the GPL/LGPL
code. The paper and the original algorithms are cited in the module
documentation and in `docs/agents/`. This option keeps the product under
PolyForm Noncommercial and keeps the core in Rust.

**Option B — dynamic LGPL linkage to `MarcoAttene/CDT` (conditional).**
LGPL-3.0 permits a combined work under other terms if the LGPL library is a
separate shared library, its source is made available, and the user can
relink/replace it. It would require a C ABI shared library, platform packaging
for macOS/Linux/Windows, an LGPL notice file, and a build that does not
statically absorb the LGPL code. It is legally defensible but operationally
heavy, and it does not satisfy the "Rust core" goal. It is a reasonable
two-week prototype or a fallback if the clean-room path stalls, but not the
recommended shipping path.

**Option C — MPL-2.0 file-level reuse of fTetWild/TetWild (conditional).**
MPL-2.0 allows a larger work under other terms, provided the MPL files remain
MPL, their source is made available, and modifications to those files are
MPL. Translating a C++ file to Rust is a modification and remains MPL. This is
legally clean, but it does not provide a CDT and does not solve exact
constraint embedding; it only provides an envelope-insertion baseline. It is
not proposed for the core.

**Option D — GPL/AGPL copy or static link (rejected).** The GPL/AGPL
obligations are incompatible with distributing the product under PolyForm
Noncommercial. This includes TetGen v1.5+, CGAL's CDT package, and the default
GPL build of `MarcoAttene/CDT`. TetGen v1.4.3's "MIT with noncommercial
clause" is closer to PolyForm in spirit, but it is an old, less robust
implementation and still C++; it is not recommended.

**Oracle policy.** The LGPL CDT tool may be built in CI or in a developer
sandbox as a separate executable for differential checking. It is not linked
into `sutura_geom`, not shipped, and its outputs are only test references. If
it is ever shipped as an optional external tool, it must be treated as a
separate work with its own LGPL notices and source offer.

**Provenance policy.** Every new module records `paper reference`, `no code
copied`, and the specific equations used. A `LICENSES/THIRD_PARTY.md` entry is
added if any BSD/MIT code is reused. A license audit precedes any release.

---

## 2. Pipeline

### 2.1 Overview

```
STL/3MF triangle soup
  │
  ├─ Phase A: canonicalise → exact arrangement → PLC sanitizer → validator
  │            (input triangle pieces, boundary loops, optional caps)
  │
  ├─ Phase B: constrained tetrahedralization of the PLC
  │            (initial DT → segment recovery → face recovery → certificates)
  │
  └─ Phase C: winding unary → contract non-constraint cells → min-cut
               → select constraint faces → manifold repair → output mesh
```

The output of Phase B is a tetrahedralization of a padded box whose vertices
include every PLC vertex (and Steiner points on PLC segments), and whose
triangular faces include every PLC triangle. The output of Phase C is a subset
of those PLC triangles (plus explicitly marked cap faces), so the surface is
made of input triangle pieces by construction.

### 2.2 Phase A — exact arrangement to a valid PLC

**Current state.** `arrangement_lite_core` (i) finds candidate triangle pairs
with a uniform-grid AABB broad phase, (ii) classifies them with the exact
`triangle_intersection` predicate, (iii) splits each host triangle with the
exact projected 2D CDT (`cdt2d.rs`), (iv) welds exact rational vertices, and
(v) emits a triangle set. It does not construct 3D cells; the CDT does. It has
two useful experimental switches: `PROPAGATE_EDGE_POINTS` (propagate segment
endpoints lying inside a host edge to all triangles sharing that edge) and
`SPLIT_COPLANAR_OVERLAPS` (add the overlap boundary as constraints). The
default path is not a valid-PLC guarantee, and the Python binding returns
`f64`, while the Rust core retains `BigRational` coordinates.

**Required work.**

1. **Canonical input.** Weld vertices on the exact `f32` bit pattern of the
   input (the STL is already `f32`), drop zero-area and duplicate faces, and
   assign canonical indices. Do not reorient the surface: the winding field
   depends on the input orientation.
2. **Exact split.** Run the proper-intersection, touch-segment and touch-point
   cases. A vertex that lies strictly inside a host triangle must be inserted
   into that host's 2D CDT and become a vertex of the host triangulation;
   otherwise the complex is not closed under intersection. A vertex on a host
   edge must be propagated to every triangle sharing that edge (the existing
   `PROPAGATE_EDGE_POINTS` logic, but enabled explicitly for this path rather
   than as a thread-local experiment).
3. **Coplanar overlaps.** For a positive-area coplanar overlap, split both
   triangles by the exact 2D arrangement of their projections. The resulting
   pieces must not have positive-area overlap. For coincident pieces with
   opposite orientation, cancel the double cover (the net winding is zero
   there); for coincident pieces with the same orientation, keep one and mark
   the fold in the report. If a positive-area sliver remains (the target's two
   overlaps are of this kind), apply the documented policy in O7: a
   deterministic one-ulp normal perturbation followed by re-arrangement, or a
   `sliver_dropped` report below a stated relative-area threshold. The choice
   must be visible in the report, not hidden in a tolerance.
4. **Conformance and validation.** Build edge/face incidence and check the PLC
   axioms: no proper intersections; every pairwise intersection is a shared
   simplex; no vertex strictly inside a triangle; no T-junction; every edge's
   incident triangles form a fan (a boundary edge may have one incident
   triangle, a non-manifold edge may have more, but the incidence must be a
   union of simplices). Record non-manifold edges/vertices and boundary loops;
   they are allowed in a PLC but are constraints on the later manifold repair.
5. **Boundary loops and caps.** Detect ordered boundary loops. For the first
   milestone, do not cap; the report exposes `boundary_loops` and
   `boundary_edges`. For the closed-output milestone, cap using the existing
   `hole.rs`/`splint/cap.rs` ear-clipper, mark every cap triangle
   `constraint_kind = cap`, and never count it as an input piece. The winding
   field may use the capped input, as in H7b, but the cap is a separate
   constraint class in the CDT and in the output report.
6. **Exact-to-`f64` handoff.** The CDT operates on a `f64` PLC for speed: after
   the exact validation, convert the welded rational vertices once to `f64`.
   The conversion error is at most half an ulp per coordinate (about 1e-15 mm
   at 30 mm coordinates) and is below the 0.0125 mm bar. The exact rational
   coordinates and the validator report are retained as provenance. If the
   owner requires exact rational vertices end-to-end, the initial Delaunay and
   all predicates must accept rational points; that is a larger, slower
   variant and is not the first milestone.

**Phase A output contract.**

```
Plc {
  vertices: Vec<[f64;3]>,            // welded, canonical
  triangles: Vec<[usize;3]>,         // pieces of input triangles
  tri_kind: Vec<ConstraintKind>,     // input | cap | synthetic-box
  edges: Vec<[usize;2]>,             // unique PLC segments
  edge_incident: Vec<Vec<usize>>,    // triangles per edge
  boundary_loops: Vec<Vec<usize>>,
  report: {proper_si, coplanar_overlap, touch_point, t_junction,
           nonmanifold_edges, nonmanifold_vertices, boundary_edges,
           sliver_dropped, plc_valid}
}
```

Phase A fails closed: if `plc_valid` is false, the CDT path refuses and the
caller falls back to the current Splint/fTetWild path. Silent repair is not
allowed.

### 2.3 Phase B — constrained tetrahedralization

The implementation follows the 2023 paper's four stages. It is a tetrahedral-
ization of the convex hull (or a padded box) of the PLC vertices; the outside
of the box is represented by ghost tetrahedra or by hard sink tets.

**B1. Initial Delaunay.** Use the vendored Geogram BDEL on the PLC vertices
plus the eight box corners (and any existing Steiner points), then canonicalise
the cell set and build the Rust tet/neighbour structure. Geogram is fast and
BSD, and its cell *set* is deterministic even though its cell *order* is not;
`canonicalize_tets` already handles that. An own incremental Delaunay is the
fallback if Geogram's ghost handling is insufficient. The box is synthetic and
is never emitted; the box corners are hard sink terminals in Phase C.

**B2. Segment recovery.** For every PLC edge, test whether it is an edge of the
tetrahedralization. A missing edge is split in the manner of Si–Gärtner:
find the encroaching vertices in the diametral sphere, choose the reference
vertex that maximises the circle through the edge endpoints and the reference,
classify the endpoints as acute/non-acute, and compute the split parameter
`t` using the sphere-intersection rule (midpoint for the both-acute case,
sphere intersection otherwise). The split point is represented as an LNC
`p = t·v1 + (1−t)·v2` with `t` rounded to the nearest `f64`; it remains exactly
on the segment. Insert it into the tetrahedralization, split the incident
triangles and edges, and repeat until no segment is missing. All tests are
exact `orient3d`/`inSphere` predicates on explicit and LNC points.

**B3. Face recovery.** A PLC face is missing if a tetrahedralization edge
pierces its interior. Collect the tetrahedra incident to the piercing edges;
each face-connected subset is a cavity. Split the cavity along the face plane
into two half-cavities; the vertices on the plane belong to both. For each
half-cavity, compute a local Delaunay tetrahedralization of its vertices and
keep the tets inside the cavity. If a boundary triangle of a half-cavity is
not present in the local Delaunay, expand the half-cavity by adding the
opposite tetrahedron and retry. This is the Si–Gärtner procedure.

**B4. Cavity-expansion fallback.** The expansion can cross the face plane and
produce intersecting half-cavities; this is the theoretical gap in TetGen.
When the expansion would cross the plane, use the paper's modified
gift-wrapping: process the known boundary triangles one at a time, choose a
valid apex vertex in the half-cavity, and build the tetrahedron while enforcing
(i) positive volume, (ii) no non-simplex intersection with the current
boundary, and (iii) the constrained-Delaunay in-sphere condition with
visibility. The two half-cavities must induce the same triangulation on the
shared face; symbolic perturbation (Section 3.3) resolves cospherical ties.
This is the hardest part of the implementation and the main schedule risk.

**B5. Certificates.** After recovery, check with exact predicates that every PLC
triangle is the union of tet faces (the face's vertices and any Steiner points
on its edges appear in the triangulation), every PLC segment is an edge or a
chain of tet edges, every tet has positive orientation, every neighbour link is
consistent, and no duplicate or degenerate tet exists. Emit a recovery digest
(canonical tets + PLC triangle incidence) for determinism.

**B6. Output.** The tetrahedralization retains, per PLC triangle, the two
incident tets (or one on a boundary), and per tet its four neighbours. The
inside/outside classification is *not* done here; the paper's `markInnerTets`
is not used because an open PLC is not a polyhedron and the winding/min-cut is
the intended classifier.

### 2.4 Phase C — winding, constrained min-cut, extraction

The existing `cut.rs` already implements a soft winding unary, a face-area
smoothness term, hard source/sink tets, island cleanup, prune and bridge. The
CDT path keeps the unary and the min-cut solver but changes the graph
topology and the extraction:

**C1. Cell contraction.** Build a dual graph whose nodes are the tetrahedra;
union any two tets that share a *non-constraint* face. The result is one node
per 3-D cell of the arrangement (the tetrahedralization minus the PLC
constraints). Constraint faces become edges between cells; a constraint face
whose two incident tets are in the same cell becomes a self-loop and is
ignored. This contraction is the formal way to enforce "the output surface is
a subset of PLC triangles": the cut cannot change labels except across a
constraint. It also keeps the max-flow graph small and avoids `INF` capacities.

**C2. Unary.** Evaluate the generalized winding number of the (capped) input
at each tet centroid, as in H7; define `phi = clamp(w − 0.5, ±WIND_CAP)`.
For a cell, sum the per-tet unary weights (`phi·a_ref`), preserving the
min-cut energy of the uncontracted graph. Hard sink for box/hull cells; hard
source for any user-specified anchor (optional; the H5 anchors are not
required if the constraints do the pinning). The winding can be evaluated in
parallel because each query is independent; the CDT core itself stays
single-threaded.

**C3. Min-cut.** Use `mincut.rs::BkGraph`. For each constraint face between
cells `a` and `b`, add an undirected edge with capacity `area · lambda`
(the existing smoothness weight; `lambda` is a tunable, default 0.5 or the H7
value). Add the cell unaries and the hard terminals. The max-flow result is a
binary cell labelling.

**C4. Extraction.** For each constraint face whose incident cells have
different labels, emit the face, oriented so the normal points from the
inside cell to the outside cell. Emit input pieces and caps separately.
Snap/weld to `f32` only at the end, using the existing `emit.rs` path, and run
the existing manifold/part/shell checks. The output is a subset of the PLC
triangles; no arbitrary tet face can appear.

**C5. Manifold repair.** A union of cells can still have a non-manifold pinch
at an edge with four selected incident faces, or a boundary at an uncapped
loop. The existing tet-level `greedy_relabel` is the wrong move set here
because tets inside a cell cannot change the boundary. Add a *cell-level*
repair: flip whole cells (or a small connected set of cells) to reduce
non-manifold edges, guarded so that the shape does not regress, deterministic
and acceptance-gated. If the PLC is non-manifold at a vertex (the target has
boundary vertices with more than two incident boundary edges), the repair may
need to choose a manifold subset of sheets; record the result. If repair
cannot reach 0 non-manifold edges, report it and do not claim fTetWild parity.

**C6. Caps.** A cap is a constraint with `kind = cap`; it is selected by the
same min-cut and emitted, but it is not counted as an input piece. The report
contains `input_faces_selected`, `cap_faces_selected`, `cap_area`, and a
two-sided distance computed both with and without cap faces. The owner decides
whether cap faces are acceptable in the shape gate (O3).

### 2.5 Integration with Splint

* New `SplintOpts.cdt3d: bool` (default false), mutually exclusive with
  `input_tets`; the existing H7/H7b/H7c flags remain experimental and
  unchanged.
* New report fields: `cdt3d`, `plc_valid`, `plc_triangles`, `recovered_triangles`,
  `missing_constraints`, `steiner_points`, `tets`, `cells`,
  `input_faces_selected`, `cap_faces_selected`, `constraint_missing`,
  `cdt_det_hash`, `cdt_seconds`, plus the existing shape fields
  (`output_parts`, `holes`, `non_manifold`, `two_sided_max`, `two_sided_p95`).
* New CLI flag `--experimental-cdt3d` and a GUI checkbox in the same Options
  group as the other experimental tiers; update
  `docs/cli-gui-parity-notes.md` if the flag is intentionally CLI-only during
  the spike.
* Failure policy: if Phase A validation fails, the CDT path returns a typed
  error; the caller falls back to the current Splint/fTetWild path. No partial
  or silently repaired output.
* Default output must remain byte-identical when the flag is off. The new
  path is measured on the target and the seven fixtures before any default-on
  proposal. No version bump in this plan.

---

## 3. Exact arithmetic and determinism

### 3.1 Number types and predicates

The point algebra is an extension of the existing `point.rs`:

* `Explicit([f64;3])`: input vertices and `f64` PLC vertices.
* `Lpi { q1,q2, r,s,t }`: line-plane intersection (existing).
* `Ppi { r1,s1,t1, r2,s2,t2, r3,s3,t3 }`: three-plane intersection (existing).
* `Lnc { a, b, t }`: `t·a + (1−t)·b`, with `a`, `b` explicit and `t` an exact
  `f64`; this is the new Steiner point type. The point is exactly on the
  segment `a–b` by construction.
* `Rational([BigRational;3])`: only for the exact fallback and for the
  arrangement provenance; not in the hot path.

Predicates:

* `orient3d`: explicit points use the `robust` crate (Shewchuk filtered
  exact); implicit points use an interval filter on the homogeneous
  `(λ,d)` representation and fall back to a `BigRational` determinant. The
  existing `orient3d_sign` and `interval::det4` are the starting point.
* `inSphere`: explicit points use `robust::insphere`; implicit points use the
  cleared 5×5 determinant in homogeneous coordinates, first enclosed by an
  interval filter (`det5`), then evaluated exactly in `BigRational` if the
  filter cannot decide. The existing crate has `det_rat` (3×3) and `det4`
  (interval); a `det5` and a 5×5 rational determinant are added.
* For LNC, `to_f64` is the paper's approximate construction; `to_rational` is
  `(t_num·a + (t_den−t_num)·b) / t_den` with `t` interpreted as its exact
  dyadic value. The same formula gives the homogeneous `(λ,d)` pair for the
  filter.
* Later optimization (not the first milestone): reimplement the paper's
  indirect `orient3d`/`inSphere` polynomial filters and Shewchuk expansion
  arithmetic to avoid `BigRational` in the common case. The first version may
  use the rational fallback; correctness precedes speed.

No ad-hoc epsilon is used to decide a predicate. Exact zero is zero; the
symbolic perturbation below resolves degeneracies. Tolerances are confined to
the input (`f32` STL), the output (`f32` STL) and the documented coplanar
sliver policy.

### 3.2 Symbolic perturbation

Cospherical and coplanar configurations are common in repaired meshes. Use a
deterministic local-global symbolic perturbation:

* Assign every vertex a stable global index (input order, then derived points
  in canonical order).
* For an exact-zero `inSphere` of a valid tet and a query vertex, apply the
  paper's perturbed in-sphere rule: sort the five indices ascending, then
  evaluate `orient3d` on the last four; if that is zero, evaluate `orient3d`
  on the first, third, fourth and fifth; the parity of the swaps determines
  the sign. This is a clean-room implementation of the published algorithm,
  not a copy of the reference code.
* For an exact-zero `orient3d`, use the same index order to break the tie
  deterministically.
* Cavity expansion and gift-wrapping use the global index order for apex
  selection and for the vertex sublists `V1`/`V2`; this is what makes the two
  half-cavities conform.

### 3.3 Determinism rules

* The CDT core is single-threaded. No rayon, no TBB, no parallel cavity work.
  The winding queries in Phase C may be parallel because each query is
  independent; the cut itself is sequential.
* No `HashMap` iteration in any decision path. Use `Vec` with explicit sort,
  `BTreeMap`, or insertion-ordered arrays. The arrangement's existing
  `HashMap` caches are not iterated for output; keep that property in the new
  code.
* Canonicalize after every bulk build: sort each tet's four vertex ids, then
  sort the tet list; sort cell ids by their smallest tet; sort constraint faces
  by their vertex triples. Use the existing `canonicalize_tets` pattern.
* Input insertion order is canonical: input vertices in file order, then
  arrangement split vertices by exact coordinate and parent ids, then box
  corners, then Steiner points in creation order (which is itself
  deterministic from the canonical input and the exact predicates).
* Use stable sorts and explicit tie-breaks (`(cost, index)`), never
  floating-point sort keys that can tie differently across platforms.
* Do not use `f64` hashing as a map key for exact work; use exact rational
  keys (`RatKey`) or integer vertex ids.
* Compile without fast-math. Rust does not contract `a*b+c` by default; do not
  use `mul_add` in predicate code. The Geogram C++ build already pins
  `-frounding-math`, `-ffp-contract=off`, `-fno-fast-math`.
* The output hash is a digest of canonical tets plus the recovered PLC
  triangle incidence, not just the emitted `f32` mesh. The emitted mesh is
  also hashed five times in one process and twice in separate processes.
* The reference fTetWild output is **not** a determinism oracle; it is only a
  shape baseline.

### 3.4 Exactness versus output rounding

The CDT is exact with respect to the `f64` PLC. The PLC is a rounded form of
the exact rational arrangement; the rounding error is ≤0.5 ulp per coordinate
and is measured. The output STL is `f32`; the final `f32` snap can create
degenerate or inverted simplices. The output path therefore checks the
selected surface after the `f32` snap and reports any degeneracy; it never
claims that a rounded `f64` Steiner point is exact in `f32`. Because the
selected output faces use input vertices and edge Steiner points, the surface
geometry is preserved to well below the 0.0125 mm bar; internal tets are not
emitted. If a selected face becomes degenerate after `f32` snapping, the
existing `snap_and_dedup_f32` drops it and the report records the drop; this is
a shape-gate failure, not a silent repair.

### 3.5 Failure handling

If an exact predicate cannot decide because of a non-finite intermediate
(overflow/NaN), the CDT aborts with a typed error and the caller falls back.
The rational fallback should make this rare; the interval filter must never
decide a sign from an undecidable interval. A failed segment/face recovery is
also a typed error, not a tolerance-based retry. The only tolerance-based
policy is the documented coplanar-sliver rule in Phase A, and it is reported.

---

## 4. Effort estimate and milestones

The estimate assumes one experienced computational-geometry engineer familiar
with Rust and exact predicates, with the existing `sutura-geom` tests and the
public fixtures available. It assumes the arrangement is extended, not
rewritten, and that Geogram remains the initial-Delaunay backend. It is an
engineering estimate, not a commitment.

| Phase | Work | Days | Deliverable / gate |
|---|---|---:|---|
| P0 | PLC validator and arrangement completion: canonical input, touch/T-junction/coplanar resolution, boundary loops, report, `f64` handoff | 5–8 | `plc_valid=true` on `100827` and the seven fixtures; defect counts match the independent diagnostic; no CDT yet |
| P1 | Exact kernel: LNC point, `orient3d`/`inSphere` for implicit points, `det5`, rational fallback, symbolic perturbation, differential tests | 4–6 | Random and near-degenerate differential tests against `BigRational`; 5-run deterministic predicates; no tolerance decisions |
| P2 | Initial DT and tet/neighbour structure: Geogram BDEL, canonicalization, box/ghost handling, exact validity checks | 5–7 | Initial DT of `100827` and fixtures; all tets positive; neighbour links consistent; canonical hash stable |
| P3 | Segment recovery: missing-edge detection, encroaching reference, acute classification, LNC split, insertion, exact tests | 6–9 | Every PLC segment is a tet edge chain on `100827` and fixtures; deterministic; Steiner count reported |
| P4 | Face recovery: missing-face detection, cavity split, local Delaunay, expansion, gift-wrapping fallback, SoS | 12–18 | Every PLC triangle is recovered on `100827` and fixtures; `missing_constraints=0`; certificate digest |
| P5 | Phase C: cell contraction, winding unary, min-cut, extraction, cell-level repair, cap class | 6–9 | Output surface is a subset of PLC triangles; 100827 shape gate; fixtures no regression; caps reported |
| P6 | Integration and hardening: `SplintOpts`, CLI/GUI, report, fallback, T0/T1/T2/T3, performance, docs, license audit | 6–10 | Default off byte-identical; target and fixtures pass; larger corpus sampled; release checklist ready |

Total: 44–67 engineer-days, approximately 10–14 weeks full-time, plus review
and integration overhead. A realistic calendar estimate with one reviewer is
3–4 months. The face-recovery phase (P4) dominates the uncertainty; if it
overruns, the plan should stop at a documented P3 milestone rather than ship a
partial CDT.

### 4.1 First milestone M1 — constraint recovery (measurable)

**Inputs.** `tests/real-world-samples/thingi10k_100827.stl` and the seven Rust
fixtures `case1_open_cup`, `case2_pipe`, `case3_broken_cup`,
`case4_cracked_sphere`, `case5_box_missing_face`, `case6_thin_cup`,
`plate_a_dent`.

**Gate (no shape gate yet).**

* Phase A: `plc_valid=true`; `proper_si=0`; `t_junction=0`; positive-area
  coplanar overlaps resolved or reported under the O7 policy; boundary loops
  and non-manifold counts reported.
* Phase B: every PLC triangle is recovered as a union of tet faces
  (`missing_constraints=0`); every PLC segment is an edge chain; every tet has
  positive orientation; every neighbour link is consistent; no duplicate tets.
* Determinism: five runs in one process and two separate processes produce the
  same canonical tet hash and the same recovery digest; no thread-count
  dependence.
* Performance target on the reference machine: `100827` ≤ 30 s; each fixture
  ≤ 5 min; peak memory ≤ 2 GB. These are targets to be measured, not
  guarantees.
* Report: `input_plc_triangles`, `recovered_triangles`, `missing_constraints`,
  `steiner_points`, `tets`, `seconds`, `cdt_det_hash`.

**Exit criterion.** All eight meshes pass. This milestone proves exact
constraint embedding without committing to the shape result. It can be
evaluated with a small Rust example/binary before any CLI wiring.

### 4.2 Second milestone M2 — shape and integration

**Gate.**

* `100827`: 1 part, 0 holes, 0 non-manifold edges after the `f32` weld,
  two-sided Hausdorff max ≤ 0.0125 mm (p95 ≤ 0.0125 mm as a secondary gate),
  with cap faces reported separately; compare against fTetWild arm A.
* Seven fixtures: no regression against the current baseline on holes,
  non-manifold edges, parts/shells and two-sided distance; open fixtures may
  require caps, which are reported and explicitly accepted by the owner.
* Determinism: five-run and cross-process hashes identical.
* Default off: the existing Splint output is byte-identical; fTetWild remains
  the fallback.
* T0/T1/T2/T3 per `.claude/skills/sutura-verify/SKILL.md`; `graphify update .`
  after any local commit.

### 4.3 Alternatives and their estimates

* **LGPL dynamic CDT prototype.** 2–3 weeks to wrap the reference CDT as a
  separate shared library and drive the existing PLC/min-cut. Legally
  conditional, packaging-heavy, not Rust, and not the recommended shipping
  path. Useful as an oracle while the Rust core is developed.
* **MPL fTetWild file-level reuse.** 3–5 weeks to integrate a C++ envelope
  inserter; still not exact, still non-deterministic, and does not meet the
  "output faces are input pieces" requirement.
* **Own initial Delaunay instead of Geogram.** 1–2 extra weeks; removes the
  last C++ dependency from the core and gives full control of ghost cells.
  Not required for M1.

---

## 5. Risks

| Risk | Likelihood | Impact | Mitigation / trigger |
|---|---|---|---|
| Face-recovery cavity expansion and gift-wrapping take longer than estimated | High | Critical path | Implement Si first; log every expansion failure; use the paper's fallback; if P4 exceeds 3 weeks, stop at P3 and reassess |
| Exact `BigRational` fallback is too slow on the larger meshes | Medium | High | Interval filters first; expansion/indirect predicates next; profile on `100827`, `plate_a_dent`, `metal-nut`; keep the rational path as a correctness oracle |
| Input PLC remains invalid (coplanar slivers, non-manifold boundary vertices) | High for `100827` | High | Phase A validator fails closed; documented O7 policy; never feed an invalid PLC to the CDT |
| Open inputs cannot meet 0 holes without caps | Certain for some fixtures | Medium | Mark caps as a separate class; owner decision on the gate; do not claim pure-input closure for `case5_box_missing_face` |
| The constrained cut selects a non-manifold surface | Medium | High | Cell-level repair with shape guard; report NM; do not silently remesh; fall back if the gate fails |
| Determinism drifts (HashMap, SoS, Geogram order, platform) | Medium | High | Canonicalization, single-thread core, 5-run and cross-process tests, two platforms, no fast-math |
| Legal contamination from GPL/AGPL code | Low if disciplined | Critical | Clean-room policy, provenance log, no copy/translation, LGPL oracle only, license audit |
| `f32` output rounding degenerates selected faces | Medium | Medium | Validate after `snap_and_dedup_f32`; report drops; measure two-sided distance; do not hide drops |
| Integration regresses the default path | Low | High | Default off, byte-identical gate, typed fallback, T0/T1/T2/T3 |
| Scope creep to replace fTetWild immediately | Medium | High | Keep fTetWild fallback; ship the exact tier opt-in only after M2; version bump is a separate owner decision |
| Private samples or paths leak into tracked docs | Low | High | Use only public sample names and relative paths; apply the existing `_scrub` / `priv_<sha8>` rules |
| Performance target is missed on `metal-nut` (90k faces) | Medium | Medium | Keep `metal-nut` and `1038441` out of M1; profile later; the first milestone is the target plus seven fixtures |

---

## Appendix A — Evidence collected in this session

The diagnostics below are public-sample measurements made while preparing this
design. They are not a substitute for the T1/T2/T3 shape gate.

* `thingi10k_100827`: 47 vertices, 71 faces, 1 edge-connected component,
  51 odd-use edges, 34 vertex-on-edge T-junctions, 2 positive-area coplanar
  overlaps (3.18e-7 and 6.36e-7 mm²), 0 proper self-intersections, area
  878.191, bbox diagonal 37.340 mm.
* `arrangement_lite` (with edge-point propagation) on `100827`: 83 output
  faces, 0 proper self-intersections, 15 odd-use edges; the 15 edges include a
  boundary vertex with four incident boundary edges, so the output is not yet
  a closed valid PLC.
* fTetWild arm A on `100827` (`/tmp` output from the overnight run, not a
  tracked file): 696 vertices, 1388 faces, 0 odd-use edges, 0 non-manifold
  edges; every new vertex is within 1.2e-6 mm of the input surface and every
  face centroid within 8.0e-7 mm. The output is a closed remesh of the same
  geometric surface, not a subset of the input triangles.
* Overnight Splint baseline: H7b best 2 parts at Hmax 7.73 mm; H7c
  (prune 0.01 + bridge) 1 part at Hmax 7.727 mm; default res=64 hardened
  7534 faces / 6 parts / 121 non-manifold edges. This is the fidelity and
  topology baseline the CDT path must beat.
* The six fixtures `case1`–`case6` have 0 vertex-on-edge T-junctions and 0
  positive-area coplanar overlaps in the exhaustive check; `plate_a_dent` was
  outside the diagnostic time box and was not exhaustively checked. Their
  odd-use edges are open boundaries, not self-intersections.

## Appendix B — Open decisions for the owner

1. **Caps.** Are cap faces allowed as a separately reported constraint class
   for genuinely open inputs, or must the exact tier be restricted to
   closed/combinatorially-closable surfaces? This determines whether
   `case5_box_missing_face` and `case2_pipe` can pass the 0-hole gate.
2. **Tiny coplanar overlaps.** Is the O7 policy (exact split, opposite
   cancellation, then a reported one-ulp perturbation or a below-`f32` drop)
   acceptable, or must the target be declared an invalid PLC and excluded from
   the exact path?
3. **Non-manifold input.** Is a non-manifold selected surface acceptable when
   the input is non-manifold, or must the cell-level repair always reach a
   manifold result (which may require sheet selection and a shape change)?
4. **Exact rational vs `f64` PLC.** Is the proposed `f64` PLC handoff (exact
   CDT on rounded arrangement vertices, with reported error) acceptable, or is
   end-to-end rational needed?
5. **Oracle.** May the LGPL CDT tool be built in CI/developer sandboxes as a
   non-shipped oracle? If it is ever shipped, the LGPL notices, source offer
   and relinking terms apply.
6. **Milestone.** Is M1 (constraint recovery, no shape gate) the go/no-go, or
   is M2 (1 part / 0 holes / 0 NM / 0.0125 mm) required before further
   investment?
7. **Performance budget.** What are the acceptable runtimes for `100827`, the
   seven fixtures, `metal-nut` and `1038441` on the reference machine, and is
   `metal-nut` in the first tier or a later one?

## Appendix C — References

* L. Diazzi, D. Panozzo, A. Vaxman, M. Attene. *Constrained Delaunay
  Tetrahedrization: A Robust and Practical Approach.* ACM TOG 42(6), 2023.
  arXiv:2309.09805, DOI 10.1145/3618352.
* H. Si, K. Gärtner. *Meshing piecewise linear complexes by constrained
  Delaunay tetrahedralizations.* Proceedings of the 14th International Meshing
  Roundtable, 2005.
* J. R. Shewchuk. *Sweep algorithms for constructing constrained Delaunay
  triangulations.* (and the constrained-Delaunay tetrahedralization theory
  cited by the 2023 paper).
* H. Edelsbrunner, E. P. Mücke. *Simulation of simplicity: a technique to cope
  with degenerate cases in geometric algorithms.* ACM TOG 9(1), 1990.
* M. Attene. *Indirect Predicates for Geometric Constructions.* Computer-Aided
  Design 125, 2020. DOI 10.1016/j.cad.2020.102856.
* H. Si. *TetGen, a Delaunay-Based Quality Tetrahedral Mesh Generator.* ACM
  TOMS 41(2), 2015.
* Y. Hu, T. Schneider, B. Wang, D. Zorin, D. Panozzo. *Fast Tetrahedral
  Meshing in the Wild.* ACM TOG 39(4), 2020 (fTetWild).
* Y. Hu, D. M. Bradley, X. Li, T. Schneider, D. Zorin, D. Panozzo. *Tetrahedral
  Meshing in the Wild.* ACM TOG 37(4), 2018 (TetWild).
* `https://github.com/MarcoAttene/CDT` (GPL/LGPL reference implementation).
* `https://github.com/MarcoAttene/Indirect_Predicates` (LGPL).
* `https://github.com/MarcoAttene/Delaunay3D` (GPL/LGPL).
* `https://www.wias-berlin.de/software/tetgen/FAQ-license.jsp` (TetGen
  AGPL/commercial licensing).
* `https://www.cgal.org/license.html` and the CGAL
  `Constrained_triangulation_3` package page (GPL package).
* `https://github.com/wildmeshing/fTetWild` (MPL-2.0).
* `rust/sutura-geom/src/arrangement.rs`, `cdt2d.rs`, `predicates3d.rs`,
  `interval.rs`, `splint/{delaunay,cut,label,emit}.rs`, `mincut.rs` (existing
  kernel).
