# Architecture and libraries

[← Back to README](../../README.md) · [Türkçe](tr/architecture.md)

## Libraries

| Library | Role | Why |
|---|---|---|
| Native stage-1 engine (`sutura_engine` + Rust `sutura_geom`) | stage 1 filter chain | VCG `meshing_*`-equivalent cleanup (duplicate/degenerate removal, non-manifold repair, coherent orientation, ear-clipping hole closure) implemented natively; no external C++ runtime, permissive-licensed |
| Manifold (via `manifold-csg` in `sutura_geom`) | stage 2 rebuild, outer-shell union, repeated-element CSG, Graft local stitch | watertight guarantee, robust boolean, same C++ kernel as Bambu Studio; static link, Apache-2.0 |
| numpy | pipeline-wide | array math for the CLI, GUI, defect detection, history and every pure-python tier; the required `sutura_geom` Rust extension is NOT optional and the Flap path has no numpy fallback |
| pytetwild | fTetWild fallback tier (installed by default) | fTetWild tetrahedralization (MPL-2.0); about 6 MB, PyVista/VTK are not used, so it ships by default and is skipped with `SUTURA_WITH_FTETWILD=0` |
| sutura-geom (Rust: `robust`, `manifold-csg`, `num-bigint`, `num-rational`, `num-traits`, `rayon`, `faer`, `kiddo`, `pyo3`, `numpy`; vendored Geogram Delaunay PSM, BSD-3-Clause) | exact arrangement, Manifold CSG, morphology, nearest-neighbour, Flap, Dressing, Tamponade and topology-cleanup acceleration | exact explicit/indirect predicates (`orient3d`, `insphere`), the statically linked C++ Manifold kernel (`manifold-csg`) for Stage 2, outer-shell union, repeated-element CSG and the Graft local stitch, interval filter, exact triangle intersection and CDT for `--experimental-autorefine` and `--experimental-indirect-autorefine`, SDF grid and dual contouring for Graft (#13), the `flap_fill` surface-hole fill, the `dressing_coat` narrow-band extractor, the `tamponade` interior-plug module, point-cloud nearest neighbours (`kiddo`), exact EDT/dilation, and the native mesh-cleaning filters (`remove_duplicate_faces`, `remove_null_faces`, duplicate/unreferenced-vertex removal, vertex welding) that back the Stage-1 cleanup with a transparent pure-numpy fallback; `faer` (MIT) provides the sparse LU for the Flap fairing. Permissive licenses only (MIT/Apache-2.0/BSD); the vendored Geogram Delaunay PSM is mandatory and compiled into every `sutura_geom` build (prebuilt wheels include it; source builds need C++20). Prebuilt wheels are installed automatically via `scripts/install_sutura_geom.sh`, or built with `maturin develop` |

Pinned in `requirements.txt`, `requirements-gui.txt` and
`requirements-ftetwild.txt` (development/test dependencies in
`requirements-dev.txt`); Rust dependencies in `rust/sutura-geom/Cargo.toml`
(source builds require Rust >= 1.89, the `kiddo` 6.3 MSRV). SciPy is not a
dependency either: nearest-neighbour, EDT and dilation live in the Rust core.
`trimesh` is no longer a runtime dependency: mesh IO and the pure-Python
geometry helpers live in the engine, and the test suite / developer scripts
install `trimesh` only through the dev-only `requirements-dev.txt`.

## Architecture

Sutura is organized as a modular engine (`sutura_engine`) designed for reliability, strict layer boundaries, and extensibility:

* **`sutura_engine.core`** — Generic geometry and validation: mesh array I/O (STL, OBJ, 3MF), topological measurements, and Hausdorff distance guards; re-exports the specialised passes.
* **`sutura_engine.diagnosis`** — Surface and defect diagnostics: component analysis, extensible geometric templates (mechanical, organic, reliefs, thin sheets, lattices), and mesh classifier integration.
* **`sutura_engine.chart`** — Content-addressed SHA-256 caching under `~/.cache/sutura/` with 500 MB LRU eviction, automatic invalidation across version updates, and CLI (`--no-cache`, `sutura clear-cache`) / GUI options.
* **`sutura_engine.hull`** — Outer-surface shell extraction for multi-component assemblies.
* **`sutura_engine.cast`** — Thin wrapper around the Rust `sutura_geom` extension (exact predicates, `arrangement_lite`).
* **`sutura_engine.methods`** — Pluggable repair methods conforming to `RepairMethodProtocol`: built-in methods (`#1` Quick Clean through `#19` Coaptation) discovered automatically from one named module per method, plus closing, proxy, morphology graft, and repeated-element repair tiers.
* **`sutura_engine.triage`** — Intelligent escalation policy: ranks repair methods based on geometric analysis, enforces user intensity presets (*Quick*, *Balanced*, *Thorough*, *Extreme*), and respects strict runtime budgets.
* **`sutura_engine.adapters`** — Strict isolation boundaries for external and third-party tools (fTetWild and external CLI engines, plus the Manifold availability probe). Manifold itself is not a Python adapter: its C++ kernel is statically linked into the mandatory `sutura_geom` Rust core through `manifold-csg`. Adapters are imported lazily only when a specific method executes.

In an installed or AppImage layout `sutura_engine` is a top-level package; in a source checkout a small root bootstrap package (`sutura_engine/__init__.py`) makes `import sutura_engine` work from the repository root without a symlink.

### What is specific to Sutura

* **X-Ray reload-honest verdict (`P-HONEST`).** Rather than relying on in-memory topological counts, Sutura confirms watertightness only when the serialized float32 mesh reloads strictly closed (`defects.detect()` reports 0 holes and 0 non-manifold edges). When float quantization introduces boundary gaps on reload, the Stitch pass (`P-WELD`) fuses coincident seams without altering face topology; this closed-loop check caught 7 false-watertight results across the v0.5.1 corpus.
* **Graft verbatim hybrid (Method #13).** Combines mathematical morphology envelopes (Nooruddin & Turk 2003; Suriyababu et al. 2023) computed via exact Euclidean distance transforms (Felzenszwalb & Huttenlocher 2012) and dual contouring (Ju et al. 2002) with verbatim preservation of undamaged original triangles. The reconstructed patch covers only defective regions, constrained by a one-sided healthy-fidelity guard (`fidelity_ok`) that falls back to fTetWild comparison when deviation occurs, achieving 0.0 healthy deviation on all adopted corpus meshes (see `docs/repair-benchmark-strict-watertight-2026-09.md`). *Note: Graft (#13) requires the `sutura_geom` Rust extension, which ships with the installers and AppImage/.dmg bundles; the extension is required by the runtime, so a missing or stale extension is a named startup error rather than a Graft/fTetWild fallback.*
* **Transplant repetition healing (Methods #11–#12).** Detects recurring geometric features across rotational, translation lattice, helical, and reflective symmetries without requiring pre-segmentation. When localized damage is identified on one instance of a repeated element, a congruent healthy copy is aligned and transplanted to seal the defect, guarded by Hausdorff constraints outside the target region (`hausdorff_outside`).
* **Triage escalation policy (`sutura_engine.triage`).** Couples per-object geometric feature diagnosis (component structure, open-boundary ratios, symmetry scores) with ranked method recommendations and a count-budgeted fallback ladder (at most three candidates, each with a deterministic tier budget; a very high wall-clock watchdog is reported explicitly if it ever trips). The fallback order can also skip a method the diagnosed damage makes pointless (duplicate faces skip Join; interior faces/regions skip Transplant and Wall Thicken); `SUTURA_TRIAGE_PRUNE=0` restores the previous order. When usage history is enabled, a bounded Bayesian update (Beta(2, 2) posterior mean) locally adjusts method selection probabilities without network communication.
* **Chart content-addressed cache (`sutura_engine.chart`).** Caches intermediate and final repair artifacts by SHA-256 geometry hash under `~/.cache/sutura/` with a 500 MB LRU eviction policy. The cache transparently accelerates repetitive pipeline runs and multi-object 3MF processing, automatically invalidating entries across version bumps or parameter modifications.

### Roadmap: moving hot paths to the Rust core

Future development aims to reduce Python runtime overhead by migrating performance-sensitive paths to `rust/sutura-geom`:
- **Phase 0 (Python Hot-Path Reductions):** Removal of unconditional `trimesh` imports from the outer-shell check shipped in v0.7.0; vectorization of candidate transforms in rotational symmetry detection shipped in v0.7.2 (batched, threaded nearest-neighbour queries, identical results, ~2-3.7x faster on the sample/corpus detector).
- **Phase 1 (Rust Topology Layer):** Implement deterministic array operations in native Rust (X-Ray reload welding, strict hole/non-manifold detection, and boundary loop defect walking), using native BVH structures for nearest-neighbor spatial queries.
- **Phase 1c (Rust Hole Triangulation — completed):** the native Rust `sutura_geom.close_holes` ear-clipping closer (matching VCGLib `meshing_close_holes` semantics) now backs Stage-1 hole closure directly on NumPy arrays. A Liepa-style minimum-weight triangulation remains future work.
- **Phase 2 (PyMeshLab Independence — completed):** duplicate/degenerate face removal, non-manifold repair, coherent orientation, duplicate/unreferenced-vertex removal and vertex welding are implemented natively in Rust (`sutura_geom`) with a pure-numpy fallback, so PyMeshLab is no longer an external C++ runtime dependency.
- **Phase 3 (Native Manifold — completed):** Manifold is linked directly into the Rust extension through `manifold-csg`; Stage 2, the outer-shell union, repeated-element CSG and the Graft local stitch run in `sutura_geom`, the Python `manifold3d` bridges are gone, and the 3.11 virtualenv remains only as the fTetWild fallback when the main interpreter is out of range.
- **Phase 4 (Standalone Rust CLI):** remains future work toward an independent Rust CLI executable.
