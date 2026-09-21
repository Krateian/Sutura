# Feasibility spike: from-scratch autorefine + alpha-wrapping for Sutura's heavy-self-intersection weak spot

**Date:** 2026-09-22 · **Scope:** research/plan only — no code written, no repo files touched.

---

## 1. Where self-intersections are handled today (`repair.py`)

Grounded in direct reads of `sutura/repair.py` (the graph at `graphify-out/` is stale, commit `49564f49`; no `graphify` CLI exists on this machine, so the code was read directly).

**Stage 1 chains — no SI handling at all in the default path:**
- `stage1_chain()` (repair.py:251): duplicate-faces → null-faces → duplicate-vertices → duplicate-faces → non-manifold-edges repair → re-orient → non-manifold-vertices repair → `close_holes` → `remove_connected_component_by_face_number` → remove-unreferenced → re-orient → `close_holes`. **No self-intersection filter anywhere.**
- `delete_fallback_chain()` (repair.py:287): the same plus a non-manifold *deletion* block (`compute_selection_by_non_manifold_edges_per_face` → `remove_selected_faces`). It handles **non-manifold edges only**, not self-intersections, and is tried only when the main chain leaves non-manifold edges/vertices (repair.py:698-706).

**The ONLY self-intersection handling in Stage 1 is the extreme-mode deletion:**
- `extreme_extra_passes()` (repair.py:323): extreme mode only, after the main chain. It *selects and deletes* self-intersecting faces (`compute_selection_by_self_intersections_per_face` → `meshing_remove_selected_faces` → `meshing_remove_unreferenced_vertices`), then re-runs the main chain once to close the newly-opened holes. This is exactly the delete-and-reclose approach that the backlog documents as worsening heavy-SI scans (removing SI faces opens more boundary edges than it closes).
- Self-intersections are otherwise **measured, never fixed**: `repair_mesh_from_arrays` reports `stage1.self_intersections_remaining` (repair.py:819-821); validate/`--dry-run` measure them the same way (repair.py:1412-1413).

**Stage 2 (manifold3d) invocation:**
- `maybe_run_stage2()` (repair.py:858) gates stage 2 on **both** `stage1.two_manifold` **and** `stage1.holes_remaining == 0` **and** the bridge existing. Called from `repair_file` (repair.py:1105) and per-object in `repair_3mf` (repair.py:1216) via `run_stage2` (repair.py:825, subprocess to venv311 or in-process bridge).

**The weak-spot anatomy (confirmed):** on a heavy-SI scan, Stage 1 either leaves holes open or the extreme deletion *opens more holes*; `two_manifold`/`holes_remaining == 0` then fails → **stage 2 never runs**. The mesh is returned broken with a `warning`. Both proposed ideas target this gap from opposite ends:
- **autorefine/snap-rounding** attacks the SI handling *inside* Stage 1 (never deletes faces → never opens new holes);
- **alpha wrapping** provides a Stage 2 *alternative* that can run on ANY input (even stage-1 output that is not watertight) and is guaranteed to emit a watertight/manifold/SI-free mesh.

---

## 2. Prototyping environment (what's actually available)

This machine (macOS, Apple Silicon) does **not** use the two-venv Linux layout described in AGENTS.md. There is a **single conda env** `sutura-env` (Python 3.11.16) — `~/.local/bin/sutura` execs `/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python`. It holds both pymeshlab and manifold3d. Findings:

| Package | Status | Install | Size (wheel) | License |
|---|---|---|---|---|
| numpy 2.4.6 | ✅ installed (sutura-env) | — | — | BSD-3-Clause |
| trimesh 5.1.0 | ✅ installed | — | — | MIT |
| pymeshlab 2025.7.post1 | ✅ installed | — | — | **GPL-3.0** (see §5) |
| manifold3d 3.5.3 | ✅ installed | — | — | Apache-2.0 (project `elalish/manifold`) |
| **scipy** | ❌ NOT installed | `pip install scipy` (no AUR needed) | macOS arm64 cp311 20.3 MB; **Linux cp314 manylinux 35.7 MB**; cp311 35.9 MB | BSD-3-Clause |
| **exact predicates (3D)** | ❌ NOT installed | `pip install pyrobust-predicates` | **11 KB** (+ mpmath 567 KB) | **Unlicense** (public domain) |

