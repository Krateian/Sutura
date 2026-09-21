#!/usr/bin/env python3
"""Stub test for the OrcaSlicer plugin (orcaslicer-plugin/
sutura_repair_linux_x86_64.py) against a MOCK orca.host.

The plugin cannot run inside a real OrcaSlicer here, so it is loaded with a
fake `orca` module and its pure helpers are exercised:
  * _model_to_mesh      reads (name, verts, tris) from the mock model API
  * _write_temp_stl     writes a valid binary STL under the mocked data_dir
  * _unique_output_name produces a DIFFERENT name for two consecutive calls
  * execute()           fails on no-model, starts a worker thread on a model

Needs numpy only (the plugin file does not import pymeshlab at module level).
Usage: ~/.local/share/sutura/venv/bin/python tests/test_orca_plugin.py
"""
import importlib.util
import os
import struct
import sys
import tempfile
import time

import numpy as np

PLUGIN = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      'orcaslicer-plugin', 'sutura_repair_linux_x86_64.py')


class _Result:
    def __init__(self, ok, msg):
        self.ok = ok
        self.msg = msg

    def __repr__(self):
        return 'Result(%r, %r)' % (self.ok, self.msg)


class _ExecutionResult:
    @staticmethod
    def success(message='', data=''):
        return _Result(True, message)

    @staticmethod
    def skipped(message=''):
        return _Result(None, message)

    @staticmethod
    def failure(status, message='', data=''):
        return _Result(False, message)


import enum  # noqa: E402


class _PluginResult(enum.Enum):
    Success = 0
    Skipped = 1
    RecoverableError = 2
    FatalError = 3


class _ScriptPluginCapabilityBase:
    pass


class _Base:
    pass


class _Host:
    def __init__(self, model, data_dir, ui):
        self._model = model
        self._data_dir = data_dir
        self.ui = ui

    def model(self):
        return self._model

    def data_dir(self):
        return self._data_dir


class _Progress:
    """Mock for orca.host.ui.create_progress_dialog()'s handle (context
    manager with pulse/update/close/is_open, matching the wiki API)."""

    def __init__(self):
        self.closed = False
        self.pulses = 0

    def pulse(self, message=''):
        self.pulses += 1
        return True

    def update(self, value, message=''):
        self.pulses += 1
        return True

    def close(self):
        self.closed = True

    def is_open(self):
        return not self.closed

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


class _UI:
    PD_APP_MODAL = 1
    PD_AUTO_HIDE = 2
    PD_CAN_ABORT = 4
    PD_CAN_SKIP = 8
    PD_ELAPSED_TIME = 16
    PD_ESTIMATED_TIME = 32
    PD_REMAINING_TIME = 64

    def __init__(self):
        self.messages = []
        self.progress_dialogs = []

    def message(self, text, title='', buttons='ok', icon='info'):
        self.messages.append((title, text, icon))

    def create_progress_dialog(self, title, message, maximum=100, style=0):
        handle = _Progress()
        self.progress_dialogs.append((title, message, maximum, style, handle))
        return handle


class _Mesh:
    """TriangleMesh mock exposing ONLY the numpy-free accessors (vertex(i) /
    triangle(i) / counts) — exactly what the real embedded mesh provides.
    No vertices()/triangles(): if the plugin ever called them, this mock would
    AttributeError, proving the plugin uses only the numpy-free path."""

    def __init__(self, verts, tris):
        self._v = [list(x) for x in verts]
        self._t = [list(x) for x in tris]

    def vertex_count(self):
        return len(self._v)

    def triangle_count(self):
        return len(self._t)

    def vertex(self, i):
        return tuple(self._v[i])

    def triangle(self, i):
        return tuple(self._t[i])


class _Volume:
    def __init__(self, mesh):
        self._m = mesh

    def mesh(self):
        return self._m


class _Object:
    def __init__(self, volumes, name='model'):
        self._vols = volumes
        self.name = name

    def volumes(self):
        return self._vols


class _Model:
    def __init__(self, objects):
        self._objs = objects

    def objects(self):
        return self._objs


