#!/usr/bin/env python3
"""The CLI report carries policy/method_control metadata.

Usage: <venv>/bin/python tests/test_method_control_report.py
"""
import json
import os
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura', 'repair.py')
SAMPLE = os.path.join(REPO, 'tests', 'real-world-samples',
                      'thingi10k_100827.stl')


def _run(args, tmp):
    env = dict(os.environ)
    env['SUTURA_POLICY_USER'] = os.path.join(tmp, 'policy.toml')
    env['HOME'] = tmp
    env['XDG_CONFIG_HOME'] = os.path.join(tmp, 'config')
    r = subprocess.run([sys.executable, SUTURA] + args, capture_output=True,
                       text=True, timeout=180, env=env, cwd=REPO)
    assert r.returncode == 0, (r.returncode, r.stderr[-800:])
    return json.loads(r.stdout)


def test_default_report_has_policy_and_method_control():
    with tempfile.TemporaryDirectory(prefix='sutura-report-') as tmp:
        out = os.path.join(tmp, 'out.stl')
        data = _run([SAMPLE, '-o', out, '--no-history', '--no-cache',
                     '--json'], tmp)
        pol = data.get('policy') or {}
        mc = data.get('method_control') or {}
        assert pol.get('schema_version') == 1, pol
        assert len(pol.get('content_hash') or '') == 16, pol
        assert pol.get('user_override') is False, pol
        assert mc.get('mode') == 'auto', mc
        assert mc.get('source') == 'default_policy', mc
        assert mc.get('user_forced') is False, mc
        assert mc.get('order') == ['local', 'graft', 'dressing', 'splint',
                                   'coaptation', 'ftetwild'], mc
        assert 'si_escalation' in (mc.get('gates') or []), mc
    print('ok  default report carries policy + method_control')


if __name__ == '__main__':
    test_default_report_has_policy_and_method_control()
    print('method-control report tests passed')