Key verifications performed this session:
- **scipy** resolves from PyPI without AUR for **both** layouts: I downloaded `scipy-1.17.1-cp311-cp311-macosx_14_0_arm64.whl` (20.3 MB) and `scipy-1.16.3-cp314-cp314-manylinux2014_x86_64` (35.7 MB) / `cp311` (35.9 MB) — the py3.14 Linux venv is covered.
- **`scipy.spatial.Delaunay` supports `incremental=True` + `add_points()` + `find_simplex()` + `neighbors`** — verified live in a scratch venv (50 → 275 simplices after `add_points`). This is the single most important primitive for alpha wrapping: Steiner-point insertion into a growing 3D Delaunay triangulation is exactly what it provides, with Qhull's robustness behind it.
- **Exact predicates:** the name "robust-predicates" is *not* on PyPI. The real candidates are:
  - `pyrobust-predicates` 0.1.0 — **Unlicense (public domain)**, pure-python wheel (`py3-none-any`, 11 KB), exposes `orient2d/3d`, `incircle`, `insphere` (+ fast variants), dep `mpmath`. **Verified live:** degenerate/coplanar → 0, regular tetra → ∓1, insphere signs correct. **This is the pick.**
  - `robust` (lycantropos) — MIT, pure-python, but only **2D** orientation/incircle (no `orient3d`, no `insphere`) → not suitable.
  - `shewchuk` (lycantropos) — MIT, C-backed, but also **2D only** (`orientation`, `incircle_test`; no 3D) → not suitable.
  - `geompreds` — MIT but **sdist-only** and **fails to build** on this machine → skip.
- **trimesh already provides the AABB/ray/proximity queries** the prototypes need: `trimesh.ray.ray_triangle.RayMeshIntersector` (pure-python, no embreex/pyembree), `trimesh.proximity.closest_point` (nearest-surface-point), mesh `section`/`slice_plane`. All MIT. No extra dependency needed for the offset-surface oracle or broad-phase SI detection.
- **pymeshlab has NO autorefine/subdivide-at-intersections filter.** The filter list contains only `compute_selection_by_self_intersections_per_face`, whole-mesh booleans (`generate_boolean_union/intersection/…`), `meshing_close_holes`, `meshing_snap_mismatched_borders`, and subdivision-refinement filters (Loop/Catmull-Clark — refinement, not arrangement). **The autorefine core (triangle-triangle split along intersection lines) must be written from scratch.** This is the central implementation fact for idea 4a.

---

## 3. Regression / benchmark targets

The AGENTS.md SI numbers (thingi10k_804302 = 14,730 SI; Ephebe = 2,491 SI + 327 NM; thingi10k_147525; Athena) come from a **75-model corpus that is not present on this machine** (not in `/tmp`, not in the benchmark tar.gz, not in `tests/real-world-samples/`). I measured SI counts directly (pymeshlab `compute_selection_by_self_intersections_per_face`) on the corpora that **are** available:

**Available SI-heavy targets — `tests/real-world-samples/` (40 meshes, committed to repo):**
| mesh | SI faces | total faces |
|---|---|---|
| thingi10k_100281.stl | **3,677** | 90,000 |
| thingi10k_1038441.stl | 2,416 | 10,418 |
| artec_metal-nut.stl | 505 | 89,999 |
| thingi10k_46012.stl | 454 | 90,000 |
| thingi10k_1038439.stl | 411 | 4,578 |
| thingi10k_145065.stl | 215 | 35,614 |
| thingi10k_40886.stl | 168 | 90,000 |

