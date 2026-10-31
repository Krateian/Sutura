# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Repair presets and configuration resolution (P1a split).

Mode/profile thresholds, the classifier-engine + tuning gates, per-tier
tunables and the CLI/env/config resolvers for every repair mode. Moved
verbatim from sutura/repair.py; no logic change.
"""
import json
import os
import sys

import numpy as np

import triage
from mesh_classifier import classify_mesh
from sutura_engine.policy import TIERS as _TIERS
from sutura_engine.policy import GATES as _GATES
from sutura_engine.policy import (
    MODES as _MODES, STAGE1_PROFILES as _STAGE1_PROFILES,
    STAGE1_TUNING as _STAGE1_TUNING, STAGE1_TYPES as _STAGE1_TYPES,
)

_TUNING_GATES = _GATES['tuning']

_DRESSING = _TIERS['dressing']
_DRESSING_GATES = _DRESSING['gates']
_SPLINT = _TIERS['splint']
_COAPTATION = _TIERS['coaptation']


def _thresholds(table):
    return {name: {'mincomponentsize': int(v['mincomponentsize']),
                   'maxholesize': int(v['maxholesize'])}
            for name, v in table.items()}


def _work_table(table):
    return {name: int(v) for name, v in table.items()}


# The Balanced intensity preset is the pre-triage behaviour; the module
# constants below keep their names for backward compatibility and are derived
# from it, so repair.py has a single source of truth (sutura/triage.py).
_BALANCED_INTENSITY = triage.PRESETS[triage.DEFAULT_INTENSITY]


SUTURA_DIR = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))


VENV311 = os.path.join(SUTURA_DIR, 'venv311', 'bin', 'python')


# Classifier engines. 'experimental' is mesh_classifier_v2 (RANSAC plane
# features + a small trained head) and is the DEFAULT: on the labeled
# synthetic set (scripts/calibrate_classifier.py) and a 16-mesh real corpus
# it is measurably better than the classic heuristic (mechanical recall
# 2/7 -> 6/7, no organic regression). 'classic' (mesh_classifier) stays
# selectable via --classifier-engine classic or SUTURA_CLASSIFIER_ENGINE.
# Experimental is NOT protected against a confident-but-wrong prediction --
# the fallback below only covers exceptions and invalid results. That is an
# accepted risk because classic remains a one-flag escape hatch.
CLASSIFIER_ENGINES = ('classic', 'experimental')


def resolve_classifier_engine(cli_value=None):
    """Resolve the requested classifier engine.

    Precedence: explicit ``--classifier-engine`` flag > the
    ``SUTURA_CLASSIFIER_ENGINE`` env var > ``'experimental'`` (the default).
    An invalid value in the env var (not classic/experimental) falls back to
    the default engine -- the env var is ambient and must never crash the
    pipeline; an invalid CLI value is rejected by argparse instead.
    """
    value = cli_value
    if value is None:
        value = os.environ.get('SUTURA_CLASSIFIER_ENGINE')
    if value not in CLASSIFIER_ENGINES:
        if cli_value is not None:
            raise ValueError('unknown classifier engine: %r' % cli_value)
        return 'experimental'
    return value


def classify_with_engine(verts, tris, engine='experimental', extra_features=False):
    """Run classify_mesh with the selected engine. Returns ``(result, used)``
    where ``used`` is the engine that actually produced the result.

    ``experimental`` (default): mesh_classifier_v2; if it raises OR returns
    an invalid/incomplete result (missing keys, NaN/non-finite confidence,
    unexpected type), it silently falls back to classic with a warning
    logged -- never a crash. ``classic``: mesh_classifier, unchanged.
    ``extra_features`` enables the opt-in edge-tiebreak head (v2 only;
    ignored by classic).
    """
    if engine != 'experimental':
        return classify_mesh(verts, tris), 'classic'
    try:
        import mesh_classifier_v2 as v2_engine
        r = v2_engine.classify_mesh(verts, tris, extra_features=extra_features)
        if not isinstance(r, dict):
            raise ValueError('classifier result is not a dict')
        if r.get('type') not in ('mechanical', 'organic', 'unknown'):
            raise ValueError('unexpected classifier type: %r' % r.get('type'))
        conf = r.get('confidence')
        if not isinstance(conf, (int, float)) or not np.isfinite(conf):
            raise ValueError('invalid classifier confidence: %r' % conf)
        return r, 'experimental'
    except Exception as e:
        print('warning: experimental classifier failed (%s); '
              'falling back to classic' % e, file=sys.stderr)
        return classify_mesh(verts, tris), 'classic'


class ExtremeRemovedAllError(ValueError):
    """Raised when a repair leaves the mesh with ZERO faces because extreme
    mode's mincomponentsize=20 deleted every connected component (a small
    mesh, e.g. the 13-face broken.stl cube, all below the threshold). This is
    the intended-but-aggressive extreme behaviour, NOT malformed input, so it
    is reported distinctly from the generic 'all faces are degenerate' error.
    """


# Confidence gate for mesh-type-aware Stage 1 tuning: a classified mesh only
# gets its per-type thresholds (see _type_params in repair_mesh_from_arrays)
# when the classifier is reasonably sure; below the gate we use the historical
# default thresholds while still REPORTING the detected type.
# Values derived from scripts/calibrate_classifier.py on the CORRECTED dihedral
# metric (the organic confidence is no longer capped at ~0.62 -- that cap was
# an artifact of a face-indexing bug in mesh_classifier._dihedral_stats, since
# fixed): mechanical confidence bottoms out at 0.867, organic at 0.693. Both
# gates sit just above the corresponding worst correct prediction, so the
# single most-ambiguous organic (a 24k-tri smooth capsule, 0.693) stays on
# defaults as a safety margin while everything else tunes.
MECH_TUNE_GATE = float(_STAGE1_TUNING['mech_gate'])


ORG_TUNE_GATE = float(_STAGE1_TUNING['org_gate'])


def tuning_applied_for(mesh_type, confidence):
    """Whether the tuned Stage 1 thresholds should be used for a classified
    mesh, given the confidence gate. Unknown always returns False (defaults
    are used); a classified mesh must clear its class-specific gate. Returns
    a plain Python bool so the value is JSON-serializable."""
    if mesh_type == 'mechanical':
        return bool(confidence >= MECH_TUNE_GATE)
    if mesh_type == 'organic':
        return bool(confidence >= ORG_TUNE_GATE)
    return False


# Repair modes: a fixed five-step ladder from conservative to aggressive.
# 'auto' is NOT a fixed parameter set - it resolves at repair time through
# classify_mesh + the confidence gate (the shipped default behaviour); the
# fixed modes (low/medium/aggressive/extreme) bypass the classifier entirely
# and use these exact thresholds. 'medium' is the historical default
# (mincomponentsize=8, maxholesize=1000). mincomponentsize stays >= 8 so
# small/degenerate meshes never survive the debris cutoff (CI regression
# risk if lowered, see the _type_params comment).
MODE_PARAMS = _thresholds(_MODES)


REPAIR_MODES = ('low', 'medium', 'auto', 'aggressive', 'extreme')


# Repair profiles: named Stage 1 threshold presets for a repair character.
# They are OPT-IN (via --profile / the GUI dropdown) and only take effect
# when the mode is 'auto' -- an explicit fixed mode always wins (it is the
# more specific aggressiveness control). Auto + profile bypasses the mesh
# classifier for parameter selection (it still runs for the informative
# detected_type/confidence). Default behaviour (no --profile) is unchanged.
#
#   mechanical : precise parts, modest hole fill (same as the auto
#                mechanical type values).
#   organic    : drop scan debris harder, close large regions.
#   scan       : aggressive debris + huge hole fill for scan meshes.
#   miniature  : keep tiny parts (mincomponentsize=1, an explicit opt-in --
#                the default path deliberately keeps >= 8).
#   fast       : quick pass, small holes only.
PROFILES = _thresholds(_STAGE1_PROFILES)


REPAIR_PROFILES = tuple(PROFILES)


# Mesh-type-aware Stage 1 thresholds (organic vs mechanical).
#
# These per-type values are ESTIMATED starting points, not calibrated on
# real repair data - a deliberate, conservative, reversible choice. They
# only shift debris/hole-closing thresholds; the classifier reports
# 'unknown' in ambiguous cases and we keep the default parameters, so a
# wrong guess cannot badly distort a mesh.
#
#   mechanical: avoid oversized hole fill on precise geometry (300).
#               mincomponentsize is kept at the default 8 (not lowered):
#               lowering it to 4 let small/degenerate meshes (e.g. the
#               2-triangle case in tests/test_adversarial.py) survive the
#               debris cutoff and be "repaired" instead of rejected - a
#               CI regression (test_adversarial 'degenerate').
#   organic   : aggressively drop scan debris (higher cutoff, 12) and
#               close large open regions (1000, same as default).
#   unknown   : fall back to the historical defaults (8, 1000).
_TYPE_PARAMS = _thresholds(_STAGE1_TYPES)


def resolve_mode_params(mode, mesh_type, confidence, profile=None):
    """Resolve the Stage 1 thresholds for a repair/dry-run run.

    Single source of truth shared by the real repair chain and --dry-run so
    they can never diverge (same principle as classification.py). Returns
    ({mincomponentsize, maxholesize}, tuning_applied).

    ``mode`` is one of REPAIR_MODES: the fixed modes (low/medium/aggressive/
    extreme) use MODE_PARAMS directly (the classifier still runs for the
    informative detected_type/confidence, but does not drive parameters);
    ``auto`` uses the mesh classifier + the class-specific confidence gate,
    UNLESS a ``profile`` is given, in which case the profile's thresholds are
    used (classifier still runs for info). An explicit fixed ``mode`` always
    wins over a profile.
    """
    if mode != 'auto':
        return dict(MODE_PARAMS[mode]), False
    if profile is not None:
        return dict(PROFILES[profile]), False
    applied = tuning_applied_for(mesh_type, confidence)
    if not applied:
        return dict(_TYPE_PARAMS['unknown']), False
    return dict(_TYPE_PARAMS.get(mesh_type, _TYPE_PARAMS['unknown'])), True


TOPOMETRICS = [
    'vertices_number', 'faces_number', 'boundary_edges', 'connected_components_number',
    'genus', 'incident_faces_on_non_two_manifold_edges',
    'incident_faces_on_non_two_manifold_vertices', 'is_mesh_two_manifold',
    'non_two_manifold_edges', 'non_two_manifold_vertices', 'number_holes',
]


# Wall-clock budget for ONE fTetWild bridge call (per mesh). fTetWild is fast
# on most meshes but default settings can stall for a long time on dense 3D
# scans (measured: one artec_* mesh did not finish within ~2h), so the fallback
# is time-boxed per mesh and a timeout is recorded as ftetwild_error='timeout'
# instead of blocking the whole batch (FAZ17). The active budget comes from the
# intensity spec (triage.py); this constant is the Balanced default.
FTETWILD_TIMEOUT = _BALANCED_INTENSITY.ftetwild_timeout


# Shape flag of the fTetWild tier: an adopted result whose one-sided
# output-to-input Hausdorff distance exceeds this fraction of the input
# bounding-box diagonal is still adopted (it is closed where stage 1 left it
# open) but reported with shape_changed=True and the issue code
# 'shape_changed'. Measured on macOS (2026-09-26): 3.7 % / 9.6 % on
# thingi10k_1038441 (40 samples / corpus), 1.7 % on 1038439, 1.1 % on
# 224108, 5 % on 1017012. Rejecting them instead cost 3 of the 40 samples
# and 4 corpus meshes. Provisional value.
FTETWILD_MAX_HAUSDORFF_REL = float(_TUNING_GATES['ftetwild_max_hausdorff_rel'])


# Second fTetWild attempt: when the default (optimize=False) boundary fails
# the holes/non-manifold guard even after the Manifold post-process, the
# mesh is tetrahedralized once more with the quality optimisation on.  The
# retry rule is DETERMINISTIC (it depends only on the first boundary's
# holes/non-manifold verdict, never on the elapsed wall clock).
# thingi10k_1038444 was adopted with the optimisation and rejected
# ('holes_nm') without it (macOS, 2026-09-26).
FTETWILD_RETRY_PARAMS = {'optimize': True}


# Outer wall-clock abort message (shared by the report and the human/GUI line).
# The wall clock only bounds the WHOLE tier; when it trips the tier is
# abandoned all-or-nothing and the previous stage (stage-1 / deep-ladder)
# result is kept -- it never picks a different (partial) attempt.
FTETWILD_ABORT_MESSAGE = (
    "fTetWild was stopped for time (%s budget, checked between attempts); "
    "the previous stage result was kept")


FTETWILD_ABORT_MESSAGE_TR = (
    "fTetWild süre nedeniyle durduruldu (%s bütçe, denemeler arasında "
    "denetlenir); önceki aşama sonucu korundu")


# Input size above which the fTetWild tier is not started
# (reject_reason='too_large'). The limit follows from the budget: the
# slowest completed runs on the 40 real-world samples took ~55 s
# (thingi10k_100281) at 90,000 faces, so even with linear scaling a mesh
# above ~300,000 faces cannot finish in FTETWILD_TIMEOUT=180 s. The six
# Artec scans of the 115-mesh corpus that reach the tier (dense scans of
# millions of faces) timed out at 180 s in every run; skipping them saves
# ~18 min per corpus run. The 90k-face samples (the largest of the 40)
# stay below the limit. Provisional; the corpus face counts are not in the
# repository, the benchmark's input_faces column verifies the split. The active
# cap comes from the intensity spec (None = no cap, Extreme).
FTETWILD_MAX_FACES = _BALANCED_INTENSITY.ftetwild_max_faces


# Bound for the Graft (#13) shell-wrap tier, the morphology sibling of the
# fTetWild tier above. Graft has no equivalent of fTetWild's solver timeout:
# it runs an r-ladder of ``morph_close`` attempts on the ORIGINAL 1-6 M-face
# scans, so without a bound the six dense Artec scans of the 115-mesh corpus
# run past the 600 s harness timeout (they do not reach watertight either).
# GRAFT_MAX_FACES is the hard input cap (a single ``morph_close`` call is not
# interruptible, so a between-attempt budget cannot bound it); GRAFT_TIMEOUT
# is the wall-clock budget for the whole ladder, checked between attempts.
# 2,000,000 faces sits above every mesh Graft adopts today (largest:
# ornate-frame, 396,549 faces; Graft's own tier time there was 133 s unloaded)
# and below the smallest observed hang (2,110,072 faces), so it changes only
# the six timeouts. The active values come from the intensity spec (None cap =
# no limit, Extreme). Provisional; the corpus face counts are not in the repo.
GRAFT_MAX_FACES = _BALANCED_INTENSITY.graft_max_faces


GRAFT_TIMEOUT = _BALANCED_INTENSITY.graft_timeout


# Bound for the Splint (#18) exact solid-rebuild tier, the last-resort tier
# AFTER Dressing and BEFORE fTetWild.  Splint runs the Rust ``splint_cut`` core
# (proxy surface + Delaunay tetrahedralization + tet cut) on the ORIGINAL input
# arrays.  Like Graft it has no interruptible solver: SPLINT_MAX_FACES is the
# hard input cap (above it the tier refuses rather than start), SPLINT_RES caps
# the sampling resolution (the main runtime bound; ``adaptive='retry'`` only
# reruns after the hardened pass), and SPLINT_WORK_BY_INTENSITY is a
# DETERMINISTIC work budget in Delaunay tetrahedra.  The former wall-clock
# timeout was a post-hoc rejection; the mesh now depends only on the input,
# preset and tet count.  SPLINT_DEFAULT_ENABLED is the single switch and stays
# OFF until the 40-mesh corpus decision proves Splint no worse on every mesh.
SPLINT_RES = int(_SPLINT['res'])

SPLINT_MAX_FACES = _BALANCED_INTENSITY.ftetwild_max_faces

SPLINT_WORK_BY_INTENSITY = _work_table(_SPLINT['work_by_intensity'])

SPLINT_DEFAULT_WORK = int(_SPLINT['default_work'])

SPLINT_DEFAULT_ENABLED = bool(_SPLINT['default_enabled'])

SPLINT_ENV = 'SUTURA_SPLINT'


# Coaptation (#19) tolerance seam-weld + per-body union tier.  It runs on the
# ORIGINAL input arrays: the reload-equivalent soup is split into bodies, seam
# vertices within COAPTATION_SEAM_ULP_K float32 ULPs of the largest coordinate
# are merged, each body is closed and the bodies are folded with the exact
# union.  It sits after Splint and before fTetWild and is ON by default because
# its result is adopted only when it is STRICTLY better than the result the
# ladder currently holds (so a mesh that does not benefit is byte-identical).
# COAPTATION_MAX_FACES is a hard input cap; COAPTATION_WORK_BY_INTENSITY is a
# DETERMINISTIC work budget in total body faces closed/merged by the tier.
COAPTATION_SEAM_ULP_K = int(_COAPTATION['seam_ulp_k'])

COAPTATION_MAX_FACES = _BALANCED_INTENSITY.ftetwild_max_faces

COAPTATION_WORK_BY_INTENSITY = _work_table(_COAPTATION['work_by_intensity'])

COAPTATION_DEFAULT_WORK = int(_COAPTATION['default_work'])

COAPTATION_DEFAULT_ENABLED = bool(_COAPTATION['default_enabled'])

COAPTATION_ENV = 'SUTURA_COAPTATION'


def _coaptation_default_enabled():
    """The single Coaptation-default switch: module constant overridden by the
    ``SUTURA_COAPTATION_DEFAULT`` env var (truthy = run the tier by default)."""
    raw = os.environ.get('SUTURA_COAPTATION_DEFAULT')
    if raw is None:
        return bool(COAPTATION_DEFAULT_ENABLED)
    return str(raw).strip().lower() in ('1', 'true', 'yes', 'on')


def resolve_coaptation(no_coaptation=False, force=False, environ=None,
                       default_enabled=None):
    """Resolve the ``coaptation`` argument (method #19, seam-weld union).

    Returns ``False`` (disabled), ``True`` (forced) or ``'auto'`` (the ladder
    fallback: tried on every full-mode input, adopting only a strictly better
    union).  Precedence: ``--no-coaptation`` disables; ``--coaptation`` forces;
    ``SUTURA_COAPTATION`` may set ``0/1``.  With none of those set the answer
    follows the single default switch (:data:`COAPTATION_DEFAULT_ENABLED`, ON,
    or the ``SUTURA_COAPTATION_DEFAULT`` env var; ``default_enabled`` overrides
    both for tests).  The library default in ``repair_mesh_from_arrays`` stays
    ``False``.
    """
    if no_coaptation:
        return False
    if force:
        return True
    env = environ if environ is not None else os.environ.get(COAPTATION_ENV)
    if env is not None:
        val = str(env).strip().lower()
        if val in ('0', 'false', 'no', 'off'):
            return False
        if val in ('1', 'true', 'yes', 'on'):
            return True
    if default_enabled is None:
        default_enabled = _coaptation_default_enabled()
    return 'auto' if default_enabled else False


# Decimation of a wastefully dense fTetWild boundary. fTetWild with
# optimize=False can return a boundary far denser than the input (measured on
# thingi10k_73444: 473,212 output faces at 7,882 input faces, against 14,012
# with optimize=True at the same solver time); the extra faces cost ~65 s of
# downstream work (Manifold, Hausdorff, self-intersection/boundary passes)
# and carry no more shape information. After such an attempt the boundary is
# decimated with a pymeshlab quadric edge collapse before the Manifold
# post-process. The face counts below are only a SIZE BUDGET (is this output
# wastefully dense?); the quality gate is the Hausdorff comparison against the
# raw fTetWild boundary (DECIMATE_HD_MARGIN), so a coarse target is accepted
# whenever it does not make the shape measurably worse.
DENSE_RATIO = _BALANCED_INTENSITY.dense_ratio


DENSE_MIN_FACES = _BALANCED_INTENSITY.dense_min_faces


# Escalating target ladder: multiples of the input face count, tried in order;
# the special entry 'threshold' means dense_ratio * max(input, dense_min_faces).
# The active ladder comes from the intensity spec.
DENSE_TARGET_LADDER = _BALANCED_INTENSITY.dense_target_ladder


# Quadric edge collapse quality threshold (0..1); 0.3 favours boundary/shape
# preservation over aggressive simplification.
DENSE_QUALITY = 0.3


# A decimated candidate is accepted only when its one-sided Hausdorff distance
# (relative to the input bbox diagonal) is at most the raw fTetWild boundary's
# own distance plus this margin: decimation may not make the shape measurably
# worse than fTetWild already did. 0.5 % of the diagonal.
DECIMATE_HD_MARGIN = float(_TUNING_GATES['decimate_hd_margin'])


# Fixed Hausdorff sample count, independent of the output face count, so a
# dense output cannot inflate the measurement cost (the previous inline
# formula scaled the sample count with the output size up to this cap). The
# active count comes from the intensity spec (never below 200000).
HAUSDORFF_SAMPLES = _BALANCED_INTENSITY.ftetwild_hausdorff_samples


def resolve_ftetwild(no_fallback=False, experimental=False):
    """CLI flags -> the ``ftetwild`` argument: the opt-out wins, the
    experimental flag forces the tier, otherwise 'auto'."""
    if no_fallback:
        return False
    if experimental:
        return True
    return 'auto'


def resolve_graft(no_graft=False, force=False, environ=None):
    """Resolve the ``graft`` argument (method #13, shell wrap).

    ``--no-graft`` disables it; ``--experimental-graft`` forces it; the
    environment variable ``SUTURA_GRAFT`` may set ``0/1`` (env value has lower
    precedence than an explicit CLI flag); otherwise 'auto' (tried by the
    deep-repair ladder before fTetWild). The library default in
    ``repair_mesh_from_arrays`` stays ``False`` so direct library calls keep
    their pre-Graft behaviour.
    """
    if no_graft:
        return False
    if force:
        return True
    env = environ if environ is not None else os.environ.get('SUTURA_GRAFT')
    if env is not None:
        val = str(env).strip().lower()
        if val in ('0', 'false', 'no', 'off'):
            return False
        if val in ('1', 'true', 'yes', 'on', 'auto'):
            return True if val not in ('auto',) else 'auto'
    return 'auto'


def _splint_default_enabled():
    """The single Splint-default switch: module constant overridden by the
    ``SUTURA_SPLINT_DEFAULT`` env var (truthy = run the tier by default)."""
    raw = os.environ.get('SUTURA_SPLINT_DEFAULT')
    if raw is None:
        return bool(SPLINT_DEFAULT_ENABLED)
    return str(raw).strip().lower() in ('1', 'true', 'yes', 'on')


def resolve_splint(no_splint=False, force=False, environ=None,
                   default_enabled=None):
    """Resolve the ``splint`` argument (method #18, exact solid rebuild).

    Returns ``False`` (disabled), ``True`` (forced) or ``'auto'`` (the ladder
    fallback: tried only when the result the ladder holds still has holes or
    non-manifold edges).  Precedence: ``--no-splint`` disables;
    ``--experimental-splint`` forces; ``SUTURA_SPLINT`` may set ``0/1``.  With
    none of those set the answer follows the single default switch
    (:data:`SPLINT_DEFAULT_ENABLED`, OFF, or the ``SUTURA_SPLINT_DEFAULT`` env
    var; ``default_enabled`` overrides both for tests).  The library default in
    ``repair_mesh_from_arrays`` stays ``False``.
    """
    if no_splint:
        return False
    if force:
        return True
    env = environ if environ is not None else os.environ.get(SPLINT_ENV)
    if env is not None:
        val = str(env).strip().lower()
        if val in ('0', 'false', 'no', 'off'):
            return False
        if val in ('1', 'true', 'yes', 'on'):
            return True
    if default_enabled is None:
        default_enabled = _splint_default_enabled()
    return 'auto' if default_enabled else False


FLAP_ENV = 'SUTURA_FLAP'


# Single default switch for the Flap pre-pass.  Flap is ON by default: the
# module constant below, overridable by the SUTURA_FLAP_DEFAULT env var.  The
# CLI honours --no-flap to disable it per run; the library default in
# ``repair_mesh_from_arrays`` stays ``False`` so direct library calls keep
# their pre-Flap behaviour.
FLAP_DEFAULT_ENABLED = bool(_TIERS['flap']['default_enabled'])


def _flap_default_enabled():
    """The single Flap-default switch: module constant overridden by the
    ``SUTURA_FLAP_DEFAULT`` env var (truthy = run the pre-pass by default)."""
    raw = os.environ.get('SUTURA_FLAP_DEFAULT')
    if raw is None:
        return bool(FLAP_DEFAULT_ENABLED)
    return str(raw).strip().lower() in ('1', 'true', 'yes', 'on')


def resolve_flap(no_flap=False, force=False, environ=None):
    """Resolve the ``flap`` argument (surface-based hole filler, Stage 1).

    Precedence: ``--no-flap`` disables, ``--flap`` / ``force`` enables,
    ``SUTURA_FLAP`` (``0/1/true/false/...``) overrides both; with none set the
    answer follows the single default switch (:data:`FLAP_DEFAULT_ENABLED` or
    the ``SUTURA_FLAP_DEFAULT`` env var).  The library default in
    ``repair_mesh_from_arrays`` stays ``False`` so direct library calls keep
    their pre-Flap behaviour.
    """
    if no_flap:
        return False
    if force:
        return True
    env = (os.environ if environ is None else environ).get(FLAP_ENV)
    if env is not None:
        val = str(env).strip().lower()
        if val in ('1', 'true', 'yes', 'on'):
            return True
        if val in ('0', 'false', 'no', 'off'):
            return False
    return _flap_default_enabled()


def resolve_dressing(no_dressing=False, force=False, environ=None,
                     default_enabled=None):
    """Resolve the ``dressing`` argument (method #16, viscosity coat).

    Returns ``False`` (disabled), ``True`` (forced, runs even without damage) or
    ``'auto'`` (the Auto-ladder fallback: only when the current result still has
    holes/non-manifold/exact-SI damage).  Precedence: ``--no-dressing`` disables;
    ``--experimental-dressing`` forces; ``SUTURA_DRESSING`` may set ``0/1``.
    With none of those set the answer follows the **single default switch**:
    the :data:`DRESSING_DEFAULT_ENABLED` module constant or the
    ``SUTURA_DRESSING_DEFAULT`` env var (``default_enabled`` overrides both for
    tests).  The library default in ``repair_mesh_from_arrays`` stays ``False``,
    and the switch is OFF until the fine-detail loss is fixed.
    """
    if no_dressing:
        return False
    if force:
        return True
    env = environ if environ is not None else os.environ.get('SUTURA_DRESSING')
    if env is not None:
        val = str(env).strip().lower()
        if val in ('0', 'false', 'no', 'off'):
            return False
        if val in ('1', 'true', 'yes', 'on'):
            return True
    if default_enabled is None:
        default_enabled = _dressing_default_enabled()
    return 'auto' if default_enabled else False


TAMPONADE_ENV = 'SUTURA_TAMPONADE'


# Single default switch for the Tamponade tier (method #17).  ON by default
# (standing rule: improvements ship enabled, opt-out): the module
# constant below, overridable by the ``SUTURA_TAMPONADE_DEFAULT`` env var.
# ``--no-tamponade`` / ``SUTURA_TAMPONADE=0`` opt out; the legacy
# ``--tamponade`` flag is still accepted (it just re-affirms the default).  The
# library default in ``repair_mesh_from_arrays`` stays ``False`` so direct
# library calls keep their pre-Tamponade behaviour -- every user-facing path
# (CLI, GUI, file-manager menu, Orca plugin) passes the flag explicitly or
# inherits this CLI default.
TAMPONADE_DEFAULT_ENABLED = bool(_TIERS['tamponade']['default_enabled'])


def _tamponade_default_enabled():
    """The single Tamponade-default switch: module constant overridden by the
    ``SUTURA_TAMPONADE_DEFAULT`` env var (truthy = run the tier by default)."""
    raw = os.environ.get('SUTURA_TAMPONADE_DEFAULT')
    if raw is None:
        return bool(TAMPONADE_DEFAULT_ENABLED)
    return str(raw).strip().lower() in ('1', 'true', 'yes', 'on')


def resolve_tamponade(no_tamponade=False, force=False, environ=None,
                      default_enabled=None):
    """Resolve the ``tamponade`` argument (method #17, interior plug).

    Precedence: ``--no-tamponade`` disables; ``--tamponade`` / ``force``
    enables; ``SUTURA_TAMPONADE`` (``0/1/true/false/...``) overrides both; with
    none set the answer follows the single default switch
    (:data:`TAMPONADE_DEFAULT_ENABLED` or the ``SUTURA_TAMPONADE_DEFAULT`` env
    var; ``default_enabled`` overrides both for tests).  The library default in
    ``repair_mesh_from_arrays`` stays ``False``.
    """
    if no_tamponade:
        return False
    if force:
        return True
    env = (os.environ if environ is None else environ).get(TAMPONADE_ENV)
    if env is not None:
        val = str(env).strip().lower()
        if val in ('1', 'true', 'yes', 'on'):
            return True
        if val in ('0', 'false', 'no', 'off'):
            return False
    if default_enabled is None:
        default_enabled = _tamponade_default_enabled()
    return bool(default_enabled)


def resolve_dressing_drain(cli_value=None, environ=None):
    """Resolve the Dressing drain override (method #16).

    Returns ``None`` (the preset's ``drain_factor`` default — full for
    Balanced/Thorough, off for Quick, deep for Extreme), a mode name
    (``none``/``half``/``full``/``deep``) or an explicit ``delta_r`` in mm.
    Precedence: CLI flag > ``SUTURA_DRESSING_DRAIN`` > ``None``.
    """
    if cli_value is not None:
        return cli_value
    env = environ if environ is not None else os.environ.get('SUTURA_DRESSING_DRAIN')
    if env is not None:
        val = str(env).strip().lower()
        if val in ('none', 'off', 'half', 'full', 'deep'):
            return val
        try:
            return float(val)
        except ValueError:
            return None
    return None


# Dressing defect-mask sets (must match sutura_engine.dressing.DEFECT_SETS).
# Structural enum, not policy data: it must mirror the runtime DEFECT_SETS, so a
# user override could desync the two. Stays in code.
DRESSING_DEFECT_SETS = ('all', 'holes_nm')


# ---------------------------------------------------------------------------
# Dressing auto-fallback switch + adoption-gate bounds (single switch)
# ---------------------------------------------------------------------------
# Dressing stays OFF by default until the fine-detail (voxel staircase) loss is
# fixed in the extraction core.  One knob turns it into an Auto-ladder fallback:
# the module constant below, or the SUTURA_DRESSING_DEFAULT env var.  When on,
# the deep-repair 'full' ladder tries Dressing after Graft whenever the current
# result still has holes, non-manifold edges or exact self-intersections.
DRESSING_DEFAULT_ENABLED = bool(_DRESSING['default_enabled'])


# Adoption-gate bounds for the auto fallback (the explicit --experimental-
# dressing path uses the same gate).  The volume is compared to the mesh the
# ladder held just before Dressing (the best available reference; the raw input
# signed volume is meaningless when it is itself damaged/open).
DRESSING_MAX_VOLUME_DELTA = float(_DRESSING_GATES['max_volume_delta'])     # |dV| / |V_before|


DRESSING_MAX_PARTS_SLACK = int(_DRESSING_GATES['max_parts_slack'])         # candidate components <= before + slack


# Healthy-region coat-vs-input normal agreement; a voxel staircase / fluting
# loss keeps positions within the Hausdorff gate but tilts the normals.  This is
# a PLACEHOLDER threshold, measured on the 13-mesh set 2026-10: Dressing p95 is
# 12-36 deg on the accepted coats and 82-88 deg on ornate-frame / 100281 /
# 1038439 / 1038441 / 145065 (the staircased, detail-lost ones).  The calibrated
# detector will replace it.
DRESSING_MAX_NORMAL_ANGLE_P95 = float(_DRESSING_GATES['max_normal_angle_p95'])  # degrees


# CAD-likeness placeholder threshold (the calibrated detector supplies the real
# value).
CAD_LIKENESS_THRESHOLD = float(_DRESSING_GATES['cad_likeness_threshold'])


def _dressing_default_enabled():
    """The single Dressing-default switch: module constant overridden by the
    ``SUTURA_DRESSING_DEFAULT`` env var (truthy = enable the Auto fallback)."""
    raw = os.environ.get('SUTURA_DRESSING_DEFAULT')
    if raw is None:
        return bool(DRESSING_DEFAULT_ENABLED)
    return str(raw).strip().lower() in ('1', 'true', 'yes', 'on', 'auto')


def cad_likeness(verts, tris):
    """Heuristic CAD/machined-vs-organic score for the Graft-vs-Dressing guard.

    Returns ``{'score', 'planar_fraction', 'sharp_fraction', 'cad_like'}``.
    Mechanical parts have many flat faces and/or many sharp dihedral edges;
    organic scans do not.  The threshold is a PLACEHOLDER
    (:data:`CAD_LIKENESS_THRESHOLD`) until the calibrated detector lands, and
    the score is currently recorded only — the ladder keeps Graft first for
    every input, which already satisfies the CAD guard (Graft runs before
    Dressing and Dressing is only tried when damage remains).
    """
    try:
        v = np.asarray(verts, dtype=np.float64)
        t = np.asarray(tris, dtype=np.int64)
        F = len(t)
        if F == 0 or len(v) == 0:
            return {'score': 0.0, 'planar_fraction': 0.0,
                    'sharp_fraction': 0.0, 'cad_like': False}
        a, b, c = v[t[:, 0]], v[t[:, 1]], v[t[:, 2]]
        n = np.cross(b - a, c - a)
        ln = np.linalg.norm(n, axis=1)
        ln[ln == 0] = 1.0
        n = n / ln[:, None]
        e = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]])
        fi = np.concatenate([np.arange(F), np.arange(F), np.arange(F)])
        key = np.sort(e, axis=1)
        key = key[:, 0].astype(np.int64) * (int(t.max()) + 1) + key[:, 1]
        order = np.argsort(key, kind='stable')
        ks = key[order]
        fs = fi[order]
        starts = np.flatnonzero(np.r_[True, ks[1:] != ks[:-1]])
        counts = np.diff(np.r_[starts, len(ks)])
        pair = starts[counts == 2]
        f1, f2 = fs[pair], fs[pair + 1]
        dot = np.abs(np.einsum('ij,ij->i', n[f1], n[f2]))
        ang = np.degrees(np.arccos(np.clip(dot, 0.0, 1.0)))
        # true-flat edges (coplanar neighbours) vs sharp edges.  Smooth organics
        # sit in between (gentle curvature), so neither fraction fires.
        flat_edge = ang < 1.0
        sharp_edge = ang > 30.0
        flat_fraction = float(flat_edge.mean()) if len(ang) else 0.0
        sharp_fraction = float(sharp_edge.mean()) if len(ang) else 0.0
        score = max(flat_fraction, sharp_fraction)
        return {'score': score, 'planar_fraction': flat_fraction,
                'sharp_fraction': sharp_fraction,
                'cad_like': bool(score >= CAD_LIKENESS_THRESHOLD)}
    except Exception:  # noqa: BLE001 - the guard is best-effort
        return {'score': None, 'planar_fraction': None,
                'sharp_fraction': None, 'cad_like': False}


def resolve_dressing_defects(cli_value=None, environ=None):
    """Resolve the Dressing defect-set override (method #16).

    Returns ``'all'`` (holes + non-manifold + self-intersections, the default),
    ``'holes_nm'`` (holes + non-manifold only) or ``None`` (the module default).
    Precedence: CLI flag > ``SUTURA_DRESSING_DEFECTS`` > ``None``.  An invalid
    value at either level is ignored.
    """
    if cli_value is not None:
        return cli_value if cli_value in DRESSING_DEFECT_SETS else None
    env = environ if environ is not None else os.environ.get('SUTURA_DRESSING_DEFECTS')
    if env is not None:
        val = str(env).strip().lower()
        if val in DRESSING_DEFECT_SETS:
            return val
    return None


def resolve_dressing_scale(cli_value=None, env_var='', environ=None):
    """Resolve a positive Dressing scale factor (method #16).

    Returns a positive ``float`` or ``None`` (the module default, 1.0).
    Precedence: CLI flag > ``env_var`` > ``None``.  A non-positive or
    unparseable value at either level is ignored.
    """
    if cli_value is not None:
        try:
            s = float(cli_value)
        except (TypeError, ValueError):
            return None
        return s if s > 0.0 else None
    env = environ if environ is not None else os.environ.get(env_var)
    if env is not None:
        try:
            s = float(str(env).strip())
        except (TypeError, ValueError):
            return None
        return s if s > 0.0 else None
    return None


def resolve_dressing_rmax_scale(cli_value=None, environ=None):
    """Resolve the Dressing ``r_max`` scale factor (method #16)."""
    return resolve_dressing_scale(cli_value, 'SUTURA_DRESSING_RMAX_SCALE',
                                  environ=environ)


def resolve_dressing_sigma_scale(cli_value=None, environ=None):
    """Resolve the Dressing ``sigma`` scale factor (method #16)."""
    return resolve_dressing_scale(cli_value, 'SUTURA_DRESSING_SIGMA_SCALE',
                                  environ=environ)


def resolve_deep_repair_flags(deep_repair=None, no_fallback=False,
                              experimental=False, environ=None, config_path=None):
    """CLI flags -> ``(deep_repair mode, ftetwild argument)``.

    ``--deep-repair`` wins; otherwise ``--no-fallback-ftetwild`` means 'off'
    and ``--experimental-fallback-ftetwild`` means 'full' (the pre-ladder
    flags keep their meaning); otherwise env / config / default via
    ``resolve_deep_repair``. The fTetWild tier only runs in 'full', with the
    unchanged ``resolve_ftetwild`` semantics."""
    if deep_repair in DEEP_REPAIR_MODES:
        mode = deep_repair
    elif no_fallback:
        mode = 'off'
    elif experimental:
        mode = 'full'
    else:
        mode = resolve_deep_repair(None, environ=environ, config_path=config_path)
    ftetwild = resolve_ftetwild(no_fallback, experimental) if mode == 'full' else False
    return mode, ftetwild


# --------------------------------------------------------------------------
# Deep-repair ladder. After the fast stage-1 chain, the remaining holes and
# non-manifold edges can be handed to slower tiers: 'local' (re-mesh only the
# damaged regions, the rest of the surface untouched) and 'ftetwild'
# (tetrahedralize the original input). Mode: 'off' runs no tier and only
# reports what is available, 'local' runs the local tier, 'full' runs the
# fTetWild tier exactly as before the ladder existed (the local tier is not
# part of 'full' until it has been measured on the corpora).

SOLID_ENGINE_MODES = ('cdt', 'ftetwild')
SOLID_ENGINE_ENV = 'SUTURA_SOLID_ENGINE'
SOLID_ENGINE_DEFAULT = 'ftetwild'
CDT_MAX_TETS = 250_000


def resolve_solid_engine(cli_value=None, environ=None, config_path=None):
    """Last-resort solid engine: CLI > SUTURA_SOLID_ENGINE > existing
    Sutura settings file (updater.load_config) > 'ftetwild'.  No new config
    path is created; the loader is the same one the GUI uses."""
    if cli_value in SOLID_ENGINE_MODES:
        return cli_value
    env = (os.environ if environ is None else environ).get(SOLID_ENGINE_ENV)
    if env in SOLID_ENGINE_MODES:
        return env
    cfg = None
    if config_path is None:
        try:
            import updater  # existing Sutura settings loader
            cfg = updater.load_config()
        except Exception:
            cfg = None
    if cfg is None:
        path = DEEP_REPAIR_CONFIG if config_path is None else config_path
        try:
            with open(path) as f:
                cfg = json.load(f)
        except (OSError, ValueError, AttributeError):
            cfg = {}
    val = (cfg or {}).get('solid_engine')
    if val in SOLID_ENGINE_MODES:
        return val
    return SOLID_ENGINE_DEFAULT

DEEP_REPAIR_MODES = ('off', 'local', 'full')


DEEP_REPAIR_DEFAULT = _BALANCED_INTENSITY.deep_repair


DEEP_REPAIR_ENV = 'SUTURA_DEEP_REPAIR'


DEEP_REPAIR_CONFIG = os.path.expanduser('~/.config/sutura/config.json')


# --------------------------------------------------------------------------
# Self-intersection policy. 'repair' keeps the historical behaviour: a residual
# positive exact-SI count is treated as damage and drives the AUTOMATIC
# escalation (the Dressing auto fallback). 'report' (the default) still measures
# and reports self-intersections but never escalates or fails a result on SI
# alone -- holes/non-manifold edges always count, and an explicit force flag
# (--experimental-fallback-ftetwild / --experimental-dressing) still acts on SI.
# 'off' skips the EXACT SI classifier entirely (the estimated stage-1 count and
# the guarded si_excise pass are unaffected) and reports SI as not measured.
SI_MODES = ('repair', 'report', 'off')


SI_MODE_DEFAULT = 'report'


SI_MODE_ENV = 'SUTURA_SI_MODE'


def resolve_si_mode(cli_value=None, environ=None):
    """Self-intersection policy: CLI ``--si-mode`` > ``SUTURA_SI_MODE`` >
    :data:`SI_MODE_DEFAULT`. An invalid env value falls back to the default (an
    invalid CLI value is rejected by argparse)."""
    if cli_value in SI_MODES:
        return cli_value
    env = (os.environ if environ is None else environ).get(SI_MODE_ENV)
    if env in SI_MODES:
        return env
    return SI_MODE_DEFAULT


# Run-time estimate coefficients. PLACEHOLDERS, not calibrated: they will be
# fitted to the estimate_s / actual_s columns of the corpus benchmark.
DEEP_ESTIMATE_LOCAL_BASE = 0.5         # s


DEEP_ESTIMATE_LOCAL_PER_REGION = 0.05  # s per damaged region


DEEP_ESTIMATE_LOCAL_PER_KFACE = 0.02   # s per 1000 faces (selection, SI check)


DEEP_ESTIMATE_FTETWILD_A = 1e-3        # s, a * faces**b


DEEP_ESTIMATE_FTETWILD_B = 1.0


# Local tier: the largest hole (in boundary edges) the re-mesh closes, and
# the fairing steps on the new interior vertices.
LOCAL_REMESH_MAX_HOLE = 5000


LOCAL_REMESH_FAIR_STEPS = 3


# Hole refinement is off: on the 90k-face samples meshing_close_holes with
# refinehole=True took ~9 s (the refinement pass works on the whole mesh)
# against ~0.04 s without it, and on the 40 real-world samples it changed no
# local-tier outcome (same adopted meshes, same holes left). With it off the
# fairing step has no new interior vertex to move.
LOCAL_REMESH_REFINE = False


# Scope gates: the local tier only runs on small, simple damage. Chosen from
# the 40 real-world samples (2026-09-26), where it ran on 9 meshes and made
# 3 strictly watertight (thingi10k_40886, 46012, 71691): all 9 had no
# non-manifold edge, boundary loops of at most 14 edges, at most 6 damaged
# regions and at most 822 deleted faces (the three gains: loops <= 11,
# regions <= 2, <= 822 faces). On the 115-mesh corpus the tier gained
# nothing and added 69 % run time, i.e. the remaining open meshes there are
# outside this range. Non-manifold edges are excluded because no measured
# case exists where the tier removed one; the gates are constants so a
# later measurement can widen them.
LOCAL_MAX_NM_EDGES = 0


LOCAL_MAX_LOOP_LEN = 16


LOCAL_MAX_REGIONS = 8


LOCAL_MAX_REMOVED_FACES = 2000


LOCAL_MAX_SECONDS = 10.0     # checked between steps; slowest sample 1.9 s


def resolve_deep_repair(cli_value=None, environ=None, config_path=None):
    """Deep-repair mode: CLI flag > SUTURA_DEEP_REPAIR > config.json
    ``deep_repair`` > DEEP_REPAIR_DEFAULT. Invalid env/config values fall
    back to the next source (an invalid CLI value is rejected by argparse)."""
    if cli_value in DEEP_REPAIR_MODES:
        return cli_value
    env = (os.environ if environ is None else environ).get(DEEP_REPAIR_ENV)
    if env in DEEP_REPAIR_MODES:
        return env
    path = DEEP_REPAIR_CONFIG if config_path is None else config_path
    try:
        with open(path) as f:
            val = json.load(f).get('deep_repair')
        if val in DEEP_REPAIR_MODES:
            return val
    except (OSError, ValueError, AttributeError):
        pass
    return DEEP_REPAIR_DEFAULT


FTETWILD_OPTIMIZE_ENV = 'SUTURA_FTETWILD_OPTIMIZE'


def resolve_ftetwild_optimize(cli_value=None, environ=None, config_path=None):
    """fTetWild tet-quality optimisation on/off: CLI ``--ftetwild-optimize``
    > SUTURA_FTETWILD_OPTIMIZE (1/0) > config.json ``ftetwild_optimize`` >
    False. Off is the measured default: only the boundary surface is kept,
    and the optimisation pass cost 34 s vs 6 s on thingi10k_46012 with the
    same shape fidelity; turning it on mainly lengthens complex repairs."""
    if cli_value:
        return True
    env = (os.environ if environ is None else environ).get(FTETWILD_OPTIMIZE_ENV)
    if env in ('1', 'true', 'yes', 'on'):
        return True
    if env in ('0', 'false', 'no', 'off'):
        return False
    path = DEEP_REPAIR_CONFIG if config_path is None else config_path
    try:
        with open(path) as f:
            return bool(json.load(f).get('ftetwild_optimize', False))
    except (OSError, ValueError, AttributeError):
        return False


def ftetwild_params():
    """Parameter overrides for the fTetWild bridge (None = bridge defaults)."""
    return {'optimize': True} if resolve_ftetwild_optimize() else None


def estimate_deep_repair_time(n_faces, n_regions, tiers,
                              ftetwild_timeout=FTETWILD_TIMEOUT):
    """Rough per-tier run-time estimate in seconds (placeholder
    coefficients, see DEEP_ESTIMATE_*). Pure function, repairs nothing.
    The fTetWild estimate is capped at its time budget."""
    est = {}
    if 'local' in tiers:
        est['local'] = round(DEEP_ESTIMATE_LOCAL_BASE
                             + DEEP_ESTIMATE_LOCAL_PER_REGION * n_regions
                             + DEEP_ESTIMATE_LOCAL_PER_KFACE * n_faces / 1000.0, 1)
    if 'ftetwild' in tiers:
        est['ftetwild'] = round(min(DEEP_ESTIMATE_FTETWILD_A
                                    * float(n_faces) ** DEEP_ESTIMATE_FTETWILD_B,
                                    float(ftetwild_timeout)), 1)
    return est
