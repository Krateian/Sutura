# Third-party notices for `sutura_geom`

## Geogram Delaunay PSM

The `sutura_geom` extension vendors the standalone **Geogram Delaunay PSM**
(`third_party/geogram_delaunay_psm/Delaunay_psm.cpp` and `.h`), the sequential
`GEO::Delaunay3d` (BDEL) tetrahedralization used by the Splint core.

- Upstream: Geogram, <https://github.com/BrunoLevy/geogram>
- Origin: the standalone PSM generated from the Geogram distribution with
  `tools/extract_psm.sh`; only the Delaunay PSM is vendored, not the full
  Geogram library.
- License: **BSD 3-Clause**, Copyright (c) 2000-2022 Inria. The full text is in
  `third_party/geogram_delaunay_psm/LICENSE`, which is also shipped in the wheel.
- Vendored files (SHA-256):

  ```
  Delaunay_psm.cpp  1294fbc62fb344febe93d805003d80a2805a297a02e952b993f1a6c9fab35980
  Delaunay_psm.h    b6dc63e67e346234b7ad290c22b2297445f313f9db6c2c1a0606e99085f1f028
  LICENSE           02e62718d092ae4c49ed0a09cc8cac710161d099ee612822ecd1ac01f552681d
  ```

- Provenance: the vendoring spike (`p2/splint-plan`, commit `d043f95`) did not
  record the exact upstream tag. The file hashes above identify the vendored
  source; pin the upstream tag/commit here before promoting the Splint core out
  of the test-hook stage.

The Geogram notice is separate from the crate's `MIT OR Apache-2.0` license and
from Sutura's PolyForm Noncommercial application license.

## meshopt / meshoptimizer

The `sutura_geom` extension depends on the **`meshopt`** crate
(`meshopt = "0.7"`), which builds the vendored **meshoptimizer** C++ core
(`vendor/src/*.cpp`, compiled by the crate's `build.rs`) and exposes it as a
Rust API.  The native quadric decimation pass (`decimate.rs`, Faz 4 W3b) uses
`meshopt::simplify` / `simplify_with_attributes_and_locks`.

- Upstream C++: meshoptimizer, <https://github.com/zeux/meshoptimizer>
- Upstream Rust: the `meshopt` crate, <https://crates.io/crates/meshopt>
- Versions: `meshopt` crate **0.7.0** (crates.io checksum
  `a403c16e3ca5752600e1d4c5ba3ba2a6496f407ded66e767f05279514363f461`), which
  vendors meshoptimizer **1.3** (`#define MESHOPTIMIZER_VERSION 1030`
  in the vendored `vendor/src/meshoptimizer.h`).
- License: **MIT** — both the Rust crate (`MIT OR Apache-2.0`) and the vendored
  meshoptimizer C++ (`MIT`, Copyright (c) 2016-2026 Arseny Kapoulkine).  The
  full MIT text ships as `third_party/meshoptimizer/LICENSE`.
- Provenance: the C++ is compiled from the crate's own `vendor/` directory at
  build time (no network); the vendored `vendor/src/simplifier.cpp`
  SHA-256 is `719303b6970398631e30724e8eebc6cd2e7e08b67fe8ab7cbf7eecafdaa08616`.
- The crate is a build-time dependency only; a `cargo build` needs a C++11
  compiler (the Geogram build already requires C++20).

