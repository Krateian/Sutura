# fTetWild Installation & Removal Manager Backend

The `sutura.ftetwild_manager` module provides a standard-library-only backend for inspecting, installing, estimating, and cleanly removing the optional fTetWild fallback tier (`pytetwild`, `pyvista`, `vtk`) without ever touching the system Python.

## Supported Environments

To maintain strict stability and isolate heavy dependencies, the manager operates **only** on Sutura's two designated environments:
1. **Linux**: `$APP_DIR/venv311/bin/pip` (where `$APP_DIR` is `$SUTURA_DIR` or `~/.local/share/sutura`), as created by `install.sh`.
2. **macOS**: The conda `sutura-env` environment (`sutura-env/bin/pip`), as created by `install-macos.sh`.

### Unsupported Environments
Any other execution context is detected and rejected with a clear instruction:
- **AppImage**: Read-only squashfs runtime. Returns `unsupported here, use install.sh SUTURA_WITH_FTETWILD=1`.
- **macOS .app / .dmg bundle**: Frozen PyInstaller / LaunchServices bundle. Returns `unsupported here, use install-macos.sh SUTURA_WITH_FTETWILD=1`.
- **Unmanaged virtualenv / system Python**: Refused to prevent polluting ambient environments.
- **Read-only file systems**: Returns an explicit read-only environment error.

## Measured Platform Sizes

Size estimates are hardcoded based on real measurements:

| Metric | macOS (Apple Silicon arm64) | Linux (x86_64) |
|---|---|---|
| **Download Size** | **~105 MB** (`vtk` 98.2M, `pyvista` 2.6M, `pytetwild` 2.3M, deps ~2M) | **~139 MB** (`vtk` 133M, `pytetwild` 3.0M, `pyvista` 2.6M, deps ~2M) |
| **Installed Disk Size** | **~538 MB** (`vtkmodules` 517.1M, `pyvista` 13.5M, `pytetwild` 5.9M) | **~580 MB** (wheel content; up to **~1.1 GB** with caches/distro deps) |

- **Download measurement**: Linux sizes were measured using `pip download --no-deps --platform manylinux2014_x86_64`.
- **Installed measurement**: macOS sizes were measured directly in `sutura-env` on disk via `importlib.metadata` and file stats.

## Dependency Pruning & Safe Uninstall

The manager dynamically resolves package dependencies from `requirements-ftetwild.txt` and base requirement files (`requirements.txt`, `requirements-gui.txt`, `requirements-311.txt`):

1. **Base Protection**: Packages required by base Sutura (e.g. `numpy`, `manifold3d`, `trimesh`, `pymeshlab`, `PySide6-Essentials`) are strictly protected and never removed.
2. **Heavy Dependency Cleanup**: Packages pulled in solely for fTetWild (notably `vtk`, `pyvista`, and `pyvista-validation`) are tracked and uninstalled, reclaiming ~517 MB from VTK alone.
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
#     'size_bytes': 564133888,
#     'size_human': '538.0 MB',
#     'layout': 'macos-conda',
#     'reason': None,
# }

# 2. Get download and disk estimates
est = mgr.estimate()
# {'download_human': '105.0 MB', 'installed_human': '538.0 MB', ...}

# 3. Dry-run install / uninstall
plan_install = mgr.install(dry_run=True)
plan_uninstall = mgr.uninstall(dry_run=True)

# 4. Live streaming execution with cancellation
cancel_event = threading.Event()
res = mgr.install(progress_cb=print, cancel_event=cancel_event)
res = mgr.uninstall(progress_cb=print, cancel_event=cancel_event)
```
