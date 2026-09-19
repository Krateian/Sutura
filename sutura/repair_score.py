"""Repair Health / Repair Risk scoring for the repaired mesh.

A NEW, SEPARATE scoring system from the classic confidence score
(sutura/confidence.py + the mesh classifier). It computes two independent
0-100 scores after a repair completes:

  REPAIR HEALTH (0-100) - how geometrically sound the FINAL mesh is.
    Cheap metrics only, read from the existing repair report:
      - watertight             (stage 2 confirmed a closed manifold solid)
      - no non-manifold edges  (stage1.non_manifold_edges_remaining == 0)
      - no self-intersections  (stage1.self_intersections_remaining == 0)
      - no holes               (stage1.holes_remaining == 0)
    Weighted sum; weights live in repair_score_config.json (not hardcoded).

  REPAIR RISK (0-100) - how much the repair process altered the mesh.
      - face delta %      (stage1.faces_before / faces_after)
      - vertex delta %    (stage1.vertices_before / vertices_after)
      - component change  (stage1.components_before / components)
      - volume delta %    (stage1.volume_change_percent - ALREADY computed
                           by repair.py via get_geometric_measures; no new
                           computation needed)
    Also weighted, configurable.

  STATUS - derived from the TWO-AXIS combination of Health and Risk tier
    pairs via a config lookup table (NOT an if/elif chain), so a
    high-health+low-risk repair and a mid-health+high-risk repair never
    collapse to the same status just because health crossed some single
    threshold.

Design rules (hard project constraints):
  * Weights and thresholds live in repair_score_config.json, never in code.
  * stdlib-only (no numpy, no pymeshlab): the CLI runs under the PyMeshLab
    venv while the GUI shells out to it, so both sides can import this
    module regardless of interpreter. Keep it free of third-party imports.
  * Fail-silent: any exception / invalid metric returns an 'unavailable'
    status instead of raising - scoring must never break a repair.
  * Expensive metrics (Hausdorff distance, surface area deviation) are
    explicitly OUT OF SCOPE and not implemented.
  * Do NOT touch confidence.py / mesh_classifier.py - this is additive and
    independent of the classic confidence system.
"""
import json
import os

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
_DEFAULTS = {
    'health': {
        'weights': {
            'watertight': 39,
            'no_non_manifold': 22,
            'no_self_intersections': 22,
            'no_holes': 17,
        }
    },
    'risk': {
        'weights': {
            'face_delta': 30,
            'vertex_delta': 20,
            'component_change': 25,
            'volume_delta': 25,
        },
        'delta_scale_percent': 25,
        'component_loss_penalty': 30,
    },
    'status': {
        'health_tiers': {'high': 80, 'mid': 60},
        'risk_tiers': {'high': 70, 'mid': 40},
        'labels': {
            'safe': 'Safe to inspect',
            'review': 'Review recommended',
            'caution': 'Caution advised',
            'failed': 'Failed / inspect',
            'unavailable': 'Scoring unavailable',
        },
        'table': {
            'high_low': 'safe', 'high_mid': 'review', 'high_high': 'review',
            'mid_low': 'review', 'mid_mid': 'review', 'mid_high': 'caution',
            'low_low': 'failed', 'low_mid': 'failed', 'low_high': 'failed',
        },
    },
}


def _deep_merge(base, override):
    """Merge ``override`` into ``base`` recursively (only known keys)."""
    out = dict(base)
    for k, v in (override or {}).items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            out[k] = _deep_merge(base[k], v)
        else:
            out[k] = v
    return out


def load_config(path=None):
    """Load the repair-score config.

    ``path`` defaults to ``repair_score_config.json`` beside this module
    (shipped with the app). A missing/corrupt file silently falls back to the
    built-in defaults (which mirror the shipped file). The result is merged
    over the defaults so partial configs are safe. Never raises.
    """
    if path is None:
        path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                            'repair_score_config.json')
    cfg = dict(_DEFAULTS)
    try:
        with open(path) as f:
            data = json.load(f)
        cfg = _deep_merge(cfg, data)
    except Exception:
        pass
    return cfg


# ---------------------------------------------------------------------------
# Health
# ---------------------------------------------------------------------------
def _health_factors(report):
    """Extract the health factors' boolean "is this ok?" values.

    Returns a dict of {factor_name: bool}. Any factor whose input metric is
    missing/not-a-number is omitted (the caller redistributes its weight).
    """
    s1 = report.get('stage1') or {}
    factors = {}

    # watertight: only computed when there is real stage-1 evidence
    # (two_manifold key present). Missing stage1 / malformed report ->
    # the factor is skipped and the caller reports 'unavailable' instead of
    # a misleading health 0. When present, watertight is only true when stage
    # 2 actually ran and confirmed a closed manifold solid (same rule as
    # classification.py - a stage-1-closed mesh with stage 2 skipped/errored
    # is NOT watertight).
    if 'two_manifold' in s1:
        s2 = report.get('stage2')
        s2_ok = bool(s2 and 'ok' in s2 and 'error' not in s2)
        watertight_s1 = bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0
        factors['watertight'] = bool(watertight_s1 and s2_ok)

    nm = s1.get('non_manifold_edges_remaining')
    if isinstance(nm, (int, float)) and not isinstance(nm, bool):
        factors['no_non_manifold'] = int(nm) == 0

    si = s1.get('self_intersections_remaining')
    if isinstance(si, (int, float)) and not isinstance(si, bool):
        factors['no_self_intersections'] = int(si) == 0

    holes = s1.get('holes_remaining')
    if isinstance(holes, (int, float)) and not isinstance(holes, bool):
        factors['no_holes'] = int(holes) == 0

    return factors


