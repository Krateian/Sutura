# Mesh corpus attribution & licenses

The real-world test corpus used by the mesh classifier (`tests/real-world-samples/`)
contains **third-party meshes** used only as test/regression fixtures. They are
**not** covered by Sutura's own license (PolyForm Noncommercial 1.0.0, which
applies to the code). Each file keeps its original license; you are responsible
for respecting it if you reuse the files outside Sutura's test suite.

- **Artec 3D scans** — 8 meshes, **CC BY 4.0**, source
  https://www.artec3d.com/3d-models/stl (decimated to ~90k faces).
- **Thingi10K** — 32 meshes, per-model **CC BY 4.0 or CC0** (Public Domain),
  source the Thingi10K dataset (Qingnan Zhou & Alec Jacobson), downloaded from
  the Hugging Face mirror `Thingi10K/Thingi10K` `raw_meshes/`. The Phase 2
  additions (2026-09) were restricted to CC BY / CC0 only; the 8 pre-existing
  Thingi10K fixtures include a few CC BY-SA / CC BY-ND models that predate that
  rule and are kept as-is. Files with > ~90k faces are decimated
  (preservetopology) to keep the corpus practical.

The complete per-file attribution table (file, license, model name, author,
Thingiverse source URL) lives in `tests/real-world-samples/ATTRIBUTION.md`.

## `sutura_geom` third-party code

The Rust extension vendors the standalone **Geogram Delaunay PSM**
(`rust/sutura-geom/third_party/geogram_delaunay_psm/`), used by the Splint core
for deterministic tetrahedralization. It is **BSD 3-Clause**, Copyright (c)
2000-2022 Inria, upstream <https://github.com/BrunoLevy/geogram>. The full
license is in that directory and is shipped in the wheel together with
`rust/sutura-geom/THIRD_PARTY.md`; it is separate from the crate's
MIT/Apache-2.0 license and from Sutura's PolyForm Noncommercial license.

