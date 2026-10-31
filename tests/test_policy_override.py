#!/usr/bin/env python3
"""Step 7 override-layer tests. Each case runs a fresh interpreter with
SUTURA_POLICY_USER pointed at a temp policy (the merge happens once at import).

Usage: <venv>/bin/python tests/test_policy_override.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')

CODE = """
import json, sys
sys.path.insert(0, %r)
import sutura_engine.policy as pol
import sutura_engine.methods as methods
import triage
print(json.dumps({
    'user_override': pol.USER_OVERRIDE,
    'policy_error': pol.POLICY_ERROR,
    'ignored': list(pol.IGNORED_KEYS),
    'hash': pol.CONTENT_HASH,
    'default_intensity': triage.DEFAULT_INTENSITY,
    'top_n': methods.RANKING_TOP_N,
    'block': pol.report_block(),
}))
""" % SUTURA

NONEXISTENT = os.path.join(tempfile.gettempdir(), 'sutura-no-such-policy.toml')


def run(user_text=None):
    env = dict(os.environ)
    if user_text is None:
        env['SUTURA_POLICY_USER'] = NONEXISTENT
        return json.loads(subprocess.check_output(
            [sys.executable, '-c', CODE], env=env, cwd=REPO, text=True))
    fd, path = tempfile.mkstemp(suffix='.toml')
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            f.write(user_text)
        env['SUTURA_POLICY_USER'] = path
        return json.loads(subprocess.check_output(
            [sys.executable, '-c', CODE], env=env, cwd=REPO, text=True))
    finally:
        os.unlink(path)


def test_no_user_file():
    out = run(None)
    assert out['user_override'] is False
    assert out['policy_error'] is None
    assert out['ignored'] == []
    assert out['block']['user_override'] is False
    assert 'override_path' not in out['block']
    assert 'path' not in out['block']


def test_gate_rejection():
    base = run(None)['hash']
    out = run('[gates.tuning]\nhausdorff_floor = 1\n')
    assert out['user_override'] is True
    assert out['policy_error'] is None
    assert any(k.startswith('gates.tuning') for k in out['ignored']), out['ignored']
    assert out['hash'] == base, 'a locked gate must not change the effective policy'


def test_unknown_key():
    base = run(None)['hash']
    out = run('[bogus]\nx = 1\n')
    assert out['user_override'] is True
    assert 'bogus' in out['ignored'], out['ignored']
    assert out['hash'] == base


def test_malformed_file():
    base = run(None)['hash']
    out = run('this is not toml [[[\n')
    assert out['user_override'] is True
    assert out['policy_error'], 'a malformed file must set policy_error'
    assert out['hash'] == base
    assert out['block']['policy_error'] == out['policy_error']


def test_only_locked_or_unknown_keys():
    base = run(None)['hash']
    out = run('[gates.tuning]\nhausdorff_floor = 1\n[auto]\ngenerative_max_hausdorff_rel = 0.5\n[nope]\ny = 1\n')
    assert out['user_override'] is True
    assert out['ignored'], out['ignored']
    assert out['hash'] == base


def test_si_escalation_locked():
    base = run(None)['hash']
    out = run('[si_escalation]\nvol_rel_tol = 0.5\n')
    assert out['user_override'] is True
    assert 'si_escalation' in out['ignored'], out['ignored']
    assert out['hash'] == base, 'the auto-SI never-worse tolerances are locked'


def test_real_override_intensity_default():
    base = run(None)
    out = run('[intensity]\ndefault = "quick"\n')
    assert out['user_override'] is True
    assert out['policy_error'] is None
    assert out['default_intensity'] == 'quick'
    assert out['hash'] != base['hash']


def test_real_override_ranking_top_n():
    base = run(None)
    out = run('[ranking]\ntop_n = 4\n')
    assert out['top_n'] == 4
    assert out['hash'] != base['hash']


if __name__ == '__main__':
    test_no_user_file()
    test_gate_rejection()
    test_unknown_key()
    test_malformed_file()
    test_only_locked_or_unknown_keys()
    test_si_escalation_locked()
    test_real_override_intensity_default()
    test_real_override_ranking_top_n()
    print('policy override tests passed')
