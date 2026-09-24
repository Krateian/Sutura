# /// script
# requires-python = ">=3.12"
# dependencies = []
#
# [tool.orcaslicer.plugin]
# name = "Sutura Repair"
# description = "EXPERIMENTAL: repairs the SELECTED model in-memory via orca.host, runs the Sutura pipeline and loads the result back."
# author = "Krateian"
# version = "0.4.0"
# ///
"""Sutura Repair — OrcaSlicer script plugin.

Repairs the currently SELECTED model: the mesh is read through the orca.host
API (`model() -> objects() -> volumes() -> mesh()`), using the numpy-free
`vertex(i)/triangle(i)` accessors (the embedded interpreter has no numpy),
written as a unique temporary STL under `data_dir()` (the audit-hook allowed
root), repaired with the Sutura pipeline, and the repaired result is loaded
back into the slicer via `--single-instance <path>`.

PURE-STDLIB by design: OrcaSlicer's embedded Python 3.12 ships only
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


def _running_orca_processes():
    """Number of distinct OrcaSlicer processes running (pgrep -f). Returns
    None when the check itself fails (treated as 'cannot verify' -> a reopen
    is still attempted)."""
    try:
        r = subprocess.run(['pgrep', '-f', 'OrcaSlicer'],
                           capture_output=True, text=True)
        pids = [l.strip() for l in (r.stdout or '').splitlines() if l.strip()]
        return len(pids)
    except Exception:
        return None


def _load_back(out_path):
    """Best-effort: reload the repaired file into the slicer.

    Linux: OrcaSlicer --single-instance <path> (path-based binary, no name
    ambiguity). macOS: `open -b com.orcaslicer.OrcaSlicer <path>` -- BUNDLE-ID
    matching prefers the running process, unlike name-based `open -a`, which
    can launch an unrelated copy when several apps share the name (e.g. an
    installed "OrcaSlicer 2.app" plus a mounted nightly DMG). Before reopening,
    pgrep counts running OrcaSlicer processes: if MORE THAN ONE is running the
    reopen is SKIPPED (ambiguous) and the caller still shows the repaired path
    in its message. Never crashes the worker."""
    try:
        if os.sys.platform == 'darwin':
            n = _running_orca_processes()
            if n is not None and n > 1:
                return  # ambiguous -- skip auto-reopen, path already reported
            subprocess.Popen(['open', '-b', 'com.orcaslicer.OrcaSlicer', out_path])
        else:
            subprocess.Popen([ORCA_BIN, '--single-instance', out_path])
    except Exception:
        pass


# --------------------------------------------------------------------------- plugin

class SuturaRepair(orca.script.ScriptPluginCapabilityBase):
    def get_name(self):
        return "Repair mesh with Sutura"

    def _worker(self, data, result):
        """Repair off the UI thread; store the outcome in `result` (plain dict).

        Deliberately performs NO host UI calls: `execute()` owns the progress
        dialog and the result message on the UI thread, so the worker only
        touches files and subprocesses. The dict keys written are `ok`, `msg`
        (failure reason) and `out_path` (successful repaired file)."""
        name, verts, tris = data
        tmp = None
        try:
            tmp = _write_temp_stl(verts, tris)
            workdir = os.path.dirname(tmp)
            out_path = os.path.join(workdir, _unique_output_name(name))

            ok, msg = _run_subprocess(tmp, out_path)
            if not ok:
                result['ok'] = False
                result['msg'] = msg
                return

            _load_back(out_path)
            result['ok'] = True
            result['out_path'] = out_path
        except Exception as exc:  # noqa: BLE001 - never crash the worker silently
            result['ok'] = False
            result['msg'] = str(exc)
        finally:
            if tmp and os.path.exists(tmp):
                try:
                    os.remove(tmp)
                except OSError:
                    pass

    def _report(self, result):
        """Show the outcome recorded by a finished _worker via the host UI."""
        if not result.get('ok'):
            orca.host.ui.message(
                "Sutura Repair failed:\n%s" % (result.get('msg') or 'unknown error'),
                title="Sutura Repair", buttons="ok", icon="error")
            return
        orca.host.ui.message(
            "Sutura repaired the selected model.\n\nRepaired file:\n%s" % result.get('out_path'),
            title="Sutura Repair", buttons="ok", icon="info")

    def execute(self):
        # execute() runs on the UI thread. A native progress dialog (pulsed
        # from here while the worker runs) shows the repair in progress; on
        # builds without the progress-dialog API it falls back to the old
        # background behaviour ("started" + silent worker).
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
        except Exception as exc:  # noqa: BLE001
            # Surface the FULL root cause (type + message + traceback) so a
            # real-instance test can see what actually failed.
            import traceback
            return orca.ExecutionResult.failure(
                orca.PluginResult.RecoverableError,
                "Sutura Repair: %s\n\n%s" % (exc, traceback.format_exc()))

        result = {}
        try:
            with orca.host.ui.create_progress_dialog(
                    "Sutura Repair", "Repairing the selected model...",
                    style=orca.host.ui.PD_APP_MODAL | orca.host.ui.PD_AUTO_HIDE) as progress:
                thread = threading.Thread(target=self._worker, args=(data, result), daemon=True)
                thread.start()
                while thread.is_alive():
                    progress.pulse("Repairing the selected model with Sutura...")
                    time.sleep(0.1)
        except Exception:  # noqa: BLE001 - no progress-dialog API -> old background path
            threading.Thread(target=self._worker, args=(data, result), daemon=True).start()
            threading.Thread(target=self._report, args=(result,), daemon=True).start()
            return orca.ExecutionResult.success("Sutura Repair started.")

        self._report(result)
        if not result.get('ok'):
            return orca.ExecutionResult.failure(
                orca.PluginResult.RecoverableError,
                "Sutura Repair failed: %s" % (result.get('msg') or 'unknown error'))
        return orca.ExecutionResult.success("Sutura Repair finished.")


@orca.plugin
class SuturaRepairPlugin(orca.base):
    def register_capabilities(self):
        # Declare the filesystem READ path up front: the external Sutura CLI
        # binary the plugin spawns. HONEST SCOPE: orca.request_permissions only
        # accepts declarative fs_read paths; the subprocess (ProcessCreate)
        # spawn is audited reactively and its persisted grant is keyed to the
        # exact command line (which contains unique temp paths), so this does
        # NOT remove the per-run subprocess permission prompt.
        orca.request_permissions(fs_read=[SUTURA_CLI])
        orca.register_capability(SuturaRepair)