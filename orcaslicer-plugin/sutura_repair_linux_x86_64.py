# /// script
# requires-python = ">=3.12"
# dependencies = []
#
# [tool.orcaslicer.plugin]
# name = "Sutura Repair"
# description = "EXPERIMENTAL: repairs the SELECTED model in-memory via orca.host, runs the Sutura pipeline and loads the result back."
# author = "Krateian"
# version = "0.2.1"
# ///
"""Sutura Repair — OrcaSlicer script plugin (v0.3.1 revision).

Repairs the currently SELECTED model: the mesh is read through the orca.host
API (`model() -> objects() -> volumes() -> mesh()`), using the numpy-free
`vertex(i)/triangle(i)` accessors (the embedded interpreter has no numpy),
written as a unique temporary STL under `data_dir()` (the audit-hook allowed
root), repaired with the Sutura pipeline, and the repaired result is loaded
back into the slicer via `--single-instance <path>`.

PURE-STDLIB by design (v0.2.1): OrcaSlicer's embedded Python 3.12 ships only
`pip` in site-packages — there is NO numpy. The plugin therefore never imports
numpy; all mesh handling uses plain Python lists and `struct`. Repair itself
always runs in the separately-installed Sutura CLI (`~/.local/bin/sutura`,
which needs numpy/pymeshlab in ITS environment) — the CLI path is the same on
Linux and macOS (`~/.local/bin/sutura`, install.sh / install-macos.sh).

Primary target is LINUX; the same file also runs on macOS as a bonus
real-device verification layer.

EXPERIMENTAL — the OrcaSlicer plugin system exists only in nightly builds /
releases NEWER than 2.4.2; stable 2.4.2 has no "Plugins" menu.

Output naming: every run writes a UNIQUE repaired file
(`<stem>_fixed_<timestamp>_<short-uuid>.<ext>`) so consecutive runs never
overwrite a previous result.
"""

import os
import struct
import subprocess
import threading
import time
import uuid

import orca

# The Sutura CLI. Same path on Linux and macOS. Overridable via SUTURA_CLI.
SUTURA_CLI = os.environ.get('SUTURA_CLI', os.path.expanduser('~/.local/bin/sutura'))

# OrcaSlicer binary used for --single-instance (Linux). On macOS the native
# `open -a OrcaSlicer <file>` is used instead. Overridable via ORCA_BIN.
ORCA_BIN = os.environ.get('ORCA_BIN', 'orca-slicer')


# --------------------------------------------------------------------------- helpers

def _to_float3(value):
    """Normalise a vertex to [float, float, float] from a list/tuple/np array."""
    return [float(value[0]), float(value[1]), float(value[2])]


def _to_int3(value):
    """Normalise a triangle to [int, int, int] from a list/tuple/np array."""
    return [int(value[0]), int(value[1]), int(value[2])]


def _cross(a, b):
    """3D cross product of two 3-vectors (pure Python)."""
    return [a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0]]


def _write_binary_stl(path, verts, tris):
    """Write a binary STL from plain sequences (list/tuple/np array).

    Pure stdlib (struct) — no numpy, because OrcaSlicer's embedded Python has
    none. Accepts any iterable of (x, y, z) vertices and (a, b, c) triangles;
    face normals are computed from the geometry."""
    v = [_to_float3(x) for x in verts]
    t = [_to_int3(x) for x in tris]
    with open(path, 'wb') as f:
        f.write(b'sutura repair staging'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(t)))
        for tri in t:
            a, b, c = v[tri[0]], v[tri[1]], v[tri[2]]
            n = _cross([b[i] - a[i] for i in range(3)],
                       [c[i] - a[i] for i in range(3)])
            ln = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]) ** 0.5
            if ln > 0:
                n = [x / ln for x in n]
            f.write(struct.pack('<3f', n[0], n[1], n[2]))
            for p in (a, b, c):
                f.write(struct.pack('<3f', p[0], p[1], p[2]))
            f.write(struct.pack('<H', 0))


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

    orca.host.model() -> objects() -> volumes() -> mesh(). The numpy-free
    accessors `vertex_count()/triangle_count()/vertex(i)/triangle(i)` are used
    (`vertices()`/`triangles()` require numpy, which the embedded interpreter
    does NOT have). verts/tris are plain Python lists. Returns None only when
    the model has no readable mesh; any real API/geometry exception PROPAGATES
    so the caller (execute) surfaces the actual error instead of hiding it.
    """
    objects = model.objects()
    if not objects:
        return None
    obj = objects[0]
    volumes = obj.volumes()
    if not volumes:
        return None
    vol = volumes[0]
    mesh = vol.mesh()
    nv = mesh.vertex_count()
    nt = mesh.triangle_count()
    if nv == 0 or nt == 0:
        return None
    verts = [_to_float3(mesh.vertex(i)) for i in range(nv)]
    tris = [_to_int3(mesh.triangle(i)) for i in range(nt)]
    name = 'model'
    try:
        if getattr(obj, 'name', None):
            name = str(obj.name)
    except Exception:
        pass
    # keep only a safe stem (no path separators in the output filename)
    name = ''.join(ch for ch in name if ch.isalnum() or ch in '._- ').strip() or 'model'
    return name, verts, tris


def _write_temp_stl(verts, tris):
    """Write a unique transient STL under data_dir()/sutura_repair/<uuid>.stl.
    Returns the path (or raises)."""
    d = os.path.join(_temp_dir(), 'sutura_repair')
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, uuid.uuid4().hex + '.stl')
    _write_binary_stl(path, verts, tris)
    return path


# --------------------------------------------------------------------------- repair

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
                    orca.PluginResult.RecoverableError,
                    "Sutura Repair: no model in the scene to repair.")
            data = _model_to_mesh(model)
            if data is None:
                return orca.ExecutionResult.failure(
                    orca.PluginResult.RecoverableError,
                    "Sutura Repair: the model has no readable mesh volume.")
            threading.Thread(target=self._worker, args=(data,), daemon=True).start()
            return orca.ExecutionResult.success("Sutura Repair started.")
        except Exception as exc:  # noqa: BLE001
            # Surface the FULL root cause (type + message + traceback) so a
            # real-instance test can see what actually failed.
            import traceback
            return orca.ExecutionResult.failure(
                orca.PluginResult.RecoverableError,
                "Sutura Repair: %s\n\n%s" % (exc, traceback.format_exc()))


@orca.plugin
class SuturaRepairPlugin(orca.base):
    def register_capabilities(self):
        orca.register_capability(SuturaRepair)