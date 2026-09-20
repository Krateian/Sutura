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
    def success(msg):
        return _Result(True, msg)

    @staticmethod
    def failure(msg):
        return _Result(False, msg)


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


class _UI:
    def __init__(self):
        self.messages = []

    def message(self, text, title='', buttons='ok', icon='info'):
        self.messages.append((title, text, icon))


class _Mesh:
    def __init__(self, verts, tris):
        self._v = verts
        self._t = tris

    def vertices(self):
        return self._v

    def triangles(self):
        return self._t


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
    orca.script = type('script', (), {'ScriptPluginCapabilityBase': _ScriptPluginCapabilityBase})
    orca.base = _Base
    orca.register_capability = lambda cap: None
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
    assert verts.shape == (8, 3) and tris.shape == (12, 3), (verts.shape, tris.shape)


def test_model_to_mesh_none_for_empty():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    assert mod._model_to_mesh(_Model([])) is None
    assert mod._model_to_mesh(None) is None


def test_write_temp_stl_valid_binary():
    v, t = _cube()
    with tempfile.TemporaryDirectory(prefix='orca-stub-') as d:
        mod, host, _ui = _load_plugin(_Model([]), d)
        path = mod._write_temp_stl(v, t)
        assert os.path.dirname(os.path.dirname(path)) == d, path
        assert path.endswith('.stl') and os.path.exists(path), path
        with open(path, 'rb') as f:
            head = f.read(80)
            n = np.frombuffer(f.read(4), dtype='<u4')[0]
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
        mod.SuturaRepair._worker = lambda self, data: calls.append(data)
        plugin = mod.SuturaRepair()
        res = plugin.execute()
        assert res.ok is True, res
        time.sleep(0.2)
        assert len(calls) == 1, calls
        assert calls[0][1].shape == (8, 3), calls[0][1].shape


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('orca plugin stub tests passed')