#!/usr/bin/env python3
"""Stub tests for the OrcaSlicer plugin (orcaslicer-plugin/
sutura_repair_linux_x86_64.py) against a MOCK orca.host.

The plugin cannot run inside a real OrcaSlicer here, so it is loaded with a
fake `orca` module and its pure helpers plus the panel message protocol are
exercised:
  * _export_object_stl   world transform, left-handed winding, multi-part
                         union, modifier skipping, numpy-free fallback
  * _build_state         object summaries (parts/modifiers/errors/manifold)
  * on_message           state / repair / cancel / analyze / settings / open_output
  * on_lifecycle_event   enqueues a debounced scan only (never works inline)
  * _notify_broken_objects  one notification per broken object per session
  * without numpy        the plugin still imports and transforms lists

Needs numpy only (the plugin file does not import pymeshlab at module level).
Usage: /opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python tests/test_orca_plugin.py
"""
import importlib.util
import json
import os
import struct
import sys
import tempfile
import threading
import time
import types

import numpy as np

PLUGIN = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      'orcaslicer-plugin', 'sutura_repair_linux_x86_64.py')

_COUNTER = [0]


# --------------------------------------------------------------------------- fake orca

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
    """Base with the config methods the C++ base provides (get_config returns
    a JSON string, save_config persists it)."""

    def get_config(self):
        return getattr(self, '_stored_config', '{}')

    def save_config(self, config_str):
        self._stored_config = config_str
        return True

    def get_default_config(self):
        return {}


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


class _Notif:
    ProgressBarNotificationLevel = 0
    HintNotificationLevel = 1
    RegularNotificationLevel = 2
    WarningNotificationLevel = 6
    ErrorNotificationLevel = 8


class _DockPanel:
    def __init__(self, html, title, on_message, on_close):
        self.html = html
        self.title = title
        self.on_message = on_message
        self.on_close = on_close
        self.posts = []
        self._open = True
        self.closed = False

    def post(self, message):
        self.posts.append(message)

    def show(self):
        self._open = True

    def hide(self):
        self._open = False

    def close(self):
        self.closed = True
        self._open = False

    def is_open(self):
        return self._open and not self.closed


class _UI:
    PD_APP_MODAL = 1
    PD_AUTO_HIDE = 2
    PD_CAN_ABORT = 4
    NotificationLevel = _Notif

    def __init__(self):
        self.dock_panels = []
        self.notifications = []
        self.messages = []

    def create_dock_panel(self, **kwargs):
        handle = _DockPanel(kwargs.get('html', ''), kwargs.get('title', ''),
                            kwargs.get('on_message'), kwargs.get('on_close'))
        self.dock_panels.append(handle)
        return handle

    def push_notification(self, notification_level, text, hyper_text='', on_click=None):
        self.notifications.append({'level': notification_level, 'text': text,
                                   'hyper_text': hyper_text, 'on_click': on_click})
        return True

    def message(self, text, title='', buttons='ok', icon='info'):
        self.messages.append((title, text, icon))


# --- model graph -------------------------------------------------------------

class _Mesh:
    """TriangleMesh mock. With zero_copy=True it exposes the numpy accessors;
    with zero_copy=False it raises on them (forcing the numpy-free path)."""

    def __init__(self, verts, tris, zero_copy=True):
        self._v = [list(map(float, x)) for x in verts]
        self._t = [list(map(int, x)) for x in tris]
        self._zc = zero_copy

    def vertex_count(self):
        return len(self._v)

    def triangle_count(self):
        return len(self._t)

    def vertex(self, i):
        return tuple(self._v[i])

    def triangle(self, i):
        return tuple(self._t[i])

    def vertices(self):
        if not self._zc:
            raise AttributeError("'vertices' requires numpy (simulated)")
        return np.array(self._v, dtype=np.float64)

    def triangles(self):
        if not self._zc:
            raise AttributeError("'triangles' requires numpy (simulated)")
        return np.array(self._t, dtype=np.int64)


class _Volume:
    def __init__(self, mesh, matrix=None, kind='part', manifold=True, name=''):
        self._m = mesh
        self._matrix = matrix
        self.kind = kind
        self._manifold = manifold
        self.name = name

    def mesh(self):
        return self._m

    def matrix(self):
        if self._matrix is None:
            return None
        return np.array(self._matrix, dtype=np.float64)

    def is_model_part(self):
        return self.kind == 'part'

    def is_modifier(self):
        return self.kind == 'modifier'

    def is_negative_volume(self):
        return self.kind == 'negative'

    def is_manifold(self):
        return self._manifold


