"""Repair confidence scoring for Sutura reports.

stdlib-only by design (same rule as classification.py / defects.py /
mesh_classifier.py): the CLI runs under the PyMeshLab venv while the GUI
shells out to it, so both sides can import this module regardless of which
interpreter is running. Keep it free of numpy/pymeshlab/manifold3d.

Two entry points:

  repair_confidence(report)                 - post-repair score for a full
                                              repair report (stage1/stage2/
                                              defects/category...).
  estimate_confidence_pre_repair(signals)   - pre-repair estimate for the
                                              read-only modes (validate /
                                              --dry-run). Uses ONLY signals
                                              known before repair
                                              (detected_type/confidence,
                                              tuning, mode, input holes /
                                              self-intersections, stage 2
                                              bridge availability); missing
                                              keys are ignored so validate
                                              and dry-run share one function.

Both return {'score': int 0..100, 'label': 'high'|'medium'|'low',
             'factors': {signal: contribution}}.

The weighting is deliberately simple and debuggable (see the per-factor
comments): a baseline score is adjusted by each signal and clamped. These
numbers are an honest assessment, not a metric; the `factors` dict makes the
reasoning visible in the report so a surprising score can be traced.
"""
from classification import classify

# Class-specific tuning gates, mirroring repair.MECH_TUNE_GATE /
# repair.ORG_TUNE_GATE (cannot import repair here: it pulls in numpy and the
# PyMeshLab venv). Keep these in sync with repair.py.
_MECH_GATE = 0.75
_ORG_GATE = 0.55

# Baseline for the post-repair score. Low enough that a bare warning report
# (no stage 2, holes remaining) lands in medium/low without extra penalties.
_POST_BASELINE = 60

# Baseline for the pre-repair estimate: nothing has been validated yet, so it
# starts lower than a confirmed post-repair report and only falls from there.
_PRE_BASELINE = 70

_LABEL_HIGH = 80
_LABEL_MEDIUM = 50


def _label(score):
    if score >= _LABEL_HIGH:
        return 'high'
    if score >= _LABEL_MEDIUM:
        return 'medium'
    return 'low'


def _clamp(score):
    return max(0, min(100, score))


def repair_confidence(report):
    """Post-repair confidence score for a full repair report dict.

    Anchored on the classification category (classification.classify is the
    single source of truth for category, so the score can never disagree with
    it): watertight -> high baseline, warning -> medium, error -> forced
    0/low. The additional signals refine the score from there.

    A hard error (malformed input, extreme_removed_object, ...) is always
    score 0 / 'low' regardless of any other field.
    """
    factors = {}
    category, _issues, _key = classify(report)

    if category == 'error':
        return {'score': 0, 'label': 'low',
                'factors': {'category_error': -100}}

    s1 = report.get('stage1') or {}
    s2 = report.get('stage2')

    # Anchor: watertight (stage 2 confirmed the closed solid) is the dominant
    # signal; a warning leaves a lot of doubt, so it starts lower.
    if category == 'watertight':
        score = _POST_BASELINE + 25
        factors['stage2_watertight'] = 25
    elif category == 'warning':
        score = _POST_BASELINE
        factors['warning_category'] = 0
    else:
        score = _POST_BASELINE - 20
        factors['unknown_category'] = -20

    # Stage 2 outcome: an explicit skip/error is a big trust hit even when
    # stage 1 closed the mesh (classification.py treats it as a warning, never
    # watertight). A closed-but-unconfirmed mesh also loses points.
    s2_skipped = bool(
        s2 and 'error' in s2 and isinstance(s2.get('error'), str)
        and s2['error'].startswith('Stage 2 skipped'))
    s2_error = bool(s2 and 'error' in s2 and not s2_skipped)
    if s2_error:
        score -= 35
        factors['stage2_error'] = -35
    elif s2_skipped:
        score -= 25
        factors['stage2_skipped'] = -25
    elif not (s2 and 'ok' in s2):
        watertight_s1 = bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0
        if watertight_s1:
            # stage 1 closed it but stage 2 never confirmed it
            score -= 25
            factors['stage2_unconfirmed'] = -25
        else:
            # stage 2 was never applicable (stage 1 left it open)
            score -= 5
            factors['stage2_not_applicable'] = -5

    # Remaining holes: -8 each (capped) — the more open holes, the less
    # reliable the result for printing.
    holes_remaining = s1.get('holes_remaining', 0)
    if holes_remaining:
        hole_pen = min(int(holes_remaining) * 8, 24)
        score -= hole_pen
        factors['holes_remaining'] = -hole_pen
    else:
        score += 5
        factors['no_holes_remaining'] = 5

    # Input defects (from defects.detect) all closed? Small bonus; anything
    # left unclosed is a small penalty.
    defects = report.get('defects') or {}
    in_holes = len(defects.get('holes', []))
    if in_holes and holes_remaining == 0:
        score += 5
        factors['all_input_holes_closed'] = 5
    elif in_holes:
        score -= 5
        factors['input_holes_remaining'] = -5

    # Classifier: an 'unknown' type means the tuning was not principled; a
    # known type is penalized by how far its confidence is from 1.0.
    mtype = report.get('detected_type')
    if mtype == 'unknown':
        score -= 10
        factors['unknown_type'] = -10
    elif report.get('detected_confidence') is not None:
        conf = min(max(float(report['detected_confidence']), 0.0), 1.0)
        pen = int(round((1.0 - conf) * 10))
        if pen:
            score -= pen
            factors['classifier_confidence'] = -pen

    # Tuning status: tuned thresholds clear the gate (good); defaults used
    # (below the gate or a fixed mode) is a small caveat.
    if report.get('tuning_applied') is True:
        score += 3
        factors['tuning_applied'] = 3
    elif report.get('tuning_applied') is False:
        score -= 5
        factors['tuning_not_applied'] = -5

    # Aggressive modes remove more geometry by design.
    mode = report.get('repair_mode')
    if mode == 'extreme':
        score -= 10
        factors['extreme_mode'] = -10
    elif mode == 'aggressive':
        score -= 5
        factors['aggressive_mode'] = -5

    # Self-intersections were found and removed during the extreme passes:
    # handled, but a known imperfection.
    if report.get('self_intersections_removed'):
        score -= 3
        factors['self_intersections_removed'] = -3

    # Large volume change is a strong caveat (classification.py flags it).
    if s1.get('volume_warning'):
        score -= 15
        factors['volume_warning'] = -15

    score = _clamp(score)
    return {'score': score, 'label': _label(score), 'factors': factors}


