#!/usr/bin/env python3
"""Central method-error isolation + Health section regression (P0).

Two things are pinned here:

1. PARITY (the gate).  Running the real CLI with method-health ON and with
   ``SUTURA_METHOD_HEALTH=0`` must produce byte-identical output meshes and an
   identical JSON report once the NEW keys (``method_health`` / ``method_errors``)
   and the known wall-clock fields (timings, *.seconds, elapsed_ms, ...) are
   removed.  If anything else differs, the error wrapper altered a repair
   result and this test FAILS (do not relax it).
2. ISOLATION.  A tier that raises is caught by the central wrapper, recorded as
   a structured ``MethodError`` and skipped; the repair still completes.  A
   missing Rust symbol is classified as ``missing_symbol``.

Synthetic fixtures only (generated in a temp dir); no machine-local inputs.
Usage: <venv>/bin/python tests/test_method_health.py
"""
import hashlib
import json
import os
import struct
import subprocess
import sys
import tempfile
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
REPAIR_PY = os.path.join(SUTURA, 'repair.py')

# NEW keys the wrapper adds (excluded from the parity report comparison).
NEW_KEYS = {'method_health', 'method_errors'}
# Wall-clock / measured fields that differ between ANY two runs and are
# unrelated to the wrapper; excluded so the comparison is about the result.
VOLATILE_KEYS = {
    'timings', 'seconds', 'time_s', 'elapsed_ms', 'budget_s',
    'prepass_seconds', 'time', 'output',
}


