# Feasibility spike: Geogram `MeshSurfaceIntersection` + `remove_internal_shells` for Sutura's heavy-self-intersection weak spot

**Date:** 2026-09-24 · **Scope:** standalone `/tmp` spike — no code written in `rust/`, no `repair.py` wiring, no feature flag.

---

## 1. Hypothesis

Sutura's Phase B Rust arrangement (`rust/sutura-geom`) is exact-`BigRational` only and does not finish on dense self-intersection scans (e.g. `thingi10k_1038441`: ~93 k candidate pairs, ~4.4 k real SI, 30+ min).  
Geogram (BrunoLevy/geogram, BSD-3-Clause) ships `MeshSurfaceIntersection::intersect()` and `remove_internal_shells()`, an exact expansion-arithmetic self-union / outer-hull pipeline.  The hypothesis was that calling Geogram as an external subprocess could solve the same dense-SI meshes in seconds and produce a watertight outer hull that Sutura's existing Stage 2 (`manifold3d`) could verify or rebuild.

---

## 2. What was tried

A minimal C++ CLI (`/tmp/geogram-spike/bin/geogram_si_clean`) was built against the Homebrew Geogram 1.10.1 bottle.  It loads an STL, runs:

```
MeshSurfaceIntersection::intersect()
MeshSurfaceIntersection::remove_internal_shells()
mesh_repair(M, MESH_REPAIR_DEFAULT, 1e-6)
```

and writes an STL.  The pipeline was measured on the five real-world SI-heavy meshes available in `tests/real-world-samples/`.

Measurements used Sutura's existing helpers:
- `scripts/benchmark_repair_corpus.py::count_self_intersections()` for SI counts,
- `sutura/defects.py::detect()` for the strict watertight verdict (0 holes and 0 non-manifold edges),
- `sutura/manifold_bridge.py::watertight_check()` for an independent manifold3d verdict,
- pymeshlab `get_hausdorff_distance` for geometry deviation vs. the input.

A 180 s timeout per mesh was enforced.

---

## 3. Five-mesh results

| Mesh | In V | In F | In SI | Out V | Out F | Out SI | Strict WT | Manifold3d | Hausdorff %bbox | Geo time (s) | OK |
|---|---|---|---|---|---|---|---|---|---|---|---|
| thingi10k_100827 | 47 | 71 | 11 | 47 | 83 | 0 | false | false | 0.0000 | 0.0018 | ok |
| thingi10k_1038439 | 2375 | 4578 | 411 | 2796 | 6028 | 12 | false | false | 1.2048 | 0.0503 | ok |
| thingi10k_46012 | 44843 | 90000 | 454 | 45176 | 91170 | 117 | false | false | 0.0000 | 0.2929 | ok |
| thingi10k_100281 | 44563 | 90000 | 3677 | 47077 | 97692 | 22 | false | false | 0.0291 | 0.3287 | ok |
| thingi10k_1038441 | 5339 | 10418 | 2416 | — | — | — | — | — | — | — | **crash** |

*Strict WT = `defects.detect()` reports 0 holes and 0 non-manifold edges.*

Three iterations of the same pipeline were also run on the non-crashing meshes; residual SI and boundary defects remained in every case (e.g. `thingi10k_1038441` was not reachable, but `thingi10k_100281` dropped only from 22 to 22 SI and still had 2 non-manifold edges).

---

## 4. Root-cause finding A: Geogram output is incompatible with Sutura's Stage-2 manifold3d rebuild

For the four meshes that produced a Geogram output, the output STL was fed through Sutura's existing Stage-2 bridge (`manifold_bridge.run_bridge()`), exactly as the real pipeline would do.

| Mesh | Geogram SI | M3d input watertight | M3d construct status | M3d output produced |
|---|---|---|---|---|
| thingi10k_100827 | 0 | false | `Error.NotManifold` | **no** |
| thingi10k_1038439 | 12 | false | `Error.NotManifold` | **no** |
| thingi10k_46012 | 117 | false | `Error.NotManifold` | **no** |
| thingi10k_100281 | 22 | false | `Error.NotManifold` | **no** |

`manifold3d` rejected every Geogram output because the input was not manifold.  Therefore the combined "Geogram self-union → manifold3d rebuild" chain does **not** reach SI=0 + strict watertight for any of these meshes.  Even the mesh on which Geogram eliminated all SI (`thingi10k_100827`) was left with boundary holes, so manifold3d could not construct a solid.

---

## 5. Root-cause finding B: community-edition Geogram crashes on `thingi10k_1038441`-class dense degenerate meshes

`thingi10k_1038441` fails deterministically inside Geogram's exact radial-sort step:

```
(E)-[RadialSort] Both triangles in same reference half-plane
FATAL ERROR: Did not manage to sort a bundle in Polyline of length 1
(if you reached this point, you may need geogramplus, contact TESSAEL)
```

Two cheap mitigations were tried without modifying Geogram itself and without the proprietary `geogramplus` kernel:

1. **Sutura autorefine as pre-cleanup.**  `autorefine.autorefine()` (float-grid snap-rounding subdivision, the same module behind `--experimental-autorefine`) was run on `thingi10k_1038441` first, then Geogram was run on the refined mesh.  Autorefine took 18.5 s and increased SI from 2416 to 3620; Geogram still crashed in `RadialSort` (`Polyline of length 2`).  **FAIL.**

2. **Geogram `algo:predicates=exact`.**  Geogram exposes `algo:predicates` (`fast` default, `exact` alternative).  The CLI was rebuilt to set `algo:predicates=exact` before intersection.  The crash reproduced with the same message.  **FAIL.**

The crash is a real blocker for community-edition Geogram 1.10.1 on this class of dense, degenerate scan meshes.  The library's own error message identifies the proprietary `geogramplus` arithmetic kernel as the intended fix.

---

## 6. License check

- Geogram itself is BSD-3-Clause.
- The Homebrew bottle's only LGPL component is bundled `libMeshb` (`.mesh`/`.meshb` I/O).  It is statically embedded in `libgeogram.1.10.1.dylib`; the STL + `MeshSurfaceIntersection` code path used here does not exercise it at runtime.
- No GPL code was added to Sutura in this spike.

---

## 7. Outcome — Geogram evaluated, NOT integrated

Following the project's standing rule for rejected experiments (the FAZ6 / FAZ8 / FAZ9 "evaluated, NOT integrated" pattern), this report records the outcome so a future attempt does not re-tread the same ground.

**Status:** Geogram is **NOT integrated** into Sutura.  There is no `--experimental-geogram` flag, no GUI control, no wiring into `repair.py`, and no module-list update in `install.sh` / `install-macos.sh` / `scripts/build_appimage.sh`.

**Why the path is closed:**
1. Geogram's `MeshSurfaceIntersection` + `remove_internal_shells` is fast and reduces SI substantially, but its output remains non-manifold / non-watertight on Sutura's real-world SI targets.
2. Sutura's existing Stage-2 `manifold3d` rebuild cannot consume the Geogram output (`Error.NotManifold` on all four non-crashing meshes).
3. The decision mesh `thingi10k_1038441` crashes in Geogram's exact `RadialSort`; neither Sutura's autorefine nor Geogram's `exact` predicate mode avoids the degeneracy.
4. The only identified fix for the crash is the proprietary `geogramplus` kernel, which is outside Sutura's open-source dependency policy.

**Forward step:** the next real attempt remains the existing Rust Phase B work in `rust/sutura-geom`: adding a filtered f64/interval/exact evaluation layer to the BigRational-only arrangement code so dense-SI scans finish in practical time without sacrificing correctness.  This spike does not alter `rust/sutura-geom`.
