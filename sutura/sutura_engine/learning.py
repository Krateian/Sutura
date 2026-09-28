# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""sutura_engine.learning - Learning-triage aggregation and ranking bonus.

Aggregates the anonymous per-repair ``methods_tried`` entries (template id,
method slug, outcome, elapsed time) into ``triage_learning.json`` under the
Sutura data directory and turns them into a small, bounded Bayesian ranking
bonus (see ``methods.rank_methods``).

PRIVACY HARD RULE (unchanged): only template ids and method slugs are stored —
never a file name, path, user, IP or machine identifier.

The legacy import path ``sutura/history.py`` re-exports this module's public
functions; the usage-history store itself stays in ``history.py``.
"""
import json
import os
import tempfile

import numpy as np

# Same data directory as history.py (SUTURA_DIR, else the user data dir). Both
# modules resolve it independently so learning.py stays importable without
# history.py.
HISTORY_DIR = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))

# Per (template, method) success/time counts, kept in its own file so it can be
# reset from the CLI/GUI Options without touching the usage history.
LEARNING_PATH = os.path.join(HISTORY_DIR, 'triage_learning.json')
LEARNING_SCHEMA_VERSION = 1
LEARNING_MAX_BONUS = 0.05          # hard cap on the rank bonus [0..1]
LEARNING_PRIOR = 2.0               # Beta(2, 2) prior -> 0.5 with no data
LEARNING_ENV = 'SUTURA_LEARNING_TRIAGE'
LEARNING_CONFIG_PATH = os.path.expanduser('~/.config/sutura/config.json')
_LEARNING_CACHE = {'mtime': None, 'stats': None}


def _learning_enabled_from_config():
    try:
        with open(LEARNING_CONFIG_PATH, encoding='utf-8') as f:
            value = json.load(f).get('learning_triage')
        return True if value is None else bool(value)
    except (OSError, ValueError, AttributeError):
        return True


def learning_enabled():
    """Whether the local-history ranking bonus is active (env > config)."""
    env = os.environ.get(LEARNING_ENV)
    if env is not None:
        return env.strip().lower() not in ('0', 'false', 'no', 'off', '')
    return _learning_enabled_from_config()


def record_learning(result, methods_tried=None):
    """Accumulate per (template, method) success/time counts. Never raises."""
    try:
        attempted = methods_tried
        if attempted is None:
            attempted = result.get('methods_tried') or []
        if not attempted:
            return
        data = _load_learning()
        templates = data.setdefault('templates', {})
        for entry in attempted:
            if not isinstance(entry, dict):
                continue
            method_id = entry.get('id')
            if not method_id:
                continue
            outcome = entry.get('outcome')
            if outcome not in ('accepted', 'rejected', 'best'):
                continue
            template = entry.get('template') or '_none_'
            bucket = templates.setdefault(template, {}).setdefault(
                method_id, {'n': 0, 'ok': 0, 'total_ms': 0})
            bucket['n'] += 1
            if outcome in ('accepted', 'best'):
                bucket['ok'] += 1
            ems = entry.get('elapsed_ms')
            if isinstance(ems, (int, float)) and ems >= 0:
                bucket['total_ms'] += int(ems)
        _save_learning(data)
    except Exception:
        pass


def _load_learning():
    try:
        with open(LEARNING_PATH, encoding='utf-8') as f:
            data = json.load(f)
        if isinstance(data, dict):
            if not isinstance(data.get('templates'), dict):
                data['templates'] = {}
            return data
    except (OSError, ValueError):
        pass
    return {'schema_version': LEARNING_SCHEMA_VERSION, 'templates': {}}


def _save_learning(data):
    try:
        data['schema_version'] = LEARNING_SCHEMA_VERSION
        os.makedirs(HISTORY_DIR, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=HISTORY_DIR, prefix='.learning-',
                                   suffix='.tmp')
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            json.dump(data, f, ensure_ascii=False)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, LEARNING_PATH)
        _LEARNING_CACHE['mtime'] = None
    except Exception:
        pass


def triage_stats():
    """Per-(template, method) counts, cached in-process by file mtime."""
    try:
        mtime = os.path.getmtime(LEARNING_PATH) if os.path.exists(LEARNING_PATH) \
            else None
    except OSError:
        mtime = None
    if _LEARNING_CACHE['mtime'] == mtime and _LEARNING_CACHE['stats'] is not None:
        return _LEARNING_CACHE['stats']
    stats = _load_learning().get('templates', {})
    _LEARNING_CACHE['mtime'] = mtime
    _LEARNING_CACHE['stats'] = stats
    return stats


def learning_bonus(template, method_id, stats=None):
    """Bounded [0, LEARNING_MAX_BONUS] Bayesian bonus for (template, method).

    The posterior success mean under a Beta(prior, prior) prior is scaled to a
    small bonus (0 at 0.5, LEARNING_MAX_BONUS at 1.0) and damped by a speed
    factor in [0.5, 1.0] so a fast, historically-successful method ranks
    slightly higher. No data (or an unknown template/method) gives exactly 0.
    """
    if not template or not method_id:
        return 0.0
    stats = triage_stats() if stats is None else stats
    per_template = stats.get(template) or {}
    bucket = per_template.get(method_id)
    if not bucket:
        return 0.0
    n = int(bucket.get('n') or 0)
    ok = int(bucket.get('ok') or 0)
    if n <= 0:
        return 0.0
    posterior = (ok + LEARNING_PRIOR) / (n + 2.0 * LEARNING_PRIOR)
    base = max(0.0, min(1.0, (posterior - 0.5) * 2.0))
    if base <= 0.0:
        return 0.0
    avg_ms = float(bucket.get('total_ms') or 0) / n
    all_ms = [b.get('total_ms', 0) / b['n'] for t in stats.values()
              for b in t.values() if isinstance(b, dict) and b.get('n')]
    median_ms = float(np.median(all_ms)) if all_ms else 0.0
    speed = 1.0
    if median_ms > 0 and avg_ms > 0:
        ratio = avg_ms / median_ms
        speed = float(max(0.5, min(1.0, 1.0 / (0.5 + 0.5 * ratio))))
    return round(LEARNING_MAX_BONUS * base * speed, 6)


def clear_learning():
    """Delete the learning file. Returns True when something was removed."""
    try:
        os.remove(LEARNING_PATH)
        _LEARNING_CACHE['mtime'] = None
        _LEARNING_CACHE['stats'] = None
        return True
    except OSError:
        return False


def learning_summary_text(stats=None):
    """Short human/CLI summary of what has been learned."""
    stats = triage_stats() if stats is None else stats
    rows = []
    for template, methods in sorted(stats.items()):
        for method_id, b in sorted((methods or {}).items()):
            if not isinstance(b, dict) or not b.get('n'):
                continue
            rows.append('  %-20s %-18s n=%d ok=%d avg=%.0fms'
                        % (template, method_id, b['n'], b['ok'],
                           (b.get('total_ms') or 0) / b['n']))
    if not rows:
        return 'no learning data yet'
    return 'learning triage data (template, method):\n' + '\n'.join(rows)
