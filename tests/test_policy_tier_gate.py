#!/usr/bin/env python3
"""A user policy removing a tier changes the attempted ladder sequence.

Each case runs a fresh interpreter so the policy is loaded once at import.
Usage: <venv>/bin/python tests/test_policy_tier_gate.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
SAMPLE = os.path.join(REPO, 'tests', 'real-world-samples',
                      'thingi10k_100827.stl')

CODE = r'''
import json, os, sys, tempfile
sys.path.insert(0, %r)
import numpy as np
import repair
import sutura_engine.pipeline.pipeline as pipeline
import sutura_engine.pipeline.order as order
from sutura_engine.pipeline.mesh_io import load_mesh

case = os.environ['TIER_CASE']
v, t = load_mesh(%r)
after = {'non_two_manifold_edges': 0}
stats = {}
calls = []

def local(vv, tt, iv, it, a, **kw):
    calls.append('local'); return vv, tt, a

def graft(vv, tt, iv, it, a, st, tmpdir, **kw):
    calls.append('graft')
    if case == 'deferred':
        st['graft'] = {'adopted': True, 'fidelity_ok': False,
                       'coverage_ok': True, 'hausdorff_healthy': 0.05}
    else:
        st['graft'] = {'adopted': False, 'reason': 'test'}
    return vv, tt, a

def dressing(vv, tt, iv, it, a, st, tmpdir, **kw):
    calls.append('dressing'); st['dressing'] = {'adopted': False}; return vv, tt, a

def splint(vv, tt, iv, it, a, st, tmpdir, **kw):
    calls.append('splint'); st['splint'] = {'adopted': False}; return vv, tt, a

def coaptation(vv, tt, iv, it, *a, **kw):
    calls.append('coaptation'); return vv, tt, a[0]

def ft(vv, tt, iv, it, a, st, tmpdir, ftetwild, spec=None):
    calls.append('ftetwild'); st['experimental_ftetwild'] = {'ran': False,
                                                             'adopted': False}
    return vv, tt, a

pipeline._local_remesh_tier = local
pipeline.graft_tier = graft
pipeline.dressing_tier = dressing
pipeline.splint_tier = splint
pipeline.coaptation_tier = coaptation
pipeline._ftetwild_tier = ft

mode = 'local' if case == 'no_local' else 'full'
flags = {'graft': 'auto', 'dressing': 'auto', 'splint': False,
         'coaptation': False, 'ftetwild': 'auto'}
if case == 'no_local':
    flags = {'graft': False, 'dressing': False, 'splint': False,
             'coaptation': False, 'ftetwild': False}
elif case in ('no_splint', 'no_coaptation', 'no_ftetwild', 'deferred'):
    flags = {'graft': False, 'dressing': False, 'splint': False,
             'coaptation': False, 'ftetwild': 'auto'}
    if case == 'no_splint':
        flags['splint'] = 'auto'
    elif case == 'no_coaptation':
        flags['coaptation'] = 'auto'
    elif case == 'deferred':
        flags['graft'] = 'auto'
pipeline.deep_repair_ladder(
    v, t, v, t, after, stats, tempfile.gettempdir(), mode=mode,
    spec=repair.triage.PRESETS['balanced'], **flags)
print(json.dumps({'order': list(order.DEEP_TIER_ORDER), 'calls': calls,
                  'graft': stats.get('graft'),
                  'tiers_run': (stats.get('deep_repair') or {}).get('tiers_run')}))
''' % (SUTURA, SAMPLE)

POLICY = {
    'no_user': None,
    'no_graft': ['local', 'dressing', 'splint', 'coaptation', 'ftetwild'],
    'no_dressing': ['local', 'graft', 'splint', 'coaptation', 'ftetwild'],
    'no_local': ['graft', 'dressing', 'splint', 'coaptation', 'ftetwild'],
    'no_splint': ['local', 'graft', 'dressing', 'coaptation', 'ftetwild'],
    'no_coaptation': ['local', 'graft', 'dressing', 'splint', 'ftetwild'],
    'no_ftetwild': ['local', 'graft', 'dressing', 'splint', 'coaptation'],
    'deferred': ['local', 'graft', 'dressing', 'splint', 'coaptation'],
}


def run(case):
    with tempfile.TemporaryDirectory(prefix='sutura-tier-gate-') as tmp:
        env = dict(os.environ)
        env['HOME'] = tmp
        env['XDG_CONFIG_HOME'] = os.path.join(tmp, 'config')
        env['TIER_CASE'] = case
        order = POLICY[case]
        if order is None:
            env['SUTURA_POLICY_USER'] = os.path.join(tmp, 'no-policy.toml')
        else:
            path = os.path.join(tmp, 'policy.toml')
            with open(path, 'w', encoding='utf-8') as f:
                f.write('[order]\ndeep_tier_order = %s\n'
                        % json.dumps(order))
            env['SUTURA_POLICY_USER'] = path
        return json.loads(subprocess.check_output(
            [sys.executable, '-c', CODE], env=env, cwd=REPO, text=True,
            stderr=subprocess.STDOUT))


def test_no_user_file_keeps_full_sequence():
    out = run('no_user')
    assert out['order'] == ['local', 'graft', 'dressing', 'splint',
                            'coaptation', 'ftetwild'], out
    assert out['calls'] == ['graft', 'dressing', 'ftetwild'], out


def test_without_graft_skips_graft():
    out = run('no_graft')
    assert 'graft' not in out['calls'], out
    assert out['calls'] == ['dressing', 'ftetwild'], out


def test_without_dressing_skips_dressing():
    out = run('no_dressing')
    assert 'dressing' not in out['calls'], out
    assert out['calls'] == ['graft', 'ftetwild'], out


def test_without_local_skips_local():
    out = run('no_local')
    assert out['calls'] == [], out


def test_without_splint_skips_splint():
    out = run('no_splint')
    assert 'splint' not in out['calls'], out
    assert out['calls'] == ['ftetwild'], out


def test_without_coaptation_skips_coaptation():
    out = run('no_coaptation')
    assert 'coaptation' not in out['calls'], out
    assert out['calls'] == ['ftetwild'], out


def test_without_ftetwild_skips_ftetwild():
    out = run('no_ftetwild')
    assert 'ftetwild' not in out['calls'], out
    assert out['calls'] == [], out


def test_without_ftetwild_resolves_deferred_graft():
    out = run('deferred')
    assert 'ftetwild' not in out['calls'], out
    assert out['calls'] == ['graft'], out
    g = out['graft'] or {}
    assert g.get('adopted') is False, out
    assert g.get('reason') == ('held aside (fidelity check failed); '
                               'fTetWild disabled by policy'), out
    assert 'graft' in (out['tiers_run'] or []), out


if __name__ == '__main__':
    test_no_user_file_keeps_full_sequence()
    test_without_graft_skips_graft()
    test_without_dressing_skips_dressing()
    test_without_local_skips_local()
    test_without_splint_skips_splint()
    test_without_coaptation_skips_coaptation()
    test_without_ftetwild_skips_ftetwild()
    test_without_ftetwild_resolves_deferred_graft()
    print('policy tier-gate tests passed')
