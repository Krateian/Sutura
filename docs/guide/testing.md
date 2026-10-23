# Testing and benchmarks (developers)

[← Back to README](../../README.md) · [Türkçe](tr/testing.md)

## Test

Synthetic broken mesh:

```sh
python3 tests/make_broken_stl.py /tmp/broken.stl
sutura /tmp/broken.stl --human
```

The generator produces a cube with a missing face, an inverted winding, a
duplicated face, a fin triangle and a self-intersecting triangle.

Regression suites:

```sh
# These spawn the in-repo sutura/repair.py, so run them with the CLI's Python:
~/.local/share/sutura/venv/bin/python tests/make_layered_multiobject_3mf.py --check   # layered multi-object 3MF
~/.local/share/sutura/venv/bin/python tests/test_adversarial.py                       # malformed-input handling
```

Real-world samples in `tests/real-world-samples/` come from the
[Thingi10K](https://ten-thousand-models.appspot.com/) dataset (Zhou &
Jacobson) and the Artec 3D STL catalog: 40 meshes total — mechanical,
organic, and a few genuinely broken models. They retain their original
licenses (per-model CC BY / CC0 for the Phase 2 additions; see
`docs/ATTRIBUTION.md` for the full per-file attribution) and the repair
result expected from each is in `tests/real-world-samples/README.md`.

### Benchmark corpus (115-mesh)

The large **repair benchmark corpus** (52 Artec STL scans + 63 Thingi10K
STLs, ~6 GB uncompressed — the corpus behind
`docs/repair-benchmark-strict-watertight-2026-09.md`) is **not** in the git
repo (it would multiply the repo size ~100×). It is published as a split
`.tar.gz` on the GitHub release **`benchmark-corpus-v1`**. Download and
verify it with:

```sh
scripts/fetch_benchmark_corpus.sh            # -> /tmp/sutura_corpus_100
scripts/fetch_benchmark_corpus.sh /some/dir  # custom destination
```

The script downloads the release parts, verifies their SHA-256, recombines
and extracts them, so a fresh machine (e.g. a future Linux box) can reproduce
the benchmark without re-scraping Thingi10K. See the script header and
`docs/ATTRIBUTION.md` for provenance and how to re-package/re-upload the
corpus when new meshes are added.

The benchmark harness (`scripts/benchmark_repair_corpus.py`) also carries an
**Manifold cross-validation column**: each repaired mesh's final geometry is
independently checked with `sutura_geom.watertight_check` (a Manifold constructs
with `NoError` and is non-empty ⇒ watertight) next to the array-based
`defects.detect()` strict check, and the two verdicts are compared. It never
changes the strict result or the pipeline.
`--experimental-indirect-autorefine` runs the exact Rust arrangement tier for
every mesh, and the developer hook `--cdt-experimental BITS` switches on its
evaluated-but-not-default CDT variants (see the crate README, "Sloan flips
and edge-point propagation").

Torture tests cover hard-but-printable geometry:

```sh
~/.local/share/sutura/venv/bin/python tests/torture_tests.py
```

Rust core (`rust/sutura-geom`, needs a Rust toolchain; results in the crate
README):

```sh
cd rust/sutura-geom
cargo test --release                      # unit + filter-vs-exact differential tests
cargo run --release --example bench_arrangement --features profile -- mesh.obj
```

The Python smoke tests for the extension (`tests/test_sutura_geom*.py`) need
it built into the active environment with `maturin develop`. For automated
installation without manual compilation, `scripts/install_sutura_geom.sh` downloads
and verifies matching prebuilt release wheels.

### Developer tooling — synthetic defect injection

`scripts/defect_injector.py` is a developer tool that corrupts a mesh on
purpose: it injects selected defect types (`--hole`, `--non-manifold`,
`--self-intersect`, `--flipped-normal`, `--degenerate`, any combination;
`--count N`, `--seed N`) and writes a broken copy to a new file — the input
is never modified. It is used to generate synthetic broken meshes for the
repair pipeline and the defect detectors, and can extend future synthetic
training/test sets. Each injected type is validated by
`tests/test_defect_injector.py` against the corresponding detector
(`defects.detect()` for holes/non-manifold, `defect_type_colors()` for
flipped/degenerate, the native self-intersection classifier for self-intersect).

This runs five scenarios and reports the before/after for each: a 5M-triangle
sphere (repair time), a 0.05 mm thin slab (feature-loss risk — it must survive
intact), a multi-part assembly (the 8-face debris-removal threshold must not
delete legitimate parts), a rough scan-style mesh with many micro-cracks
(residual-holes expectation), and two interpenetrating spheres run in `--mode
extreme`, where the extra self-intersection passes must remove every crossing
face and report `extreme_passes_applied=True`.
