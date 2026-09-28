"""Anonymous usage history for the repair engine.

Records purely technical mesh/motor data (mesh sizes, defect counts,
classifier outcome, mode, timing) for every repair, so the community can
steer the engine (mesh_classifier + stage-1/2 tuning) toward the cases it
actually meets. PRIVACY HARD RULE: no file names, no paths, no user names,
no IPs, no machine identifiers — the mesh fingerprint is derived from mesh
geometry only.

stdlib + numpy only (no pymeshlab), consistent with mesh_classifier.py /
defects.py / confidence.py.
"""
import os
import json
import hashlib
import tempfile
from datetime import datetime, timezone

import numpy as np

HISTORY_DIR = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
HISTORY_PATH = os.path.join(HISTORY_DIR, 'history.jsonl')
SCHEMA_VERSION = 1

# Learning-triage aggregation: per (template, method) success/time counts,
# derived from the same anonymous repair log. Kept in its own file so it can be
# reset from the CLI/GUI Options without touching the usage history. Privacy
# rule unchanged: only template ids and method ids, never a file or a user.
LEARNING_PATH = os.path.join(HISTORY_DIR, 'triage_learning.json')
LEARNING_SCHEMA_VERSION = 1
LEARNING_MAX_BONUS = 0.05          # hard cap on the rank bonus [0..1]
LEARNING_PRIOR = 2.0               # Beta(2, 2) prior -> 0.5 with no data
LEARNING_ENV = 'SUTURA_LEARNING_TRIAGE'
LEARNING_CONFIG_PATH = os.path.expanduser('~/.config/sutura/config.json')
_LEARNING_CACHE = {'mtime': None, 'stats': None}

# A filename is only ever used to derive the mesh FORMAT ('stl'/'obj'/'3mf').
_ALLOWED_FORMATS = ('stl', 'obj', '3mf')


def mesh_fingerprint(verts, tris):
    """Deterministic sha256 hex digest of a mesh's geometry (quantized).

    Coordinates are rounded to 4 decimals so the same mesh loaded through
    slightly different float paths still hashes identically. Purely
    geometry-derived — contains no file or user information.
    """
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    q = np.round(v, 4)
    h = hashlib.sha256()
    h.update(q.tobytes())
    h.update(t.tobytes())
    return h.hexdigest()


def _mesh_format(result):
    ext = os.path.splitext(str(result.get('input', '')))[1].lower().lstrip('.')
    return ext if ext in _ALLOWED_FORMATS else None


def _stage2_status(result):
    s2 = result.get('stage2')
    if not isinstance(s2, dict):
        return 'not_run'
    if s2.get('ok'):
        return 'ok'
    if 'error' in s2:
        return 'error'
    return 'skipped'


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


def build_record(result, fingerprint, version, elapsed_ms, fmt=None):
    """Build the history record dict for one repair report.

    ``result`` is a report dict like the CLI JSON payload (may be a top-level
    single-mesh report or a per-object 3MF sub-report). Only fixed, technical
    keys are emitted — anything identifying the input file is excluded.
    ``fmt`` overrides the mesh format (per-object 3MF reports carry no input
    path of their own, so the parent report's format is passed in).
    """
    s1 = result.get('stage1') or {}
    defects = result.get('defects') or {}
    mu = result.get('method_used') or {}
    tried = result.get('methods_tried') or []
    return {
        'schema_version': SCHEMA_VERSION,
        'seq': 0,  # assigned by append_record
        'ts': datetime.now(timezone.utc).isoformat(),
        'app_version': version,
        'format': fmt if fmt else _mesh_format(result),
        'mesh_fingerprint': fingerprint,
        'elapsed_ms': elapsed_ms,
        'faces_before': s1.get('faces_before'),
        'faces_after': s1.get('faces_after'),
        'vertices_before': s1.get('vertices_before'),
        'vertices_after': s1.get('vertices_after'),
        'defect_holes': len(defects.get('holes') or []),
        'defect_non_manifold': len(defects.get('non_manifold') or []),
        'holes_closed': s1.get('holes_closed'),
        'holes_remaining': s1.get('holes_remaining'),
        'non_manifold_edges_fixed': s1.get('non_manifold_edges_fixed'),
        'non_manifold_edges_remaining': s1.get('non_manifold_edges_remaining'),
        'detected_type': result.get('detected_type'),
        'detected_confidence': result.get('detected_confidence'),
        'repair_mode': result.get('repair_mode'),
        'tuning_applied': result.get('tuning_applied'),
        'method_used_num': mu.get('num'),
        'method_used_id': mu.get('id'),
        'method_used_source': mu.get('source'),
        'methods_tried': [
            {'num': t.get('num'), 'id': t.get('id'),
             'outcome': t.get('outcome'), 'template': t.get('template'),
             'elapsed_ms': t.get('elapsed_ms')}
            for t in tried if isinstance(t, dict)
        ],
        'filters_applied': s1.get('applied_filters'),
        'filters_skipped': len(s1.get('skipped') or {}),
        'stage2_status': _stage2_status(result),
        'category': result.get('category'),
        'issues': list(result.get('issues') or []),
        'repair_confidence': result.get('repair_confidence'),
        'repair_confidence_label': result.get('repair_confidence_label'),
        'volume_change_percent': s1.get('volume_change_percent'),
        'extreme_passes_applied': result.get('extreme_passes_applied'),
        'self_intersections_found': result.get('self_intersections_found'),
        'self_intersections_removed': result.get('self_intersections_removed'),
        'autorefine_applied': bool(result.get('experimental_autorefine')
                                   and result.get('experimental_autorefine')
                                   .get('adopted')),
        'ftetwild_applied': bool(result.get('experimental_ftetwild')
                                 and result.get('experimental_ftetwild')
                                 .get('adopted')),
        'indirect_autorefine_applied': bool(
            result.get('experimental_indirect_autorefine')
            and result.get('experimental_indirect_autorefine')
            .get('adopted')),
    }