**Available SI-heavy targets — `/tmp/sutura_corpus_100/` Thingi10K subset (63 meshes, not committed):**
| mesh | SI faces | total faces | also in warning set? |
|---|---|---|---|
| thingi10k_1017012.stl | **4,349** | 10,524 | yes (1 hole opened) |
| thingi10k_105382.stl | 3,211 | 10,676 | no |
| thingi10k_1038441.stl | 2,416 | 10,418 | yes |
| thingi10k_1038439.stl | 411 | 4,578 | yes (heavy scan holes) |

**The documented Stage-1-fails-so-stage-2-never-runs set** (from `docs/repair-benchmark-strict-watertight-2026-09.md`, the 13 warning meshes on the 115 corpus): `artec_smart-car` (146 input holes), `artec_church-facade` (22), `thingi10k_1038439` (10), `thingi10k_100827` (hole count grew 5→9, output not 2-manifold), `thingi10k_1038441` (5→6), `thingi10k_1017012`, `thingi10k_1038444`, `thingi10k_224108`, plus the 1-stubborn-hole Artec scans. **These are the ideal alpha-wrap Stage-2 evaluation set** because manifold3d currently never gets a chance on them.

**Note for the plan:** `scripts/benchmark_repair_corpus.py` currently records `input_holes`/`input_non_manifold` but **not input SI counts** — a one-line harness addition (`compute_selection_by_self_intersections_per_face` on input) would be needed to track SI before/after. (Same-measurement `run_one` shape as the existing input defects.)

---

## 4. Staged implementation plan

Both ideas follow the established Sutura precedent: read the published algorithm, implement the math from scratch in numpy, never read CGAL source. Both are **flag-gated experimental features** (precedent: `--experimental-join-components`, `--experimental-edge-tiebreak`) and must be added to the module lists in `install.sh`, `install-macos.sh`, `scripts/build_appimage.sh`.

### 4a. Autorefine + snap-rounding (Lazard & Valque 2025) — smaller, nearer-term

**What it replaces:** the SI *deletion* block in `extreme_extra_passes` (and, flag-gated, the whole delete-and-reclose approach to SI). The paper's loop, from the SGP/HAL text:

