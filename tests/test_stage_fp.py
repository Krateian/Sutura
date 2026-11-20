#!/usr/bin/env python3
"""Tests for the opt-in stage-fingerprint collection in gh_mesh_shard.py.

The feature exists so a platform-dependent divergence (`broken_37111`,
`broken_38631` differ from Python only on Linux) can be attributed to a stage
instead of just "the bytes differ". These tests pin the gate, the parser and the
degradation behaviour without needing a real candidate venv.

    python3 tests/test_stage_fp.py
"""
import importlib.util
import os
import shutil
import stat
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, '..', 'scripts', 'gh_mesh_shard.py')

FAIL = []


def check(cond, label):
    print(('  ok   ' if cond else '  FAIL ') + label)
    if not cond:
        FAIL.append(label)


def load_shard():
    spec = importlib.util.spec_from_file_location('gh_mesh_shard', SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def make_candidate(root, stderr_body, rc=0):
    """A fake candidate venv whose interpreter emits `stderr_body`."""
    os.makedirs(os.path.join(root, 'cand-venv', 'bin'), exist_ok=True)
    os.makedirs(os.path.join(root, 'cand', 'sutura'), exist_ok=True)
    with open(os.path.join(root, 'cand', 'sutura', 'repair.py'), 'w') as f:
        f.write('# placeholder\n')
    py = os.path.join(root, 'cand-venv', 'bin', 'python')
    with open(py, 'w') as f:
        f.write('#!/bin/sh\n')
        f.write('prev=""\n')
        f.write('for a in "$@"; do [ "$prev" = "-o" ] && echo x > "$a"; prev="$a"; done\n')
        f.write("cat >&2 <<'FP'\n%s\nFP\n" % stderr_body.strip())
        f.write('echo \'{"stage1":{"two_manifold":true}}\'\n')
        f.write('exit %d\n' % rc)
    os.chmod(py, os.stat(py).st_mode | stat.S_IEXEC)
    return py


def main():
    mod = load_shard()
    work = tempfile.mkdtemp(prefix='sfp-test-')
    cwd = os.getcwd()
    try:
        print('gate: only ENV_CAND naming SUTURA_STAGE_FP=1 enables it')
        check(mod.stage_fp_requested('SUTURA_STAGE_FP=1'), 'exact flag')
        check(mod.stage_fp_requested('A=1,SUTURA_STAGE_FP=1,B=2'), 'inside a comma list')
        check(mod.stage_fp_requested('SUTURA_STAGE_FP=true'), 'true')
        check(not mod.stage_fp_requested(''), 'empty env')
        check(not mod.stage_fp_requested('SUTURA_GRAFT_ENGINE=rust'),
              'an unrelated env must NOT enable the extra repair')
        check(not mod.stage_fp_requested('SUTURA_STAGE_FP=0'), 'explicit 0')

        print()
        print('parsing: the exact format from repair.rs')
        os.chdir(work)
        make_candidate(work, '''
[stage-fp] after_prepass 8072460375843944825 766 385
[stage-fp] after_stage1 8747291123582326809 766 385
[stage-fp] after_stage2 13295859424028655141 720 362
[stage-fp] after_pweld 13295859424028655141 720 362
''')
        fp = mod.collect_stage_fp('/tmp/x.stl', 'SUTURA_STAGE_FP=1')
        check(fp is not None and len(fp) == 4, 'four stages parsed')
        if fp:
            check([s['label'] for s in fp] ==
                  ['after_prepass', 'after_stage1', 'after_stage2', 'after_pweld'],
                  'labels in emission order')
            check(fp[0]['hash'] == '8072460375843944825', 'hash kept as a string')
            check(fp[0]['faces'] == 766 and fp[0]['verts'] == 385, 'counts are ints')
            # The whole point: stage 2 changed the mesh, P-WELD did not.
            check(fp[2]['hash'] != fp[1]['hash'], 'stage 2 differs from stage 1')
            check(fp[3]['hash'] == fp[2]['hash'], 'P-WELD matches stage 2')

        print()
        print('degradation: no fingerprints must read as "not measured"')
        shutil.rmtree(os.path.join(work, 'cand-venv'))
        check(mod.collect_stage_fp('/tmp/x.stl', 'SUTURA_STAGE_FP=1') is None,
              'missing candidate venv -> None, no exception')

        make_candidate(work, 'no stage lines at all\n')
        check(mod.collect_stage_fp('/tmp/x.stl', 'SUTURA_STAGE_FP=1') is None,
              'output without stage lines -> None')

        make_candidate(work, '''
[stage-fp] after_prepass 8072460375843944825 766 385
''')
        fp1 = mod.collect_stage_fp('/tmp/x.stl', 'SUTURA_STAGE_FP=1')
        check(fp1 is not None and len(fp1) == 1,
              'a partial run still reports the stages it produced')

        print()
        print('env passing: ENV_CAND pairs reach the child')
        os.chdir(work)
        py = os.path.join(work, 'cand-venv', 'bin', 'python')
        with open(py, 'w') as f:
            f.write('#!/bin/sh\n')
            f.write('[ -n "$SUTURA_GRAFT_ENGINE" ] && echo "[stage-fp] envok 1 1 1" >&2\n')
            f.write('exit 0\n')
        os.chmod(py, os.stat(py).st_mode | stat.S_IEXEC)
        fp2 = mod.collect_stage_fp('/tmp/x.stl', 'SUTURA_GRAFT_ENGINE=rust')
        check(fp2 is not None and fp2[0]['label'] == 'envok',
              'ENV_CAND pair forwarded to the child')
    finally:
        os.chdir(cwd)
        shutil.rmtree(work, ignore_errors=True)

    print()
    if FAIL:
        print('FAILURES: %d' % len(FAIL))
        return 1
    print('ALL PASS')
    return 0


if __name__ == '__main__':
    sys.exit(main())