def estimate_confidence_pre_repair(signals):
    """Pre-repair confidence estimate for validate / --dry-run.

    Uses ONLY signals available before repair; any missing key is simply
    ignored (no penalty), so validate (no mode/tuning) and --dry-run (all of
    them) share one function. `signals` may carry:
      detected_type, detected_confidence, tuning_applied, mode,
      holes (input hole count), non_manifold (input region count),
      self_intersections (input self-intersecting face count),
      stage2_bridge_available.
    """
    factors = {}
    score = _PRE_BASELINE

    mtype = signals.get('detected_type')
    if mtype == 'unknown':
        score -= 10
        factors['unknown_type'] = -10
    elif mtype in ('mechanical', 'organic'):
        score += 5
        factors['type_known'] = 5

    conf = signals.get('detected_confidence')
    if conf is not None:
        c = min(max(float(conf), 0.0), 1.0)
        gate = {'mechanical': _MECH_GATE, 'organic': _ORG_GATE}.get(mtype)
        if gate is not None and c < gate:
            score -= 5
            factors['below_confidence_gate'] = -5

    tuning = signals.get('tuning_applied')
    if tuning is True:
        score += 5
        factors['tuning_applied'] = 5
    elif tuning is False:
        score -= 5
        factors['tuning_not_applied'] = -5

    mode = signals.get('mode')
    if mode == 'extreme':
        score -= 10
        factors['extreme_mode'] = -10
    elif mode == 'aggressive':
        score -= 5
        factors['aggressive_mode'] = -5

    holes = signals.get('holes')
    if holes is not None:
        if holes == 0:
            score += 10
            factors['no_input_holes'] = 10
        elif int(holes) > 3:
            score -= 10
            factors['many_input_holes'] = -10

    nm = signals.get('non_manifold')
    if nm:
        score -= 5
        factors['non_manifold'] = -5

    si = signals.get('self_intersections')
    if si:
        score -= 8
        factors['self_intersections'] = -8

    bridge = signals.get('stage2_bridge_available')
    if bridge is False:
        # watertight can never be confirmed on this system
        score -= 15
        factors['stage2_unavailable'] = -15
    elif bridge is True:
        score += 5
        factors['stage2_available'] = 5

    score = _clamp(score)
    return {'score': score, 'label': _label(score), 'factors': factors}