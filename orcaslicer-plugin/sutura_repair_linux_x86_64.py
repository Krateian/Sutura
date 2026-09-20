# /// script
# requires-python = ">=3.12"
# dependencies = []
#
# [tool.orcaslicer.plugin]
# name = "Sutura Repair"
# description = "EXPERIMENTAL: repairs the SELECTED model in-memory via orca.host, runs the Sutura pipeline and loads the result back."
# author = "Krateian"
# version = "0.2.0"
# ///
"""Sutura Repair — OrcaSlicer script plugin (v0.3.1 revision).

Repairs the currently SELECTED model: the mesh is read in memory through the
orca.host API (`model() -> objects() -> volumes() -> mesh().vertices()/
triangles()`), written as a unique temporary STL under `data_dir()` (the
audit-hook allowed root), repaired with the Sutura pipeline, and the repaired
result is loaded back into the slicer via `--single-instance <path>`.

Repair execution is attempted IN-PROCESS first (importing the installed sutura
modules directly), falling back to the separately-installed Sutura CLI
(`~/.local/bin/sutura`) when the embedded Python cannot run the pipeline
(e.g. pymeshlab is not importable there).

Primary target is LINUX; the same file also runs on macOS as a bonus
real-device verification layer (the CLI lives at the same ~/.local/bin/sutura
path on both platforms — install.sh / install-macos.sh).

EXPERIMENTAL — the OrcaSlicer plugin system exists only in nightly builds /
releases NEWER than 2.4.2; stable 2.4.2 has no "Plugins" menu. Verified
against the documented API and stub tests; a real-instance GUI test is done
separately (user / computer-use).

Output naming: every run writes a UNIQUE repaired file
(`<stem>_fixed_<timestamp>_<short-uuid>.<ext>`) so consecutive runs never
overwrite a previous result.
"""

import os
import subprocess
import threading
import time
import uuid

import numpy as np

import orca

# The Sutura CLI. Same path on Linux and macOS (install.sh / install-macos.sh
# both create ~/.local/bin/sutura). Overridable via SUTURA_CLI.
SUTURA_CLI = os.environ.get('SUTURA_CLI', os.path.expanduser('~/.local/bin/sutura'))

# OrcaSlicer binary used for --single-instance (Linux). On macOS the native
# `open -a OrcaSlicer <file>` is used instead. Overridable via ORCA_BIN.
ORCA_BIN = os.environ.get('ORCA_BIN', 'orca-slicer')


# --------------------------------------------------------------------------- helpers

def _write_binary_stl(path, verts, tris):
    """Write a binary STL from numpy arrays (no pymeshlab needed)."""
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    n = len(t)
    a = v[t[:, 0]]; b = v[t[:, 1]]; c = v[t[:, 2]]
    cr = np.cross(b - a, c - a)
    mag = np.linalg.norm(cr, axis=1, keepdims=True)
    mag[mag == 0] = 1.0
    normals = (cr / mag).astype(np.float32)
    with open(path, 'wb') as f:
        f.write(b'sutura repair staging'.ljust(80, b'\0'))
        f.write(np.uint32(n).tobytes())
        for i in range(n):
            f.write(normals[i].tobytes())
            f.write(v[t[i][0]].tobytes())
            f.write(v[t[i][1]].tobytes())
            f.write(v[t[i][2]].tobytes())
            f.write(np.uint16(0).tobytes())


def _unique_output_name(stem, ext='stl'):
    """A UNIQUE repaired-output filename (never a fixed name, so consecutive
    runs never overwrite each other): <stem>_fixed_<timestamp>_<uuid>.<ext>."""
    ts = time.strftime('%Y%m%d-%H%M%S')
    return '%s_fixed_%s_%s.%s' % (stem, ts, uuid.uuid4().hex[:8], ext)


def _temp_dir():
    """The audit-hook allowed root: orca.host.data_dir(), else a fallback."""
    try:
        return orca.host.data_dir()
    except Exception:
        return os.path.expanduser('~/.local/share/sutura')


def _model_to_mesh(model):
    """Read the model's first readable volume as (name, verts, tris).

    orca.host.model() -> objects() -> volumes() -> mesh().vertices()/
    triangles(). Returns None when the model has no readable mesh."""
    try:
        objects = model.objects()
        if not objects:
            return None
        obj = objects[0]
        volumes = obj.volumes()
        if not volumes:
            return None
        vol = volumes[0]
        mesh = vol.mesh()
        verts = np.asarray(mesh.vertices(), dtype=np.float32)
        tris = np.asarray(mesh.triangles(), dtype=np.int32)
        if len(verts) == 0 or len(tris) == 0:
            return None
        name = 'model'
        try:
            if getattr(obj, 'name', None):
                name = str(obj.name)
        except Exception:
            pass
        # keep only a safe stem (no path separators in the output filename)
        name = ''.join(ch for ch in name if ch.isalnum() or ch in '._- ').strip() or 'model'
        return name, verts, tris
    except Exception:
        return None