The extension also builds the **meshoptimizer** C++ core through the `meshopt`
crate (`meshopt = "0.7"`, MIT OR Apache-2.0; vendored meshoptimizer 1.3, MIT,
Copyright (c) 2016-2026 Arseny Kapoulkine, <https://github.com/zeux/meshoptimizer>).
It provides the native quadric decimation pass (`decimate.rs`) used by the
Dressing tier and the interactive-viewer LOD. The C++ is compiled from
the crate's own vendored sources at build time and its MIT license text is shipped
as `rust/sutura-geom/third_party/meshoptimizer/LICENSE` (also in the wheel).

Corpus composition (40 meshes):

| Group | Count | Files |
|---|---|---|
| Artec scans (CC BY 4.0) | 8 | `artec_*.stl` |
| Thingi10K mechanical (Phase 2, CC BY/CC0) | 13 | `thingi10k_55772/71691/235725/81221/228302/248395/42844/60916/59226/70561/57854/475828/145065` |
| Thingi10K organic (Phase 2, CC BY) | 11 | `thingi10k_260537/313444/39507/55280/40886/100281/63785/136634/1356633/331105/46012` |
| Thingi10K pre-existing fixtures | 8 | `thingi10k_100045/100077/100173/100827/1038439/1038441/224108/502009` |

## Benchmark corpus release (115 meshes)

The large **repair benchmark corpus** — 52 Artec STL scans (CC BY 4.0, from
https://www.artec3d.com/3d-models/stl) + 63 Thingi10K STLs (Zhou & Jacobson;
per-model CC BY 4.0 / CC0, from the Thingi10K dataset / Hugging Face mirror
`Thingi10K/Thingi10K`) — is ~6 GB and is **deliberately not committed to the
git repo** (it would multiply the repo size ~100× and be painful to remove
from history later). It is published instead as a **split `.tar.gz`** on the
GitHub release **`benchmark-corpus-v1`** (two `< 2 GB` parts because GitHub
caps release assets at 2 GB), with a `SHA256SUMS` per-part checksum file.

- **Fetch**: `scripts/fetch_benchmark_corpus.sh [dest_dir]` downloads all
  parts, verifies SHA-256, recombines and extracts (default
  `/tmp/sutura_corpus_100`, the default corpus path used by
  `scripts/benchmark_repair_corpus.py`).
- **Licenses**: the files retain their original licenses (CC BY 4.0 for the
  Artec scans, per-model CC BY 4.0 / CC0 for Thingi10K); reuse outside
  Sutura's benchmark must respect each model's license.
- **How to re-package / re-upload when meshes change**:
  1. Put the new/updated STLs into the corpus directory (keep `*.stl` files
     directly in the directory root).
  2. Re-pack: `cd <corpus_dir> && tar -czf sutura-benchmark-corpus-v1.tar.gz ./*.stl`
     (or a new version tag, e.g. `benchmark-corpus-v2`).
  3. Split into `< 2 GB` parts: `split -b 1900m ... sutura-benchmark-corpus-v1.tar.gz.part-`
     and replace the `PARTS` list + `SHA256SUMS` in `scripts/fetch_benchmark_corpus.sh`.
  4. Compute the new per-part hashes into a `SHA256SUMS` file.
  5. `gh release create <tag> --draft --title "Repair benchmark corpus (115 mesh)" --notes-file <notes>` then `gh release upload <tag> <parts...> SHA256SUMS`.
  6. Re-verify: `scripts/fetch_benchmark_corpus.sh` on a clean machine and re-run
     `scripts/benchmark_repair_corpus.py` to confirm the corpus round-trips.

## Typical-user corpus (50 meshes)

The **typical-user repair corpus** represents everyday 3D print models curated
from the Thingi10K dataset (Zhou & Jacobson 2016) with mild defects commonly
encountered on sharing platforms (Printables, Thingiverse). It spans three
strata (20 topologically open meshes, 20 self-intersecting meshes, and 10 solid
clean controls) filtered to common consumer-print complexity (1,000–300,000
facets, at most 20 components) under permissive licenses (CC0, Public Domain,
CC BY, and CC BY-SA; no NonCommercial or NoDerivatives).

To keep the git repository lightweight, mesh binaries are not committed. Only
the model manifest containing IDs, strata, facet counts, licenses, and authors
is tracked in `tests/typical-corpus-ids.csv`.

- **Fetch**: `python3 scripts/fetch_typical_corpus.py` downloads the selected
  meshes from the Thingi10K Hugging Face repository, caches them locally
  (default `/tmp/thingi10k_cache`), and extracts standard STLs to
  `/tmp/sutura_typical_corpus`.
- **Licenses**: each model preserves its original permissive upstream license;
  reuse outside the benchmark must respect the individual model's license terms
  recorded in `tests/typical-corpus-ids.csv`.

## Third-party library dependencies

Sutura's own code is licensed under the PolyForm Noncommercial 1.0.0 license.
The following third-party libraries are bundled or depended upon at runtime
and keep **their own licenses** (reuse/distribution of Sutura does not change
their terms):

- **fTetWild / pytetwild** — MPL-2.0. `sutura/ftetwild_bridge.py` calls the
  `pytetwild` wrapper around fTetWild (https://github.com/wildmeshing/fTetWild,
  wrapper https://github.com/pyvista/pytetwild). Since v0.5.0 the fallback is
  installed and active by default; the CLI opt-out is
  `--no-fallback-ftetwild` (or `SUTURA_WITH_FTETWILD=0` at install time), while
  `--experimental-fallback-ftetwild` additionally
  runs it on closed-but-self-intersecting results (GUI checkbox, FAZ17). Used only as
  an unmodified dependency: no Sutura source is converted to MPL (MPL-2.0 is
  file-level copyleft), and the MPL-2.0 license text + upstream URLs are the
  full notice required when distributing the dependency or a bundled wheel.
- **pyrobust-predicates** — Unlicense / public domain. Shewchuk-style exact
  3D orientation predicates used by `sutura/autorefine.py` (FAZ16).
- **meshopt / meshoptimizer** — MIT OR Apache-2.0 (vendored meshoptimizer 1.3,
  MIT). Native quadric decimation (`decimate.rs`) compiled into `sutura_geom`
  (see the meshoptimizer note above); used by the Dressing tier and the
  interactive-viewer LOD.
- **Manifold** — Apache-2.0. The C++ Manifold kernel
  (https://github.com/elalish/manifold) is compiled statically into
  `sutura_geom` through the `manifold-csg` Rust crate (Apache-2.0 OR MIT,
  https://github.com/zmerlynn/manifold-csg). It powers Stage 2, the
  outer-shell union, the repeated-element CSG transplant and the Graft local
  stitch. The build uses no TBB (sequential, deterministic).
- **PySide6 / Qt** — LGPL-3.0; the GUI toolkit.

`pytetwild`'s optional PyVista accessor is imported at load time upstream, but
Sutura does not use it: `sutura/ftetwild_bridge.py` stubs that accessor, so
PyVista and VTK are neither bundled nor installed.

**Test-only oracle (not a runtime dependency):** **PyMeshLab** — GPL-3.0 /
Commercial (VCG library). Since the native engine migration, stage-1 topology,
hole closing, self-intersection classification and decimation run in
`sutura_geom` / `sutura_engine`, so PyMeshLab is no longer bundled or imported
by `sutura/` (enforced by `tests/test_no_runtime_pymeshlab.py`). It is retained
in `requirements-dev.txt` solely as an independent parity oracle for the tests.

## Algorithm references (clean-room implementations)

Sutura's codebase implements several published geometric algorithms clean-room
without reusing third-party source code:

- **Shrink-Wrapping & Surface Envelopes**: Kobbelt, Vorsatz, Labsik & Seidel, *A Shrink Wrapping Approach to Remeshing Polygonal Surfaces* (Computer Graphics Forum 18(3), Eurographics 1999).
- **Morphological Closing Envelopes**: Nooruddin & Turk, *Simplification and Repair of Polygonal Models Using Volumetric Techniques* (IEEE TVCG 9(2), 2003); Suriyababu, Vuik & Möller, *Towards a High Quality Shrink Wrap Mesh Generation Algorithm Using Mathematical Morphology* (Computer-Aided Design, 2023).
- **Fast Winding Numbers**: Barill et al., *Fast Winding Numbers for Soups and Clouds* (ACM TOG / SIGGRAPH 2018).
- **Robust Generalized Winding Numbers (antipodal)**: Martens, Trettner & Bessmeltsev, *The Antipodal Method: Fast, Accurate, and Robust 3D Generalized Winding Numbers* (ACM TOG 45(4), 2026, art. 43, doi 10.1145/3811323). Sutura's `rust/sutura-geom/src/antipodal.rs` is a clean-room reimplementation in Rust of the published algorithm (exact integer sign predicates plus the boundary term, hand-written symbolic perturbation); no reference source code was copied. The reference implementation (`github.com/MartensCedric/antipodal`) is MIT-licensed and was consulted only as an algorithmic specification.
- **Exact Euclidean Distance Transform**: Felzenszwalb & Huttenlocher, *Distance Transforms of Sampled Functions* (Theory of Computing 2012).
- **Dual Contouring**: Ju et al., *Dual Contouring of Hermite Data* (SIGGRAPH 2002).