def _cube():
    v = np.array([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
                  [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float32)
    def sq(a, b, c, d):
        return [[a, b, c], [a, c, d]]
    t = np.array(sq(0, 3, 2, 1) + sq(4, 5, 6, 7) + sq(0, 4, 7, 3) +
                 sq(1, 2, 6, 5) + sq(0, 1, 5, 4) + sq(3, 7, 6, 2), dtype=np.int32)
    return v, t


def _load_plugin(model, data_dir):
    """Build a mock `orca` and import the plugin file."""
    ui = _UI()
    host = _Host(model, data_dir, ui)
    orca = type('orca', (), {})
    orca.host = host
    orca.ExecutionResult = _ExecutionResult
    orca.PluginResult = _PluginResult
    orca.script = type('script', (), {'ScriptPluginCapabilityBase': _ScriptPluginCapabilityBase})
    orca.base = _Base
    orca.register_capability = lambda cap: None
    orca.request_permissions = lambda **kw: None
    orca.plugin = lambda cls: cls
    sys.modules['orca'] = orca

    spec = importlib.util.spec_from_file_location('sutura_orca_plugin', PLUGIN)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod, host, ui


def test_unique_output_name_different_for_consecutive_calls():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    a = mod._unique_output_name('model')
    b = mod._unique_output_name('model')
    assert a != b, (a, b)
    assert a.startswith('model_fixed_') and a.endswith('.stl'), a
    assert '_fixed_' in a and a.split('_fixed_')[1][:8].isdigit(), a


def test_model_to_mesh_reads_volume():
    v, t = _cube()
    model = _Model([_Object([_Volume(_Mesh(v, t))], name='part.1')])
    mod, _host, _ui = _load_plugin(model, tempfile.mkdtemp())
    name, verts, tris = mod._model_to_mesh(model)
    assert name == 'part.1', name
    assert len(verts) == 8 and len(tris) == 12, (len(verts), len(tris))
    assert len(verts[0]) == 3 and len(tris[0]) == 3, (verts[0], tris[0])


def test_model_to_mesh_none_for_empty():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    assert mod._model_to_mesh(_Model([])) is None  # no objects
    assert mod._model_to_mesh(_Model([_Object([])])) is None  # object, no volumes
    # a None model is handled in execute(); _model_to_mesh now PROPAGATES the
    # real error instead of hiding it (the bug that swallowed the numpy
    # ImportError from mesh.vertices()/triangles()).
    try:
        mod._model_to_mesh(None)
        assert False, 'expected AttributeError to propagate'
    except AttributeError:
        pass


def test_write_temp_stl_valid_binary():
    v, t = _cube()
    with tempfile.TemporaryDirectory(prefix='orca-stub-') as d:
        mod, host, _ui = _load_plugin(_Model([]), d)
        path = mod._write_temp_stl(v, t)
        assert os.path.dirname(os.path.dirname(path)) == d, path
        assert path.endswith('.stl') and os.path.exists(path), path
        with open(path, 'rb') as f:
            head = f.read(80)
            n = struct.unpack('<I', f.read(4))[0]
        assert head.startswith(b'sutura repair staging'), head[:21]
        assert n == 12, n
        assert os.path.getsize(path) == 84 + 12 * 50, os.path.getsize(path)


def test_execute_no_model_returns_failure():
    # host.model() is None -> 'no model in the scene'
    mod, _host, _ui = _load_plugin(None, tempfile.mkdtemp())
    plugin = mod.SuturaRepair()
    res = plugin.execute()
    assert res.ok is False and 'no model' in res.msg, res

    # a present but empty model -> 'no readable mesh volume'
    mod2, _host2, _ui2 = _load_plugin(_Model([]), tempfile.mkdtemp())
    plugin2 = mod2.SuturaRepair()
    res2 = plugin2.execute()
    assert res2.ok is False and 'no readable mesh' in res2.msg, res2


def test_execute_with_model_starts_worker():
    v, t = _cube()
    model = _Model([_Object([_Volume(_Mesh(v, t))])])
    with tempfile.TemporaryDirectory(prefix='orca-stub-') as d:
        mod, host, ui = _load_plugin(model, d)
        calls = []

        def fake_worker(self, data, result):
            calls.append(data)
            result['ok'] = True
            result['out_path'] = os.path.join(d, 'fake.stl')

        mod.SuturaRepair._worker = fake_worker
        plugin = mod.SuturaRepair()
        res = plugin.execute()
        assert res.ok is True, res
        assert len(calls) == 1, calls
        assert len(calls[0][1]) == 8, calls[0][1]          # verts now a plain list
        assert len(calls[0][2]) == 12, calls[0][2]          # tris now a plain list
        # a native progress dialog was opened for the repair duration and the
        # result message was shown only after the worker finished
        assert len(ui.progress_dialogs) == 1, ui.progress_dialogs
        title, msg, maximum, style, handle = ui.progress_dialogs[0]
        assert title == 'Sutura Repair' and handle.closed, (title, handle.closed)
        assert style == (ui.PD_APP_MODAL | ui.PD_AUTO_HIDE), style
        assert any('Sutura repaired' in m[1] for m in ui.messages), ui.messages


def test_execute_falls_back_without_progress_dialog():
    """Builds without orca.host.ui.create_progress_dialog fall back to the old
    background behaviour: execute() returns 'started' immediately and the
    worker still runs."""
    v, t = _cube()
    model = _Model([_Object([_Volume(_Mesh(v, t))])])
    with tempfile.TemporaryDirectory(prefix='orca-stub-') as d:
        mod, host, ui = _load_plugin(model, d)
        ui.create_progress_dialog = None  # simulate an older host build
        calls = []
        mod.SuturaRepair._worker = lambda self, data, result: calls.append(data)
        plugin = mod.SuturaRepair()
        res = plugin.execute()
        assert res.ok is True, res
        time.sleep(0.3)
        assert len(calls) == 1, calls


def test_register_capabilities_declares_cli_fs_read():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    calls = []
    import sys
    sys.modules['orca'].request_permissions = lambda **kw: calls.append(kw)
    mod.SuturaRepairPlugin().register_capabilities()
    assert calls, 'request_permissions was not called'
    assert calls[0]['fs_read'] == [mod.SUTURA_CLI], calls[0]


def test_plugin_imports_and_writes_without_numpy():
    """Regression for the real GUI failure: OrcaSlicer's embedded Python has
    NO numpy. Block numpy entirely (meta-path finder) in a subprocess and
    verify the plugin still imports and its pure-stdlib helpers work."""
    import subprocess
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    plugin = os.path.join(repo, 'orcaslicer-plugin', 'sutura_repair_linux_x86_64.py')
    code = (
        "import sys, os, importlib.util, tempfile, enum\n"
        "class _B:\n"
        "    def find_spec(self, name, path=None, target=None):\n"
        "        if name.split('.')[0] == 'numpy':\n"
        "            raise ImportError('no numpy in embedded env')\n"
        "        return None\n"
        "sys.meta_path.insert(0, _B())\n"
        "class ER:\n"
        "    @staticmethod\n"
        "    def success(m='', d=''): return m\n"
        "    @staticmethod\n"
        "    def failure(s, m='', d=''): return m\n"
        "class PR(enum.Enum): Success=0; Skipped=1; RecoverableError=2; FatalError=3\n"
        "class U:\n"
        "    def message(self, *a, **k): pass\n"
        "class Host:\n"
        "    def __init__(s, d): s._d=d; s.ui=U()\n"
        "    def model(s): return None\n"
        "    def data_dir(s): return s._d\n"
        "class Cap: pass\n"
        "class Base: pass\n"
        "orca = type('orca', (), {})\n"
        "orca.host = Host(tempfile.mkdtemp())\n"
        "orca.ExecutionResult = ER\n"
        "orca.PluginResult = PR\n"
        "orca.script = type('script', (), {'ScriptPluginCapabilityBase': Cap})\n"
        "orca.base = Base\n"
        "orca.register_capability = lambda c: None\n"
        "orca.plugin = lambda c: c\n"
        "sys.modules['orca'] = orca\n"
        "spec = importlib.util.spec_from_file_location('sutura_orca_plugin', %r)\n"
        "mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)\n"
        "v = [[0.0,0.0,0.0],[1.0,0.0,0.0],[0.0,1.0,0.0]]\n"
        "t = [[0,1,2]]\n"
        "p = mod._write_temp_stl(v, t)\n"
        "assert os.path.exists(p) and os.path.getsize(p) == 84 + 50, os.path.getsize(p)\n"
        "assert mod._unique_output_name('m') != mod._unique_output_name('m')\n"
        "print('NONUMPY-OK')\n" % plugin
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                       text=True, timeout=120)
    assert r.returncode == 0, r.stderr[-600:]
    assert 'NONUMPY-OK' in r.stdout, r.stdout[-200:]


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('orca plugin stub tests passed')