def _write_temp_stl(verts, tris):
    """Write a unique transient STL under data_dir()/sutura_repair/<uuid>.stl.
    Returns the path (or raises)."""
    d = os.path.join(_temp_dir(), 'sutura_repair')
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, uuid.uuid4().hex + '.stl')
    _write_binary_stl(path, verts, tris)
    return path


# --------------------------------------------------------------------------- repair paths

def _repair_in_process(verts, tris, workdir, out_path):
    """Try to repair with the installed sutura modules IN-PROCESS.

    Returns (True, '') on success, (False, reason) on any failure. Needs
    numpy + pymeshlab (+ manifold3d for stage 2) in the embedded Python;
    when they are missing this falls back to the subprocess path."""
    try:
        app_dir = os.path.expanduser('~/.local/share/sutura')
        if app_dir not in os.sys.path:
            os.sys.path.insert(0, app_dir)
        import repair as _repair
        report, new_v, new_t = _repair.repair_mesh_from_arrays(
            verts, tris, workdir, mode='auto')
        new_v, new_t = _repair.maybe_run_stage2(report, new_v, new_t, workdir)
        _repair.save_mesh(out_path, new_v, new_t)
        return True, ''
    except Exception as exc:  # noqa: BLE001 - any failure falls back
        return False, 'in-process unavailable (%s)' % exc


def _run_subprocess(src, out_path):
    """Run the separately-installed Sutura CLI on the staged file.

    Returns (True, '') on success, (False, message) on failure. Note: each
    subprocess spawn may trigger an OrcaSlicer audit-hook permission prompt."""
    try:
        proc = subprocess.run(
            [SUTURA_CLI, src, '--mode', 'auto', '-o', out_path],
            capture_output=True, text=True, timeout=300)
        if proc.returncode == 0 and os.path.exists(out_path):
            return True, ''
        return False, ((proc.stdout or '') + (proc.stderr or '')).strip() or 'exit %d' % proc.returncode
    except subprocess.TimeoutExpired:
        return False, 'repair timed out'
    except FileNotFoundError:
        return False, 'Sutura CLI not found at %s (install with install.sh)' % SUTURA_CLI


def _load_back(out_path):
    """Best-effort: reload the repaired file into the slicer.

    Linux: OrcaSlicer --single-instance <path> (proven method). macOS: the
    native `open -a OrcaSlicer <path>`. Never crashes the worker."""
    try:
        if os.sys.platform == 'darwin':
            subprocess.Popen(['open', '-a', 'OrcaSlicer', out_path])
        else:
            subprocess.Popen([ORCA_BIN, '--single-instance', out_path])
    except Exception:
        pass


# --------------------------------------------------------------------------- plugin

class SuturaRepair(orca.script.ScriptPluginCapabilityBase):
    def get_name(self):
        return "Repair mesh with Sutura"

    def _worker(self, data):
        """Repair off the UI thread and report via the host UI."""
        name, verts, tris = data
        tmp = None
        try:
            tmp = _write_temp_stl(verts, tris)
            workdir = os.path.dirname(tmp)
            out_path = os.path.join(workdir, _unique_output_name(name))

            ok, msg = _repair_in_process(verts, tris, workdir, out_path)
            if not ok:
                ok, msg = _run_subprocess(tmp, out_path)
            if not ok:
                orca.host.ui.message(
                    "Sutura Repair failed:\n%s" % msg,
                    title="Sutura Repair", buttons="ok", icon="error")
                return

            _load_back(out_path)
            orca.host.ui.message(
                "Sutura repaired the selected model.\n\nRepaired file:\n%s" % out_path,
                title="Sutura Repair", buttons="ok", icon="info")
        except Exception as exc:  # noqa: BLE001 - never crash the worker silently
            orca.host.ui.message(
                "Sutura Repair error:\n%s" % exc,
                title="Sutura Repair", buttons="ok", icon="error")
        finally:
            if tmp and os.path.exists(tmp):
                try:
                    os.remove(tmp)
                except OSError:
                    pass

    def execute(self):
        # execute() runs on the UI thread; return immediately, work in a
        # daemon thread that reports back via orca.host.ui.
        try:
            model = orca.host.model()
            if model is None:
                return orca.ExecutionResult.failure(
                    "Sutura Repair: no model in the scene to repair.")
            data = _model_to_mesh(model)
            if data is None:
                return orca.ExecutionResult.failure(
                    "Sutura Repair: the model has no readable mesh volume.")
            threading.Thread(target=self._worker, args=(data,), daemon=True).start()
            return orca.ExecutionResult.success("Sutura Repair started.")
        except Exception as exc:  # noqa: BLE001
            return orca.ExecutionResult.failure("Sutura Repair: %s" % exc)


@orca.plugin
class SuturaRepairPlugin(orca.base):
    def register_capabilities(self):
        orca.register_capability(SuturaRepair)