# fTetWild Installation & Removal Manager Backend

The `sutura.ftetwild_manager` module provides a standard-library-only backend for inspecting, installing, estimating, and cleanly removing the fTetWild fallback tier (`pytetwild` only — PyVista and VTK are not used) without ever touching the system Python.

## Supported Environments

To maintain strict stability and isolate heavy dependencies, the manager operates **only** on Sutura's own environments:
1. **Linux**: `$APP_DIR/venv/bin/pip` (where `$APP_DIR` is `$SUTURA_DIR` or `~/.local/share/sutura`), as created by `install.sh`. When the main interpreter is outside 3.10–3.14 the tier uses the `venv311` fallback instead. Stage 2 no longer uses these environments: Manifold runs in-process in the mandatory `sutura_geom` Rust extension, so `venv311` exists only as the fTetWild fallback interpreter.
2. **macOS**: The conda `sutura-env` environment (`sutura-env/bin/pip`), as created by `install-macos.sh`.

### Unsupported Environments
Any other execution context is detected and rejected with a clear instruction:
- **AppImage**: Read-only squashfs runtime, bundled with the tier. Returns `unsupported here, use install.sh SUTURA_WITH_FTETWILD=0`.
- **macOS .app / .dmg bundle**: Frozen PyInstaller / LaunchServices bundle, bundled with the tier. Returns `unsupported here, use install-macos.sh SUTURA_WITH_FTETWILD=0`.
- **Unmanaged virtualenv / system Python**: Refused to prevent polluting ambient environments.
- **Read-only file systems**: Returns an explicit read-only environment error.

## Measured Platform Sizes

Size estimates are hardcoded based on real measurements (pytetwild wheel only):

| Metric | macOS (Apple Silicon arm64) | Linux (x86_64) | Linux (aarch64) |
|---|---|---|---|
| **Download Size** | **~2.3 MB** (`pytetwild` 2.3M) | **~3.0 MB** (`pytetwild` 3.0M) | **~2.7 MB** (`pytetwild` 2.7M) |
| **Installed Disk Size** | **~6.0 MB** | **~6.0 MB** | **~6.0 MB** |

- **Download measurement**: Linux sizes were measured using `pip download --no-deps --platform manylinux2014_x86_64` (and `manylinux2014_aarch64`).
- **Installed measurement**: macOS sizes were measured directly in `sutura-env` on disk via `importlib.metadata` and file stats.

## Dependency Pruning & Safe Uninstall

The manager dynamically resolves package dependencies from `requirements-ftetwild.txt` and base requirement files (`requirements.txt`, `requirements-gui.txt`):

1. **Base Protection**: Packages required by base Sutura (e.g. `numpy`, `PySide6-Essentials`, `pip`, `setuptools`, `wheel`) are strictly protected and never removed. Manifold is not a Python package: it is statically linked into `sutura_geom` through `manifold-csg`, so it does not appear in the base package set; `trimesh` and `pymeshlab` are development/test-only dependencies, so they are not protected either.
2. **Legacy Cleanup**: Packages from older installs that pulled in the former PyVista/VTK stack (`pyvista`, `vtk`, `pyvista-validation`) are still tracked and removed on uninstall.
3. **Cancellation & Half-Install Cleanup**: Pip runs in a detached process group (`start_new_session=True`). When cancelled via `cancel_event`, the entire process tree is terminated (`SIGTERM` -> `SIGKILL`), and any partially extracted fTetWild packages are cleaned up automatically.

## Python API

```python
import ftetwild_manager as mgr

# 1. Check status
st = mgr.status()
# {
#     'installed': True,
#     'supported': True,
#     'version': '0.4.2',
#     'location': '/.../site-packages',
#     'size_bytes': 6291456,
#     'size_human': '6.0 MB',
#     'layout': 'macos-conda',
#     'reason': None,
# }

# 2. Get download and disk estimates
est = mgr.estimate()
# {'download_human': '2.3 MB', 'installed_human': '6.0 MB', ...}

# 3. Dry-run install / uninstall
plan_install = mgr.install(dry_run=True)
plan_uninstall = mgr.uninstall(dry_run=True)

# 4. Live streaming execution with cancellation
cancel_event = threading.Event()
res = mgr.install(progress_cb=print, cancel_event=cancel_event)
res = mgr.uninstall(progress_cb=print, cancel_event=cancel_event)
```