def _health(report, cfg):
    """Weighted health score. Returns (score, factors_dict)."""
    weights = dict((cfg.get('health') or {}).get('weights') or {})
    if not weights:
        return None, {}
    ok = _health_factors(report)
    if not ok:
        return None, {}

    total_w = sum(max(float(w), 0.0) for w in weights.values())
    if total_w <= 0:
        return None, {}
    used = 0.0
    num = 0.0
    factors = {}
    for name, weight in weights.items():
        if name not in ok:
            continue  # metric missing -> skip factor, weight redistributes
        w = max(float(weight), 0.0)
        used += w
        val = 1.0 if ok[name] else 0.0
        num += val * w
        factors[name] = {'ok': bool(ok[name]), 'weight': round(w, 2)}
    if used <= 0:
        return None, {}
    score = int(round(num / used * 100.0))
    return max(0, min(100, score)), factors


# ---------------------------------------------------------------------------
# Risk
# ---------------------------------------------------------------------------
def _pct_delta(before, after):
    """Percentage change (signed). before<=0 or missing -> None."""
    try:
        b = float(before)
        a = float(after)
    except (TypeError, ValueError):
        return None
    if not abs(b) > 1e-12:
        return None
    return (a - b) / abs(b) * 100.0


def _risk(report, cfg):
    """Weighted risk score. Returns (score, factors_dict)."""
    weights = dict((cfg.get('risk') or {}).get('weights') or {})
    if not weights:
        return None, {}
    scale = float((cfg.get('risk') or {}).get('delta_scale_percent', 25) or 25)
    comp_pen = float((cfg.get('risk') or {}).get('component_loss_penalty', 30) or 30)
    if scale <= 0:
        scale = 25.0

    s1 = report.get('stage1') or {}
    factors = {}

    def _add(name, risk01, detail):
        """risk01 in [0,1] -> weighted contribution."""
        factors[name] = {'risk': round(max(0.0, min(1.0, risk01)), 3),
                         'detail': detail}

    face = _pct_delta(s1.get('faces_before'), s1.get('faces_after'))
    if face is not None:
        _add('face_delta', abs(face) / scale, '%.1f%%' % face)

    vert = _pct_delta(s1.get('vertices_before'), s1.get('vertices_after'))
    if vert is not None:
        _add('vertex_delta', abs(vert) / scale, '%.1f%%' % vert)

    # component change: 1.0 if components were lost or merged (or new ones
    # appeared); 0 if unchanged. component_loss_penalty is a fixed bump for
    # an actual loss/merge, applied on top of the weighted sum.
    cb = s1.get('components_before')
    ca = s1.get('components')
    if isinstance(cb, (int, float)) and not isinstance(cb, bool) \
            and isinstance(ca, (int, float)) and not isinstance(ca, bool):
        changed = int(ca) != int(cb)
        lost = int(cb) > int(ca)
        detail = '%d -> %d' % (int(cb), int(ca))
        if changed:
            _add('component_change', 1.0, detail)
        else:
            _add('component_change', 0.0, detail)
    else:
        lost = False

    vol = s1.get('volume_change_percent')
    if isinstance(vol, (int, float)) and not isinstance(vol, bool):
        _add('volume_delta', abs(float(vol)) / scale, '%.1f%%' % float(vol))

    if not factors:
        return None, {}

    total_w = sum(max(float(w), 0.0) for w in weights.values())
    used = 0.0
    num = 0.0
    for name, weight in weights.items():
        if name not in factors:
            continue
        w = max(float(weight), 0.0)
        used += w
        num += factors[name]['risk'] * w
    if used <= 0:
        return None, {}
    score = num / used * 100.0
    if lost:
        score += comp_pen
    score = max(0, min(100, int(round(score))))
    return score, factors


# ---------------------------------------------------------------------------
# Status (two-axis tier lookup table - NOT an if/elif chain)
# ---------------------------------------------------------------------------
def _tier(value, boundaries):
    """Classify a score into a tier name given {name: lower_bound} pairs."""
    tier = 'low'
    for name, lower in sorted(boundaries.items(), key=lambda kv: -kv[1]):
        if value >= float(lower):
            tier = name
            break
    return tier


def _status(health, risk, cfg):
    """Derive the status (code, label) from the two-axis tier combination."""
    st = cfg.get('status') or {}
    labels = st.get('labels') or _DEFAULTS['status']['labels']
    table = st.get('table') or _DEFAULTS['status']['table']

    if health is None or risk is None:
        return 'unavailable', labels.get('unavailable', 'Scoring unavailable')

    ht = _tier(health, st.get('health_tiers') or _DEFAULTS['status']['health_tiers'])
    rt = _tier(risk, st.get('risk_tiers') or _DEFAULTS['status']['risk_tiers'])
    code = table.get('%s_%s' % (ht, rt), 'unavailable')
    return code, labels.get(code, code)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------
def compute_scores(report, config=None):
    """Compute health, risk and status for a repair report dict.

    Returns {'health': int|None, 'health_factors': dict,
             'risk': int|None, 'risk_factors': dict,
             'status': str, 'status_code': str}.

    Never raises: any invalid metric is skipped (weight redistributed to the
    remaining factors); a total failure yields None scores and the
    'unavailable' status. The caller (repair.py) also wraps the call so
    scoring can never break a repair.
    """
    if config is None:
        config = load_config()
    try:
        health, hf = _health(report, config)
        risk, rf = _risk(report, config)
        code, label = _status(health, risk, config)
        return {
            'health': health,
            'health_factors': hf,
            'risk': risk,
            'risk_factors': rf,
            'status': label,
            'status_code': code,
        }
    except Exception:
        return {
            'health': None,
            'health_factors': {},
            'risk': None,
            'risk_factors': {},
            'status': 'Scoring unavailable',
            'status_code': 'unavailable',
        }