1. Identify pairs of triangles that *properly* intersect — cheap broad phase, no intersection geometry built.
2. Snap the vertices of those pairs to the finest float-exact uniform grid (scale so `max|x| ∈ [2^23, 2^24)`; rounding = scale → round-to-nearest-int → unscale; relative error ≤ 2⁻²⁴ ≈ 6·10⁻⁸).
3. **Also** snap every vertex that falls in a cell containing a snapped vertex (the paper's key contribution over Zhou et al.; ablation shows it is load-bearing).
4. Compute all triangle-pair intersections with **exact predicates**, subdivide each triangle along its intersection segments (autorefine → each segment becomes a shared edge, fan-triangulate, weld, drop degenerates/duplicates).
5. Round the new intersection vertices to **doubles** (2²⁹× finer than the vertex grid).
6. Iterate ≤ 5 times or until no proper intersections remain; output an intersection-free double triangle soup.

**Never deletes input faces → directly fixes the documented "extreme worsens" failure mode** (no new boundary edges from SI removal). It does **not** close existing holes — it is a complementary Stage-1 step, so the hole-closing chain stays as is.

**Building blocks (all verified this session):** `pyrobust-predicates` orient3d (exact intersection tests, Unlicense) + trimesh/`scipy.spatial.Delaunay`-style broad phase + a from-scratch triangle-triangle-intersection/subdivision routine. pymeshlab cannot do the subdivision.

**Effort estimate: medium-high.** The bulk (~300–500 lines pure numpy) is the autorefine subdivision: per-pair segment intersection (Möller 1997 formula + orient3d predicate), polygon splitting + fan triangulation + vertex welding + degenerate elimination, plus the 5-iteration loop and exact self-intersection verification. The snap-rounding steps (ii/iii) are ~30 lines.

**Risk: medium.**
- **Correctness guarantee:** the paper uses *exact constructions* (CGAL EPECK, rational coords) for the arrangement to bound input→output distance. Pure-python exact constructions (mpmath/Fractions) will be **slow** on real meshes — the pragmatic from-scratch option is EPICK-style (exact predicates, double construction), which the paper itself notes gives certified topology with less distance control. Acceptable for an experimental flag, must be documented.
- **Termination is heuristic** (≤5 iterations; the paper notes termination is not proven in theory but holds on all 4,524 Thingi10K SI models).
- **Performance:** paper claims ~50k verts/s (C++); a numpy/python implementation will be maybe 5–20× slower. Fine for opt-in.
- **Integration:** behind `--experimental-autorefine`; runs after the main chain (or replaces the extreme SI-deletion) only when SI remain; report gains `autorefine_passes` / `si_remaining` fields. JSON contract additive.

### 4b. Alpha wrapping (Portaneri et al. 2022) — experimental Stage 2 alternative

**What it provides:** a from-scratch `wrap_mesh()` that takes ANY input (triangle soup) and emits a guaranteed watertight, 2-manifold, intersection-free, strictly-enclosing mesh. Algorithm (from the published paper / CGAL user-manual algorithm text — no CGAL source consulted):

1. Insert the 8 corners of a loose bounding box into a 3D Delaunay triangulation; tag infinite cells `outside`, all finite cells `inside`.
2. Flood-fill outside→inside through a **priority queue of gates** (Delaunay facets separating an outside from an inside cell). A gate is *alpha-traversable* iff its **circumradius > alpha** (facets with circumradius ≤ alpha cannot be crossed → cavities/holes smaller than alpha are sealed).
3. For a traversable gate, insert a **Steiner point** by rule (1): first intersection of the gate's **dual Voronoi edge** (segment between the two adjacent cells' circumcenters) with the **offset surface** — the level set `dist(input) = offset` of the *unsigned* distance field.
4. If the dual Voronoi edge misses the offset surface but the target cell intersects the input, insert the **projection of the cell's circumcenter onto the offset surface** (rule 2).
5. After each insertion the new cells are tagged `inside` and new gates pushed; otherwise the cell is traversed to `outside`. Queue-empty termination is guaranteed (insertions shrink facet circumradii).
6. Output = facets separating inside from outside cells.

**Building blocks (all verified this session):** `scipy.spatial.Delaunay(incremental=True, add_points)` for the growing triangulation; `pyrobust-predicates.orient3d` for Delaunay/predicate robustness; circumcenter computation in numpy; the unsigned-distance oracle via `trimesh.proximity.closest_point` + `trimesh.ray` (AABB) for the segment-offset intersection (root-finding on the distance function along the dual Voronoi edge) and for the projection rule.

**Effort estimate: high.** ~500–900 lines across several nontrivial geometric primitives: incremental Delaunay wrapper + inside/outside cell tagging, gate priority queue, circumradius test, dual-Voronoi-edge ↔ offset-surface intersection, circumcenter-to-offset projection, plus a Hausdorff/quality verification pass (the `viewer_data_render` Hausdorff code is reusable). Higher than 4a.