def pop_fingerprints(result):
    """Pop the transient '_fp' keys from a result (and per-object reports).

    Always called (even when history is disabled) so the transient geometry
    fingerprint never leaks into the CLI's JSON stdout report. Returns the
    fingerprints in record order: top-level first, then one per object.
    """
    fps = []
    top = result.pop('_fp', None)
    if top is not None:
        fps.append(top)
    for rep in result.get('object_reports', []):
        fp = rep.pop('_fp', None)
        if fp is not None:
            fps.append(fp)
    return fps


def append_record(record):
    """Append one record line, assigning its sequence number. Never raises."""
    try:
        os.makedirs(HISTORY_DIR, exist_ok=True)
        seq = 1
        if os.path.exists(HISTORY_PATH):
            with open(HISTORY_PATH) as f:
                seq = sum(1 for _ in f) + 1
        record['seq'] = seq
        with open(HISTORY_PATH, 'a') as f:
            f.write(json.dumps(record, ensure_ascii=False) + '\n')
    except Exception:
        pass


def write(result, version, elapsed_ms, fingerprints):
    """Append history record(s) for a finalized repair result.

    One record for a single-mesh report, one per object for multi-object 3MF.
    Swallows every error — recording must never break the repair.
    """
    if not fingerprints:
        return
    try:
        record_learning(result)
    except Exception:
        pass
    try:
        if 'object_reports' in result:
            fmt = _mesh_format(result)
            reps = result['object_reports']
            for i, rep in enumerate(reps):
                fp = (fingerprints[i] if i < len(fingerprints)
                      else fingerprints[0] if fingerprints else None)
                if fp is None:
                    continue
                append_record(build_record(rep, fp, version, elapsed_ms, fmt=fmt))
        else:
            append_record(build_record(result, fingerprints[0], version, elapsed_ms))
    except Exception:
        pass


def load_records(last=0):
    """Read records from the history file; ``last`` > 0 keeps only the most
    recent N. Missing/corrupt lines are skipped."""
    records = []
    if not os.path.exists(HISTORY_PATH):
        return records
    try:
        with open(HISTORY_PATH) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except Exception:
                    continue
    except Exception:
        return records
    if last > 0:
        records = records[-last:]
    return records


def summary_text(records):
    """Concise, copy-paste friendly summary of the records. No geometry, no
    identifying data."""
    from collections import Counter
    lines = ['Sutura usage history: %d repair record(s)' % len(records)]
    ts = [r.get('ts') for r in records if r.get('ts')]
    if ts:
        lines.append('time range: %s .. %s' % (min(ts), max(ts)))
    for label, key in (('category', 'category'),
                       ('repair_mode', 'repair_mode'),
                       ('detected_type', 'detected_type'),
                       ('stage2_status', 'stage2_status')):
        c = Counter(str(r.get(key)) for r in records)
        if c:
            lines.append('%s: %s' % (label, ', '.join(
                '%s=%d' % (k, v) for k, v in sorted(c.items()))))
    conf = [r.get('repair_confidence') for r in records
            if r.get('repair_confidence') is not None]
    if conf:
        lines.append('repair_confidence: avg=%.1f min=%d max=%d'
                     % (sum(conf) / len(conf), min(conf), max(conf)))
    el = [r.get('elapsed_ms') for r in records if r.get('elapsed_ms') is not None]
    if el:
        lines.append('elapsed_ms: avg=%.0f total=%.1fs'
                     % (sum(el) / len(el), sum(el) / 1000.0))
    fps = {r.get('mesh_fingerprint') for r in records if r.get('mesh_fingerprint')}
    lines.append('distinct meshes (geometry fingerprint): %d' % len(fps))
    ext = sum(1 for r in records if r.get('extreme_passes_applied'))
    lines.append('extreme extra passes applied: %d' % ext)
    return '\n'.join(lines)


def export_history(last=0, summary_only=False, clear=False):
    """CLI: print a summary, then the full JSON array for copy-paste sharing.

    Default prints BOTH in one command; ``--summary-only`` prints the summary
    alone. ``--clear`` wipes the history file afterwards."""
    records = load_records(last=last)
    if not records:
        print('no history yet (no repairs recorded)')
    else:
        print(summary_text(records))
        if not summary_only:
            print()
            print('--- full data (paste this into the issue) ---')
            print(json.dumps(records, ensure_ascii=False))
    if clear:
        try:
            os.remove(HISTORY_PATH)
            print()
            print('history cleared (%d record(s) removed)' % len(records))
        except OSError:
            pass