class _Instance:
    def __init__(self, matrix=None):
        self._matrix = matrix

    def matrix(self):
        if self._matrix is None:
            return None
        return np.array(self._matrix, dtype=np.float64)

    def is_left_handed(self):
        return False


class _Object:
    def __init__(self, volumes, name='model', oid=1, instances=None, errors=0,
                 facets=None):
        self._vols = volumes
        self.name = name
        self._oid = oid
        self._instances = instances if instances is not None else [_Instance()]
        self._errors = errors
        self._facets = facets

    def id(self):
        return self._oid

    def volumes(self):
        return self._vols

    def instances(self):
        return self._instances

    def instance(self, i):
        return self._instances[i]

    def volume_count(self):
        return len(self._vols)

    def instance_count(self):
        return len(self._instances)

    def facets_count(self):
        if self._facets is not None:
            return self._facets
        return sum(v.mesh().triangle_count() for v in self._vols if v.mesh())

    def mesh_errors_count(self):
        return self._errors

    def is_multiparts(self):
        return len(self._vols) > 1

    def is_cut(self):
        return False


class _Model:
    def __init__(self, objects):
        self._objs = objects

    def objects(self):
        return self._objs


# --------------------------------------------------------------------------- helpers

def _load_plugin(model, data_dir):
    """Build a mock `orca` and import the plugin file."""
    ui = _UI()
    host = _Host(model, data_dir, ui)
    _COUNTER[0] += 1
    orca = types.ModuleType('orca')
    orca.host = host
    orca.ExecutionResult = _ExecutionResult
    orca.PluginResult = _PluginResult
    orca.script = type('script', (), {'ScriptPluginCapabilityBase': _ScriptPluginCapabilityBase})
    orca.base = _Base
    orca.register_capability = lambda cap: None
    orca.request_permissions = lambda **kw: None
    orca.plugin = lambda cls: cls
    sys.modules['orca'] = orca

    name = 'sutura_orca_plugin_%d' % _COUNTER[0]
    spec = importlib.util.spec_from_file_location(name, PLUGIN)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod, host, ui