**Risk: high for correctness; medium for schedule.**
- **Numerical delicacy:** coplanar/co-spherical point sets, degenerate tetrahedra and robust circumradii are the classic Delaunay-refinement failure points; scipy/Qhull gives a robust base but the Steiner-insertion + gate bookkeeping is where bugs live.
- **Performance unknown** in numpy; CGAL is C++. Expect it to be a minutes-scale operation on scan meshes (acceptable for an opt-in fallback, not a default).
- **Two-sided-wrap caveat — must be explicitly guarded** (this is the standing rule the task demands): with an *unsigned* distance field the algorithm has no inside/outside notion, so if `alpha` is smaller than a genuine opening, it **double-wraps** legitimately open/thin shells (documented in the paper/CGAL docs, Fig. 9). Guard rails baked into the plan:
  1. **Never a silent default** — opt-in flag only (`--experimental-alpha-wrap`), and only offered when the normal stage 2 is unavailable/failed.
  2. **alpha from real geometry:** measure the input's actual hole diameters (`defects.detect` / `boundary_loop_stats`) and set `alpha ≥ largest genuine opening` so real holes are traversed rather than double-walled; only seal gaps *smaller than alpha*.
  3. **Post-verification:** compute Hausdorff distance output↔input and a volume-sanity check; if the wrap is suspiciously thick relative to `offset` or the Hausdorff blow-up exceeds a budget, report `alpha_wrap_warning` and **fall back** to the current result rather than silently shipping a double-wall.
  4. Only invoke when stage 1 output is *not* watertight (holes or SI remain) — on already-watertight meshes the existing manifold3d path is cheaper and safer.
- **Classification/confidence:** a wrapped mesh is watertight by construction; `classification.classify`/`confidence.repair_confidence` already treat a stage-2 `ok` as watertight — the alpha-wrap output should be reported through the same channel with an explicit `stage2_engine: "alpha_wrap"` so claims stay honest.

---

## 5. Licensing red flags

| Item | Status |
|---|---|
| **CGAL `Alpha_wrap_3` / `Polygon_mesh_processing`** | **GPL-3.0 — must not be read or copied.** This report used only the published paper + the CGAL *user-manual algorithm prose* (which is a plain-language description of the algorithm, not code). Same rule as the existing MeshCNN/eigenvalue precedent. |
| **CGAL `autorefine`** (GPL, underlies 4a) | Same rule: reimplement from the Lazard & Valque paper, never read CGAL. The paper is CC-BY on HAL; a crawlable preprint (`hal-04907149`) exists. |
| **`pyrobust-predicates`** (chosen exact-predicate package) | **Unlicense (public domain)** — clean, mirrors Shewchuk's own public-domain C. **This is the correct pick over `robust`/`shewchuk`/`geompreds`** (the alternatives are MIT but 2D-only or unbuildable). |
| **scipy** | BSD-3-Clause; embeds Qhull under a permissive (BSD-style) license. Clean. |
| **trimesh** | MIT. Clean. |
| **manifold3d** (existing stage 2) | Apache-2.0. Clean. |
| **Geogram (Inria)** | BSD-3-Clause permissively licensed (verified) — cited as a *permissive alternative* in the task; **not needed**, because scipy + pyrobust-predicates + trimesh cover all primitives without importing any GPL. |
| ⚠️ **pymeshlab — PRE-EXISTING dependency** | **GPL-3.0.** This is a real, already-shipped fact: the entire Stage 1 chain runs on GPL-3.0 pymeshlab while the app itself is PolyForm Noncommercial. It predates this spike and is the maintainer's existing distribution decision, but it is worth explicitly acknowledging in any licensing review — the two proposals here **do not add new GPL code** (they are from-scratch numpy), so they do not make the posture worse. |

No currently-installed helper turned out to be AGPL/GPL where a permissive swap is needed for the prototypes; the only GPL items are (a) CGAL, which is explicitly avoided by the from-scratch approach, and (b) pymeshlab, which is already in the product.

---

## Recommended next step

If approved, write this report to `docs/alpha-wrap-feasibility-2026-09.md` (consistent with the existing `*-2026-09.md` docs), then proceed in this order: **(1)** the ~20-min harness fix to record input SI in `benchmark_repair_corpus.py`; **(2)** prototype 4a (autorefine + snap-rounding) on the 8 real-world SI targets, flag-gated; **(3)** only if 4a shows a clear win on SI-remaining while keeping holes/surface-area stable, prototype 4b (alpha wrapping) on the 13 warning meshes with the two-sided-wrap guards, flag-gated. Both stay experimental until corpus evidence clears the project's conservative-defaults bar.