def _write_stl(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'method-health'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for a, b, c in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            f.write(struct.pack('<3f', *verts[a]))
            f.write(struct.pack('<3f', *verts[b]))
            f.write(struct.pack('<3f', *verts[c]))
            f.write(struct.pack('<H', 0))


def _cube_missing_face(path):
    v = [
        [1, 0, 0], [1, 0, 1], [1, 1, 0], [1, 1, 1],
        [0, 0, 0], [0, 0, 1], [0, 1, 0], [0, 1, 1],
    ]
    quads = [
        (0, 1, 3, 2), (4, 6, 7, 5), (0, 2, 6, 4),
        (1, 5, 7, 3), (0, 4, 5, 1), (2, 3, 7, 6),
    ]
    tris = []
    for a, b, c, d in quads:
        tris += [(a, b, c), (a, c, d)]
    tris = [t for i, t in enumerate(tris) if i not in (0, 1)]  # open the +X face
    _write_stl(path, v, tris)


def _make_obj(path):
    """A cube with a removed face written as OBJ (via the repo generator)."""
    r = subprocess.run([sys.executable, os.path.join(REPO, 'tests', 'make_broken_obj.py'),
                        path], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def _make_3mf(path):
    r = subprocess.run([sys.executable, os.path.join(REPO, 'tests',
                        'make_layered_multiobject_3mf.py'), path],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


# --- report normalisation -------------------------------------------------

def _normalise(obj):
    if isinstance(obj, dict):
        out = {}
        for k, val in obj.items():
            if k in NEW_KEYS or k in VOLATILE_KEYS or k.endswith('_s'):
                continue
            out[k] = _normalise(val)
        return out
    if isinstance(obj, list):
        return [_normalise(x) for x in obj]
    return obj


def _json(r):
    return json.loads(r.stdout.strip().splitlines()[-1])


def _sha(path):
    with open(path, 'rb') as f:
        return hashlib.sha256(f.read()).hexdigest()


def _model_text(path):
    """Concatenated .model XML text of a 3MF (zip timestamps are volatile)."""
    with zipfile.ZipFile(path) as z:
        return b'\n'.join(z.read(n) for n in sorted(z.namelist())
                          if n.endswith('.model'))


ARGS = ['--no-fallback-ftetwild', '--no-graft']  # keep the run deterministic


def _run_collect(src, out, env_extra):
    env = dict(os.environ, SUTURA_DIR=SUTURA)
    env.update(env_extra)
    r = subprocess.run([sys.executable, REPAIR_PY] + ARGS + ['-o', out, src],
                       capture_output=True, text=True, timeout=600, env=env)
    return r


def _parity_fixture(tmp, name, maker, ext, compare_bytes=True):
    src = os.path.join(tmp, name + ext)
    maker(src)
    out_on = os.path.join(tmp, 'on' + ext)
    out_off = os.path.join(tmp, 'off' + ext)
    r_on = _run_collect(src, out_on, {})
    r_off = _run_collect(src, out_off, {'SUTURA_METHOD_HEALTH': '0'})
    assert r_on.returncode == 0, (name, r_on.stderr[-800:])
    assert r_off.returncode == 0, (name, r_off.stderr[-800:])
    d_on, d_off = _json(r_on), _json(r_off)
    # the health section is present exactly once, and absent from the OFF run
    assert isinstance(d_on.get('method_health'), dict), (name, d_on.get('method_health'))
    assert d_on['method_health'].get('total', 0) >= 1, (name, d_on['method_health'])
    assert not d_on['method_health'].get('errors'), (name, d_on['method_health'])
    assert _normalise(d_on) == _normalise(d_off), (
        name, 'report differs beyond the new keys',
        _first_diff(_normalise(d_on), _normalise(d_off)))
    if compare_bytes:
        assert _sha(out_on) == _sha(out_off), (name, 'output bytes differ')
    else:
        assert _model_text(out_on) == _model_text(out_off), (name, '3MF content differs')


def _first_diff(a, b, path=''):
    if type(a) is not type(b):
        return '%s: type %r != %r' % (path, type(a), type(b))
    if isinstance(a, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                return '%s/%s: missing on one side' % (path, k)
            d = _first_diff(a[k], b[k], path + '/' + str(k))
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return '%s: len %d != %d' % (path, len(a), len(b))
        for i, (x, y) in enumerate(zip(a, b)):
            d = _first_diff(x, y, '%s[%d]' % (path, i))
            if d:
                return d
        return None
    if a != b:
        return '%s: %r != %r' % (path, a, b)
    return None


def test_parity_broken_stl(tmp):
    _parity_fixture(tmp, 'broken', _cube_missing_face, '.stl')


def test_parity_broken_obj(tmp):
    _parity_fixture(tmp, 'broken', _make_obj, '.obj')


def test_parity_layered_3mf(tmp):
    _parity_fixture(tmp, 'layered', _make_3mf, '.3mf', compare_bytes=False)


def test_parity_layered_3mf_with_rust_extension_blocked(tmp):
    """A missing OPTIONAL dependency (sutura_geom) is a skip, not an error:
    the repair still produces output and method_health.errors stays empty."""
    src = os.path.join(tmp, 'layered_nog.3mf')
    _make_3mf(src)
    out = os.path.join(tmp, 'nog.3mf')
    env = dict(os.environ, SUTURA_DIR=SUTURA)
    launcher = (
        "import sys; sys.path.insert(0, %r); sys.modules['sutura_geom'] = None; "
        "sys.argv = %r; "
        "exec(compile(open(%r, 'r', encoding='utf-8').read(), %r, 'exec'))"
        % (SUTURA, [REPAIR_PY] + ARGS + ['-o', out, src], REPAIR_PY, REPAIR_PY))
    r = subprocess.run([sys.executable, '-c', launcher],
                       capture_output=True, text=True, timeout=600, env=env)
    assert r.returncode == 0, r.stderr[-1200:]
    d = _json(r)
    mh = d['method_health']
    assert mh['errors'] == [], mh
    assert mh.get('skips'), mh                       # the rule fired
    assert all(s.get('kind') == 'skipped' for s in mh['skips']), mh['skips']
    assert os.path.isfile(out) and os.path.getsize(out) > 0


# --- in-process isolation -------------------------------------------------

def _import_engine():
    if SUTURA not in sys.path:
        sys.path.insert(0, SUTURA)
    os.environ.setdefault('SUTURA_DIR', SUTURA)
    # The pipeline facade is what triage._repair_mod() now returns, so a
    # monkeypatch on its repair_file must be visible to _attempt.
    import sutura_engine.pipeline as repair  # noqa: E402
    import sutura_engine.health as health  # noqa: E402
    import sutura_engine.triage as triage  # noqa: E402
    return repair, health, triage


def _open_cube_arrays():
    import numpy as np
    v = np.array([
        [1, 0, 0], [1, 0, 1], [1, 1, 0], [1, 1, 1],
        [0, 0, 0], [0, 0, 1], [0, 1, 0], [0, 1, 1]], dtype=np.float32)
    quads = [(0, 1, 3, 2), (4, 6, 7, 5), (0, 2, 6, 4),
             (1, 5, 7, 3), (0, 4, 5, 1), (2, 3, 7, 6)]
    t = []
    for a, b, c, d in quads:
        t += [(a, b, c), (a, c, d)]
    t = [x for i, x in enumerate(t) if i not in (0, 1)]
    return v, np.asarray(t, dtype=np.int32)


def test_tier_exception_is_recorded_and_skipped(tmp):
    repair, health, _triage = _import_engine()
    import sutura_engine.flap as flap_mod
    v, t = _open_cube_arrays()
    orig = flap_mod.flap_fill

    def _boom(*_a, **_k):
        raise RuntimeError('injected flap failure')

    flap_mod.flap_fill = _boom
    try:
        health.reset_if_unscoped()
        report, ov, ot = repair.repair_mesh_from_arrays(
            v, t, tmp, flap=True, tamponade=False, deep_repair=None)
    finally:
        flap_mod.flap_fill = orig
    assert len(ot) > 0, 'repair must still produce geometry'
    errs = health.current().errors
    assert any(e.method == 'flap' and e.kind == 'exception' for e in errs), errs
    # the failure is isolated, not fatal, and reported
    assert report.get('flap', {}).get('reason', '').startswith('flap error:'), report.get('flap')


def test_missing_symbol_is_classified(tmp):
    _repair, health, _triage = _import_engine()
    health.reset_if_unscoped()

    def _no_symbol():
        raise AttributeError("module 'sutura_geom' has no attribute 'nope'")

    ok, err = health.guard('probe', 'call', _no_symbol)
    assert ok is False and isinstance(err, AttributeError)
    assert health.current().errors[-1].kind == health.KIND_MISSING_SYMBOL
    # an import failure is classified as import
    health.guard('probe2', 'call', _raise_importerror)
    assert health.current().errors[-1].kind == health.KIND_IMPORT


def _raise_importerror():
    raise ImportError('no such module')


def test_triage_attempt_isolates_pipeline_error(tmp):
    repair, health, triage = _import_engine()
    health.reset_if_unscoped()
    orig = repair.repair_file

    def _boom(*_a, **_k):
        raise RuntimeError('injected pipeline failure')

    repair.repair_file = _boom
    try:
        res = triage._attempt('in.stl', 'out.stl', tmp, {}, False)
    finally:
        repair.repair_file = orig
    assert res.get('error', '').startswith('repair failed:'), res
    assert any(e.method == 'pipeline' and e.stage == 'attempt'
               for e in health.current().errors), health.current().errors


def test_health_off_records_nothing(tmp):
    _repair, health, _triage = _import_engine()
    os.environ['SUTURA_METHOD_HEALTH'] = '0'
    try:
        health.reset_if_unscoped()
        ok, err = health.guard('probe', 'call', _raise_importerror)
        assert ok is False
        assert health.current().errors == [], health.current().errors
    finally:
        os.environ.pop('SUTURA_METHOD_HEALTH', None)


def test_availability_api(tmp):
    _repair, health, _triage = _import_engine()
    avail = health.check_availability()
    assert avail, 'the registry must expose methods'
    assert all(isinstance(s, health.Status) for s in avail.values()), avail
    detail = health.check_availability_detail()
    assert set(detail) == set(avail)
    rep = health.build_report()
    assert rep['total'] == len(detail)
    assert rep['ok'] + rep['failed'] + rep['skipped'] == rep['total']
    line = health.human_line(rep)
    assert line.startswith('Methods: %d/%d OK' % (rep['ok'], rep['total'])), line


def main():
    with tempfile.TemporaryDirectory(prefix='sutura-health-') as tmp:
        for name in sorted(list(globals())):
            fn = globals()[name]
            if name.startswith('test_') and callable(fn):
                fn(tmp)
                print('ok  %s' % name)
    print('method_health tests passed')


if __name__ == '__main__':
    main()