def _cube():
    v = [[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
         [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]]
    t = [[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
         [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
         [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]]
    return v, t


def _trans(x, y, z):
    return [[1, 0, 0, x], [0, 1, 0, y], [0, 0, 1, z], [0, 0, 0, 1]]


def _stl_triangles(path):
    with open(path, 'rb') as f:
        f.read(80)
        n = struct.unpack('<I', f.read(4))[0]
        tris = []
        for _ in range(n):
            f.read(12)
            vs = [struct.unpack('<3f', f.read(12)) for _ in range(3)]
            f.read(2)
            tris.append(vs)
    return tris


def _signed_volume(tris):
    vol = 0.0
    for a, b, c in tris:
        vol += (a[0] * (b[1] * c[2] - b[2] * c[1])
                - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
    return vol / 6.0


def _bbox(tris):
    xs = [v[0] for tri in tris for v in tri]
    ys = [v[1] for tri in tris for v in tri]
    zs = [v[2] for tri in tris for v in tri]
    return (min(xs), min(ys), min(zs)), (max(xs), max(ys), max(zs))


def _wait(predicate, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        if predicate():
            return True
        time.sleep(0.02)
    return predicate()


def _posts(handle, command):
    return [p for p in handle.posts if p.get('command') == command]


# --------------------------------------------------------------------------- tests

def test_unique_output_name_different_for_consecutive_calls():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    a = mod._unique_output_name('model')
    b = mod._unique_output_name('model')
    assert a != b, (a, b)
    assert a.startswith('model_fixed_') and a.endswith('.stl'), a
    assert a.split('_fixed_')[1][:8].isdigit(), a


def test_config_defaults():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    cfg = mod.SuturaRepair().get_default_config()
    assert cfg['preset'] == 'balanced'
    assert cfg['open_panel_at_startup'] is True
    assert cfg['notify_broken'] is True
    assert cfg['sutura_cli'] == ''


def test_execute_opens_panel():
    v, t = _cube()
    model = _Model([_Object([_Volume(_Mesh(v, t))])])
    mod, _host, ui = _load_plugin(model, tempfile.mkdtemp())
    res = mod.SuturaRepair().execute()
    assert res.ok is True, res
    assert len(ui.dock_panels) == 1, ui.dock_panels
    assert ui.dock_panels[0].title == 'Sutura'
    assert 'Sutura' in ui.dock_panels[0].html


def test_export_world_transform():
    v, t = _cube()
    vol = _Volume(_Mesh(v, t), matrix=_trans(10, 0, 0))
    obj = _Object([vol], instances=[_Instance(_trans(0, 5, 0))])
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'part.stl')
        info = mod._export_object_stl(obj, path)
        assert info['parts'] == 1 and info['skipped'] == 0, info
        tris = _stl_triangles(path)
        assert len(tris) == 12, len(tris)
        lo, hi = _bbox(tris)
        assert lo == (10.0, 5.0, 0.0), lo
        assert hi == (11.0, 6.0, 1.0), hi


def test_export_left_handed_flips_winding():
    v, t = _cube()
    # mirror across X: negative determinant -> the exporter must flip winding
    inst = [[-1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]
    obj = _Object([_Volume(_Mesh(v, t))], instances=[_Instance(inst)])
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'part.stl')
        mod._export_object_stl(obj, path)
        tris = _stl_triangles(path)
        lo, hi = _bbox(tris)
        assert lo == (-1.0, 0.0, 0.0), lo
        assert hi == (0.0, 1.0, 1.0), hi
        # a left-handed transform with flipped winding stays outward (volume > 0)
        assert _signed_volume(tris) > 0.5, _signed_volume(tris)


def test_export_multi_part_union_skips_modifier():
    v, t = _cube()
    parts = [
        _Volume(_Mesh(v, t), matrix=_trans(0, 0, 0)),
        _Volume(_Mesh(v, t), matrix=_trans(5, 0, 0)),
    ]
    modifier = _Volume(_Mesh(v, t), matrix=_trans(100, 0, 0), kind='modifier')
    obj = _Object(parts + [modifier])
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'part.stl')
        info = mod._export_object_stl(obj, path)
        assert info['parts'] == 2, info
        assert info['skipped'] == 1, info
        tris = _stl_triangles(path)
        assert len(tris) == 24, len(tris)  # both model parts, no modifier
        _lo, hi = _bbox(tris)
        assert hi[0] == 6.0, hi  # the at-100 modifier was skipped


def test_export_numpy_free_fallback():
    v, t = _cube()
    mesh = _Mesh(v, t, zero_copy=False)  # vertices()/triangles() raise
    obj = _Object([_Volume(mesh)])
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, 'part.stl')
        info = mod._export_object_stl(obj, path)
        assert info['triangles'] == 12, info
        assert len(_stl_triangles(path)) == 12


def test_refresh_posts_state():
    v, t = _cube()
    obj = _Object([_Volume(_Mesh(v, t))], name='gear', oid=7, errors=0)
    mod, _host, ui = _load_plugin(_Model([obj]), tempfile.mkdtemp())
    mod._resolve_cli = lambda cfg=None: ('/usr/bin/sutura', True, '0.7.1')
    plugin = mod.SuturaRepair()
    plugin._open_panel()
    handle = ui.dock_panels[-1]
    handle.on_message({'command': 'refresh'})
    assert _wait(lambda: _posts(handle, 'state')), handle.posts
    state = _posts(handle, 'state')[-1]
    assert state['cli'] == {'found': True, 'path': '/usr/bin/sutura', 'version': '0.7.1'}
    assert len(state['objects']) == 1, state['objects']
    o = state['objects'][0]
    assert o['id'] == 7 and o['name'] == 'gear'
    assert o['parts'] == 1 and o['manifold'] is True and o['status'] == 'ok'
    assert state['settings']['preset'] == 'balanced'


def test_state_flags_broken_object():
    v, t = _cube()
    broken = _Object([_Volume(_Mesh(v, t), manifold=False)], name='bad', oid=3, errors=2)
    mod, _host, ui = _load_plugin(_Model([broken]), tempfile.mkdtemp())
    mod._resolve_cli = lambda cfg=None: ('/usr/bin/sutura', True, '0.7.1')
    plugin = mod.SuturaRepair()
    plugin._open_panel()
    handle = ui.dock_panels[-1]
    handle.on_message({'command': 'refresh'})
    assert _wait(lambda: _posts(handle, 'state')), handle.posts
    o = _posts(handle, 'state')[-1]['objects'][0]
    assert o['status'] == 'bad' and o['manifold'] is False, o


def test_repair_message_protocol():
    v, t = _cube()
    obj = _Object([_Volume(_Mesh(v, t))], name='part', oid=1)
    mod, _host, ui = _load_plugin(_Model([obj]), tempfile.mkdtemp())
    report = {'category': 'watertight',
              'stage1': {'holes_remaining': 0, 'two_manifold': True},
              'stage2': {'ok': True},
              'defects': {'holes': [1, 2], 'non_manifold': []}}
    mod._resolve_cli = lambda cfg=None: ('/usr/bin/sutura', True, '0.7.1')
    mod._run_repair = lambda cli, src, out, preset, cancel_event, timeout=None, on_proc=None: \
        ('ok', report, '')
    mod._load_back = lambda path: None
    plugin = mod.SuturaRepair()
    plugin._open_panel()
    handle = ui.dock_panels[-1]
    plugin._handle_repair({'ids': ['1'], 'preset': 'thorough'})
    assert _wait(lambda: any(p.get('phase') == 'done' for p in _posts(handle, 'job'))), handle.posts
    phases = [p['phase'] for p in _posts(handle, 'job')]
    for expected in ('queued', 'exporting', 'repairing', 'loading', 'done'):
        assert expected in phases, phases
    done = _posts(handle, 'job')[-1]
    assert done['phase'] == 'done', done
    assert done['result']['watertight'] is True, done
    assert done['result']['method'] == 'two-stage rebuild', done
    assert done['result']['holes_before'] == 2, done
    assert done['result']['holes_after'] == 0, done
    assert done['result']['output_path'].endswith('.stl'), done


class _Proc:
    def kill(self):
        pass

    def communicate(self, timeout=None):
        return ('', '')


def test_cancel_message():
    v, t = _cube()
    obj = _Object([_Volume(_Mesh(v, t))], name='part', oid=1)
    mod, _host, ui = _load_plugin(_Model([obj]), tempfile.mkdtemp())

    def fake_repair(cli, src, out, preset, cancel_event, timeout=None, on_proc=None):
        if on_proc:
            on_proc(_Proc())
        cancel_event.wait(3)
        return ('cancelled', None, 'Cancelled') if cancel_event.is_set() else ('failed', None, 'no cancel')

    mod._resolve_cli = lambda cfg=None: ('/usr/bin/sutura', True, '0.7.1')
    mod._run_repair = fake_repair
    plugin = mod.SuturaRepair()
    plugin._open_panel()
    handle = ui.dock_panels[-1]
    plugin._handle_repair({'ids': ['1'], 'preset': 'balanced'})
    plugin._handle_cancel({'id': '1'})
    assert _wait(lambda: any(p.get('phase') == 'cancelled' for p in _posts(handle, 'job'))), handle.posts


def test_analyze_message():
    v, t = _cube()
    obj = _Object([_Volume(_Mesh(v, t))], name='part', oid=1)
    mod, _host, ui = _load_plugin(_Model([obj]), tempfile.mkdtemp())
    report = {'analysis': {'boundary_loops': 3, 'non_manifold_edges': 1,
                           'self_intersections': 5},
              'recommendations': [{'id': 'graft', 'name': 'Graft', 'score': 0.8},
                                  {'id': 'ftetwild', 'name': 'fTetWild', 'score': 0.5}]}
    mod._resolve_cli = lambda cfg=None: ('/usr/bin/sutura', True, '0.7.1')
    mod._run_analyze = lambda cli, src, cancel_event=None, timeout=None, on_proc=None: \
        ('ok', report, '')
    plugin = mod.SuturaRepair()
    plugin._open_panel()
    handle = ui.dock_panels[-1]
    plugin._handle_analyze({'id': '1'})
    assert _wait(lambda: _posts(handle, 'analysis')), handle.posts
    msg = _posts(handle, 'analysis')[-1]
    assert msg['id'] == '1', msg
    assert msg['defects'] == {'holes': 3, 'non_manifold': 1, 'self_intersections': 5}, msg
    assert msg['recommended'][0] == {'method': 'graft', 'name': 'Graft', 'confidence': 0.8}, msg


def test_settings_persist():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    plugin = mod.SuturaRepair()
    plugin._handle_settings({'preset': 'thorough', 'notify_broken': False})
    cfg = plugin._settings()
    assert cfg['preset'] == 'thorough', cfg
    assert cfg['notify_broken'] is False, cfg
    stored = json.loads(plugin.get_config())
    assert stored['preset'] == 'thorough' and stored['notify_broken'] is False, stored
    # an invalid preset falls back to balanced
    plugin._handle_settings({'preset': 'nonsense'})
    assert plugin._settings()['preset'] == 'balanced'


def test_notification_deduplication():
    v, t = _cube()
    obj = _Object([_Volume(_Mesh(v, t))], name='part', oid=1, errors=3)
    mod, _host, ui = _load_plugin(_Model([obj]), tempfile.mkdtemp())
    plugin = mod.SuturaRepair()
    plugin._notify_broken_objects()
    plugin._notify_broken_objects()
    assert len(ui.notifications) == 1, ui.notifications
    n = ui.notifications[0]
    assert 'mesh error' in n['text'] and 'part' in n['text'], n
    assert n['hyper_text'] == 'Repair with Sutura', n
    assert n['level'] == ui.NotificationLevel.WarningNotificationLevel


def test_notification_skipped_when_disabled():
    v, t = _cube()
    obj = _Object([_Volume(_Mesh(v, t))], name='part', oid=1, errors=3)
    mod, _host, ui = _load_plugin(_Model([obj]), tempfile.mkdtemp())
    plugin = mod.SuturaRepair()
    plugin.save_config(json.dumps({'notify_broken': False}))
    plugin._notify_broken_objects()
    assert ui.notifications == [], ui.notifications


def test_lifecycle_only_enqueues():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    plugin = mod.SuturaRepair()
    calls = []
    plugin._debounced_scan = lambda: calls.append(1)  # instance override
    plugin.on_lifecycle_event(types.SimpleNamespace(name='ObjectAdded'))
    assert calls == [], 'lifecycle handler must only enqueue'
    st = plugin._st()
    assert st['scan_timer'] is not None, 'a debounced scan should be scheduled'
    st['scan_timer'].cancel()

    # a non-scan event schedules nothing
    plugin2 = mod.SuturaRepair()
    plugin2.on_lifecycle_event(types.SimpleNamespace(name='PresetSelected'))
    assert plugin2._st()['scan_timer'] is None


def test_plugin_imports_and_transforms_without_numpy():
    """OrcaSlicer's embedded Python may lack numpy even though it is declared;
    block numpy entirely in a subprocess and verify the plugin still imports,
    embeds the panel, and its pure list transform path works."""
    import subprocess
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    plugin = os.path.join(repo, 'orcaslicer-plugin', 'sutura_repair_linux_x86_64.py')
    code = (
        "import sys, importlib.util, enum, types\n"
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
        "    class NotificationLevel: WarningNotificationLevel=6\n"
        "    def push_notification(self, *a, **k): pass\n"
        "    def create_dock_panel(self, **k): return None\n"
        "class Host:\n"
        "    def __init__(s): s.ui=U()\n"
        "    def model(s): return None\n"
        "    def data_dir(s): return '/tmp'\n"
        "class Cap:\n"
        "    def get_config(self): return '{}'\n"
        "    def save_config(self, s): return True\n"
        "class Base: pass\n"
        "orca = types.ModuleType('orca')\n"
        "orca.host = Host()\n"
        "orca.ExecutionResult = ER\n"
        "orca.PluginResult = PR\n"
        "orca.script = types.SimpleNamespace(ScriptPluginCapabilityBase=Cap)\n"
        "orca.base = Base\n"
        "orca.register_capability = lambda c: None\n"
        "orca.request_permissions = lambda **k: None\n"
        "orca.plugin = lambda c: c\n"
        "sys.modules['orca'] = orca\n"
        "spec = importlib.util.spec_from_file_location('sutura_orca_plugin', %r)\n"
        "mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)\n"
        "assert '<!DOCTYPE html>' in mod._EMBEDDED_PANEL_HTML\n"
        "assert mod._np is None\n"
        "m = [[1,0,0,2],[0,1,0,3],[0,0,1,4],[0,0,0,1]]\n"
        "out = mod._apply_matrix([[0.0,0.0,0.0]], m)\n"
        "assert out == [[2.0,3.0,4.0]], out\n"
        "assert mod._det3([[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]]) == 1.0\n"
        "print('NONUMPY-OK')\n" % plugin
    )
    r = subprocess.run([sys.executable, '-c', code], capture_output=True,
                       text=True, timeout=120)
    assert r.returncode == 0, r.stderr[-800:]
    assert 'NONUMPY-OK' in r.stdout, r.stdout[-300:]


def test_register_capabilities_declares_cli_fs_read():
    mod, _host, _ui = _load_plugin(_Model([]), tempfile.mkdtemp())
    calls = []
    sys.modules['orca'].request_permissions = lambda **kw: calls.append(kw)
    registered = []
    sys.modules['orca'].register_capability = lambda cap: registered.append(cap)
    mod.SuturaRepairPlugin().register_capabilities()
    assert calls, 'request_permissions was not called'
    assert calls[0]['fs_read'], calls[0]
    assert registered == [mod.SuturaRepair], registered


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_'):
            fn()
            print('ok  %s' % name)
    print('orca plugin stub tests passed')
