# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""CLI sub-command helpers used by main() (P1a split). Moved verbatim from
sutura/repair.py; no logic change."""
import json
import os
import sys

import numpy as np

from sutura_engine.pipeline.tiers.engines import load_engine_run


def _fail(msg, human, code=1):
    """Report a subcommand error in the selected format."""
    if human:
        print('error: %s' % msg, file=sys.stderr)
    else:
        print(json.dumps({'error': msg}))
    return code


def _confirm_cli(action, yes):
    """Require confirmation for a destructive CLI action (install/uninstall),
    unless ``--yes``. Non-interactive without --yes refuses (never hangs)."""
    if yes:
        return True
    if not sys.stdin.isatty():
        return False
    try:
        ans = input('Proceed with %s? [y/N] ' % action).strip().lower()
    except EOFError:
        return False
    return ans in ('y', 'yes')


def run_engines_cli(args, human=False):
    """``sutura engines list|check``: validate the TOML configs and resolve
    the binaries WITHOUT running anything. Returns an exit code."""
    sub = args[0] if args else 'list'
    if sub not in ('list', 'check'):
        return _fail("engines: subcommand must be 'list' or 'check'",
                     human, code=2)
    try:
        import engines as _engines
    except Exception as e:  # noqa: BLE001
        return _fail('engines module unavailable: %s' % e, human)
    eng, chain, warnings = load_engine_run()
    ordered = [n for n in chain if n in eng]
    ordered += [n for n in sorted(eng) if n not in ordered]
    items = []
    problems = list(warnings)
    for name in ordered:
        cfg = eng[name]
        found = bool(_engines.is_engine_available(cfg))
        item = {
            'name': cfg.name,
            'placement': cfg.placement,
            'enabled': cfg.enabled,
            'in_chain': name in chain,
            'binary_found': found,
            'command': cfg.command,
            'input_format': cfg.input_format,
            'output_format': cfg.output_format,
            'timeout': cfg.timeout,
            'source': str(cfg.source_path) if cfg.source_path else None,
        }
        items.append(item)
        if cfg.enabled and not found:
            problems.append("engine '%s': executable %r not found"
                            % (name, cfg.command[0] if cfg.command else '?'))
    report = {
        'config_dir': str(_engines.get_engines_dir()),
        'chain': chain,
        'engines': items,
        'warnings': problems,
    }
    if sub == 'check':
        report['ok'] = not problems
    if human:
        print('Engines (%s):' % report['config_dir'])
        if not items:
            print('  none configured')
        for it in items:
            print('  %-20s %-16s enabled=%-5s binary=%s%s' % (
                it['name'], it['placement'], it['enabled'],
                'found' if it['binary_found'] else 'MISSING',
                '' if it['in_chain'] else ' (not in chain)'))
        for w in problems:
            print('  ! %s' % w)
    else:
        print(json.dumps(report, ensure_ascii=False))
    return 0 if not (sub == 'check' and problems) else 1


def run_ftetwild_cli(args, dry_run=False, yes=False, human=False):
    """``sutura ftetwild status|install|uninstall`` via ``ftetwild_manager``.

    ``--dry-run`` never touches the environment (it prints the plan/estimate);
    install/uninstall require ``--yes`` or an interactive confirmation, and
    always show the download / installed / freed size first. Returns an exit
    code."""
    sub = args[0] if args else 'status'
    if sub not in ('status', 'install', 'uninstall'):
        return _fail("ftetwild: subcommand must be 'status', 'install' or "
                     "'uninstall'", human, code=2)
    try:
        import ftetwild_manager as mgr
    except Exception as e:  # noqa: BLE001
        return _fail('ftetwild manager unavailable: %s' % e, human)

    if sub == 'status':
        st = mgr.status()
        st['estimate'] = mgr.estimate()
        if human:
            if not st.get('supported'):
                print('fTetWild: unsupported (%s)' % st.get('reason'))
            elif st.get('installed'):
                print('fTetWild: installed v%s (%s) at %s'
                      % (st.get('version'), st.get('size_human'), st.get('location')))
            else:
                print('fTetWild: not installed (supported; download ~%s, '
                      'installed ~%s)' % (st['estimate']['download_human'],
                                          st['estimate']['installed_human']))
        else:
            print(json.dumps(st, ensure_ascii=False))
        return 0 if st.get('supported') else 1

    if sub == 'install':
        est = mgr.estimate()
        if dry_run:
            plan = mgr.install(dry_run=True)
            print(json.dumps({'estimate': est, 'plan': plan}, ensure_ascii=False))
            return 0 if plan.get('ok') else 1
        print('fTetWild install: download ~%s, installed ~%s'
              % (est['download_human'], est['installed_human']), file=sys.stderr)
        if not _confirm_cli('fTetWild install', yes):
            return _fail('install cancelled (use --yes to confirm)', human)
        res = mgr.install(progress_cb=lambda s: print(s, file=sys.stderr))
        print(json.dumps({'result': res, 'estimate': est}, ensure_ascii=False))
        return 0 if res.get('ok') else 1

    # uninstall
    plan = mgr.uninstall(dry_run=True)
    if dry_run:
        print(json.dumps({'plan': plan}, ensure_ascii=False))
        return 0 if plan.get('ok') else 1
    print('fTetWild uninstall: %d package(s), frees ~%s'
          % (len(plan.get('packages', [])), plan.get('freed_human', '0 B')),
          file=sys.stderr)
    if not _confirm_cli('fTetWild uninstall', yes):
        return _fail('uninstall cancelled (use --yes to confirm)', human)
    res = mgr.uninstall(progress_cb=lambda s: print(s, file=sys.stderr))
    print(json.dumps({'result': res}, ensure_ascii=False))
    return 0 if res.get('ok') else 1
