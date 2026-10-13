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

from . import paths

# Classifier identity shared with the analysis cache (sutura.mesh_classifier).
# Every (template, method) bucket is stamped with the classifier version whose
# counters it holds, so a later classifier revision never reuses a previous
# one's counters. Defensive import: when the module is unavailable the version
# is None and the gate is DISABLED (every bucket is counted) - never replaced
# by a concrete value that would zero existing bonuses.
try:
    from mesh_classifier import CLASSIFIER_VERSION
except ImportError:
    try:
        from sutura.mesh_classifier import CLASSIFIER_VERSION
    except ImportError:
        CLASSIFIER_VERSION = None

# Runtime user data lives in <data>/, never in the code/install dir (SUTURA_DIR).
HISTORY_DIR = paths.data_dir()

# Per (template, method) success/time counts, kept in its own file so it can be
# reset from the CLI/GUI Options without touching the usage history.
LEARNING_PATH = paths.data_file('triage_learning.json')
LEARNING_SCHEMA_VERSION = 2
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


def _is_other_version(bucket):
    """True when a bucket holds a KNOWN classifier version different from the
    current one (older or newer), so its counters must be ignored.

    A missing stamp is legacy data written before versioning: it is treated as
    current and counted. When the classifier version is unavailable the gate is
    disabled entirely (every bucket counted)."""
    if CLASSIFIER_VERSION is None:
        return False
    cv = bucket.get('cv')
    return cv is not None and cv != CLASSIFIER_VERSION


def _merge_bucket(dest, src):
    """Add ``src``'s counters into ``dest`` without losing either count."""
    dest['n'] = int(dest.get('n') or 0) + int(src.get('n') or 0)
    dest['ok'] = int(dest.get('ok') or 0) + int(src.get('ok') or 0)
    dest['total_ms'] = int(dest.get('total_ms') or 0) + int(src.get('total_ms') or 0)
    if src.get('cv') is not None:
        dest['cv'] = src['cv']


def _filter_stats(raw):
    """Counted buckets only.

    Drops the ``method_id@cv`` relocation keys (stale buckets preserved on disk
    but never counted) and any bucket stamped with a different known classifier
    version. Legacy (unstamped) buckets are kept."""
    filtered = {}
    if not isinstance(raw, dict):
        return filtered
    for template, methods in raw.items():
        if not isinstance(methods, dict):
            continue
        kept = {}
        for method_id, bucket in methods.items():
            if not isinstance(bucket, dict) or '@' in method_id:
                continue
            if _is_other_version(bucket):
                continue
            kept[method_id] = bucket
        if kept:
            filtered[template] = kept
    return filtered


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
            methods = templates.setdefault(template, {})
            bucket = methods.get(method_id)
            if isinstance(bucket, dict) and _is_other_version(bucket):
                # Preserve a bucket from a different known classifier version
                # non-destructively under a version-qualified key, merging on
                # collision (never overwrite an existing count), then start a
                # fresh current-version bucket below.
                stale_key = '%s@%s' % (method_id, bucket['cv'])
                existing = methods.get(stale_key)
                if isinstance(existing, dict):
                    _merge_bucket(existing, bucket)
                else:
                    methods[stale_key] = bucket
                bucket = None
            if not isinstance(bucket, dict):
                bucket = {'n': 0, 'ok': 0, 'total_ms': 0}
                methods[method_id] = bucket
            if CLASSIFIER_VERSION is not None:
                bucket['cv'] = CLASSIFIER_VERSION
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
        paths.ensure_data_dir()
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
    stats = _filter_stats(_load_learning().get('templates', {}))
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
    # Filter even an externally supplied stats map, so a stale or relocation
    # bucket can never enter the looked-up bucket or the speed median below.
    stats = _filter_stats(triage_stats() if stats is None else stats)
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
