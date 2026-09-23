# sutura-geom

Foundation crate for Sutura's future indirect-predicates geometry core.

**Phase A scope (this crate, currently):** a thin PyO3 wrapper exposing the
explicit-point robust predicates `orient3d` and `insphere` from the `robust`
crate (Shewchuk adaptive-precision floating-point arithmetic) to Python as the
module `sutura_geom`.

Indirect/implicit predicates, expansion arithmetic, 2D CDT, triangle-triangle
intersection and any repair-pipeline integration are **out of scope for Phase A**
and will be layered on top of this crate in later phases (see
`docs/alpha-wrap-feasibility-2026-09.md` and the Phase A plan in
`/tmp/rust-core-research-plan.md`).

## Dependencies and licenses

All dependencies are permissive (MIT / Apache-2.0 / BSD) and verified against
`cargo tree` before shipping. Any dependency resolving to GPL/LGPL/AGPL is
rejected.

| Crate | Version | License |
|---|---|---|
| `robust` | `=1.2.0` | MIT OR Apache-2.0 |
| `pyo3` | `=0.29.2` | MIT OR Apache-2.0 |
| `numpy` (rust-numpy) | `=0.29.0` | BSD-2-Clause |

`maturin` (currently `==1.15.0`) is the build tool; it is **not** a Cargo
dependency. Install it into the target venv:

```bash
<venv>/bin/pip install "maturin==1.15.0"
```

## Building and wiring into the venv

```bash
cd rust/sutura-geom
cargo build                 # plain build (rlib)
cargo test                  # Rust unit tests
maturin develop --interpreter <venv>/bin/python
```

`maturin develop` installs the compiled `sutura_geom` extension module into the
given venv (the `extension-module` feature is applied automatically via
`pyproject.toml`). The extension is built with `abi3-py311`, so it runs on
Python 3.11+ without per-version rebuilds.

## Python API

```python
import sutura_geom

# Each point is a length-3 sequence (tuple, list, or numpy 1-D array).
sutura_geom.orient3d((0,0,0), (1,0,0), (0,1,0), (0,0,1))   # > 0
sutura_geom.orient3d((0,0,0), (1,0,0), (0,1,0), (0,0,-1))  # < 0
sutura_geom.orient3d((0,0,0), (1,0,0), (0,1,0), (0,0,0))   # == 0

sutura_geom.insphere((0,0,0), (1,0,0), (0,1,0), (0,0,1), (0.25,0.25,0.25))  # > 0
```

Smoke test: `tests/test_sutura_geom.py` (run with the venv python that has the
extension installed).