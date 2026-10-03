#!/usr/bin/env python3
"""Sutura Triage Engine: a staged PyMeshLab + manifold3d mesh-repair pipeline with 16 ranked repair methods.

Stage 1 (PyMeshLab/VCG): clean up, orient, close holes, drop debris.
Stage 2 (manifold3d): rebuild the closed mesh as a watertight solid and
merge overlapping shells.

Multi-object 3MF files are repaired per object and written back, so no
object is lost. Meshes are handled in memory as numpy arrays to preserve
the original vertex structure. Output is a new file; the input is never
overwritten.
"""
import sys
import os
import re
import json
import math
import struct
import tempfile
import shutil
import zipfile
import subprocess
import importlib.util
import time
import hashlib
from collections import defaultdict, OrderedDict
import numpy as np

from classification import classify, issue_label
from confidence import (estimate_confidence_pre_repair, repair_confidence)
from defects import detect as detect_defects
from mesh_classifier import classify_mesh
import repair_score
import history
import local_exact
import topology
import triage
import methods as method_registry

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


def _resolve_bridge():
    """Locate manifold_bridge.py.

    Prefers a copy bundled next to the executable in a PyInstaller bundle
    (onefile: ``<dir>/manifold_bridge.py``; onedir: ``<dir>/<tool>/``), so a
    standalone .app can run stage 2 in-process with no system install. Falls
    back to the standard ``SUTURA_DIR`` layout (unchanged) outside bundles.
    """
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'manifold_bridge.py'),
                     os.path.join(base, '..', 'manifold_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'manifold_bridge.py')


def _resolve_ftetwild_bridge():
    """Locate ftetwild_bridge.py (bundled-copy aware, same as _resolve_bridge)."""
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'ftetwild_bridge.py'),
                     os.path.join(base, '..', 'ftetwild_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'ftetwild_bridge.py')


def _resolve_indirect_bridge():
    """Locate indirect_bridge.py (bundled-copy aware, same as _resolve_bridge)."""
    if getattr(sys, 'frozen', False):
        base = os.path.dirname(os.path.abspath(sys.executable))
        for cand in (os.path.join(base, 'indirect_bridge.py'),
                     os.path.join(base, '..', 'indirect_bridge.py')):
            cand = os.path.abspath(cand)
            if os.path.isfile(cand):
                return cand
    return os.path.join(SUTURA_DIR, 'indirect_bridge.py')


BRIDGE = _resolve_bridge()
FTETWILD_BRIDGE = _resolve_ftetwild_bridge()
INDIRECT_BRIDGE = _resolve_indirect_bridge()

VERSION = "0.8.1"


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
MECH_TUNE_GATE = 0.75
ORG_TUNE_GATE = 0.70


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
MODE_PARAMS = {
    'low': {'mincomponentsize': 8, 'maxholesize': 200},
    'medium': {'mincomponentsize': 8, 'maxholesize': 1000},
    'aggressive': {'mincomponentsize': 12, 'maxholesize': 3000},
    'extreme': {'mincomponentsize': 20, 'maxholesize': 10000},
}
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
PROFILES = {
    'mechanical': {'mincomponentsize': 8, 'maxholesize': 300},
    'organic': {'mincomponentsize': 12, 'maxholesize': 1000},
    'scan': {'mincomponentsize': 4, 'maxholesize': 10000},
    'miniature': {'mincomponentsize': 1, 'maxholesize': 50},
    'fast': {'mincomponentsize': 8, 'maxholesize': 200},
}
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
_TYPE_PARAMS = {
    'mechanical': {'mincomponentsize': 8, 'maxholesize': 300},
    'organic': {'mincomponentsize': 12, 'maxholesize': 1000},
    'unknown': {'mincomponentsize': 8, 'maxholesize': 1000},
}


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


def _refine_hole_enabled():
    """Experimental (env-gated): refine the Stage-1 hole fill.

    ``SUTURA_REFINE_HOLE=1`` makes ``meshing_close_holes`` use Liepa-style
    refinement (``refinehole=True``, default 3 % edge length) so large /
    non-convex boundary loops get interior vertices instead of long ear-cut
    chords. OFF by default; not yet proven on the real-world corpus, so it
    stays behind the env flag and out of the CLI/GUI (see
    ``docs/cli-gui-parity-notes.md``).
    """
    return os.environ.get('SUTURA_REFINE_HOLE', '').strip().lower() in (
        '1', 'true', 'yes', 'on')


def _close_holes_step(maxholesize):
    """The ``meshing_close_holes`` chain entry, refined only when opted in."""
    params = {'maxholesize': maxholesize}
    if _refine_hole_enabled():
        params['refinehole'] = True
    return ('meshing_close_holes', params)


def stage1_chain(ml, maxholesize=1000, mincomponentsize=8, join_components=False):
    chain = [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
        ('meshing_remove_duplicate_vertices', {}),
        # layered/duplicated-vertex meshes (e.g. Bambu/Orca exports) collapse
        # onto few unique vertices here; the faces become duplicates only
        # AFTER vertex dedup, so a second duplicate-faces pass is required or
        # meshing_repair_non_manifold_edges sees a per-edge face soup and
        # deletes the mesh (the layered-3MF macOS failure, see
        # docs/stage2-3mf-per-object.md Investigation A).
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_re_orient_faces_coherently', {}),
        # non-manifold vertices are repaired BEFORE hole closing: closing a
        # hole on a mesh with non-manifold vertices fan-fills it and can
        # CREATE new non-manifold edges (verified in the corpus analysis),
        # so the mesh must be vertex-manifold first. A final close_holes pass
        # re-closes anything the debris removal / re-orient opened.
        ('meshing_repair_non_manifold_vertices', {}),
        _close_holes_step(maxholesize),
        # --experimental-join-components replaces this removal step with a
        # join (small components are moved onto the nearest larger component
        # instead of being deleted) -- prototype, flag-gated, see
        # join_small_components.
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_re_orient_faces_coherently', {}),
        _close_holes_step(maxholesize),
    ]
    if join_components:
        chain = [s for s in chain if s[0] != 'meshing_remove_connected_component_by_face_number']
    return chain


def delete_fallback_chain(ml, maxholesize=1000, mincomponentsize=8, join_components=False):
    chain = [
        ('meshing_remove_duplicate_faces', {}),
        ('meshing_remove_null_faces', {}),
        ('meshing_remove_duplicate_vertices', {}),
        # same layered-mesh fix as stage1_chain: faces become duplicates only
        # after vertex dedup, so dedup them again before the non-manifold pass.
        ('meshing_remove_duplicate_faces', {}),
        ('compute_selection_by_non_manifold_edges_per_face', {}),
        ('meshing_remove_selected_faces', {}),
        ('set_selection_none', {}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_repair_non_manifold_edges', {}),
        ('meshing_repair_non_manifold_vertices', {}),
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_re_orient_faces_coherently', {}),
        _close_holes_step(maxholesize),
    ]
    if join_components:
        chain = [s for s in chain if s[0] != 'meshing_remove_connected_component_by_face_number']
    return chain


def apply_chain(ms, chain):
    applied = 0
    skipped = {}
    for name, params in chain:
        try:
            ms.apply_filter(name, **params)
            applied += 1
        except Exception as e:
            skipped[name] = str(e)
    return applied, skipped


def extreme_extra_passes(ms, ml, params):
    """Extreme-mode extra Stage 1 passes (Stage C).

    After the main chain has run, select and remove self-intersecting faces
    (``compute_selection_by_self_intersections_per_face`` ->
    ``meshing_remove_selected_faces`` -> ``meshing_remove_unreferenced_vertices``)
    and then run the main chain ONE more time with the same thresholds to close
    the new holes / drop the new debris the removal exposed.

    Deliberately NOT meshing_isotropic_explicit_remeshing: a full remesh can
    unpredictably change topology, so it stays out of scope.

    Returns
    (passes_applied, self_intersections_found, self_intersections_removed,
     second_chain_applied, second_chain_skipped).
    When the mesh already has no self-intersecting faces the extra passes are
    skipped harmlessly (no error, nothing removed) and passes_applied is False.
    """
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    found = int(ms.current_mesh().face_selection_array().sum())
    if found == 0:
        return False, 0, 0, 0, {}
    before_removal = ms.current_mesh().face_number()
    ms.apply_filter('meshing_remove_selected_faces')
    removed = max(before_removal - ms.current_mesh().face_number(), 0)
    ms.apply_filter('meshing_remove_unreferenced_vertices')
    applied2, skipped2 = apply_chain(ms, stage1_chain(ml, **params))
    return True, found, removed, applied2, skipped2


# --- Guarded self-intersection excise + re-cap (Full Mend) ------------------
# After the Stage-1 chain, Full Mend (deep_repair='full') may still carry
# residual self-intersecting faces (overlapping / folded caps). The legacy
# extreme-only pass deletes them blindly and re-runs the whole chain (which
# can drop newly-severed small components); this guarded loop instead
# localises the excision (SI faces + 1 ring), re-caps with Liepa refinement
# and accepts a round ONLY when self-intersections strictly decrease and
# holes / non-manifold edges do not increase -- otherwise it atomically
# rolls back to the pre-round mesh. The connected-component removal filter is
# never called, so no component is ever dropped. OFF-by-env switch
# (SUTURA_SI_EXCISE=0); no CLI/GUI flag (see docs/cli-gui-parity-notes.md).
SI_EXCISE_MAX_ROUNDS = 3
SI_EXCISE_MAX_FACE_FRACTION = 0.30  # scope brake: never excise > 30 % at once
SI_EXCISE_REFINE_EDGE_PCT = 2.0     # refineholeedgelen, % of bbox diagonal
# Liepa refinement (refinehole=True) costs ~3 s per round on a 90k-face mesh
# but, measured on the 40-mesh corpus, changed NO excise outcome vs the plain
# cap; it is therefore used only where it is cheap (small meshes).
SI_EXCISE_REFINE_MAX_FACES = 20000
SI_EXCISE_DEFAULT_BUDGET = 5.0
SI_EXCISE_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 5.0,
    'thorough': 20.0,
    'extreme': 60.0,
}


def si_excise_enabled(enabled=None):
    """Resolve the SI-excise switch (default on) from an explicit value then
    ``SUTURA_SI_EXCISE``."""
    if enabled is None:
        env = os.environ.get('SUTURA_SI_EXCISE', '').strip().lower()
        if env in ('0', 'false', 'no', 'off'):
            enabled = False
        elif env in ('1', 'true', 'yes', 'on'):
            enabled = True
        else:
            enabled = True
    return bool(enabled)


def _si_excise_budget(spec):
    """SI-excise wall-clock budget (seconds) from the intensity preset."""
    key = getattr(spec, 'base', None) or getattr(spec, 'name', None)
    return SI_EXCISE_BUDGET_BY_INTENSITY.get(key, SI_EXCISE_DEFAULT_BUDGET)


# Localized exact self-union of residual self-intersection clusters (after
# Stage 2 + P-WELD). EXPERIMENTAL: measured on the 40-mesh corpus it removes SI
# on only an isolated real mesh (see docs/cli-gui-parity-notes.md); the
# ornate-frame fold is global, not local, so patch boundaries never match
# (97 % `boundary_split`). OFF by default; `SUTURA_LOCAL_EXACT=1` forces it.
LOCAL_EXACT_DEFAULT_ENABLED = False
LOCAL_EXACT_BUDGET_BY_INTENSITY = {
    'quick': 0.0,
    'balanced': 10.0,
    'thorough': 45.0,
    'extreme': 120.0,
}
LOCAL_EXACT_DEFAULT_BUDGET = 10.0


def local_exact_enabled(enabled=None):
    """Resolve the local-exact switch (default OFF) from an explicit value then
    ``SUTURA_LOCAL_EXACT``."""
    if enabled is None:
        env = os.environ.get('SUTURA_LOCAL_EXACT', '').strip().lower()
        if env in ('1', 'true', 'yes', 'on'):
            enabled = True
        elif env in ('0', 'false', 'no', 'off'):
            enabled = False
        else:
            enabled = LOCAL_EXACT_DEFAULT_ENABLED
    return bool(enabled)


def _local_exact_budget(spec):
    """Local-exact wall-clock budget (seconds) from the intensity preset."""
    key = getattr(spec, 'base', None) or getattr(spec, 'name', None)
    return LOCAL_EXACT_BUDGET_BY_INTENSITY.get(key, LOCAL_EXACT_DEFAULT_BUDGET)


def _should_run_local_exact(report, spec):
    """Gate: enabled, Stage 2 succeeded, and a positive time budget."""
    if not local_exact_enabled():
        return False
    if not (report.get('stage2') or {}).get('ok'):
        return False
    return _local_exact_budget(spec or triage.resolve_intensity(None)) > 0.0


def _si_count(ms):
    """Number of faces flagged by the self-intersection filter (selection is
    cleared afterwards; the filter never changes geometry)."""
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    n = int(ms.current_mesh().face_selection_array().sum())
    ms.apply_filter('set_selection_none')
    return n


def si_excise_recap(ms, ml, *, maxholesize=1000,
                    max_rounds=SI_EXCISE_MAX_ROUNDS,
                    time_budget=SI_EXCISE_DEFAULT_BUDGET,
                    face_fraction=SI_EXCISE_MAX_FACE_FRACTION):
    """Guarded self-intersection excise + refined re-cap for the Full Mend path.

    Selects the self-intersecting faces, dilates the selection by one ring,
    deletes it, then re-caps the opened loops with ``meshing_close_holes``
    (``selfintersection=True`` -- which PREVENTS new self-intersecting caps;
    ``refinehole=True`` only up to ``SI_EXCISE_REFINE_MAX_FACES``, since the
    Liepa refinement costs ~3 s/round on a 90k mesh without changing the
    corpus outcome). A round is committed only when the
    self-intersection count strictly decreases and holes / non-manifold edges
    do not increase; otherwise the pre-round mesh is restored byte-for-byte.
    The connected-component removal filter is never called (no component is
    dropped). Returns ``(ms, report)``; ``report`` is JSON-serialisable.
    """
    report = {
        'ran': False, 'applied': False, 'rounds': 0, 'initial_si': 0,
        'si_after': 0, 'removed_si': 0, 'reason': None,
        'budget_s': (None if time_budget is None else float(time_budget)),
        'seconds': 0.0, 'history': [],
    }
    try:
        report['initial_si'] = _si_count(ms)
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        report['reason'] = 'error: %s' % e
        return ms, report
    if report['initial_si'] == 0:
        report['reason'] = 'no_si'
        return ms, report
    if time_budget is not None and time_budget <= 0.0:
        report['reason'] = 'budget'
        return ms, report

    report['ran'] = True
    t0 = time.perf_counter()
    current_si = report['initial_si']
    total_faces = int(ms.current_mesh().face_number())
    for round_idx in range(max_rounds):
        if time_budget is not None and (time.perf_counter() - t0) > time_budget:
            report['reason'] = 'budget'
            break
        topo_before = ms.apply_filter('get_topological_measures')
        # Use boundary EDGES, not ``number_holes``: the latter is unreliable on
        # meshes with non-manifold vertices (reported -1 on thingi10k_145065,
        # which let a round that opened 42 boundary loops be accepted).
        holes_before = int(topo_before.get('boundary_edges', 0) or 0)
        nm_before = (int(topo_before.get('non_two_manifold_edges', 0) or 0)
                     + int(topo_before.get('non_two_manifold_vertices', 0) or 0))
        saved_v = ms.current_mesh().vertex_matrix().copy()
        saved_t = ms.current_mesh().face_matrix().copy()
        try:
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            ms.apply_filter('apply_selection_dilatation')
            selected = int(ms.current_mesh().face_selection_array().sum())
            if selected == 0:
                ms.apply_filter('set_selection_none')
                report['reason'] = 'no_selection'
                break
            if selected > face_fraction * max(total_faces, 1):
                ms.apply_filter('set_selection_none')
                report['reason'] = 'scope'
                break
            ms.apply_filter('meshing_remove_selected_faces')
            ms.apply_filter('meshing_remove_unreferenced_vertices')
            ms.apply_filter('meshing_repair_non_manifold_edges')
            ms.apply_filter('meshing_repair_non_manifold_vertices')
            close_kwargs = {'maxholesize': maxholesize,
                            'newfaceselected': False,
                            'selfintersection': True}
            if total_faces <= SI_EXCISE_REFINE_MAX_FACES:
                close_kwargs['refinehole'] = True
                close_kwargs['refineholeedgelen'] = ml.PercentageValue(
                    SI_EXCISE_REFINE_EDGE_PCT)
            ms.apply_filter('meshing_close_holes', **close_kwargs)
            ms.apply_filter('meshing_remove_unreferenced_vertices')
            topo_after = ms.apply_filter('get_topological_measures')
            holes_after = int(topo_after.get('boundary_edges', 0) or 0)
            nm_after = (int(topo_after.get('non_two_manifold_edges', 0) or 0)
                        + int(topo_after.get('non_two_manifold_vertices', 0) or 0))
            si_after = _si_count(ms)
        except Exception:  # noqa: BLE001 - reject and roll back on any error
            ms.clear()
            ms.add_mesh(ml.Mesh(vertex_matrix=saved_v, face_matrix=saved_t))
            report['reason'] = 'rollback'
            break
        if (si_after < current_si and holes_after <= holes_before
                and nm_after <= nm_before):
            report['history'].append({
                'round': round_idx + 1,
                'si_before': int(current_si),
                'si_after': int(si_after),
                'faces_after': int(topo_after.get('faces_number', 0) or 0),
            })
            current_si = si_after
            report['rounds'] += 1
            if current_si == 0:
                report['reason'] = 'clean'
                break
            if round_idx + 1 >= max_rounds:
                report['reason'] = 'max_rounds'
        else:
            ms.clear()
            ms.add_mesh(ml.Mesh(vertex_matrix=saved_v, face_matrix=saved_t))
            report['reason'] = 'rollback'
            break
    report['si_after'] = int(current_si)
    report['removed_si'] = int(report['initial_si'] - current_si)
    report['applied'] = report['rounds'] > 0
    report['seconds'] = round(time.perf_counter() - t0, 3)
    return ms, report


def join_small_components(ms, ml, mincomponentsize, maxholesize):
    """Prototype (flag-gated): join small components onto the nearest larger
    one instead of deleting them.

    Every connected component with fewer than ``mincomponentsize`` faces is
    translated so its closest vertex coincides with the nearest vertex of the
    nearest larger component, then duplicate vertices are merged and holes
    re-closed. This is a deliberate geometry change (small debris is MOVED,
    not dropped) so it must stay behind ``--experimental-join-components`` and
    is NOT part of the default chain.

    Returns ``(moved, remaining_small)`` -- moved = number of small components
    joined, remaining_small = small components that could not be joined (no
    larger component to move onto, or the mesh has only small components).
    """
    v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float64)
    t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int64)
    if len(t) == 0:
        return 0, 0

    # connected components via union-find over faces sharing a vertex
    parent = list(range(len(t)))

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    def union(i, j):
        ri, rj = find(i), find(j)
        if ri != rj:
            parent[ri] = rj

    vertex_faces = {}
    for fi, tri in enumerate(t):
        for vi in tri:
            vertex_faces.setdefault(int(vi), []).append(fi)
    for faces in vertex_faces.values():
        first = faces[0]
        for fi in faces[1:]:
            union(first, fi)

    comps = np.array([find(i) for i in range(len(t))], dtype=np.int64)
    comp_ids, counts = np.unique(comps, return_counts=True)
    large = [c for c, n in zip(comp_ids, counts) if n >= mincomponentsize]
    small = [c for c, n in zip(comp_ids, counts) if n < mincomponentsize]
    if not large or not small:
        return 0, len(small)

    # per-component vertex index sets (component id -> sorted vertex ids)
    comp_verts = {}
    for fi, ci in enumerate(comps):
        comp_verts.setdefault(int(ci), set()).update(int(x) for x in t[fi])
    comp_verts = {c: np.array(sorted(s), dtype=np.int64) for c, s in comp_verts.items()}

    v_new = v.copy()
    moved = 0
    for sc in small:
        sv = comp_verts[sc]
        sv3 = v[sv]
        # nearest larger component + closest vertex pair (small comps are tiny,
        # so the O(|small| * |large|) scan is cheap in practice)
        best_dist = np.inf
        best_translate = None
        for lc in large:
            lv3 = v[comp_verts[lc]]
            d = np.linalg.norm(sv3[:, None, :] - lv3[None, :, :], axis=2)
            i, j = np.unravel_index(np.argmin(d), d.shape)
            if d[i, j] < best_dist:
                best_dist = float(d[i, j])
                best_translate = lv3[j] - sv3[i]
        if best_translate is None:
            continue
        # move the whole small component onto the nearest larger component
        v_new[sv] = v[sv] + best_translate
        moved += 1

    # fuse the coincident vertices and re-close what the move opened
    # (add_mesh makes the rebuilt mesh current)
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(v_new, dtype=np.float32),
                        face_matrix=np.asarray(t, dtype=np.int32)))
    for name, params in (('meshing_remove_duplicate_vertices', {}),
                         ('meshing_repair_non_manifold_vertices', {}),
                         ('meshing_remove_duplicate_faces', {}),
                         ('meshing_close_holes', {'maxholesize': maxholesize}),
                         ('meshing_remove_unreferenced_vertices', {})):
        try:
            ms.apply_filter(name, **params)
        except Exception:
            pass
    return moved, max(len(small) - moved, 0)


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# sutura intermediate\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def surface_area(verts, tris):
    """Total triangle surface area of a mesh, computed directly from geometry.

    Works for both open and closed meshes (VCG's geometric-measures volume
    and area are only meaningful on closed meshes). Pure numpy, platform
    independent."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    cross = np.cross(b - a, c - a)
    return float(np.sum(0.5 * np.linalg.norm(cross, axis=1)))


def boundary_loop_stats(verts, tris):
    """(n_loops, max_loop_len) of the mesh boundary: walks the boundary-edge
    graph (edges used by exactly one face). A closed mesh returns (0, 0).
    Pure numpy + dict (no pymeshlab); each boundary vertex of a 2-manifold
    mesh has degree 2, so loops = connected boundary components."""
    tris = np.asarray(tris, dtype=np.int64)
    if len(tris) == 0:
        return 0, 0
    f0, f1, f2 = tris[:, 0], tris[:, 1], tris[:, 2]
    V = int(tris.max()) + 1
    keys = np.concatenate([
        np.minimum(f0, f1) * V + np.maximum(f0, f1),
        np.minimum(f1, f2) * V + np.maximum(f1, f2),
        np.minimum(f0, f2) * V + np.maximum(f0, f2),
    ]).astype(np.int64)
    uniq, counts = np.unique(keys, return_counts=True)
    bkeys = uniq[counts == 1]
    if len(bkeys) == 0:
        return 0, 0
    ba, bb = bkeys // V, bkeys % V
    adj = defaultdict(list)
    for a, c in zip(ba.tolist(), bb.tolist()):
        adj[a].append(c)
        adj[c].append(a)
    seen, loops = set(), []
    for s in adj:
        if s in seen:
            continue
        n, cur, prev = 0, s, None
        while cur not in seen:
            seen.add(cur)
            n += 1
            nxt = [x for x in adj[cur] if x != prev]
            if not nxt:
                break
            prev, cur = cur, nxt[0]
        loops.append(n)
    return len(loops), (max(loops) if loops else 0)


def signed_volume(verts, tris):
    """Signed volume of a triangle mesh (sum of origin-tetrahedra volumes).

    The sign reflects the winding orientation: a negative value means the
    faces are globally inverted (normals pointing inward). Pure numpy, works
    for open meshes too (the value is then not a real volume, only a signed
    sum - interpret with care)."""
    if len(tris) == 0 or len(verts) == 0:
        return 0.0
    v = np.asarray(verts, dtype=np.float64)
    t = np.asarray(tris, dtype=np.int64)
    a = v[t[:, 0]]
    b = v[t[:, 1]]
    c = v[t[:, 2]]
    return float(np.sum(np.einsum('ij,ij->i', a, np.cross(b, c))) / 6.0)


def check_units(verts, declared_unit=None):
    """Return a unit warning dict or None.

    A mesh whose bounding-box dimensions only become plausible when scaled
    from inches or centimetres to millimetres was probably not authored in
    millimetres. This matters because the size-based repair logic
    (maxholesize, hole diameter, volume-change thresholds) assumes mm, so a
    wrong-scale mesh is worth a visible, NON-BLOCKING warning -- it never
    changes the repair itself.

    ``declared_unit``: the <model unit="..."> attribute for 3MF inputs (None
    for STL/OBJ, which carry no unit metadata). A declared non-millimetre
    unit is definitive and reported as-is; ``'millimeter'`` (the 3MF spec
    default) is trusted and suppresses the heuristic. With no declared unit
    the heuristic below is used.

    Heuristic: a plausible printable part spans ~25-400 mm on its longest
    axis. If the mesh's longest axis only lands in that range after scaling
    by 25.4 (inches) or 10 (centimetres), flag it. Inches is checked first
    (the most common alternate CAD/export default); in the overlapping
    ~2.5-16-unit band the hint names both.

    Returns ``{'unit_warning': True, 'unit_hint': '...'}`` or None.
    """
    if declared_unit is not None and declared_unit != 'millimeter':
        return {'unit_warning': True,
                'unit_hint': ('The 3MF model declares its unit as "%s"; '
                              'size-based repair thresholds assume millimetres.'
                              % declared_unit)}
    if declared_unit == 'millimeter':
        return None
    if verts is None or len(verts) == 0:
        return None
    v = np.asarray(verts, dtype=np.float32)
    if len(v) == 0 or not np.isfinite(v).all():
        return None
    L = float(np.max(v.max(axis=0) - v.min(axis=0)))
    if not (L > 0.0):
        return None
    if not (25.0 <= 25.4 * L <= 400.0) and not (25.0 <= 10.0 * L <= 400.0):
        return None
    return {'unit_warning': True,
            'unit_hint': ('Model may not be in millimetres: the longest '
                          'bounding-box axis is %.3g units (as inches '
                          '≈ %.1f mm, as centimetres ≈ %.1f mm). Size-based '
                          'repair thresholds assume millimetres.'
                          % (L, 25.4 * L, 10.0 * L))}


def read_3mf_units(path):
    """Return {model_name: declared_unit} for every .model file in a 3MF.

    The 3MF spec defaults the <model> unit attribute to ``millimeter`` when
    absent; allowed values are micron/millimeter/centimeter/inch. STL/OBJ
    carry no unit metadata, so this only applies to .3mf inputs. Never
    raises: a broken archive yields an empty dict (the caller's own 3MF
    parsing will report the real error)."""
    out = {}
    try:
        with zipfile.ZipFile(path) as z:
            for name in z.namelist():
                if not name.endswith('.model'):
                    continue
                xml = _read_zip_entry(z, name).decode('utf-8', errors='replace')
                m = re.search(r'<model\b[^>]*\bunit="([^"]+)"', xml)
                out[name] = m.group(1) if m else 'millimeter'
    except Exception:
        return {}
    return out


def read_obj(path):
    verts = []
    tris = []
    with open(path) as f:
        for line in f:
            parts = line.split()
            if not parts:
                continue
            if parts[0] == 'v':
                verts.append((float(parts[1]), float(parts[2]), float(parts[3])))
            elif parts[0] == 'f':
                tris.append((int(parts[1]) - 1, int(parts[2]) - 1, int(parts[3]) - 1))
    return np.array(verts, dtype=np.float32), np.array(tris, dtype=np.int32)


def stl_write_binary(path, verts, tris):
    with open(path, 'wb') as f:
        f.write(b'Sutura intermediate'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(tris)))
        for t in tris:
            f.write(struct.pack('<3f', 0, 0, 0))
            for i in t:
                f.write(struct.pack('<3f', verts[i][0], verts[i][1], verts[i][2]))
            f.write(struct.pack('<H', 0))


def should_extract_outer_shell(verts, tris, mesh_type=None, si_count=0):
    """True ONLY when mesh fits mechanical or dense_scan_heavy_si templates,
    components > 1, and the closed components overlap or touch.

    Pure-numpy replacement for the former ``trimesh.Trimesh(...).split()`` path:
    the unconditional ``import trimesh`` (and its scipy stack) cost ~0.5 s on
    every mechanical repair even when this pre-check returned False.  Faces are
    grouped into connected components through edges shared by exactly two faces
    (trimesh's ``face_adjacency`` rule); a component counts as closed only when
    every edge of its own faces appears exactly twice.  A component that is open
    (>=4 faces and a genuine boundary loop) makes the whole check decline, which
    preserves the previous trimesh ``fill_holes`` outcome.
    """
    is_mech = (mesh_type == 'mechanical')
    is_heavy_si = (si_count >= 200)
    if not (is_mech or is_heavy_si):
        return False
    try:
        verts = np.asarray(verts)
        tris = np.asarray(tris, dtype=np.int64)
        n_faces = len(tris)
        if n_faces == 0 or len(verts) == 0:
            return False
        # Per-face edges, sorted so that the two faces sharing an edge agree on
        # its vertex pair.  Each edge is tagged with the face it came from.
        face_of_edge = np.concatenate(
            [np.arange(n_faces), np.arange(n_faces), np.arange(n_faces)])
        edges = np.concatenate(
            [tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0)
        edges = np.sort(edges, axis=1)
        order = np.lexsort((edges[:, 1], edges[:, 0]))
        edges = edges[order]
        face_of_edge = face_of_edge[order]
        starts = np.concatenate(
            [[0], np.nonzero(np.any(edges[1:] != edges[:-1], axis=1))[0] + 1])
        sizes = np.diff(np.append(starts, len(edges)))
        # Connected components over faces, joined only by edges used by exactly
        # two faces.  Vectorised union-find: hook each edge's endpoint roots to
        # their minimum, shortcut, repeat until stable (converges in a handful
        # of passes on real meshes).
        pair_starts = starts[sizes == 2]
        edge_a = face_of_edge[pair_starts].astype(np.int64)
        edge_b = face_of_edge[pair_starts + 1].astype(np.int64)
        parent = np.arange(n_faces, dtype=np.int64)

        def _roots(x):
            while True:
                px = parent[x]
                if np.array_equal(px, x):
                    return x
                x = px

        while True:
            root_a = _roots(edge_a)
            root_b = _roots(edge_b)
            low = np.minimum(root_a, root_b)
            new_parent = parent.copy()
            np.minimum.at(new_parent, root_a, low)
            np.minimum.at(new_parent, root_b, low)
            new_parent = new_parent[new_parent]
            if np.array_equal(new_parent, parent):
                break
            parent = new_parent
        roots = _roots(np.arange(n_faces, dtype=np.int64))
        _, roots = np.unique(roots, return_inverse=True)
        n_comp = int(roots.max()) + 1 if n_faces else 0
        if n_comp <= 1:
            return False
        # Per-component edge multiset (a component's own faces only, matching
        # trimesh's reindexed submesh): group edge occurrences by
        # (component, vertex pair).
        edge_comp = roots[face_of_edge]
        order = np.lexsort((edges[:, 1], edges[:, 0], edge_comp))
        grouped_comp = edge_comp[order]
        grouped_lo = edges[order, 0]
        grouped_hi = edges[order, 1]
        changed = ((grouped_comp[1:] != grouped_comp[:-1]) |
                   (grouped_lo[1:] != grouped_lo[:-1]) |
                   (grouped_hi[1:] != grouped_hi[:-1]))
        grp_start = np.concatenate([[0], np.nonzero(changed)[0] + 1])
        grp_size = np.diff(np.append(grp_start, len(grouped_comp)))
        grp_comp = grouped_comp[grp_start]
        comp_faces = np.bincount(roots, minlength=n_comp)
        boundary_edges = np.bincount(grp_comp[grp_size == 1], minlength=n_comp)
        not_closed = np.zeros(n_comp, dtype=bool)
        not_closed[grp_comp[grp_size != 2]] = True
        # The former trimesh fill_holes path declined whenever a component with
        # >=4 faces had a genuine boundary loop (>=3 boundary edges).
        if np.any((comp_faces >= 4) & (boundary_edges >= 3)):
            return False
        closed_count = int((~not_closed).sum())
        if closed_count < n_comp * 0.7:
            return False
        # Per-component bounding boxes from the referenced vertices.
        vert_comp = np.repeat(roots, 3)
        vert_idx = tris.reshape(-1)
        vorder = np.argsort(vert_comp, kind='stable')
        sorted_comp = vert_comp[vorder]
        sorted_verts = verts[vert_idx][vorder]
        comp_start = np.searchsorted(sorted_comp, np.arange(n_comp), side='left')
        bounds = list(zip(np.minimum.reduceat(sorted_verts, comp_start, axis=0),
                          np.maximum.reduceat(sorted_verts, comp_start, axis=0)))
        # Pairwise bounding-box overlap check
        diag = float(np.linalg.norm(np.ptp(verts, axis=0)))
        eps = 1e-4 * (diag if diag > 1e-6 else 1.0)
        n = len(bounds)
        for i in range(n):
            min_i, max_i = bounds[i][0], bounds[i][1]
            for j in range(i + 1, n):
                min_j, max_j = bounds[j][0], bounds[j][1]
                if (min(max_i[0], max_j[0]) >= max(min_i[0], min_j[0]) - eps and
                    min(max_i[1], max_j[1]) >= max(min_i[1], min_j[1]) - eps and
                    min(max_i[2], max_j[2]) >= max(min_i[2], min_j[2]) - eps):
                    return True
        return False
    except Exception:
        return False


def repair_mesh_from_arrays(verts, tris, tmpdir, mode='auto', profile=None,
                            engine='experimental', declared_unit=None,
                            join_components=False, autorefine=False,
                            ftetwild=False, indirect_autorefine=False,
                            extra_features=False, deep_repair=None,
                            triage_spec=None, engines=None, engine_chain=None,
                            closing=None, proxy_template=False, repeat=None,
                            repeat_source=None, repeat_target=None,
                             wall_thicken=False, wall_min_thickness=None,
                             graft=False, flap=False, dressing=False,
                             dressing_drain=None, dressing_defects=None,
                             dressing_rmax_scale=None, dressing_sigma_scale=None,
                             si_mode=None, dressing_force_adopt=False):
    """Repair one mesh given as numpy arrays. Returns (report, verts, tris).

    ``mode`` is one of REPAIR_MODES: 'auto' (the default) uses the mesh
    classifier + confidence gate exactly as before; the fixed modes
    (low/medium/aggressive/extreme) use the MODE_PARAMS thresholds directly.
    ``profile`` (optional, only effective when mode is 'auto') selects a
    named threshold preset (see PROFILES). ``engine`` selects the classifier
    engine ('experimental' default; 'classic' opt-in). ``declared_unit`` is
    the 3MF <model unit="..."> attribute (None for STL/OBJ) fed to the
    non-blocking unit-warning heuristic. ``join_components`` enables the
    experimental join-small-components prototype (flag-gated; NOT the default).
    ``autorefine`` enables the experimental self-intersection subdivision
    prototype (Lazard & Valque 2025; flag-gated; NEVER deletes input faces).
    ``ftetwild`` selects the fTetWild fallback tier, which tetrahedralizes the
    input and extracts a watertight boundary: ``'auto'`` (the CLI/GUI
    default) runs it only when fTetWild is installed and stage 1 still leaves
    holes or non-manifold edges; ``True`` (``--experimental-fallback-ftetwild``)
    also runs it on a closed result that still self-intersects and reports an
    explicit skip when fTetWild is missing; ``False`` disables it (library
    default, ``--no-fallback-ftetwild``).
    ``indirect_autorefine`` enables the experimental exact indirect-predicate
    arrangement-lite split (rust/sutura-geom; flag-gated; same adopt/fallback
    guard as ``autorefine``).
    ``extra_features`` enables the opt-in edge-tiebreak classifier head.
    ``deep_repair`` selects the deep-repair ladder mode ('off'/'local'/
    'full', see ``deep_repair_ladder``); None (library default) keeps the
    pre-ladder behaviour and adds no ``deep_repair`` report.
    ``si_mode`` selects the self-intersection policy ('repair'/'report'/'off',
    see ``resolve_si_mode``); None resolves to the default 'report', which
    measures/reports SI without escalating or failing on SI alone.
    ``triage_spec`` is the resolved intensity preset (sutura/triage.py) that
    supplies the post-Stage-1 knobs (deep-repair tier timeout/size cap,
    decimation ladder, Hausdorff sample count); None resolves to Balanced
    (the pre-triage behaviour). It never changes Stage 1.
    ``engines``/``engine_chain`` are the configs and resolved chain from
    ``load_engine_run``; None (library default) runs no external engine and
    keeps the output byte-identical to the pre-engines behaviour.
    ``closing`` selects the standalone scan-closing tier (registry methods 8/9):
    ``'poisson'`` (screened Poisson reconstruction, ``sutura/closing.py``) or
    ``'flat_back'`` (flat back plane + side walls); ``proxy_template`` selects
    the proxy-template rebuild (registry method 10, ``sutura/proxy_repair.py``).
    Both run on the ORIGINAL input arrays (like the fTetWild tier) and the
    candidate is adopted only when it is strict-watertight (no holes, no
    non-manifold edges) and no worse than the stage-1 baseline; None/False
    (library default) keeps the output byte-identical to today.
    ``flap`` enables the surface-based hole filler (``sutura_engine.flap``) as
    the FIRST Stage-1 step: a pre-pass on the input arrays as they stand before
    the VCG chain (normally the original surface, though outer-shell extraction
    may already have replaced them), so the deep-repair tiers that work from
    those arrays also see its patches. It is adopted only when the reload-honest
    damage (holes + non-manifold edges) does not rise; the chain's own
    ``meshing_close_holes`` remains the fallback for any loop it skips or
    rejects. That gate is topological-only and pre-chain, so ``repair_file``
    adds a final safety net that re-runs with Flap OFF when the final result is
    not strict-watertight or has more parts than the input; False (library
    default) keeps the output byte-identical.
    """
    import pymeshlab as ml
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)

    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    stats = {'stage1': {}}
    if triage_spec is None:
        triage_spec = triage.resolve_intensity(None)
    stats.update(triage.triage_report_fields(triage_spec))
    si_mode = resolve_si_mode(si_mode)
    stats['si_mode'] = si_mode

    # before_stage1 engines run on the ORIGINAL input, before anything else
    # (including the unit heuristic and the classifier, which then see the
    # engine output). Adoption uses the same holes/non-manifold guard as the
    # fTetWild tier; a shape-changing output is flagged, not rejected.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'before_stage1'):
            _base_ms = ml.MeshSet()
            _base_ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
            _base_topo = _base_ms.apply_filter('get_topological_measures')
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'before_stage1', tmpdir, v, t,
                boundary_loop_stats(v, t)[0],
                _base_topo.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                v = np.asarray(_cv, dtype=np.float32)
                t = np.asarray(_ct, dtype=np.int32)
                verts, tris = v, t
        _record_engine_entries(stats, _entries)

    # Non-blocking unit warning: the size-based Stage 1 thresholds assume
    # millimetres, so a mesh that was probably authored in inches/cm deserves
    # a visible warning (reported, never changes the repair).
    _u = check_units(v, declared_unit)
    if _u:
        stats['unit_warning'] = True
        stats['unit_hint'] = _u['unit_hint']

    # Mesh-type-aware Stage 1 tuning (organic vs mechanical): resolved through
    # the shared resolve_mode_params so repair and --dry-run stay in sync.
    _cls, stats['classifier_engine'] = classify_with_engine(verts, tris, engine, extra_features=extra_features)
    stats['detected_type'] = _cls['type']
    stats['detected_confidence'] = _cls['confidence']
    stats['repair_mode'] = mode
    if profile is not None:
        stats['repair_profile'] = profile
    _p, stats['tuning_applied'] = resolve_mode_params(
        mode, _cls['type'], _cls['confidence'], profile=profile)
    # Mesh-sensitive maxholesize: a fixed value (1000) skips any boundary
    # loop longer than that (VCG counts each hole edge twice), so large
    # scan holes stay open. Raise to cover the input's largest loop, never
    # below the mode/type base. Measured on the 75-model corpus: closes
    # large loops without ever degrading a mesh (7 cases improved, 0 worse).
    _p['maxholesize'] = max(_p['maxholesize'],
                            2 * boundary_loop_stats(v, t)[1])

    # Outer-shell extraction pre-step (dense_scan_heavy_si / mechanical templates):
    # For inputs with multiple overlapping/nested closed components (e.g. Rubik-style
    # assemblies), dissolves internal interfaces via slight dilation
    # (dilation = 5e-4 * bbox_diagonal, ~0.05 mm for 100 mm object) and boolean union.
    if mode == 'auto' and should_extract_outer_shell(verts, tris, mesh_type=_cls['type']):
        try:
            import repeat_repair as _rr
            _shell_v, _shell_t = _rr.extract_outer_shell(verts, tris)
            if len(_shell_t) > 0:
                verts = np.asarray(_shell_v, dtype=np.float32)
                tris = np.asarray(_shell_t, dtype=np.int32)
                v, t = verts, tris
                stats['outer_shell_extracted'] = True
        except Exception:
            pass

    before_ms = ml.MeshSet()
    before_ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    before = before_ms.apply_filter('get_topological_measures')
    before_geom = before_ms.apply_filter('get_geometric_measures')
    before_volume = before_geom.get('mesh_volume', 0)
    before_area = surface_area(v, t)
    before_verts = before.get('vertices_number', 0)
    before_faces = before.get('faces_number', 0)
    components_before = before.get('connected_components_number', 0)
    holes_before = boundary_loop_stats(v, t)[0]
    nm_before = before.get('non_two_manifold_edges', 0)

    # Flap: surface-based hole fill (on by default in the CLI). It is the FIRST Stage-1
    # step, a pre-pass on the input as it stands here (normally the original
    # surface; note the outer-shell extraction above may already have replaced
    # v,t for mode='auto', so this is not unconditionally the raw input). The
    # chain's own meshing_close_holes remains the fallback for any loop Flap
    # skips or rejects, and a candidate is adopted only when the reload-honest
    # damage (holes + non-manifold edges) does not rise. Running here -- rather
    # than post-chain -- is what lets the deep-repair Graft tier, which also
    # works from these arrays, see the flap patches: measured on ornate-frame,
    # the standalone Flap->Graft output (Orca 2 parts, 3494 SI faces, volume
    # 58762) is reproduced exactly, whereas a post-chain flap cannot reach Graft
    # and left the output byte-identical to flap-OFF. This gate is pre-chain and
    # topological-only, so repair_file adds a final safety net (re-run with Flap
    # OFF when the final result is not strict-watertight or has more parts than
    # the input). The reported 'before' metrics above are still the input's.
    if flap:
        _fms = ml.MeshSet()
        _fms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        _fafter = _fms.apply_filter('get_topological_measures')
        _fms, _fafter = _flap_step(ml, _fms, _fafter, stats, None)
        if (stats.get('flap') or {}).get('adopted'):
            v = np.asarray(_fms.current_mesh().vertex_matrix(), dtype=np.float32)
            t = np.asarray(_fms.current_mesh().face_matrix(), dtype=np.int32)
            verts, tris = v, t

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))

    applied, skipped = apply_chain(
        ms, stage1_chain(ml, **_p, join_components=join_components))
    after = ms.apply_filter('get_topological_measures')

    # --experimental-autorefine prototype: resolve self-intersections on the
    # INPUT mesh by subdividing the intersecting triangles along their
    # intersection segments (Lazard & Valque 2025), NEVER deleting input faces.
    # When enabled, the SAME chain is re-run on the autorefine-preprocessed
    # input and the better final result is adopted (adopt/fallback guard:
    # autorefine is only used when its final output has holes AND non-manifold
    # regions no worse than the baseline chain — its float64 construction can
    # leave non-manifold edges on dense-SI scans, see
    # docs/alpha-wrap-feasibility-2026-09.md section 4a). Flag-gated, NOT the
    # default.
    stats['experimental_autorefine'] = False
    if autorefine and len(t) > 0:
        ar_v = np.asarray(v, dtype=np.float64)
        ar_t = np.asarray(t, dtype=np.int64)
        ar_rep = {'skipped': False}
        try:
            # Imported here, inside the guard: autorefine needs
            # pyrobust-predicates, and a missing optional dependency must be
            # reported, never crash the repair.
            import autorefine as _autorefine
            ar_v, ar_t, ar_info = _autorefine.autorefine(ar_v, ar_t)
            ar_rep.update(ar_info)
            if len(ar_t) > 0:
                ms_cand = ml.MeshSet()
                ms_cand.add_mesh(ml.Mesh(vertex_matrix=np.asarray(ar_v, np.float32),
                                         face_matrix=np.asarray(ar_t, np.int32)))
                apply_chain(ms_cand, stage1_chain(ml, **_p, join_components=join_components))
                cand_after = ms_cand.apply_filter('get_topological_measures')
                cand_holes = boundary_loop_stats(
                    ms_cand.current_mesh().vertex_matrix(),
                    ms_cand.current_mesh().face_matrix())[0]
                cand_nm = cand_after.get('non_two_manifold_edges', 0)
                base_holes = boundary_loop_stats(
                    ms.current_mesh().vertex_matrix(),
                    ms.current_mesh().face_matrix())[0]
                base_nm = after.get('non_two_manifold_edges', 0)
                adopted = bool(cand_holes <= base_holes and cand_nm <= base_nm)
                ar_rep['adopted'] = adopted
                if adopted:
                    ms = ms_cand
                    after = cand_after
        except Exception as e:  # noqa: BLE001 - the prototype never crashes a repair
            ar_rep['error'] = str(e)
        stats['experimental_autorefine'] = ar_rep

    # --experimental-indirect-autorefine (Phase B): exact arrangement-lite
    # self-intersection split via the rust/sutura-geom extension (B4).  The
    # INPUT mesh is split along its proper-intersection segments with exact
    # indirect predicates (broad phase + exact classifier + per-triangle 2D
    # CDT + exact-rational welding), then the SAME stage-1 chain is re-run on
    # the split output.  Adopt/fallback guard identical to autorefine: the
    # indirect result is adopted only when its final holes AND non-manifold
    # edge counts are no worse than the baseline chain's.  Flag-gated, NOT the
    # default; reports an explicit skip when sutura_geom is unavailable.
    stats['experimental_indirect_autorefine'] = False
    if indirect_autorefine and len(t) > 0:
        ia_rep = {'skipped': False}
        try:
            import indirect_bridge
        except Exception as e:  # noqa: BLE001
            ia_rep['skipped'] = True
            ia_rep['error'] = 'indirect bridge unavailable: %s' % e
        else:
            try:
                ia_v, ia_t, al_info = indirect_bridge.arrangement_lite_arrays(v, t)
                ia_rep.update(al_info)
                if len(ia_t) > 0:
                    ms_cand = ml.MeshSet()
                    ms_cand.add_mesh(ml.Mesh(vertex_matrix=np.asarray(ia_v, np.float32),
                                             face_matrix=np.asarray(ia_t, np.int32)))
                    apply_chain(ms_cand, stage1_chain(ml, **_p, join_components=join_components))
                    cand_after = ms_cand.apply_filter('get_topological_measures')
                    cand_holes = boundary_loop_stats(
                        ms_cand.current_mesh().vertex_matrix(),
                        ms_cand.current_mesh().face_matrix())[0]
                    cand_nm = cand_after.get('non_two_manifold_edges', 0)
                    base_holes = boundary_loop_stats(
                        ms.current_mesh().vertex_matrix(),
                        ms.current_mesh().face_matrix())[0]
                    base_nm = after.get('non_two_manifold_edges', 0)
                    adopted = bool(cand_holes <= base_holes and cand_nm <= base_nm)
                    ia_rep['adopted'] = adopted
                    if adopted:
                        ms = ms_cand
                        after = cand_after
            except ImportError as e:
                ia_rep['skipped'] = True
                ia_rep['error'] = ('indirect autorefine skipped: sutura_geom '
                                   'extension unavailable (%s)' % e)
            except Exception as e:  # noqa: BLE001 - the prototype never crashes a repair
                ia_rep['error'] = str(e)
        stats['experimental_indirect_autorefine'] = ia_rep

    if after.get('non_two_manifold_edges', 0) > 0 or after.get('non_two_manifold_vertices', 0) > 0:
        fb = ml.MeshSet()
        fb.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
        fapplied, fskipped = apply_chain(
            fb, delete_fallback_chain(ml, **_p, join_components=join_components))
        fafter = fb.apply_filter('get_topological_measures')
        if (fafter.get('non_two_manifold_edges', 0) + fafter.get('non_two_manifold_vertices', 0)
                < after.get('non_two_manifold_edges', 0) + after.get('non_two_manifold_vertices', 0)):
            ms, after, applied, skipped = fb, fafter, fapplied, fskipped

    # --experimental-join-components prototype: instead of deleting small
    # components (the mincomponentsize filter was dropped from the chain
    # above), move them onto the nearest larger component and re-close.
    stats['experimental_join_components'] = False
    if join_components:
        moved, remaining = join_small_components(
            ms, ml, _p['mincomponentsize'], _p['maxholesize'])
        stats['experimental_join_components'] = {'moved': moved,
                                                 'remaining_small': remaining}
        after = ms.apply_filter('get_topological_measures')

    if after.get('faces_number', 0) == 0:
        if mode == 'extreme':
            raise ExtremeRemovedAllError(
                'Extreme mode removed all geometry (small connected component '
                'below the size threshold); try a less aggressive mode (e.g. Auto)')
        raise ValueError('all faces are degenerate; nothing to repair')

    # Guarded SI excise + refined re-cap (Full Mend path). Runs after the
    # Stage-1 chain (+ delete-fallback + join), before the deep-repair ladder.
    # Gated to deep_repair == 'full' (Quick is 'off'; the library default None
    # is untouched) and skipped for mode == 'extreme', which keeps its legacy
    # extreme_extra_passes. A no-op on meshes with no self-intersections.
    if deep_repair == 'full' and mode != 'extreme' and si_excise_enabled():
        ms, _si_rec = si_excise_recap(
            ms, ml, maxholesize=_p['maxholesize'],
            time_budget=_si_excise_budget(triage_spec))
        # Only report when a round was actually accepted (self-intersections
        # strictly reduced, holes/nm not worse); a mesh the excision cannot
        # improve -- including any 0-SI mesh -- keeps the pre-existing report
        # and output byte-identical.
        if _si_rec.get('applied'):
            stats['si_excise'] = _si_rec
            after = ms.apply_filter('get_topological_measures')

    # Extreme-only extra passes (Stage C): self-intersection cleanup + one more
    # run of the main chain to close the holes / drop the debris the removal
    # exposed. ONLY for mode == 'extreme'; every other mode is untouched.
    # meshing_isotropic_explicit_remeshing (full remesh) is deliberately out of
    # scope: it can unpredictably change topology.
    stats['extreme_passes_applied'] = False
    if mode == 'extreme':
        applied_extra, si_found, si_removed, applied2, skipped2 = \
            extreme_extra_passes(ms, ml, _p)
        stats['extreme_passes_applied'] = applied_extra
        applied += applied2
        if skipped2:
            skipped.update(skipped2)
        if si_found:
            stats['self_intersections_found'] = si_found
            stats['self_intersections_removed'] = si_removed
        after = ms.apply_filter('get_topological_measures')
        if after.get('faces_number', 0) == 0:
            raise ExtremeRemovedAllError(
                'Extreme mode removed all geometry (small connected component '
                'below the size threshold); try a less aggressive mode (e.g. Auto)')

    # fTetWild fallback tier (FAZ17): guaranteed-correct last-resort
    # solidifier. Tetrahedralize the ORIGINAL input surface and extract its
    # boundary as a watertight, SI-free triangle mesh (fTetWild, MPL-2.0, via
    # the pytetwild bridge in venv311). Adopt only when the extracted surface
    # is no worse on the same holes+non-manifold metric used by the autorefine
    # guard.
    # - 'auto' (default when installed): only when stage 1 still leaves holes
    #   or non-manifold edges. Measured on the 40 real-world samples: strict
    #   watertight 31 -> 39, and a mesh stage 1 already closed is never
    #   remeshed (its residual self-intersections are left alone).
    # - True (--experimental-fallback-ftetwild): also when the closed result
    #   still self-intersects (slow: up to FTETWILD_TIMEOUT per mesh).
    # after_stage1 engines run on the current stage-1 result, before the deep
    # ladder; replace_ftetwild engines replace the built-in fTetWild tier
    # inside the ladder.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'after_stage1'):
            _cur_v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
            _cur_t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'after_stage1', tmpdir, _cur_v, _cur_t,
                boundary_loop_stats(_cur_v, _cur_t)[0],
                after.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=_cv, face_matrix=_ct))
                after = ms.apply_filter('get_topological_measures')
        _record_engine_entries(stats, _entries)

    # Registry methods 8/9/10 (P-INT): standalone closing / proxy-template
    # modules. They run on the ORIGINAL input arrays, before the deep-repair
    # ladder, and adopt only a strict-watertight, no-worse candidate (see
    # ``closing_ladder``).
    ms, after = closing_ladder(ml, ms, after, stats, v, t, tmpdir,
                               closing=closing, proxy_template=proxy_template)

    # Registry method 13 (Graft / shell wrap): the morphology last-resort tier,
    # run on the ORIGINAL input arrays before the fTetWild ladder. Adopts only a
    # reload-watertight candidate; records the fidelity verdict + warnings under
    # ``stats['graft']``.
    ms, after = graft_tier(ml, ms, after, stats, v, t, tmpdir,
                           graft=(graft is True),
                           intensity=(getattr(triage_spec, 'base', None)
                                      or getattr(triage_spec, 'name', None)),
                           spec=triage_spec)

    ms, after = deep_repair_ladder(ml, ms, after, stats, v, t, tmpdir,
                                   mode=deep_repair, ftetwild=ftetwild,
                                   spec=triage_spec, engines=engines,
                                   engine_chain=engine_chain, graft=graft,
                                   dressing=dressing,
                                   dressing_drain=dressing_drain,
                                   dressing_defects=dressing_defects,
                                   dressing_rmax_scale=dressing_rmax_scale,
                                   dressing_sigma_scale=dressing_sigma_scale,
                                   si_mode=si_mode,
                                   dressing_force_adopt=dressing_force_adopt)

    # Registry methods 11/12 (P-REP): repeated-element transplant, after the
    # whole stage-1 chain (the transplant needs a watertight M).
    ms, after = repeat_tier(ml, ms, after, stats, mode=repeat,
                            source_point=repeat_source,
                            target_point=repeat_target)

    # Registry method 15 (P-WALL): opt-in thin-wall thicken-to-min. Runs on the
    # ORIGINAL input arrays (like the closing tier) and adopts only a
    # strict-watertight, no-worse candidate; never automatic.
    if wall_thicken:
        ms, after = wall_thicken_tier(ml, ms, after, stats, v, t,
                                      min_thickness=wall_min_thickness)

    # A closed result with no non-manifold edge can still have pinched
    # ("bowtie") vertices, e.g. when the final close_holes fan-fills a hole at
    # a vertex shared by two boundary loops; stage 2 then never runs and a
    # strictly watertight mesh is reported as a warning (thingi10k_145065).
    ms, after, stats['pinched_vertices_split'] = _split_pinched_vertices(ml, ms, after)

    # final_fallback engines run once, on the ORIGINAL input, after every
    # built-in tier (including replace_ftetwild) and after the pinch split.
    if engines:
        _entries = []
        if engine_placement_order(engines, engine_chain, 'final_fallback'):
            _cur_v = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
            _cur_t = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'final_fallback', tmpdir, v, t,
                boundary_loop_stats(_cur_v, _cur_t)[0],
                after.get('non_two_manifold_edges', 0), triage_spec, _entries)
            if _adopted:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=_cv, face_matrix=_ct))
                after = ms.apply_filter('get_topological_measures')
        _record_engine_entries(stats, _entries)

    holes_after = boundary_loop_stats(ms.current_mesh().vertex_matrix(),
                                      ms.current_mesh().face_matrix())[0]
    nm_after = after.get('non_two_manifold_edges', 0)
    _deep_repair_offer(stats, holes_after, nm_after, len(t), spec=triage_spec)

    stats['stage1']['holes_closed'] = max(holes_before - holes_after, 0)
    stats['stage1']['holes_remaining'] = holes_after
    stats['stage1']['non_manifold_edges_fixed'] = max(nm_before - nm_after, 0)
    stats['stage1']['non_manifold_edges_remaining'] = nm_after
    stats['stage1']['two_manifold'] = bool(after.get('is_mesh_two_manifold'))
    stats['stage1']['components'] = after.get('connected_components_number')
    stats['stage1']['faces_after'] = after.get('faces_number')
    stats['stage1']['faces_before'] = before.get('faces_number')
    stats['stage1']['faces_removed'] = max(before.get('faces_number', 0) - after.get('faces_number', 0), 0)
    stats['stage1']['applied_filters'] = applied
    if skipped:
        stats['stage1']['skipped'] = skipped

    geom = ms.apply_filter('get_geometric_measures')
    if geom.get('mesh_volume', 0) < 0:
        ms.apply_filter('meshing_invert_face_orientation')
        geom = ms.apply_filter('get_geometric_measures')
    after_volume = geom.get('mesh_volume', 0)

    new_verts = np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32)
    new_tris = np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32)
    fin = ms.apply_filter('get_topological_measures')
    after_area = surface_area(new_verts, new_tris)
    after_verts = fin.get('vertices_number', 0)
    after_faces = fin.get('faces_number', 0)

    volume_change_pct = 0.0
    if before_volume and after_volume:
        volume_change_pct = (after_volume - before_volume) / abs(before_volume) * 100
    surface_change_pct = 0.0
    if before_area and after_area:
        surface_change_pct = (after_area - before_area) / abs(before_area) * 100
    stats['stage1']['volume_before'] = float(before_volume)
    stats['stage1']['volume_after'] = float(after_volume)
    stats['stage1']['volume_change_percent'] = round(volume_change_pct, 2)
    stats['stage1']['surface_area_before'] = round(before_area, 3)
    stats['stage1']['surface_area_after'] = round(after_area, 3)
    stats['stage1']['surface_area_change_percent'] = round(surface_change_pct, 2)
    if abs(volume_change_pct) > 15:
        stats['stage1']['volume_warning'] = (
            'Volume changed by %.1f%% - verify in slicer before printing.'
            % abs(volume_change_pct))

    stats['stage1'].update({
        'two_manifold': bool(fin.get('is_mesh_two_manifold')),
        'holes_remaining': boundary_loop_stats(new_verts, new_tris)[0],
        'components': fin.get('connected_components_number'),
        'components_before': int(components_before),
        'vertices_before': int(before_verts),
        'vertices_after': int(after_verts),
        'faces_before': int(before_faces),
        'faces_after': int(after_faces),
    })

    # Self-intersection count on the FINAL stage-1 output (repair_score.py's
    # health metric). We reuse the existing `ms` MeshSet: ms.current_mesh()
    # IS the repaired mesh right now (new_verts/new_tris were pulled from it
    # above), so building a fresh MeshSet and reloading arrays would be wasted
    # work on large meshes. The filter only sets the face-selection array and
    # does not alter geometry, so this cannot disturb anything downstream.
    #
    # Assumption (documented, not silent): stage 2 (manifold3d) only runs on a
    # watertight closed mesh and REBUILDS the watertight solid, so its output
    # is assumed free of self-intersections by construction -- therefore stage
    # 2's output is NOT re-measured here. When stage 2 runs and writes the
    # final mesh, this value is a conservative stage-1-based upper bound on
    # the health signal, which is fine for scoring.
    # Under --si-mode off the policy is 'not measured': skip the (estimated)
    # count entirely and report None. The guarded si_excise pass keeps its own
    # pymeshlab count independently, so it is unaffected.
    if si_mode == 'off':
        stats['stage1']['self_intersections_remaining'] = None
    else:
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        stats['stage1']['self_intersections_remaining'] = int(
            ms.current_mesh().face_selection_array().sum())
    return stats, new_verts, new_tris


def run_stage2(inter, out_obj):
    """Run stage 2. Returns (report, ok); reports a skip, never silence."""
    # 1) primary: the fixed Linux two-venv layout (python3.11 + manifold3d)
    if os.path.exists(VENV311) and os.path.exists(BRIDGE):
        r = subprocess.run(
            [VENV311, BRIDGE, inter, out_obj],
            capture_output=True, text=True, timeout=600,
        )
        if r.returncode == 0:
            try:
                report = json.loads(r.stdout.strip().splitlines()[-1])
            except Exception:
                report = {'error': 'unparseable manifold output'}
            if 'error' not in report:
                return report, True
        # fall through to the in-process attempt if the venv failed

    # 2) single-environment installs (macOS/conda): call the bridge in-process
    try:
        import importlib.util
        spec = importlib.util.spec_from_file_location('sutura_manifold_bridge', BRIDGE)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        report = module.run_bridge(inter, out_obj)
        if 'error' not in report:
            return report, True
    except Exception:
        pass

    # 3) unavailable: report it explicitly, never silently
    return {'error': 'Stage 2 skipped: manifold3d not available in this environment.'}, False


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
FTETWILD_MAX_HAUSDORFF_REL = 0.01

# Second fTetWild attempt: when the default (optimize=False) boundary fails
# the holes/non-manifold guard even after the manifold3d post-process, the
# mesh is tetrahedralized once more with the quality optimisation on, within
# what is left of FTETWILD_TIMEOUT (skipped when less than
# FTETWILD_RETRY_MIN_SECONDS remain). thingi10k_1038444 was adopted with the
# optimisation and rejected ('holes_nm') without it (macOS, 2026-09-26).
FTETWILD_RETRY_PARAMS = {'optimize': True}
FTETWILD_RETRY_MIN_SECONDS = 10.0

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

# Decimation of a wastefully dense fTetWild boundary. fTetWild with
# optimize=False can return a boundary far denser than the input (measured on
# thingi10k_73444: 473,212 output faces at 7,882 input faces, against 14,012
# with optimize=True at the same solver time); the extra faces cost ~65 s of
# downstream work (manifold3d, Hausdorff, self-intersection/boundary passes)
# and carry no more shape information. After such an attempt the boundary is
# decimated with a pymeshlab quadric edge collapse before the manifold3d
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
DECIMATE_HD_MARGIN = 0.005

# Fixed Hausdorff sample count, independent of the output face count, so a
# dense output cannot inflate the measurement cost (the previous inline
# formula scaled the sample count with the output size up to this cap). The
# active count comes from the intensity spec (never below 200000).
HAUSDORFF_SAMPLES = _BALANCED_INTENSITY.ftetwild_hausdorff_samples

_FTETWILD_AVAILABLE = None


def ftetwild_available():
    """True when the optional fTetWild extra (pytetwild + pyvista,
    requirements-ftetwild.txt) is installed where run_ftetwild will look:
    the venv311 stage-2 environment, or the current interpreter for
    single-environment installs. Checked on disk / via find_spec, without
    importing the (heavy) packages; cached per process. Also requires the
    bridge module itself (FTETWILD_BRIDGE)."""
    global _FTETWILD_AVAILABLE
    if _FTETWILD_AVAILABLE is None:
        ok = False
        if not os.path.isfile(FTETWILD_BRIDGE):
            # A stale install without the bridge module: the packages alone
            # are not enough, and 'auto' must stay a silent no-op.
            _FTETWILD_AVAILABLE = False
            return False
        if os.path.exists(VENV311):
            import glob
            root = os.path.dirname(os.path.dirname(VENV311))
            site = glob.glob(os.path.join(root, 'lib', 'python3*', 'site-packages'))
            ok = any(os.path.isdir(os.path.join(sp, 'pytetwild')) and
                     os.path.isdir(os.path.join(sp, 'pyvista')) for sp in site)
        if not ok:
            try:
                ok = (importlib.util.find_spec('pytetwild') is not None and
                      importlib.util.find_spec('pyvista') is not None)
            except (ImportError, ValueError):
                ok = False
        _FTETWILD_AVAILABLE = ok
    return _FTETWILD_AVAILABLE


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


FLAP_ENV = 'SUTURA_FLAP'

# Single default switch for the Flap pre-pass.  Flap is ON by default: the
# module constant below, overridable by the SUTURA_FLAP_DEFAULT env var.  The
# CLI honours --no-flap to disable it per run; the library default in
# ``repair_mesh_from_arrays`` stays ``False`` so direct library calls keep
# their pre-Flap behaviour.
FLAP_DEFAULT_ENABLED = True


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
DRESSING_DEFECT_SETS = ('all', 'holes_nm')

# ---------------------------------------------------------------------------
# Dressing auto-fallback switch + adoption-gate bounds (single switch)
# ---------------------------------------------------------------------------
# Dressing stays OFF by default until the fine-detail (voxel staircase) loss is
# fixed in the extraction core.  One knob turns it into an Auto-ladder fallback:
# the module constant below, or the SUTURA_DRESSING_DEFAULT env var.  When on,
# the deep-repair 'full' ladder tries Dressing after Graft whenever the current
# result still has holes, non-manifold edges or exact self-intersections.
DRESSING_DEFAULT_ENABLED = False

# Adoption-gate bounds for the auto fallback (the explicit --experimental-
# dressing path uses the same gate).  The volume is compared to the mesh the
# ladder held just before Dressing (the best available reference; the raw input
# signed volume is meaningless when it is itself damaged/open).
DRESSING_MAX_VOLUME_DELTA = 0.10     # |dV| / |V_before|
DRESSING_MAX_PARTS_SLACK = 1         # candidate components <= before + slack
# Healthy-region coat-vs-input normal agreement; a voxel staircase / fluting
# loss keeps positions within the Hausdorff gate but tilts the normals.  This is
# a PLACEHOLDER threshold, measured on the 13-mesh set 2026-10: Dressing p95 is
# 12-36 deg on the accepted coats and 82-88 deg on ornate-frame / 100281 /
# 1038439 / 1038441 / 145065 (the staircased, detail-lost ones).  The calibrated
# detector will replace it.
DRESSING_MAX_NORMAL_ANGLE_P95 = 30.0  # degrees
# CAD-likeness placeholder threshold (the calibrated detector supplies the real
# value).
CAD_LIKENESS_THRESHOLD = 0.5


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


# ---------------------------------------------------------------------------
# External repair engines (sutura/engines.py, user-installed third-party
# binaries). Engine configs are loaded ONCE per run and slotted into the
# pipeline at their named placement. Every engine output passes through the
# SAME holes/non-manifold guard as the fTetWild tier, and an adopted output
# far from the input is flagged (``shape_changed``) exactly like fTetWild.
# With no engines configured and no chain.toml the whole layer is inert: the
# repair is byte-identical to the pre-engines behaviour.
ENGINE_MESH_FORMATS = ('stl', 'obj', 'ply')


def load_engine_run(engines_dir=None):
    """Load engine configs and resolve the execution chain, ONCE per run.

    Returns ``(engines, chain, warnings)``. ``sutura/engines.py`` is treated
    as optional: a stale install without it, or a broken config, only yields a
    warning and behaves as if no engine were configured (never a crash and
    never a repair failure)."""
    try:
        import engines as _engines
    except Exception as e:  # noqa: BLE001
        return {}, [], ['external engines unavailable: %s' % e]
    try:
        eng, warns = _engines.load_all_engines(engines_dir)
        chain, cwarns = _engines.resolve_chain(eng, config_dir=engines_dir)
        return eng, chain, warns + cwarns
    except Exception as e:  # noqa: BLE001 - ambient config never fails a repair
        return {}, [], ['failed to load engines: %s' % e]


def engine_placement_order(engines, chain, placement):
    """Enabled engine names for one placement, in chain order."""
    if not engines:
        return []
    order = list(chain or [])
    names = [n for n in order if n in engines
             and engines[n].enabled and engines[n].placement == placement]
    # Engines present in the config dir but omitted from a custom chain.toml
    # are not run; a default chain (build_default_chain) includes them all.
    return names


def _read_engine_mesh(path):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    return (np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32),
            np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32))


def _write_engine_mesh(path, verts, tris):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.save_current_mesh(path)


def run_engines(ml, engines, chain, placement, tmpdir, in_v, in_t,
                base_holes, base_nm, spec, entries):
    """Run the enabled engines at ``placement`` on ``(in_v, in_t)``.

    Engines run in chain order; the first output that passes the same
    holes/non-manifold guard as the fTetWild tier is adopted and returned.
    Returns ``(cand_v, cand_t, adopted)`` (cand arrays None when nothing was
    adopted). One report entry per engine is appended to ``entries``."""
    import engines as _engines
    for name in engine_placement_order(engines, chain, placement):
        eng = engines[name]
        entry = {'name': name, 'placement': placement, 'adopted': False}
        try:
            in_path = os.path.join(tmpdir,
                                   'engine_%s_in.%s' % (name, eng.input_format))
            out_path = os.path.join(tmpdir,
                                    'engine_%s_out.%s' % (name, eng.output_format))
            _write_engine_mesh(in_path, in_v, in_t)
            eng_tmp = os.path.join(tmpdir, 'engine_%s_tmp' % name)
            os.makedirs(eng_tmp, exist_ok=True)
            res = _engines.run_engine(eng, in_path, out_path, tmp_dir=eng_tmp)
            entry['rc'] = res.get('returncode')
            entry['time'] = round(float(res.get('duration_sec') or 0.0), 3)
            entry['stdout_tail'] = res.get('stdout_tail', '')
            entry['stderr_tail'] = res.get('stderr_tail', '')
            if not res.get('success'):
                entry['reject_reason'] = res.get('error') or 'engine failed'
                entries.append(entry)
                continue
            ok, verr = _engines.validate_output(eng, in_path, out_path)
            if not ok:
                entry['reject_reason'] = verr
                entries.append(entry)
                continue
            cand_v, cand_t = _read_engine_mesh(out_path)
            if len(cand_t) == 0:
                entry['reject_reason'] = 'engine output has no faces'
                entries.append(entry)
                continue
            cand_holes = boundary_loop_stats(cand_v, cand_t)[0]
            cand_ms = ml.MeshSet()
            cand_ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
            cand_after = cand_ms.apply_filter('get_topological_measures')
            if cand_after.get('faces_number', 0) == 0:
                entry['reject_reason'] = 'engine removed all geometry'
                entries.append(entry)
                continue
            cand_nm = cand_after.get('non_two_manifold_edges', 0)
            entry['output_holes'] = int(cand_holes)
            entry['output_non_manifold'] = int(cand_nm)
            if cand_holes <= base_holes and cand_nm <= base_nm:
                hd, _ = _hausdorff_rel(ml, in_v, in_t, cand_v, cand_t,
                                       samples=spec.ftetwild_hausdorff_samples)
                entry['hausdorff_rel'] = hd
                entry['shape_changed'] = bool(
                    hd is not None and hd > FTETWILD_MAX_HAUSDORFF_REL)
                entry['adopted'] = True
                entries.append(entry)
                return cand_v, cand_t, True
            entry['reject_reason'] = 'holes_nm'
        except Exception as e:  # noqa: BLE001 - an engine never fails a repair
            entry['reject_reason'] = 'error: %s' % e
        entries.append(entry)
    return None, None, False


def _record_engine_entries(stats, entries):
    """Append engine entries to ``stats['engines']`` and flag shape changes."""
    if not entries:
        return
    stats.setdefault('engines', []).extend(entries)
    if any(e.get('shape_changed') for e in entries):
        stats['shape_changed'] = True


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


def run_ftetwild(inter, out_obj, params=None, timeout=None):
    """Run the fTetWild fallback bridge. Returns (report, ok); reports a skip,
    never silence. Time-boxed: a per-mesh wall-clock budget (FTETWILD_TIMEOUT)
    bounds the call so a dense scan cannot stall the batch; on timeout the
    report carries error='timeout'.

    Dispatch: the fixed Linux two-venv layout (python3.11 + pytetwild) first,
    then a killable `sys.executable` subprocess for single-environment installs
    (macOS/conda and frozen bundles), then an in-process attempt wrapped in a
    timed thread as a last resort. The bridge itself reports an explicit skip
    when pytetwild/pyvista are absent. ``params`` (dict) overrides the
    bridge's DEFAULT_PARAMS for pytetwild.tetrahedralize; ``timeout``
    (seconds) replaces FTETWILD_TIMEOUT for this call."""
    extra = [json.dumps(params)] if params is not None else []
    budget = FTETWILD_TIMEOUT if timeout is None else timeout
    # fTetWild writes a debug file (__tracked_surface.stl, ~1 MB) into the
    # current working directory; run the bridge from the private temp dir of
    # the output so nothing lands in the user's folder (e.g. a Dolphin /
    # Finder right-click repair runs with the file's folder as cwd).
    workdir = os.path.dirname(os.path.abspath(out_obj))
    # 1) primary: the fixed Linux two-venv layout (python3.11 + pytetwild)
    if os.path.exists(VENV311) and os.path.exists(FTETWILD_BRIDGE):
        try:
            r = subprocess.run(
                [VENV311, FTETWILD_BRIDGE, inter, out_obj] + extra,
                capture_output=True, text=True, timeout=budget,
                cwd=workdir,
            )
        except subprocess.TimeoutExpired:
            return {'error': 'timeout'}, False
        if r.returncode == 0:
            try:
                report = json.loads(r.stdout.strip().splitlines()[-1])
            except Exception:
                report = {'error': 'unparseable ftetwild output'}
            if 'error' not in report:
                return report, True
        # fall through to the sys.executable attempt if the venv failed

    # 2) single-environment installs (macOS/conda, frozen bundles): run the
    # bridge as a KILLABLE subprocess with the current interpreter (which has
    # pytetwild installed), so a hung fTetWild call can be terminated instead
    # of blocking the batch forever.
    try:
        r = subprocess.run(
            [sys.executable, FTETWILD_BRIDGE, inter, out_obj] + extra,
            capture_output=True, text=True, timeout=budget,
            cwd=workdir,
        )
        if r.returncode == 0:
            try:
                report = json.loads(r.stdout.strip().splitlines()[-1])
            except Exception:
                report = {'error': 'unparseable ftetwild output'}
            if 'error' not in report:
                return report, True
    except subprocess.TimeoutExpired:
        return {'error': 'timeout'}, False
    except Exception:
        pass

    # 3) last-resort in-process attempt, wrapped in a timed thread so it can
    # never block the caller beyond the budget (the thread is not killable, but
    # the caller proceeds and records the timeout).
    try:
        import threading
        import importlib.util
        spec = importlib.util.spec_from_file_location('sutura_ftetwild_bridge', FTETWILD_BRIDGE)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        box = {}
        def _go():
            box['report'] = module.run_bridge(inter, out_obj, params)
        th = threading.Thread(target=_go, daemon=True)
        th.start()
        th.join(budget)
        if th.is_alive():
            return {'error': 'timeout'}, False
        report = box.get('report')
        if report is not None and 'error' not in report:
            return report, True
    except Exception:
        pass

    # 4) unavailable: report it explicitly, never silently
    return {'error': 'fTetWild fallback skipped: pytetwild not available '
                     'in this environment.'}, False


PINCH_SPLIT_MAX_PASSES = 5


def _split_pinched_vertices(ml, ms, topo):
    """Split pinched vertices of a closed mesh so it becomes two-manifold.

    Applies only when the mesh has no hole, no non-manifold edge and at least
    one non-manifold vertex. ``meshing_repair_non_manifold_vertices`` is
    repeated (at most ``PINCH_SPLIT_MAX_PASSES`` times, one pass can leave
    some pinches) until the mesh is two-manifold. The vertices are only
    duplicated, the geometry is unchanged; the result is kept only when it
    still has no hole and no non-manifold edge.

    Returns ``(meshset, topo, report)`` where report is False when the step
    did not apply, else a dict with ``before``/``after`` (pinched vertices),
    ``passes`` and ``adopted``.
    """
    pinched = topo.get('non_two_manifold_vertices', 0)
    m = ms.current_mesh()
    if (pinched == 0 or topo.get('is_mesh_two_manifold')
            or topo.get('non_two_manifold_edges', 0) != 0
            or boundary_loop_stats(m.vertex_matrix(), m.face_matrix())[0] != 0):
        return ms, topo, False
    trial = ml.MeshSet()
    trial.add_mesh(ml.Mesh(vertex_matrix=np.asarray(m.vertex_matrix(), np.float64),
                           face_matrix=np.asarray(m.face_matrix(), np.int32)))
    t_topo, passes = topo, 0
    while passes < PINCH_SPLIT_MAX_PASSES:
        passes += 1
        trial.apply_filter('meshing_repair_non_manifold_vertices')
        t_topo = trial.apply_filter('get_topological_measures')
        if t_topo.get('is_mesh_two_manifold'):
            break
    tm = trial.current_mesh()
    holes = boundary_loop_stats(tm.vertex_matrix(), tm.face_matrix())[0]
    adopted = bool(holes == 0 and t_topo.get('non_two_manifold_edges', 0) == 0
                   and t_topo.get('non_two_manifold_vertices', 0) < pinched)
    report = {'before': int(pinched),
              'after': int(t_topo.get('non_two_manifold_vertices', 0)),
              'passes': passes, 'adopted': adopted}
    if adopted:
        return trial, t_topo, report
    return ms, topo, report


def _ftetwild_manifold_postprocess(ml, tmpdir, cand_v, cand_t, cand_ms, cand_after,
                                   cand_holes, cand_nm):
    """manifold3d post-process of the fTetWild boundary.

    fTetWild's raw boundary can carry non-manifold edges on dense-SI scans
    (e.g. thingi10k_1038441: 0 SI / 0 holes but 531 nm edges), and it can be
    closed with no non-manifold edge yet still not two-manifold: two
    tetrahedra meeting in a single vertex leave a pinched ("bowtie") vertex
    (thingi10k_248395). Stage 2 only runs on a two-manifold stage-1 result,
    so such a boundary ended as a warning. In both cases the boundary is
    rebuilt as a Manifold solid via the same stage-2 bridge; the rebuilt
    boundary is used only when it is no worse on holes and non-manifold
    edges than the raw fTetWild boundary. A boundary that is already
    two-manifold with no hole is returned unchanged.

    Returns ``(verts, tris, meshset, topo, holes, nm, postprocessed)``.
    """
    two_manifold = bool(cand_after.get('is_mesh_two_manifold'))
    if cand_nm == 0 and cand_holes == 0 and two_manifold:
        return cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, False
    m3_in = os.path.join(tmpdir, 'ftetwild_m3d_in.obj')
    m3_out = os.path.join(tmpdir, 'ftetwild_m3d_out.obj')
    write_obj(m3_in, cand_v, cand_t)
    m3rep, m3ok = run_stage2(m3_in, m3_out)
    if m3ok and 'error' not in m3rep and os.path.exists(m3_out):
        m3_v, m3_t = read_obj(m3_out)
        if len(m3_t) > 0:
            m3_ms = ml.MeshSet()
            m3_ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(m3_v, np.float32),
                                   face_matrix=np.asarray(m3_t, np.int32)))
            m3_after = m3_ms.apply_filter('get_topological_measures')
            m3_holes = boundary_loop_stats(m3_v, m3_t)[0]
            m3_nm = m3_after.get('non_two_manifold_edges', 0)
            if m3_holes <= cand_holes and m3_nm <= cand_nm:
                return m3_v, m3_t, m3_ms, m3_after, m3_holes, m3_nm, True
    return cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, False


def _ftetwild_candidate(ml, tmpdir, cand_v, cand_t):
    """Measure a boundary candidate and run the manifold3d post-process on it.

    Returns ``(verts, tris, meshset, topo, holes, nm, postprocessed)``; the
    same measurement/post-process the tier always applied to a single fTetWild
    boundary."""
    cand_ms = ml.MeshSet()
    cand_ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(cand_v, np.float32),
                             face_matrix=np.asarray(cand_t, np.int32)))
    cand_after = cand_ms.apply_filter('get_topological_measures')
    cand_holes = boundary_loop_stats(cand_v, cand_t)[0]
    cand_nm = cand_after.get('non_two_manifold_edges', 0)
    (cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, pp) = \
        _ftetwild_manifold_postprocess(
            ml, tmpdir, cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm)
    return cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, pp


def _decimate_boundary(ml, cand_v, cand_t, target):
    """Quadric edge collapse of an fTetWild boundary to ``target`` faces.

    Boundary and topology are preserved and a planar quadric is used, so a
    closed boundary stays closed. The raw bridge output carries every
    tetrahedron vertex (interior ones included), so the unreferenced vertices
    are dropped first. Returns ``(verts, tris)`` or None on failure/empty
    output (the caller then falls back to the undecimated boundary)."""
    try:
        rv, rt = _referenced_only(cand_v, cand_t)
        if len(rt) == 0:
            return None
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=rv,
                            face_matrix=np.asarray(rt, np.int32)))
        ms.apply_filter('meshing_decimation_quadric_edge_collapse',
                        targetfacenum=int(target),
                        qualitythr=DENSE_QUALITY,
                        preserveboundary=True,
                        preservenormal=True,
                        preservetopology=True,
                        planarquadric=True,
                        autoclean=True)
        m = ms.current_mesh()
        dv = np.asarray(m.vertex_matrix(), np.float64)
        dt = np.asarray(m.face_matrix(), np.int64)
        if len(dt) == 0:
            return None
        return dv, dt
    except Exception:
        return None


def _ftetwild_decimate_and_postprocess(ml, tmpdir, cand_v, cand_t, v, t, spec):
    """Decimate a wastefully dense fTetWild boundary and pick a candidate.

    The escalating target ladder ``spec.dense_target_ladder`` (multiples of the
    input face count, plus the special 'threshold' entry) is tried in order. A
    target is accepted only when its
    post-processed result is strictly watertight AND its one-sided Hausdorff
    distance to the input is at most the raw boundary's own distance plus
    ``DECIMATE_HD_MARGIN``; the raw distance is measured once on the raw
    boundary, so decimation may not make the shape measurably worse than
    fTetWild already did (its own result may already exceed the shape
    threshold, which is a flag, not a rejection). A raw boundary already
    within ``FTETWILD_MAX_HAUSDORFF_REL`` additionally requires the
    decimated candidate to stay within it, so decimation can never turn an
    unflagged result into a flagged one. Returns
    ``(payload_or_None, info)`` where payload is the tuple built by
    ``_ftetwild_candidate``; None means every target failed and the caller
    falls back to the undecimated result. ``info`` carries the reported
    ``ftetwild_*`` fields either way."""
    input_faces = int(len(t))
    raw_faces = int(len(cand_t))
    info = {'ftetwild_decimated': False, 'ftetwild_faces_final': raw_faces,
            'ftetwild_decimate_time': 0.0}
    hd_raw, _ = _hausdorff_rel(ml, v, t, cand_v, cand_t,
                               samples=spec.ftetwild_hausdorff_samples)
    info['ftetwild_hausdorff_raw'] = hd_raw
    limit = (hd_raw if hd_raw is not None else 0.0) + DECIMATE_HD_MARGIN
    # A raw boundary already inside the shape threshold must not be pushed
    # over it by decimation: otherwise an UNFLAGGED fTetWild result becomes a
    # FLAGGED one for a face-count gain (thingi10k_78968: raw 0.0087, a
    # decimated 0.0126 within the margin but past the 0.01 threshold). When
    # the raw boundary is already above the threshold the margin rule alone
    # applies -- fTetWild's own result is the reference, and a flag is not a
    # rejection there.
    raw_within_shape = (hd_raw is not None
                        and hd_raw <= FTETWILD_MAX_HAUSDORFF_REL)
    targets = []
    for mult in spec.dense_target_ladder:
        if mult == 'threshold':
            tg = spec.dense_ratio * max(input_faces, spec.dense_min_faces)
        else:
            tg = max(int(round(mult * input_faces)), spec.dense_min_faces)
        if tg not in targets:
            targets.append(tg)
    attempts = []
    for tg in targets:
        t0 = time.perf_counter()
        try:
            dec = _decimate_boundary(ml, cand_v, cand_t, tg)
        except Exception:
            dec = None
        info['ftetwild_decimate_time'] = round(
            info['ftetwild_decimate_time'] + time.perf_counter() - t0, 3)
        rec = {'target': tg}
        if dec is None:
            rec['reason'] = 'decimation_failed'
            attempts.append(rec)
            continue
        dv, dt = dec
        rec['faces'] = int(len(dt))
        payload = _ftetwild_candidate(ml, tmpdir, dv, dt)
        cv, ct, _cms, cafter, choles, cnm, _pp = payload
        rec['holes'] = int(choles)
        rec['non_manifold'] = int(cnm)
        if not (choles == 0 and cnm == 0 and cafter.get('is_mesh_two_manifold')):
            rec['reason'] = 'not_watertight'
            attempts.append(rec)
            continue
        hd_dec, _ = _hausdorff_rel(ml, v, t, cv, ct,
                                   samples=spec.ftetwild_hausdorff_samples)
        rec['hausdorff_rel'] = hd_dec
        if (hd_dec is None or hd_dec > limit
                or (raw_within_shape
                    and hd_dec > FTETWILD_MAX_HAUSDORFF_REL)):
            rec['reason'] = 'hausdorff'
            attempts.append(rec)
            continue
        info.update({'ftetwild_decimated': True,
                     # ``*_faces_final`` is the candidate actually adopted,
                     # AFTER the in-tier manifold3d post-process (which can
                     # add faces); ``*_faces_decimated`` is the quadric output
                     # before it. Neither is the final file's face count --
                     # stage 2 rebuilds the adopted boundary again.
                     'ftetwild_faces_final': int(len(ct)),
                     'ftetwild_faces_decimated': int(len(dt)),
                     'ftetwild_decimate_target': tg,
                     'ftetwild_hausdorff_decimated': hd_dec,
                     'ftetwild_decimate_attempts': attempts + [rec],
                     'hausdorff_rel': hd_dec})
        return payload, info
    info['ftetwild_decimate_attempts'] = attempts
    if attempts:
        info['ftetwild_decimate_fallback'] = attempts[-1].get('reason')
    return None, info


def _ftetwild_boundary(ml, tmpdir, inter, tag, params, timeout, v, t, spec):
    """One fTetWild run on ``inter`` plus the dense-boundary handling.

    A wastefully dense boundary
    (``spec.dense_ratio * max(input_faces, spec.dense_min_faces)``) is
    decimated first; when every decimation target fails the undecimated
    boundary is measured instead and ``dense_failed`` is True. Returns
    ``(attempt_report, candidate, dense_failed)`` where candidate is
    ``(verts, tris, meshset, topo, holes, nm)`` or None."""
    out_obj = os.path.join(tmpdir, 'ftetwild_out_%s.obj' % tag)
    t0 = time.perf_counter()
    mrep, _ok = run_ftetwild(inter, out_obj, params, timeout=timeout)
    att = {'attempt': tag, 'wall_time': round(time.perf_counter() - t0, 2)}
    att.update(mrep)
    if 'error' in mrep or not os.path.exists(out_obj):
        return att, None, False
    cand_v, cand_t = read_obj(out_obj)
    if len(cand_t) == 0:
        return att, None, False
    raw_faces = int(len(cand_t))
    att['ftetwild_faces_raw'] = raw_faces
    att['ftetwild_decimated'] = False
    att['ftetwild_faces_final'] = raw_faces
    att['ftetwild_decimate_time'] = 0.0
    dense_failed = False
    if (v is not None and t is not None and spec.dense_target_ladder
            and raw_faces > spec.dense_ratio * max(len(t), spec.dense_min_faces)):
        payload, dec_info = _ftetwild_decimate_and_postprocess(
            ml, tmpdir, cand_v, cand_t, v, t, spec)
        att.update(dec_info)
        if payload is not None:
            cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, pp = payload
            att['manifold_postprocessed'] = pp
            att['output_holes'] = cand_holes
            att['output_non_manifold'] = cand_nm
            return att, (cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm), False
        # every decimation target failed: fall through to the undecimated
        # boundary, but flag the failure so the caller can retry (Extreme)
        dense_failed = True
    (cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, pp) = \
        _ftetwild_candidate(ml, tmpdir, cand_v, cand_t)
    att['manifold_postprocessed'] = pp
    att['output_holes'] = cand_holes
    att['output_non_manifold'] = cand_nm
    return att, (cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm), dense_failed


def _ftetwild_attempt(ml, tmpdir, inter, tag, params, timeout, v=None, t=None,
                      spec=None):
    """One attempt of the fTetWild tier. Returns ``(attempt_report,
    candidate)``; candidate is ``(verts, tris, meshset, topo, holes, nm)`` or
    None.

    When the attempt used the bridge defaults and every dense decimation rung
    failed, the Extreme preset makes one more run with the tetrahedron-quality
    optimisation on before falling back to the undecimated boundary; Balanced
    and Thorough keep the plain undecimated fallback."""
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    att, cand, dense_failed = _ftetwild_boundary(
        ml, tmpdir, inter, tag, params, timeout, v, t, spec)
    if (cand is not None and dense_failed
            and spec.ftetwild_optimize_retry_on_dense_fail
            and not (params or {}).get('optimize')):
        o_att, o_cand, _ = _ftetwild_boundary(
            ml, tmpdir, inter, tag + '_optimize', {'optimize': True},
            timeout, v, t, spec)
        att['optimize_retry'] = dict(o_att, ran=True)
        if o_cand is not None:
            att['optimize_retry_adopted'] = True
            att['ftetwild_faces_final'] = int(len(o_cand[1]))
            att['manifold_postprocessed'] = o_att.get('manifold_postprocessed')
            att['output_holes'] = o_cand[4]
            att['output_non_manifold'] = o_cand[5]
            return att, o_cand
        att['optimize_retry_adopted'] = False
    return att, cand


def _ftetwild_tier(ml, ms, after, stats, v, t, tmpdir, ftetwild, spec=None):
    """fTetWild basamak of the deep-repair ladder. Returns ``(ms, after)``.

    Skipped for inputs above the intensity spec's fTetWild size cap
    (reject_reason 'too_large'; None means no cap). The first attempt uses the
    bridge defaults (optimize=False); when its boundary fails the
    holes/non-manifold guard, one retry with FTETWILD_RETRY_PARAMS runs within
    the remaining budget. ``attempts`` lists every run, ``adopted_attempt``
    names the adopted one; the top-level report fields describe the adopted
    (or else the last) attempt. An adopted result far from the input is
    flagged (``shape_changed``), not rejected."""
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    stats['experimental_ftetwild'] = False
    if ftetwild == 'auto' and not ftetwild_available():
        ftetwild = False
    if ftetwild and len(t) > 0:
        ft_rep = {'ran': True, 'trigger': 'auto' if ftetwild == 'auto' else 'always'}
        cur_holes = boundary_loop_stats(ms.current_mesh().vertex_matrix(),
                                        ms.current_mesh().face_matrix())[0]
        cur_nm = after.get('non_two_manifold_edges', 0)
        cur_si = 0
        if ftetwild is True:
            ms.apply_filter('compute_selection_by_self_intersections_per_face')
            cur_si = int(ms.current_mesh().face_selection_array().sum())
        if cur_si > 0 or cur_holes > 0 or cur_nm > 0:
            if (spec.ftetwild_max_faces is not None
                    and len(t) > spec.ftetwild_max_faces):
                stats['experimental_ftetwild'] = {
                    'ran': False, 'adopted': False, 'reject_reason': 'too_large',
                    'input_faces': int(len(t)),
                    'max_faces': spec.ftetwild_max_faces,
                    'trigger': ft_rep['trigger']}
                return ms, after
            try:
                inter = os.path.join(tmpdir, 'ftetwild_in.obj')
                write_obj(inter, v, t)
                attempts = []
                t_start = time.perf_counter()
                # the user option (--ftetwild-optimize / config) runs the
                # optimising attempt directly; otherwise default, then retry
                user_params = ftetwild_params()
                plan = ([('optimize', user_params)] if user_params else
                        [('default', None), ('optimize', FTETWILD_RETRY_PARAMS)])
                chosen = None
                for tag, params in plan:
                    if attempts:
                        last = attempts[-1]
                        # an Extreme dense-failure retry already ran the
                        # optimising attempt inside _ftetwild_attempt
                        if (tag == 'optimize'
                                and (last.get('optimize_retry') or {}).get('ran')):
                            break
                        # retry only when the first boundary exists but
                        # fails the holes/non-manifold guard
                        if last.get('reject_reason') != 'holes_nm':
                            break
                        remaining = spec.ftetwild_timeout - (time.perf_counter() - t_start)
                        if remaining < FTETWILD_RETRY_MIN_SECONDS:
                            last['retry_skipped'] = 'budget'
                            break
                    else:
                        remaining = spec.ftetwild_timeout
                    att, cand = _ftetwild_attempt(ml, tmpdir, inter, tag,
                                                  params, remaining, v, t, spec)
                    attempts.append(att)
                    if cand is None:
                        continue
                    if cand[4] <= cur_holes and cand[5] <= cur_nm:
                        chosen = (att, cand)
                        break
                    att['reject_reason'] = 'holes_nm'
                final = chosen[0] if chosen else attempts[-1]
                ft_rep.update({k: val for k, val in final.items() if k != 'attempt'})
                ft_rep['attempts'] = attempts
                ft_rep['adopted'] = chosen is not None
                if chosen:
                    att, (cand_v, cand_t, cand_ms, cand_after, _h, _n) = chosen
                    ft_rep['adopted_attempt'] = att['attempt']
                    ft_rep.pop('reject_reason', None)
                    # shape flag: fTetWild can close openings and cavities,
                    # adding surface far from the input. A decimated candidate
                    # already carries the distance measured during its ladder,
                    # so it is reused instead of recomputed.
                    hd_max = ft_rep.get('hausdorff_rel')
                    if hd_max is None:
                        hd_max, _hd_mean = _hausdorff_rel(
                            ml, v, t, cand_v, cand_t,
                            samples=spec.ftetwild_hausdorff_samples)
                    ft_rep['hausdorff_rel'] = hd_max
                    ft_rep['shape_changed'] = bool(
                        hd_max is not None and hd_max > FTETWILD_MAX_HAUSDORFF_REL)
                    if ft_rep['shape_changed']:
                        stats['shape_changed'] = True
                    ms = cand_ms
                    after = cand_after
            except Exception as e:  # noqa: BLE001 - the prototype never crashes a repair
                ft_rep['error'] = str(e)
            stats['experimental_ftetwild'] = ft_rep
    return ms, after


# --------------------------------------------------------------------------
# Deep-repair ladder. After the fast stage-1 chain, the remaining holes and
# non-manifold edges can be handed to slower tiers: 'local' (re-mesh only the
# damaged regions, the rest of the surface untouched) and 'ftetwild'
# (tetrahedralize the original input). Mode: 'off' runs no tier and only
# reports what is available, 'local' runs the local tier, 'full' runs the
# fTetWild tier exactly as before the ladder existed (the local tier is not
# part of 'full' until it has been measured on the corpora).
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


def _edge_uses(tris):
    """Undirected edges of ``tris`` as (keys, counts, V) with key = a*V+b."""
    tris = np.asarray(tris, dtype=np.int64)
    V = int(tris.max()) + 1
    a = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    b = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    keys = np.minimum(a, b) * V + np.maximum(a, b)
    uniq, inv, counts = np.unique(keys, return_inverse=True, return_counts=True)
    return keys, uniq, inv, counts


def _damaged_region(tris, max_faces=None):
    """Faces of the damaged region: every face with a boundary edge (used
    once) or a non-manifold edge (used more than twice), grown by one vertex
    ring. Returns (face mask, number of connected regions); the region count
    is None when the region has more than ``max_faces`` faces (not
    computed)."""
    tris = np.asarray(tris, dtype=np.int64)
    F = len(tris)
    _keys, _uniq, inv, counts = _edge_uses(tris)
    bad_edge = (counts[inv] != 2)
    seed = bad_edge[:F] | bad_edge[F:2 * F] | bad_edge[2 * F:]
    if not seed.any():
        return seed, 0
    vmark = np.zeros(int(tris.max()) + 1, dtype=bool)
    vmark[tris[seed].ravel()] = True
    region = vmark[tris].any(axis=1)
    if max_faces is not None and int(region.sum()) > max_faces:
        return region, None
    # connected regions (faces sharing a vertex), union-find over vertices
    parent = np.arange(len(vmark))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x
    for f in tris[region]:
        r0 = find(f[0])
        for x in f[1:]:
            rx = find(x)
            if rx != r0:
                parent[rx] = r0
    roots = {find(x) for x in np.unique(tris[region])}
    return region, len(roots)


def _faces_preserved(verts, tris, new_verts, new_tris):
    """True when every face of (verts, tris) occurs in (new_verts,
    new_tris) with bit-identical float32 vertex coordinates and the same
    orientation, counted with multiplicity (index- and order-independent,
    a face may start at any of its three corners)."""
    a = np.asarray(verts, dtype=np.float32)[np.asarray(tris, dtype=np.int64)]
    b = np.asarray(new_verts, dtype=np.float32)[np.asarray(new_tris, dtype=np.int64)]
    pts = np.ascontiguousarray(np.concatenate([a.reshape(-1, 3), b.reshape(-1, 3)]))
    _u, ids = np.unique(pts.view(np.dtype((np.void, 12))).ravel(), return_inverse=True)
    ids = ids.reshape(-1, 3)
    fa, fb = ids[:len(a)], ids[len(a):]

    def canon(f):
        # lexicographically smallest cyclic rotation (also well defined when
        # two corners share a coordinate)
        best = f
        for k in (1, 2):
            r = np.roll(f, -k, axis=1)
            less = ((r[:, 0] < best[:, 0])
                    | ((r[:, 0] == best[:, 0])
                       & ((r[:, 1] < best[:, 1])
                          | ((r[:, 1] == best[:, 1]) & (r[:, 2] < best[:, 2])))))
            best = np.where(less[:, None], r, best)
        return best
    ua, ca = np.unique(canon(fa), axis=0, return_counts=True)
    ub, cb = np.unique(canon(fb), axis=0, return_counts=True)
    if len(ua) == 0:
        return True
    # count of each needed face among the available ones
    both = np.concatenate([ub, ua])
    _uu, inv = np.unique(both, axis=0, return_inverse=True)
    inv = inv.ravel()
    have = np.zeros(len(_uu), dtype=np.int64)
    have[inv[:len(ub)]] = cb
    return bool(np.all(have[inv[len(ub):]] >= ca))


def _referenced_only(verts, tris):
    """(verts, tris) without the vertices no face references (float64)."""
    tris = np.asarray(tris, dtype=np.int64)
    used = np.unique(tris)
    remap = np.full(len(verts), -1, dtype=np.int64)
    remap[used] = np.arange(len(used))
    return np.asarray(verts, np.float64)[used], remap[tris]


def _hausdorff_rel(ml, in_v, in_t, out_v, out_t, samples=HAUSDORFF_SAMPLES):
    """One-sided Hausdorff distance, samples on the output, distance to the
    input, relative to the input bounding-box diagonal: ``(max, mean)``, or
    ``(None, None)`` for an empty mesh or a zero diagonal. Also used by
    scripts/benchmark_repair_corpus.py.

    Both meshes are reduced to the vertices their faces reference first:
    ``get_hausdorff_distance(samplevert=True)`` also samples unreferenced
    vertices, and the fTetWild bridge output carries every tetrahedron
    vertex, interior ones included (a closed sphere plus random interior
    points measures ~26 % instead of 0)."""
    if len(in_t) == 0 or len(out_t) == 0:
        return None, None
    in_v, in_t = _referenced_only(in_v, in_t)
    out_v, out_t = _referenced_only(out_v, out_t)
    diag = float(np.linalg.norm(in_v.max(0) - in_v.min(0)))
    if not diag:
        return None, None
    # The filter's ``maxdist`` is a ``PercentageValue`` and CLIPS every
    # distance above it, reporting exactly the cap (a raw and a decimated
    # candidate would then look equally far from the input). It is hard-capped
    # at 100 by pymeshlab (``InvalidPercentageException`` above that), and
    # 100 % is measured against the UNION bounding box of the two meshes, i.e.
    # exactly the largest possible point-to-point distance between them
    # (verified: displacements of 1.5x-100x the input diagonal come back
    # unclipped). ``PercentageValue(100)`` is therefore the maximum the API
    # allows AND provably clipping-free; smaller values do clip (5 %/50 %
    # return a capped or empty result). Do not lower it.
    hd = ml.MeshSet()
    hd.add_mesh(ml.Mesh(vertex_matrix=in_v,
                        face_matrix=np.asarray(in_t, np.int32)))      # id 0
    hd.add_mesh(ml.Mesh(vertex_matrix=np.asarray(out_v, np.float64),
                        face_matrix=np.asarray(out_t, np.int32)))     # id 1
    r = hd.apply_filter('get_hausdorff_distance', sampledmesh=1, targetmesh=0,
                        samplevert=True, sampleface=True,
                        samplenum=samples,
                        maxdist=ml.PercentageValue(100))
    return (round(float(r.get('max') or 0) / diag, 6),
            round(float(r.get('mean') or 0) / diag, 6))


def _count_si(ml, ms):
    ms.apply_filter('compute_selection_by_self_intersections_per_face')
    n = int(ms.current_mesh().face_selection_array().sum())
    ms.apply_filter('set_selection_none')
    return n


def _umbrella_fair(verts, tris, first_free, steps):
    """Umbrella-operator smoothing (each vertex moves to the mean of its
    1-ring neighbours) of the vertices with index >= ``first_free``; all
    other vertices are fixed."""
    n = len(verts)
    if first_free >= n or steps <= 0:
        return verts
    a = np.concatenate([tris[:, 0], tris[:, 1], tris[:, 2]])
    b = np.concatenate([tris[:, 1], tris[:, 2], tris[:, 0]])
    src = np.concatenate([a, b])
    dst = np.concatenate([b, a])
    deg = np.bincount(src, minlength=n).astype(np.float64)
    free = np.zeros(n, dtype=bool)
    free[first_free:] = True
    free &= deg > 0
    out = verts.copy()
    for _ in range(steps):
        acc = np.zeros_like(out)
        np.add.at(acc, src, out[dst])
        out[free] = acc[free] / deg[free, None]
    return out


def _local_remesh_tier(ml, ms, after):
    """Local re-mesh basamak (pymeshlab-only prototype).

    The damaged region (faces on a boundary or non-manifold edge, grown by
    one vertex ring) is deleted, pinched vertices of the new boundary are
    split, the openings are closed with ``meshing_close_holes`` (refined to
    the mean edge length of the deleted region) and only the new interior
    vertices are smoothed (Laplacian, the hole boundary stays fixed). The
    result is adopted only when holes and non-manifold edges are no worse
    and not both unchanged, self-intersections do not increase, and every
    face outside the deleted region is still present with identical
    coordinates. Returns ``(ms, after, report)``.
    """
    t0 = time.perf_counter()
    m = ms.current_mesh()
    v = np.asarray(m.vertex_matrix(), dtype=np.float64)
    t = np.asarray(m.face_matrix(), dtype=np.int64)
    rep = {'ran': True, 'adopted': False}
    base_holes, base_loop = boundary_loop_stats(v, t)
    base_nm = after.get('non_two_manifold_edges', 0)
    rep.update(holes_before=int(base_holes), nm_before=int(base_nm),
               max_loop_len=int(base_loop))

    def skip(reason):
        rep['reject_reason'] = reason
        rep['time'] = round(time.perf_counter() - t0, 3)
        return ms, after, rep
    if base_nm > LOCAL_MAX_NM_EDGES:
        return skip('scope: non-manifold edges')
    if base_loop > LOCAL_MAX_LOOP_LEN:
        return skip('scope: boundary loop too long')
    region, n_regions = _damaged_region(t, max_faces=LOCAL_MAX_REMOVED_FACES)
    rep['faces_removed'] = int(region.sum())
    if n_regions is None:
        return skip('scope: region too large')
    rep['regions'] = int(n_regions)
    if n_regions == 0:
        return skip('no damaged region')
    if n_regions > LOCAL_MAX_REGIONS:
        return skip('scope: too many regions')
    kept = t[~region]
    if len(kept) == 0:
        return skip('region covers the whole mesh')
    rt = t[region]
    el = np.linalg.norm(v[rt] - v[np.roll(rt, 1, axis=1)], axis=2)
    edge_len = float(el.mean()) if el.size else 0.0
    used = np.unique(kept)
    remap = -np.ones(len(v), dtype=np.int64)
    remap[used] = np.arange(len(used))
    kv, kt = v[used], remap[kept]
    try:
        trial = ml.MeshSet()
        trial.add_mesh(ml.Mesh(vertex_matrix=kv, face_matrix=kt.astype(np.int32)))
        trial.apply_filter('meshing_repair_non_manifold_vertices')
        n0 = trial.current_mesh().face_number()
        nv0 = trial.current_mesh().vertex_number()
        trial.apply_filter('meshing_close_holes', maxholesize=LOCAL_REMESH_MAX_HOLE,
                           newfaceselected=True, selfintersection=True,
                           refinehole=LOCAL_REMESH_REFINE and edge_len > 0,
                           refineholeedgelen=ml.PureValue(edge_len))
        rep['faces_added'] = int(trial.current_mesh().face_number() - n0)
        if time.perf_counter() - t0 > LOCAL_MAX_SECONDS:
            return skip('time')
        tm = trial.current_mesh()
        nv = np.asarray(tm.vertex_matrix(), dtype=np.float64)
        nt = np.asarray(tm.face_matrix(), dtype=np.int64)
        # Fairing on the new interior vertices only (the refinement appends
        # them after the existing ones), so the hole boundary stays fixed.
        # pymeshlab's selected-only Laplacian also moved vertices of the
        # surrounding surface, hence the explicit umbrella operator here.
        nv = _umbrella_fair(nv, nt, nv0, LOCAL_REMESH_FAIR_STEPS)
        rep['fairing_iters'] = LOCAL_REMESH_FAIR_STEPS
        trial = ml.MeshSet()
        trial.add_mesh(ml.Mesh(vertex_matrix=nv, face_matrix=nt.astype(np.int32)))
        t_after = trial.apply_filter('get_topological_measures')
        holes = boundary_loop_stats(nv, nt)[0]
        nm = t_after.get('non_two_manifold_edges', 0)
        rep.update(holes_after=int(holes), nm_after=int(nm))
        # guard 1: outside faces unchanged (exact coordinates, any order)
        outside_ok = _faces_preserved(kv, kt, nv, nt)
        rep['outside_unchanged'] = bool(outside_ok)
        # guard 2: holes / nm no worse, and some progress
        better = (holes <= base_holes and nm <= base_nm
                  and (holes, nm) != (base_holes, base_nm))
        si_ok = True
        if outside_ok and better and time.perf_counter() - t0 > LOCAL_MAX_SECONDS:
            return skip('time')
        if outside_ok and better:
            base_si = _count_si(ml, ms)
            new_si = _count_si(ml, trial)
            rep.update(si_before=base_si, si_after=new_si)
            si_ok = new_si <= base_si
        if not outside_ok:
            rep['reject_reason'] = 'faces outside the region changed'
        elif not better:
            rep['reject_reason'] = 'no improvement on holes/non-manifold edges'
        elif not si_ok:
            rep['reject_reason'] = 'more self-intersections'
        else:
            rep['hausdorff_rel'] = _hausdorff_rel(ml, v, t, nv, nt)[0]
            rep['adopted'] = True
            rep['time'] = round(time.perf_counter() - t0, 3)
            return trial, t_after, rep
    except Exception as e:  # noqa: BLE001 - a prototype never crashes a repair
        rep['error'] = str(e)
    rep['time'] = round(time.perf_counter() - t0, 3)
    return ms, after, rep


def weld_reload_equivalent(verts, tris):
    """Weld a mesh into the form an STL save/reload reproduces.

    STL stores no vertex sharing: on reload a loader re-welds vertices whose
    stored float32 positions are exactly equal. Two positions that are distinct
    in the in-memory float64 arrays can therefore collapse after a float32
    write/read, creating edges used by more than two triangles that the
    in-memory index topology does not show (observed on the proxy/closing
    seam vertices: 3052 vertices, 3046 unique in float64 but 3040 in float32,
    i.e. 5 non-manifold edges after reload).

    Returns ``(verts, tris)`` in that reload-equivalent form: positions cast
    to float32, exactly-coincident positions merged, degenerate and duplicate
    faces dropped, unreferenced vertices removed. Pure numpy (no pymeshlab) so
    the method registry's guard can apply the same rule without importing it.
    """
    return topology.weld_reload_equivalent(verts, tris)


def _repair_welded_topology(ml, verts, tris):
    """Topology repair of a reload-equivalent mesh (P-FIX).

    Merging coincident positions can leave non-manifold edges / duplicate
    faces; this runs the same repair subset the Stage-1 chain uses so a
    legitimately repairable candidate is not falsely rejected. Never raises.
    """
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    for name, params in (('meshing_repair_non_manifold_edges', {}),
                         ('meshing_remove_duplicate_faces', {}),
                         ('meshing_repair_non_manifold_vertices', {}),
                         ('meshing_remove_unreferenced_vertices', {})):
        try:
            ms.apply_filter(name, **params)
        except Exception:  # noqa: BLE001 - a repair pass never crashes a tier
            pass
    return (np.asarray(ms.current_mesh().vertex_matrix(), dtype=np.float32),
            np.asarray(ms.current_mesh().face_matrix(), dtype=np.int32))


def reload_strict_holes_nm(verts, tris):
    """Strict (holes, non-manifold) on the STL save/reload-equivalent mesh.

    Applies ``weld_reload_equivalent`` (positions cast to float32 with
    exactly-coincident positions merged) and then ``defects.detect`` -- the
    same strict metric the repair guards use, measured on the mesh the user
    gets back after a save/reload rather than the in-memory index topology.
    Pure numpy (no pymeshlab)."""
    wv, wt = weld_reload_equivalent(verts, tris)
    d = detect_defects(wv, wt)
    return len(d['holes']), len(d['non_manifold'])


P_WELD_MAX_NUDGE_ATTEMPTS = 12
P_WELD_MAX_HOLE = 200
P_WELD_MIN_FACE_FRACTION = 0.98


def _separate_weld_collisions(verts, tris):
    """Split vertices that are distinct by index but coincide after the float32
    STL write.

    STL stores no vertex sharing, so two in-memory vertices whose float32
    positions are exactly equal are welded by the loader; when the in-memory
    index topology is a clean 2-manifold this weld is the ONLY reason the
    reloaded mesh is non-manifold. Keeping the vertices distinct by nudging
    all but one member of each collision group by a few float32 ULPs preserves
    the in-memory (manifold) topology on reload, with a geometry change below
    one part in 1e6. Pure numpy.
    """
    v64 = np.asarray(verts, dtype=np.float64).copy()
    t = np.asarray(tris, dtype=np.int64)
    if len(v64) == 0 or len(t) == 0:
        return v64.astype(np.float32), t
    v32 = v64.astype(np.float32)
    inv = topology.weld_vertices(v32)[1]
    counts = np.bincount(inv, minlength=int(inv.max()) + 1)
    if (counts <= 1).all():
        return v32, t
    order = np.argsort(inv, kind='stable')
    pos = np.empty(len(inv), dtype=np.int64)
    pos[order] = np.arange(len(inv))
    group_start = (np.cumsum(counts) - counts)[inv]
    rank = (pos - group_start).astype(np.float64)
    nonrep = rank > 0
    mag = np.maximum(np.abs(v32), np.float32(1.0))
    ulp = np.maximum(np.spacing(mag).max(axis=1).astype(np.float64), 1e-7)
    direction = np.array([1.0, 1.0, 1.0]) / np.sqrt(3.0)
    for attempt in range(P_WELD_MAX_NUDGE_ATTEMPTS):
        d = v64.copy()
        step = ulp[nonrep] * rank[nonrep] * (2.0 ** (attempt + 1))
        d[nonrep] += step[:, None] * direction[None, :]
        d32 = d.astype(np.float32)
        if len(topology.weld_vertices(d32)[0]) == len(d32):
            return d32, t
    return v32, t


def _close_small_holes(ml, verts, tris):
    """Repair non-manifold vertices then cap small boundary loops.

    Only used by the P-WELD fallback for a welded mesh that is not a clean
    in-memory topology; a failure leaves the input unchanged."""
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        ms.apply_filter('meshing_repair_non_manifold_vertices')
        ms.apply_filter('meshing_close_holes', maxholesize=P_WELD_MAX_HOLE)
        ms.apply_filter('meshing_remove_unreferenced_vertices')
        m = ms.current_mesh()
        return (np.asarray(m.vertex_matrix(), dtype=np.float32),
                np.asarray(m.face_matrix(), dtype=np.int32))
    except Exception:  # noqa: BLE001 - a repair pass never crashes a tier
        return np.asarray(verts, np.float32), np.asarray(tris, np.int32)


def p_weld_final(ml, verts, tris):
    """Reload-safe final pass (P-WELD).

    The last mesh of every STL repair is checked in the form the user reloads
    (float32 positions with exactly-coincident vertices welded). When that form
    is not strict-watertight the pass tries, in order:

      1. splitting float32-coincidence collisions when the in-memory index
         topology is already a clean 2-manifold (the common stage-2 seam case);
      2. topologically repairing the welded mesh (remove/split the extra faces)
         and capping the small holes that creates.

    A candidate is adopted only when it is strict-watertight after the weld
    (0 holes, 0 non-manifold) and, for the fallback, keeps at least
    ``P_WELD_MIN_FACE_FRACTION`` of the welded faces. The untouched mesh is
    returned otherwise, so the pass can never make the output worse. Returns
    ``(verts, tris, report)`` with ``report`` None when the mesh was already
    reload-watertight.
    """
    v0 = np.asarray(verts)
    t0 = np.asarray(tris)
    base_h, base_nm = reload_strict_holes_nm(v0, t0)
    if base_h == 0 and base_nm == 0:
        return v0, t0, None
    rec = {'applied': False, 'method': None,
           'holes_before': int(base_h),
           'non_manifold_before': int(base_nm),
           'holes_after': int(base_h),
           'non_manifold_after': int(base_nm),
           'vertices_nudged': 0, 'faces_before': int(len(t0)),
           'faces_after': int(len(t0))}
    try:
        d = detect_defects(v0, t0)
        if len(d['holes']) == 0 and len(d['non_manifold']) == 0:
            cv, ct = _separate_weld_collisions(v0, t0)
            ch, cnm = reload_strict_holes_nm(cv, ct)
            if ch == 0 and cnm == 0:
                nudged = int((np.asarray(cv, np.float32)
                              != np.asarray(v0, np.float32)).any(axis=1).sum())
                rec.update(applied=True, method='split-collisions',
                           holes_after=0, non_manifold_after=0,
                           vertices_nudged=nudged,
                           faces_after=int(len(ct)))
                return cv, ct, rec
        wv, wt = weld_reload_equivalent(v0, t0)
        welded_faces = max(len(wt), 1)
        cands = [('weld-repair', _repair_welded_topology(ml, wv, wt))]
        rep_v, rep_t = cands[0][1]
        cands.append(('weld-repair+close', _close_small_holes(ml, rep_v, rep_t)))
        for name, (cv, ct) in cands:
            cv = np.asarray(cv, np.float32)
            ct = np.asarray(ct, np.int32)
            if len(ct) < P_WELD_MIN_FACE_FRACTION * welded_faces:
                continue
            ch, cnm = reload_strict_holes_nm(cv, ct)
            if ch == 0 and cnm == 0:
                rec.update(applied=True, method=name, holes_after=0,
                           non_manifold_after=0, faces_after=int(len(ct)))
                return cv, ct, rec
    except Exception:  # noqa: BLE001 - the final pass never crashes a repair
        pass
    return v0, t0, rec


def enforce_reload_verdict(report, verts, tris):
    """Honest top-level verdict: never claim watertight for a mesh that is
    not strict-watertight after the reload-equivalent weld (P-HONEST).

    Only a report that currently claims watertight is considered: stage 1
    two-manifold with no hole AND a successful stage-2 rebuild. If the saved
    mesh fails the reload check, the fields ``classification.classify()``
    reads are rewritten so the category can no longer be watertight, plus
    explicit reload markers. A genuinely watertight mesh returns False with
    NO report change, so its report and output stay byte-identical.
    Returns True when the report was downgraded."""
    if not isinstance(report, dict):
        return False
    s1 = report.get('stage1')
    if not isinstance(s1, dict):
        return False
    s2 = report.get('stage2')
    claims = (bool(s1.get('two_manifold'))
              and s1.get('holes_remaining', 0) == 0
              and bool(s2 and s2.get('ok')))
    if not claims:
        return False
    holes, nm = reload_strict_holes_nm(verts, tris)
    if holes == 0 and nm == 0:
        return False
    s1['reload_holes'] = int(holes)
    s1['reload_non_manifold'] = int(nm)
    s1['two_manifold'] = bool(nm == 0)
    s1['holes_remaining'] = int(holes)
    report['reload_watertight'] = False
    if isinstance(s2, dict):
        s2['watertight_after_reload'] = False
    return True


FLAP_SKIP_KEYS = ('loops_skipped_nm', 'loops_skipped_flat',
                  'loops_skipped_sheet', 'loops_skipped_coplanar',
                  'loops_skipped_sliver', 'loops_fan_fallback')


def _flap_adopt(base_holes, base_nm, cand_holes, cand_nm):
    """Whether a flap candidate should replace the pre-flap Stage-1 mesh.

    Holes and non-manifold edges are both boundary defects, so a candidate is
    accepted when the combined reload-honest damage ``holes + non-manifold``
    does not increase. This lets a flap that closes many loops while welding
    near-coincident rims (which introduces a few new non-manifold edges) be
    adopted -- the deep-repair ladder rebuilds the residual non-manifold edges
    -- while still rejecting a candidate that opens holes or explodes the
    non-manifold count. Measured on ornate-frame (67/16 -> 16/22) and
    artec_metal-nut (2/1 -> 0/2), which the old ``holes<= and nm<=`` gate
    rejected despite the standalone flap->Graft pipeline being a clear win.
    """
    return (int(cand_holes) + int(cand_nm)) <= (int(base_holes) + int(base_nm))


def _flap_max_orig_move(frep):
    """Largest movement of an ORIGINAL vertex from the flap's crack weld (mm).

    ``flap_fill`` welds nearly-coincident open-chain ends before filling; each
    merge records ``(va, vb, dist, is_orig)`` in ``boundary_normalize``'s
    ``weld_sites``. Only merges that touch an original vertex move one, so the
    max ``dist`` over those is the geometry change the flap imposes on the input
    surface (the patch itself is new geometry built from hole vertices).
    """
    sites = (frep.get('boundary_normalize') or {}).get('weld_sites') or []
    moves = [float(s[2]) for s in sites if len(s) >= 4 and s[3]]
    return round(max(moves), 6) if moves else 0.0


def _reload_component_count(verts, tris):
    """Vertex-connected components in the reload-equivalent mesh (the "parts"
    a slicer sees, since STL drops vertex sharing)."""
    wv, wt = weld_reload_equivalent(verts, tris)
    if len(wt) == 0:
        return 0
    import scipy.sparse as _sp
    from scipy.sparse.csgraph import connected_components as _cc
    n = len(wv)
    edges = np.vstack([wt[:, [0, 1]], wt[:, [1, 2]], wt[:, [2, 0]]])
    adj = _sp.coo_matrix(
        (np.ones(len(edges), dtype=np.int8), (edges[:, 0], edges[:, 1])),
        shape=(n, n))
    return int(_cc(adj, directed=False)[0])


def _flap_final_verdict(in_verts, in_tris, out_verts, out_tris):
    """(holes, non-manifold, input_parts, output_parts) for the safety net."""
    holes, nm = reload_strict_holes_nm(out_verts, out_tris)
    parts_in = _reload_component_count(in_verts, in_tris)
    parts_out = _reload_component_count(out_verts, out_tris)
    return int(holes), int(nm), int(parts_in), int(parts_out)


def _flap_final_needs_off(in_verts, in_tris, out_verts, out_tris):
    """Whether a flap-adopted FINAL mesh warrants a flap-OFF re-run (HIGH-1).

    The flap adoption gate is topological-only and decided on the pre-chain
    mesh, so it cannot guarantee the final result is no worse than flap-OFF.
    This cheap check flags the two observable regressions: the final mesh is
    not strict-watertight, or it has more connected parts than the input had.
    Returns ``(reason, metrics)`` with ``reason`` None when no re-run is needed.
    """
    holes, nm, parts_in, parts_out = _flap_final_verdict(
        in_verts, in_tris, out_verts, out_tris)
    metrics = {'holes': holes, 'non_manifold': nm, 'input_parts': parts_in,
               'output_parts': parts_out,
               'watertight': holes == 0 and nm == 0}
    if holes or nm:
        return 'not_watertight', metrics
    if parts_out > parts_in:
        return 'more_parts', metrics
    return None, metrics


def _flap_off_preferred(on_verdict, off_verdict):
    """Whether the flap-OFF result should replace the flap-ON one.

    OFF wins only when it is lexicographically better on (damage, part-count);
    a tie keeps ON, so Flap is discarded only when it is actually worse.
    """
    def _score(verdict):
        m = verdict[1]
        return m['holes'] + m['non_manifold'], m['output_parts']
    return _score(off_verdict) < _score(on_verdict)


# Flap time budget by intensity preset (seconds). ``quick`` disables Flap
# entirely; a run that exceeds its preset budget is measured (and cached for a
# repeat attempt) but never adopted, so a slow Flap can stall neither the
# repair nor the triage escalation. Mirrors ``SI_EXCISE_BUDGET_BY_INTENSITY``.
FLAP_TIMEOUT_BY_INTENSITY = {'quick': 0.0, 'balanced': 15.0,
                             'thorough': 30.0, 'extreme': 60.0}
FLAP_DEFAULT_BUDGET = 15.0

# Per-process cache so the Flap pre-pass runs once per unique input mesh, even
# when triage re-runs the repair for several escalation attempts (each attempt
# reloads the same bytes from ``src``). Keyed by the exact input arrays; a
# bounded LRU. Storing at most two entries costs at most two candidate meshes.
_FLAP_CACHE = OrderedDict()
_FLAP_CACHE_MAX = 2


def _flap_cache_key(base_v, base_t):
    h = hashlib.sha256()
    h.update(np.ascontiguousarray(base_v, dtype=np.float64).tobytes())
    h.update(np.ascontiguousarray(base_t, dtype=np.int64).tobytes())
    return h.digest()


def _flap_step(ml, ms, after, stats, _p=None):
    """Stage-1 hole fill via the Flap surface filler (on by default in the CLI).

    Runs ``sutura_engine.flap.flap_fill`` on the mesh in ``ms`` (the input as
    it stands before the VCG chain -- normally the original surface, though
    outer-shell extraction may already have replaced it; see
    ``repair_mesh_from_arrays``), covering the input's open boundary loops with
    a patch that continues the surrounding curvature. The candidate is adopted
    when the combined reload-honest damage ``holes + non-manifold edges`` does
    not increase; the deep-repair ladder then rebuilds the residual
    non-manifold edges. This gate is intentionally topological-only and is
    decided on the pre-chain mesh, so ``repair_file`` adds a final safety net
    that re-runs with Flap OFF when the *final* result is not strict-watertight
    or has more parts than the input had (see ``_flap_final_needs_off``). The
    Flap pre-pass runs at most once per unique input mesh (bounded
    ``_FLAP_CACHE``) and is skipped when the intensity preset sets a zero
    budget; a run slower than its budget is not adopted. Records
    ``stats['flap']``; never raises.

    ``_p`` (the resolved Stage-1 params) is accepted for call-site symmetry and
    ignored: Flap has no threshold that maps onto them.
    """
    rec = {'ran': False, 'adopted': False, 'reason': None,
           'baseline_holes': None, 'baseline_non_manifold': None,
           'baseline_damage': None, 'candidate_holes': None,
           'candidate_non_manifold': None, 'candidate_damage': None,
           'loops_found': None, 'loops_filled': None, 'loops_skipped': None,
           'loops_skipped_by_reason': None, 'patch_faces': None,
           'new_vertices': None, 'time_s': None, 'cache_hit': False,
           'budget_s': None, 'max_orig_vertex_move_mm': None}
    stats['flap'] = rec
    try:
        return _flap_step_impl(ml, ms, after, stats, rec)
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        rec['reason'] = 'flap error: %s' % e
        return ms, after


def _flap_step_impl(ml, ms, after, stats, rec):
    base_v = np.asarray(ms.current_mesh().vertex_matrix())
    base_t = np.asarray(ms.current_mesh().face_matrix())
    try:
        base_holes, base_nm = reload_strict_holes_nm(base_v, base_t)
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        rec['reason'] = 'baseline measure failed: %s' % e
        return ms, after
    rec['baseline_holes'] = int(base_holes)
    rec['baseline_non_manifold'] = int(base_nm)
    if base_holes == 0 and base_nm == 0:
        rec['reason'] = 'no residual boundary loops or non-manifold edges'
        return ms, after

    intensity = stats.get('triage_intensity') or 'balanced'
    budget = FLAP_TIMEOUT_BY_INTENSITY.get(intensity, FLAP_DEFAULT_BUDGET)
    rec['budget_s'] = budget
    if budget <= 0.0:
        rec['reason'] = ('budget: disabled by intensity preset %r' % intensity)
        return ms, after

    cache_key = _flap_cache_key(base_v, base_t)
    cached = _FLAP_CACHE.get(cache_key)
    if cached is not None:
        cand_v, cand_t, frep, prev_time = cached
        _FLAP_CACHE.move_to_end(cache_key)
        rec['ran'] = True
        rec['cache_hit'] = True
        rec['time_s'] = prev_time
    else:
        t0 = time.perf_counter()
        try:
            from sutura_engine import flap as _flap
            cand_v, cand_t, frep = _flap.flap_fill(base_v, base_t,
                                                   separate_stl=False)
        except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
            rec['reason'] = 'flap error: %s' % e
            return ms, after
        rec['ran'] = True
        rec['time_s'] = round(time.perf_counter() - t0, 3)
        _FLAP_CACHE[cache_key] = (cand_v, cand_t, frep, rec['time_s'])
        while len(_FLAP_CACHE) > _FLAP_CACHE_MAX:
            _FLAP_CACHE.popitem(last=False)

    rec['loops_found'] = frep.get('loops_found')
    rec['loops_filled'] = frep.get('loops_filled')
    rec['loops_skipped'] = frep.get('loops_skipped')
    rec['loops_skipped_by_reason'] = {
        k: int(frep.get(k) or 0) for k in FLAP_SKIP_KEYS}
    rec['patch_faces'] = frep.get('patch_faces')
    rec['new_vertices'] = frep.get('new_vertices')
    rec['max_orig_vertex_move_mm'] = _flap_max_orig_move(frep)

    if not len(cand_t):
        rec['reason'] = 'flap returned no geometry'
        return ms, after
    try:
        cand_holes, cand_nm = reload_strict_holes_nm(cand_v, cand_t)
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        rec['reason'] = 'candidate measure failed: %s' % e
        return ms, after
    rec['candidate_holes'] = int(cand_holes)
    rec['candidate_non_manifold'] = int(cand_nm)
    base_damage = int(base_holes) + int(base_nm)
    cand_damage = int(cand_holes) + int(cand_nm)
    rec['baseline_damage'] = base_damage
    rec['candidate_damage'] = cand_damage

    # Budget: a run slower than the preset budget is never adopted, so a heavy
    # mesh cannot slow a repair through Flap. The candidate is cached above so
    # a repeat attempt pays the measure, not the run again.
    if rec['time_s'] is not None and rec['time_s'] > budget:
        rec['reason'] = ('budget: flap took %ss > %ss at intensity %r; '
                         'candidate not adopted'
                         % (rec['time_s'], budget, intensity))
        return ms, after

    if _flap_adopt(base_holes, base_nm, cand_holes, cand_nm):
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(cand_v, np.float32),
                            face_matrix=np.asarray(cand_t, np.int32)))
        after = ms.apply_filter('get_topological_measures')
        rec['adopted'] = True
        rec['reason'] = ('adopted (holes %d->%d, non-manifold %d->%d; '
                         'damage %d->%d)' % (
                             base_holes, cand_holes, base_nm, cand_nm,
                             base_damage, cand_damage))
    else:
        rec['reason'] = ('rejected: reload damage %d->%d would worsen '
                         '(holes %d->%d, non-manifold %d->%d)' % (
                             base_damage, cand_damage, base_holes, cand_holes,
                             base_nm, cand_nm))
    return ms, after


# Flap debris cleanup: after Flap changes the surface the deep-repair tiers
# start from, the shell wrap / Stage-2 rebuild can leave a few extra closed
# shells (thingi10k_1038441: 2 output parts -> 11, nine of them 4-8 faces with
# ~zero volume). They are BOTH collapsed sheets (effective thickness 2*V/A far
# below the part's own bounding-box diagonal) AND effectively zero-volume, and
# so carry no printable material alone -- exactly the debris
# ``manifold_bridge._is_debris_part`` already prunes before a union. A genuine
# solid part has 2*V/A ~= its wall thickness (measured 2.0-2.6 on every corpus
# part) and is never dropped. See ``drop_flap_debris``.
FLAP_DEBRIS_VOLUME_EPS = 1e-3
FLAP_DEBRIS_VOLUME_REL = 1e-9
FLAP_DEBRIS_FLATNESS = 1e-4


def drop_flap_debris(in_verts, in_tris, out_verts, out_tris):
    """Drop flat, effectively zero-volume components the repair left behind.

    Flap changes the surface the deep-repair tiers start from, and on a small
    number of meshes the shell-wrap / Stage-2 rebuild then leaves a handful of
    extra closed shells (thingi10k_1038441: 2 output parts -> 11). They are
    flat sheets with no printable material, not parts: a component is dropped
    only when it is NOT the largest AND BOTH

    - carries effectively no volume (at or below the input-scaled floor), and
    - is a collapsed sheet: its effective thickness ``2*V/A`` is at or below
      ``FLAP_DEBRIS_FLATNESS`` of its own bounding-box diagonal.

    Requiring BOTH conditions is the symmetric rule: a component with real
    volume is never dropped no matter how flat it is, and a non-flat chunk is
    never dropped no matter how small its volume. The volume floor scales with
    the original input bbox (``max(FLAP_DEBRIS_VOLUME_EPS, in_diag**3 *
    FLAP_DEBRIS_VOLUME_REL)``); a genuine solid part has thickness ~= its wall
    size and orders of magnitude more volume, so it is never dropped (measured:
    every legit corpus part has ``2*V/A`` ~2.0-2.6 vs debris 4e-9..4e-4). The
    cleanup is gated on the flap-adopted path only, so identical debris from a
    non-flap repair is left untouched. The largest component is always kept and
    the mesh is never emptied. Runs on the STL save/reload-equivalent form
    (``weld_reload_equivalent``) so the verdict sees exactly what is dropped.
    Pure numpy/scipy; never raises; a byte-identical no-op (returns the input
    arrays and ``None``) when nothing qualifies, when scipy is unavailable, or
    on any error. Returns ``(verts, tris, info)``; ``info['dropped']`` logs
    every removed component (faces, volume, thickness, diag, centroid).
    """
    try:
        import scipy.sparse as _sp
        from scipy.sparse.csgraph import connected_components as _cc

        iv, _it = weld_reload_equivalent(in_verts, in_tris)
        wv, wt = weld_reload_equivalent(out_verts, out_tris)
        if len(iv) == 0 or len(wt) == 0:
            return out_verts, out_tris, None
        n = len(wv)
        edges = np.vstack([wt[:, [0, 1]], wt[:, [1, 2]], wt[:, [2, 0]]])
        adj = _sp.coo_matrix(
            (np.ones(len(edges), dtype=np.int8), (edges[:, 0], edges[:, 1])),
            shape=(n, n))
        ncomp, labels = _cc(adj, directed=False)
        face_label = labels[wt[:, 0]]
        face_counts = np.bincount(face_label, minlength=ncomp)
        in_diag = float(np.linalg.norm(iv.max(0) - iv.min(0)))
        vol_eps = max(FLAP_DEBRIS_VOLUME_EPS,
                      (in_diag ** 3) * FLAP_DEBRIS_VOLUME_REL)
        main = int(np.argmax(face_counts))
        keep = np.ones(ncomp, dtype=bool)
        dropped = []
        for c in range(ncomp):
            if c == main or face_counts[c] == 0:
                continue
            tri = wt[face_label == c]
            p0 = wv[tri[:, 0]].astype(np.float64)
            p1 = wv[tri[:, 1]].astype(np.float64)
            p2 = wv[tri[:, 2]].astype(np.float64)
            vol = abs(float(np.einsum('ij,ij->i', p0,
                                      np.cross(p1, p2)).sum() / 6.0))
            area = float(0.5 * np.linalg.norm(
                np.cross(p1 - p0, p2 - p0), axis=1).sum())
            cxyz = wv[np.unique(tri)].astype(np.float64)
            diag = float(np.linalg.norm(cxyz.max(0) - cxyz.min(0)))
            thickness = (2.0 * vol / area) if area > 0.0 else 0.0
            flat = (area <= 0.0 or diag <= 0.0 or vol <= 0.0
                    or thickness <= FLAP_DEBRIS_FLATNESS * diag)
            # Symmetric (AND) rule: drop only a component that is BOTH a
            # collapsed sheet AND effectively zero-volume. Requiring both
            # means a component with real volume is never dropped, regardless
            # of how flat it is (the review's MEDIUM/LOW-5); known repair
            # debris is both flat and ~zero-volume, so it is still removed.
            if flat and vol <= vol_eps:
                keep[c] = False
                dropped.append({
                    'faces': int(face_counts[c]),
                    'volume': vol,
                    'thickness': thickness,
                    'diag': diag,
                    'centroid': [round(float(x), 4) for x in cxyz.mean(0)]})
        if not dropped:
            return out_verts, out_tris, None
        fkeep = keep[face_label]
        if not fkeep.any():
            return out_verts, out_tris, None
        cv, ct = weld_reload_equivalent(wv, wt[fkeep])
        info = {
            'components_before': int(np.count_nonzero(face_counts)),
            'components_after': int(np.unique(face_label[fkeep]).size),
            'dropped_faces': int(np.count_nonzero(~fkeep)),
            'dropped_volume': float(sum(d['volume'] for d in dropped)),
            'volume_eps': vol_eps,
            'dropped': dropped,
        }
        return cv, ct, info
    except Exception:  # noqa: BLE001 - a cleanup never crashes a repair
        return out_verts, out_tris, None


def closing_ladder(ml, ms, after, stats, v, t, tmpdir,
                   closing=None, proxy_template=False):
    """Scan-closing / proxy-template tier for registry methods 8/9/10 (P-INT).

    ``closing='poisson'`` runs ``closing.poisson_close``; ``'flat_back'`` runs
    ``closing.flat_back_close``; ``proxy_template=True`` runs
    ``proxy_repair.proxy_template_repair``. All three operate on the ORIGINAL
    input arrays ``(v, t)`` rather than the stage-1 result: stage 1's own
    ``meshing_close_holes`` already flat-caps a single boundary loop
    (``maxholesize`` is raised to cover the largest loop), so a closing method
    run on the cleaned arrays would only ever see an already-closed mesh. This
    mirrors the fTetWild tier, which also works from the original input.

    The candidate is adopted only when it is strict-watertight (the same
    ``defects.detect`` metric ``methods._evaluate`` uses: zero holes and zero
    non-manifold edges) and no worse than the stage-1 baseline on holes +
    non-manifold edges. The per-method one-sided (input -> output) Hausdorff
    shape guard is applied later by ``methods._evaluate`` for
    ``invents_geometry`` methods. Records ``stats['closing']`` (tier, ran,
    adopted, counts, the module's ``notes``); never raises.
    """
    if not closing and not proxy_template:
        return ms, after
    tier = 'proxy_template' if proxy_template else closing
    base_v = np.asarray(ms.current_mesh().vertex_matrix())
    base_t = np.asarray(ms.current_mesh().face_matrix())
    base_holes = boundary_loop_stats(base_v, base_t)[0]
    base_nm = int(after.get('non_two_manifold_edges', 0))
    rec = {'tier': tier, 'ran': True, 'adopted': False, 'watertight': False,
           'holes': None, 'non_manifold': None, 'faces': None,
           'notes': [], 'reason': None}
    try:
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        if proxy_template:
            import proxy_repair as _proxy
            cand_v, cand_t, rep = _proxy.proxy_template_repair(in_v, in_t)
        else:
            import closing as _closing
            if closing == 'poisson':
                cand_v, cand_t, rep = _closing.poisson_close(
                    in_v, in_t, tmpdir=tmpdir)
            elif closing == 'flat_back':
                cand_v, cand_t, rep = _closing.flat_back_close(in_v, in_t)
            elif closing == 'mirror':
                import mirror_repair as _mirror
                cand_v, cand_t, rep = _mirror.mirror_close(
                    in_v, in_t, tmpdir=tmpdir)
            else:
                rec.update(ran=False, reason='unknown closing mode %r' % closing)
                stats['closing'] = rec
                return ms, after
        rec['notes'] = list(rep.get('notes') or [])
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if len(cand_t):
            # P-FIX: validate the mesh the user will actually reload. STL drops
            # vertex sharing and the loader re-welds coincident float32
            # positions, which can expose non-manifold edges the in-memory
            # index arrays do not show. Weld into that reload-equivalent form,
            # repair what the weld exposes, then check -- never adopt a
            # candidate that is not strict-watertight after a save/reload.
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            if (len(cand_t) and cand_holes == 0 and cand_nm == 0
                    and cand_holes + cand_nm <= base_holes + base_nm):
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                # 5.2: this is a stage-1 strict-watertight CANDIDATE only --
                # 'watertight' is claimed by the top-level verdict after stage 2
                # actually runs and confirms it, not here.
                rec['watertight'] = False
                rec['candidate_watertight'] = True
                rec['reason'] = 'strict-watertight candidate (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        else:
            rec['reason'] = 'module returned no geometry'
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        rec['ran'] = False
        rec['reason'] = 'error: %s' % e
    stats['closing'] = rec
    return ms, after


def graft_tier(ml, ms, after, stats, v, t, tmpdir, graft=False, intensity=None,
               spec=None):
    """Morphology shell-wrap tier for registry method 13 (Graft).

    Runs ``sutura_engine.graft.shell_wrap`` on the ORIGINAL input arrays (like
    the closing and fTetWild tiers).  The candidate is adopted only when it is
    strict-watertight after the save/reload-equivalent weld.  ``stats['graft']``
    records the fidelity verdict (``fidelity_ok``, ``hausdorff_healthy``,
    ``detail_max_mm``/``detail_area_moved``) and the EN/TR detail-loss
    warnings the CLI/GUI surface; the auto policy reads ``fidelity_ok`` to
    decide whether to also try fTetWild.

    The tier is bounded like the fTetWild tier: ``spec`` (the Triage
    ``IntensitySpec``) supplies ``graft_max_faces`` (a hard input cap; a single
    ``morph_close`` attempt is not interruptible) and ``graft_timeout`` (the
    wall-clock budget for the closing ladder).  Inputs above the cap, or
    ladders that spend the budget before finding a watertight candidate, are
    reported honestly (``skipped_budget`` / ``timed_out`` / ``reject_reason``)
    and keep the stage-1 result.  Never raises.
    """
    if not graft:
        return ms, after
    cap = getattr(spec, 'graft_max_faces', GRAFT_MAX_FACES) if spec is not None \
        else GRAFT_MAX_FACES
    timeout = getattr(spec, 'graft_timeout', GRAFT_TIMEOUT) if spec is not None \
        else GRAFT_TIMEOUT
    rec = {'tier': 'graft', 'ran': False, 'adopted': False,
           'watertight': False, 'fidelity_ok': None,
           'hausdorff_healthy': None, 'detail_max_mm': None,
           'detail_area_moved': None, 'r_used': None, 'mode': None,
           'faces': None, 'holes': None, 'non_manifold': None,
           'warnings': [], 'reason': None,
           'skipped_budget': False, 'timed_out': False,
           'reject_reason': None,
           'input_faces': None,
           'max_faces': int(cap) if cap is not None else None,
           'time_budget': (float(timeout) if timeout else None)}
    try:
        try:
            from sutura_engine import graft as _graft
        except Exception:  # noqa: BLE001
            import shell_wrap as _graft
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        rec['input_faces'] = int(len(in_t))
        if cap is not None and len(in_t) > int(cap):
            # Hard input cap: a morph_close call is not interruptible, so do
            # not start the ladder on a mesh too large to finish in budget.
            rec['skipped_budget'] = True
            rec['reject_reason'] = 'too_large'
            rec['holes'] = (stats.get('stage1') or {}).get('holes_remaining')
            rec['non_manifold'] = (stats.get('stage1') or {}).get(
                'non_manifold_edges_remaining')
            rec['reason'] = ('input has %d faces, above the Graft cap of %d; '
                             'stage-1 result kept' % (len(in_t), int(cap)))
            stats['graft'] = rec
            return ms, after
        cand_v, cand_t, grec = _graft.shell_wrap(in_v, in_t, ml=ml,
                                                 intensity=intensity,
                                                 time_budget=timeout)
        rec['ran'] = True
        rec['budget_exceeded'] = bool(grec.get('budget_exceeded'))
        if rec['budget_exceeded']:
            rec['timed_out'] = True
            rec['reject_reason'] = 'budget'
        rec['fidelity_ok'] = grec.get('fidelity_ok')
        rec['hausdorff_healthy'] = grec.get('hausdorff_healthy')
        rec['detail_max_mm'] = grec.get('detail_max_mm')
        rec['detail_area_moved'] = grec.get('detail_area_moved')
        rec['r_used'] = grec.get('r_used')
        rec['mode'] = grec.get('mode')
        rec['seconds'] = grec.get('seconds')
        rec['warnings'] = list(grec.get('warnings') or [])
        if len(cand_t):
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            if len(cand_t) and cand_holes == 0 and cand_nm == 0:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                rec['watertight'] = False  # stage 2 confirms at the top level
                rec['candidate_watertight'] = True
                rec['reason'] = 'strict-watertight candidate (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        else:
            rec['reason'] = 'graft returned no geometry'
        if rec['adopted']:
            # A watertight candidate was found before the budget ran out.
            rec['timed_out'] = False
            rec['reject_reason'] = None
        elif rec.get('timed_out'):
            # Budget spent without a watertight candidate: keep stage 1.
            rec['skipped_budget'] = True
            if rec['holes'] is None:
                rec['holes'] = (stats.get('stage1') or {}).get(
                    'holes_remaining')
                rec['non_manifold'] = (stats.get('stage1') or {}).get(
                    'non_manifold_edges_remaining')
            rec['reason'] = ('Graft exceeded its %ss budget before a '
                             'watertight candidate; stage-1 result kept'
                             % (('%g' % timeout) if timeout else '?'))
    except BaseException as e:  # noqa: BLE001 - a tier never crashes a repair
        # PyO3 surfaces a Rust panic as pyo3_runtime.PanicException, which
        # subclasses BaseException (not Exception); a tier must degrade to its
        # reason field rather than abort the whole repair.
        if isinstance(e, (KeyboardInterrupt, SystemExit)):
            raise
        rec['reason'] = 'error: %s: %s' % (type(e).__name__, e)
    stats['graft'] = rec
    return ms, after


# CLI flags the Dressing suggestion points the user at: the plain opt-in, and
# the shape-gate override that keeps a gate-rejected (but watertight) coat.
DRESSING_SUGGEST_FLAG = '--experimental-dressing'
DRESSING_FORCE_ADOPT_FLAG = '--dressing-force-adopt'


def _obj_closed_result(r):
    """True when one object's stage-1 output is a closed manifold (local copy
    of ``classification._obj_closed`` so repair.py does not import a private
    helper)."""
    s1 = r.get('stage1', {}) if isinstance(r, dict) else {}
    return bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0


def _not_watertight_result(report):
    """True when the FINAL repaired geometry is open or non-manifold.

    The verdict is the post-ladder topology in ``stage1`` (``two_manifold`` /
    ``holes_remaining`` / ``non_manifold_edges_remaining``), which
    ``repair_mesh_from_arrays`` rewrites after every tier, so it describes the
    mesh actually saved.  A residual self-intersection count is NOT a
    "not watertight" signal: SI is reported, not repaired, under the default
    ``si_mode='report'`` (a closed, SI-carrying result must not get a "Not
    watertight" hint).  A hard error or a declined budget save is not a "try
    Dressing" situation either.  Multi-object 3MF is judged across every
    object."""
    if not isinstance(report, dict):
        return False
    if report.get('error') or report.get('status') == 'budget_declined':
        return False
    reports = report.get('object_reports')
    if reports:
        return any(not _obj_closed_result(r) for r in reports)
    s1 = report.get('stage1')
    if not isinstance(s1, dict) or not s1:
        return False
    if not s1.get('two_manifold'):
        return True
    if s1.get('holes_remaining', 0):
        return True
    if s1.get('non_manifold_edges_remaining', 0):
        return True
    return False


def _dressing_record(report):
    """The Dressing record of a single-mesh or first-run multi-object report."""
    dress = report.get('dressing')
    if isinstance(dress, dict):
        return dress
    for r in report.get('object_reports') or []:
        if isinstance(r.get('dressing'), dict):
            return r['dressing']
    return None


def _dressing_gate_detail(sug):
    """Human parenthesis with the gate numbers a rejected coat failed."""
    parts = []
    v = sug.get('volume_delta_rel')
    if v is not None:
        parts.append('volume %.2f%%' % (100.0 * v))
    n = sug.get('normal_angle_p95')
    if n is not None:
        parts.append('normal p95 %.1f deg' % n)
    if not parts:
        r = sug.get('reason')
        if r:
            parts.append(str(r))
    return ' (%s)' % ', '.join(parts) if parts else ''


def _dressing_suggestions(report):
    """Return the Dressing opt-in suggestion(s) for a still-broken result.

    Empty list when the repair errored / was budget-declined, the result is
    watertight, or Dressing was already adopted.  When Dressing ran but the
    quality gate rejected it, the suggestion points at
    ``--dressing-force-adopt``; otherwise it points at the plain
    ``--experimental-dressing`` opt-in.  Never raises."""
    try:
        if not _not_watertight_result(report):
            return []
        dress = _dressing_record(report)
        if isinstance(dress, dict) and dress.get('adopted'):
            return []
        suggestion = {
            'method': 'dressing',
            'flag': DRESSING_SUGGEST_FLAG,
            'force': False,
            'reason': 'dressing_not_run',
            'warning': 'may_deform',
        }
        if isinstance(dress, dict) and dress.get('ran'):
            suggestion['flag'] = DRESSING_FORCE_ADOPT_FLAG
            suggestion['force'] = True
            suggestion['reason'] = dress.get('reason') or 'gate_rejected'
            suggestion['volume_delta_rel'] = dress.get('volume_delta_rel')
            suggestion['normal_angle_p95'] = dress.get('normal_angle_p95')
            suggestion['fidelity_ok'] = dress.get('fidelity_ok')
        return [suggestion]
    except Exception:  # noqa: BLE001 - a suggestion never breaks a repair
        return []


def _dressing_suggestion_text(sug):
    """English CLI lines for one Dressing suggestion (CLI/human report)."""
    if sug.get('force'):
        return ('  \u26a0 Not watertight. Dressing ran but its result was '
                'rejected by the quality gate%s.\n'
                '    Force it with %s if you accept the deformation '
                '(the shape may deform).'
                % (_dressing_gate_detail(sug), sug.get('flag')))
    return ('  \u26a0 Not watertight. You can try Dressing: %s (watertight, '
            'but the shape may deform / fine detail may be lost).'
            % sug.get('flag'))


def dressing_tier(ml, ms, after, stats, v, t, tmpdir, dressing=False,
                  intensity=None, drain=None, defects=None,
                  r_max_scale=None, sigma_scale=None, force_adopt=False):
    """Variable-viscosity coat tier for registry method 16 (Dressing).

    Runs ``sutura_engine.dressing.dressing_coat`` on the ORIGINAL input arrays
    (like the closing / Graft / fTetWild tiers).  The candidate is adopted only
    when it is strict-watertight after the save/reload-equivalent weld, free of
    exact self-intersections and inside the preset's coat->input fidelity gate.
    ``drain`` selects the healthy-region erode-back (``None`` = preset default,
    a mode name, or an explicit delta in mm).  ``defects`` selects the mask
    (``'all'`` default, ``'holes_nm'``); ``r_max_scale`` / ``sigma_scale``
    multiply the resolved ``r_max`` / ``sigma``.  ``force_adopt``
    (``--dressing-force-adopt``) skips the shape-preservation gates (volume,
    component count, healthy-surface normal angle, coat fidelity) while STILL
    requiring a strict-watertight, self-intersection-free candidate, so the
    caller can keep a deformed coat it explicitly asked for.
    ``stats['dressing']`` records
    the parameters, the deviations, the exact-SI verdict and the EN/TR warnings
    the CLI/GUI surface.  Never raises.
    """
    if not dressing:
        return ms, after
    rec = {'tier': 'dressing', 'ran': False, 'adopted': False,
           'watertight': False, 'fidelity_ok': None, 'engine': None,
           'voxel': None, 'r_base': None, 'r_max': None, 'sigma': None,
           'faces': None, 'faces_coat': None, 'decimated': False,
           'drain': None, 'defects': None, 'r_max_scale': None,
           'sigma_scale': None, 'cleanup': None,
           'hausdorff_rel_max': None, 'hausdorff_input_to_coat': None,
           'normal_angle_p50': None, 'normal_angle_p95': None,
           'normal_angle_n': 0,
           'volume_before': None, 'volume_after': None,
           'volume_delta_rel': None, 'components_before': None,
           'components_after': None,
           'volume_ok': None, 'parts_ok': None, 'normal_ok': None,
           'si_before': None, 'si_after': None, 'si_exact_unknown': False,
           'holes': None, 'non_manifold': None,
           'within_time_budget': None, 'seconds': None,
           'forced': False,
           'warnings': [], 'reason': None}
    try:
        from sutura_engine import dressing as _dressing
    except Exception as e:  # noqa: BLE001
        rec['reason'] = 'sutura_engine.dressing unavailable: %s' % e
        stats['dressing'] = rec
        return ms, after
    try:
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        cand_v, cand_t, drec = _dressing.dressing_coat(
            in_v, in_t, ml=ml, intensity=intensity, drain=drain,
            defects=defects, r_max_scale=r_max_scale, sigma_scale=sigma_scale)
        rec['ran'] = True
        rec['drain'] = drec.get('drain')
        rec['defects'] = drec.get('defects')
        rec['r_max_scale'] = drec.get('r_max_scale')
        rec['sigma_scale'] = drec.get('sigma_scale')
        rec['engine'] = drec.get('engine')
        rec['voxel'] = drec.get('voxel')
        rec['r_base'] = drec.get('r_base')
        rec['r_max'] = drec.get('r_max')
        rec['sigma'] = drec.get('sigma')
        rec['faces_coat'] = drec.get('faces_coat')
        rec['cleanup'] = drec.get('cleanup')
        rec['decimated'] = bool(drec.get('decimated'))
        rec['fidelity_ok'] = drec.get('fidelity_ok')
        rec['hausdorff_rel_max'] = drec.get('hausdorff_rel_max')
        rec['hausdorff_input_to_coat'] = drec.get('hausdorff_input_to_coat')
        rec['si_before'] = drec.get('si_before')
        rec['si_after'] = drec.get('si_after')
        rec['si_exact_unknown'] = bool(drec.get('si_exact_unknown'))
        rec['within_time_budget'] = drec.get('within_time_budget')
        rec['seconds'] = drec.get('seconds')
        rec['warnings'] = list(drec.get('warnings') or [])
        rec['normal_angle_p50'] = drec.get('normal_angle_p50')
        rec['normal_angle_p95'] = drec.get('normal_angle_p95')
        rec['normal_angle_n'] = int(drec.get('normal_angle_n') or 0)
        if len(cand_t) == 0:
            rec['reason'] = 'dressing returned no geometry'
        else:
            cand_v, cand_t = weld_reload_equivalent(cand_v, cand_t)
            cand_v, cand_t = _repair_welded_topology(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            si_after = drec.get('si_after')
            si_ok = si_after in (0, None)

            # global-shape guards vs the mesh the ladder held before Dressing:
            # a component-count explosion or a large volume change is not a
            # repair.  The raw input signed volume is meaningless when the input
            # is itself damaged/open, so the current result is the reference.
            vol_before = parts_before = None
            try:
                cur_v = np.asarray(ms.current_mesh().vertex_matrix(),
                                   dtype=np.float64)
                cur_t = np.asarray(ms.current_mesh().face_matrix(),
                                   dtype=np.int64)
                vol_before = _dressing.signed_volume(cur_v, cur_t)
                parts_before = _dressing.count_components(cur_t)
            except Exception:  # noqa: BLE001 - guards degrade to "unknown"
                pass
            vol_after = drec.get('volume_after')
            parts_after = drec.get('components_after')
            rec['volume_before'] = (round(vol_before, 6)
                                    if vol_before is not None else None)
            rec['volume_after'] = vol_after
            rec['components_before'] = parts_before
            rec['components_after'] = parts_after
            if vol_before is not None and vol_after is not None:
                denom = max(abs(vol_before), 1e-12)
                rec['volume_delta_rel'] = abs(vol_after - vol_before) / denom
            vol_ok = (rec['volume_delta_rel'] is None
                      or rec['volume_delta_rel'] <= DRESSING_MAX_VOLUME_DELTA)
            parts_ok = (parts_before is None or parts_after is None
                        or parts_after <= parts_before + DRESSING_MAX_PARTS_SLACK)
            na95 = rec['normal_angle_p95']
            normal_ok = (na95 is None
                         or na95 <= DRESSING_MAX_NORMAL_ANGLE_P95)
            rec['volume_ok'] = bool(vol_ok)
            rec['parts_ok'] = bool(parts_ok)
            rec['normal_ok'] = bool(normal_ok)
            shape_ok = (drec.get('fidelity_ok') is not False
                        and vol_ok and parts_ok and normal_ok)
            rec['shape_ok'] = bool(shape_ok)

            if (cand_holes == 0 and cand_nm == 0 and si_ok
                    and (shape_ok or force_adopt)):
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                rec['forced'] = bool(force_adopt and not shape_ok)
                rec['watertight'] = False  # stage 2 confirms at the top level
                rec['candidate_watertight'] = True
                rec['reason'] = ('strict-watertight candidate (stage 2 pending'
                                 + (', shape gates forced)' if rec['forced']
                                    else ')'))
            else:
                reasons = []
                if cand_holes or cand_nm:
                    reasons.append('holes=%d non-manifold=%d'
                                   % (cand_holes, cand_nm))
                if not si_ok:
                    reasons.append('exact self-intersections=%s' % si_after)
                if not force_adopt:
                    if drec.get('fidelity_ok') is False:
                        reasons.append('fidelity %.3f%% > gate'
                                       % (100.0 * (rec['hausdorff_rel_max'] or 0)))
                    if not vol_ok:
                        reasons.append('volume delta %.2f%% > gate'
                                       % (100.0 * (rec['volume_delta_rel'] or 0)))
                    if not parts_ok:
                        reasons.append('components %s > %s + %d'
                                       % (parts_after, parts_before,
                                          DRESSING_MAX_PARTS_SLACK))
                    if not normal_ok:
                        reasons.append('normal p95 %.1f deg > gate'
                                       % (na95 or 0.0))
                rec['reason'] = ('candidate not adopted (%s)'
                                 % ', '.join(reasons) if reasons
                                 else 'candidate not adopted')
    except BaseException as e:  # noqa: BLE001 - a tier never crashes a repair
        if isinstance(e, (KeyboardInterrupt, SystemExit)):
            raise
        rec['reason'] = 'error: %s: %s' % (type(e).__name__, e)
    stats['dressing'] = rec
    return ms, after


def wall_thicken_tier(ml, ms, after, stats, v, t, min_thickness=None):
    """Thin-wall thicken-to-min tier for registry method 15 (P-WALL).

    ``wall_thickness.thicken_to_min`` measures the per-vertex wall thickness
    from an SDF grid and thickens walls below ``min_thickness`` (default: 1% of
    the bbox diagonal) with a morphological closing of the solid. It runs on
    the ORIGINAL input arrays; the candidate is cleaned with PyMeshLab, then
    adopted only when it is strict-watertight and no worse than the stage-1
    baseline on holes + non-manifold edges. Records ``stats['wall_thicken']``
    (before/after minimum thickness, counts, notes); never raises.
    """
    rec = {'tier': 'wall_thicken', 'ran': True, 'adopted': False,
           'watertight': False, 'min_before': None, 'min_after': None,
           'min_thickness': None, 'delta': None, 'thin_before': None,
           'faces': None, 'notes': [], 'reason': None}
    try:
        import wall_thickness as _wt
        base_v = np.asarray(ms.current_mesh().vertex_matrix())
        base_t = np.asarray(ms.current_mesh().face_matrix())
        base_holes = boundary_loop_stats(base_v, base_t)[0]
        base_nm = int(after.get('non_two_manifold_edges', 0))
        in_v = np.asarray(v, dtype=np.float64)
        in_t = np.asarray(t, dtype=np.int64)
        cand_v, cand_t, rep = _wt.thicken_to_min(
            in_v, in_t, min_thickness=min_thickness)
        rec['min_thickness'] = rep.get('min_thickness')
        rec['min_before'] = rep.get('min_before')
        rec['thin_before'] = rep.get('thin_before')
        rec['delta'] = rep.get('delta')
        rec['notes'] = list(rep.get('notes') or [])
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if len(cand_t):
            cand_v, cand_t = _clean_and_orient(ml, cand_v, cand_t)
            cand_v = np.asarray(cand_v, dtype=np.float32)
            cand_t = np.asarray(cand_t, dtype=np.int32)
            det = detect_defects(cand_v, cand_t)
            cand_holes = len(det['holes'])
            cand_nm = len(det['non_manifold'])
            rec['holes'] = cand_holes
            rec['non_manifold'] = cand_nm
            rec['faces'] = int(len(cand_t))
            rec['min_after'] = rep.get('min_after')
            rec['thin_after'] = rep.get('thin_after')
            if (cand_holes == 0 and cand_nm == 0
                    and cand_holes + cand_nm <= base_holes + base_nm):
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
                after = ms.apply_filter('get_topological_measures')
                rec['adopted'] = True
                rec['candidate_watertight'] = True
                rec['reason'] = 'walls thickened (stage 2 pending)'
            elif rec['reason'] is None:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
        elif rec['reason'] is None:
            rec['reason'] = 'module returned no geometry'
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        rec['ran'] = False
        rec['reason'] = 'error: %s' % e
    stats['wall_thicken'] = rec
    return ms, after


def _clean_and_orient(ml, verts, tris):
    """PyMeshLab cleanup of a generated surface: dedup + coherent orientation."""
    try:
        ms = ml.MeshSet()
        ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float64),
                            face_matrix=np.asarray(tris, np.int32)))
        for filt in ('meshing_remove_duplicate_vertices',
                     'meshing_remove_duplicate_faces',
                     'meshing_remove_unreferenced_vertices',
                     'meshing_repair_non_manifold_edges',
                     'meshing_re_orient_faces_coherently'):
            try:
                ms.apply_filter(filt)
            except Exception:
                pass
        cur = ms.current_mesh()
        return (np.asarray(cur.vertex_matrix(), dtype=np.float64),
                np.asarray(cur.face_matrix(), dtype=np.int32))
    except Exception:
        return verts, tris


# One-sided (original -> result, outside the repaired boxes) Hausdorff guard
# for the repeated-element transplant tier: the transplanted copy must not move
# the untouched geometry. Mirrors methods.GENERATIVE_MAX_HAUSDORFF_REL.
REPEAT_MAX_HAUSDORFF_OUTSIDE = 0.01


def repeat_tier(ml, ms, after, stats, mode=None, source_point=None,
                target_point=None):
    """Repeated-element transplant tier (registry methods 11/12, P-REP).

    Runs AFTER the whole stage-1 chain because the transplant needs a
    watertight (closed 2-manifold) ``M``: if stage 1 left holes or non-manifold
    edges the tier is skipped. ``mode='auto'`` detects the repeated pattern
    (``repeat_repair.detect_repetition``) and repairs the damaged copies;
    ``mode='manual'`` transplants the element nearest ``source_point`` onto the
    one nearest ``target_point``. The candidate is adopted only when it is
    reload-equivalent strict-watertight, no worse than the stage-1 baseline,
    and the untouched geometry did not move (one-sided Hausdorff outside the
    repaired boxes <= ``REPEAT_MAX_HAUSDORFF_OUTSIDE``). Records
    ``stats['repeat']``; never raises.
    """
    if not mode:
        return ms, after
    rec = {'mode': mode, 'ran': False, 'adopted': False, 'watertight': False,
           'pattern_type': None, 'positions_repaired': None,
           'hausdorff_outside': None, 'notes': [], 'reason': None}
    try:
        cur_v = np.asarray(ms.current_mesh().vertex_matrix())
        cur_t = np.asarray(ms.current_mesh().face_matrix())
        holes = boundary_loop_stats(cur_v, cur_t)[0]
        nm = int(after.get('non_two_manifold_edges', 0))
        if holes or nm or not after.get('is_mesh_two_manifold'):
            rec['reason'] = ('repeated-element transplant needs a watertight '
                             'input; stage 1 left holes=%d non-manifold=%d'
                             % (holes, nm))
            stats['repeat'] = rec
            return ms, after
        rec['ran'] = True
        import repeat_repair as _rr
        if mode == 'manual':
            if source_point is None or target_point is None:
                rec['reason'] = ('manual repeat repair needs a source and a '
                                 'target point')
                stats['repeat'] = rec
                return ms, after
            rv, rt, rep = _rr.repair_repeat_manual(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64),
                np.asarray(source_point, dtype=np.float64),
                np.asarray(target_point, dtype=np.float64))
        else:
            rv, rt, rep = _rr.repair_repeat_auto(
                np.asarray(cur_v, dtype=np.float64),
                np.asarray(cur_t, dtype=np.int64))
        rec['notes'] = list(rep.get('notes') or [])
        rec['pattern_type'] = rep.get('pattern_type')
        rec['positions_repaired'] = rep.get('positions_repaired')
        rec['hausdorff_outside'] = rep.get('hausdorff_outside')
        if 'error' in rep:
            rec['reason'] = 'module error: %s' % rep['error']
        if not rep.get('repaired') or not len(rt):
            if rec['reason'] is None:
                notes = rep.get('notes') or []
                rec['reason'] = notes[0] if notes else 'nothing to repair'
            stats['repeat'] = rec
            return ms, after
        # Validate the raw manifold3d transplant (in-memory). unlike the
        # closing tiers it is NOT float32-welded here: the CSG output has
        # coincident cut-seam vertices whose pymeshlab weld-repair opens holes,
        # while the later stage-2 manifold3d rebuild makes the mesh reload-safe
        # as a side effect. The reload-equivalent counts are still recorded for
        # transparency; the top-level P-HONEST verdict remains authoritative.
        cand_v = np.asarray(rv, dtype=np.float32)
        cand_t = np.asarray(rt, dtype=np.int32)
        det = detect_defects(cand_v, cand_t)
        cand_holes = len(det['holes'])
        cand_nm = len(det['non_manifold'])
        rec['holes'] = cand_holes
        rec['non_manifold'] = cand_nm
        try:
            rh, rnm = reload_strict_holes_nm(rv, rt)
            rec['reload_holes'] = int(rh)
            rec['reload_non_manifold'] = int(rnm)
        except Exception:  # noqa: BLE001 - reporting only
            pass
        hd = rep.get('hausdorff_outside')
        if (len(cand_t) and cand_holes == 0 and cand_nm == 0
                and cand_holes + cand_nm <= holes + nm
                and hd is not None and hd <= REPEAT_MAX_HAUSDORFF_OUTSIDE):
            ms = ml.MeshSet()
            ms.add_mesh(ml.Mesh(vertex_matrix=cand_v, face_matrix=cand_t))
            after = ms.apply_filter('get_topological_measures')
            rec['adopted'] = True
            rec['watertight'] = False
            rec['candidate_watertight'] = True
            rec['reason'] = 'repeated element(s) transplanted (stage 2 pending)'
        elif rec['reason'] is None:
            if hd is not None and hd > REPEAT_MAX_HAUSDORFF_OUTSIDE:
                rec['reason'] = ('untouched geometry moved (Hausdorff %.3f > %.3f)'
                                 % (hd, REPEAT_MAX_HAUSDORFF_OUTSIDE))
            else:
                rec['reason'] = ('candidate not strict-watertight '
                                 '(holes=%d non-manifold=%d)'
                                 % (cand_holes, cand_nm))
    except Exception as e:  # noqa: BLE001 - a tier never crashes a repair
        rec['ran'] = False
        rec['reason'] = 'error: %s' % e
    stats['repeat'] = rec
    return ms, after


def _dressing_wanted(dressing, holes, nm, si, si_mode='report'):
    """Whether the Dressing fallback should run on the current result.

    ``dressing is True`` (forced) always runs -- it overrides ``si_mode``.
    ``'auto'`` (the default switch) runs when the result still has holes or
    non-manifold edges; a POSITIVE exact self-intersection count also triggers
    it only under ``si_mode == 'repair'`` (``si is None`` = unmeasurable never
    triggers).
    """
    if dressing is True:
        return True
    if dressing == 'auto':
        si_damage = (si_mode == 'repair' and si is not None and si > 0)
        return bool(holes > 0 or nm > 0 or si_damage)
    return False


def _dressing_damage(ms, after, measure_si=True):
    """``(holes, non_manifold, exact_si)`` of the current result.

    ``exact_si`` is ``None`` when the classifier cannot measure it (empty/too
    large mesh, or a build without the Rust extension) or when ``measure_si``
    is False (``--si-mode off``).  Used to decide whether the Dressing fallback
    should run on the result the ladder currently holds (after Graft), so under
    the default 'report' mode an SI-only residual is reported but not escalated.
    """
    m = ms.current_mesh()
    holes = boundary_loop_stats(m.vertex_matrix(), m.face_matrix())[0]
    nm = int(after.get('non_two_manifold_edges', 0))
    si = None
    if measure_si:
        try:
            from sutura_engine import dressing as _d
            si = _d.si_face_count(
                np.asarray(m.vertex_matrix(), dtype=np.float64),
                np.asarray(m.face_matrix(), dtype=np.int64))
        except Exception:  # noqa: BLE001 - unknown SI never blocks a repair
            si = None
    return int(holes), nm, si


def deep_repair_ladder(ml, ms, after, stats, v, t, tmpdir, mode=None,
                       ftetwild=False, spec=None, engines=None, engine_chain=None,
                       graft=False, dressing=False, dressing_drain=None,
                       dressing_defects=None, dressing_rmax_scale=None,
                       dressing_sigma_scale=None, si_mode='report',
                       dressing_force_adopt=False):
    """Single entry point of the deep-repair ladder, called after the stage-1
    chain. ``mode`` is one of DEEP_REPAIR_MODES, or None for the library
    default (no deep-repair report; ``ftetwild`` alone decides, as before
    the ladder existed). 'local' runs the local re-mesh tier; 'full' and None
    run the fTetWild tier (``ftetwild``: 'auto'/True/False, unchanged
    semantics); 'off' runs nothing. ``spec`` is the resolved intensity spec
    (defaults to Balanced); it supplies the tier's timeout, size cap,
    decimation ladder and Hausdorff sample count. Records
    ``stats['deep_repair']`` (mode, tiers run, per-tier reports); the
    ``available`` offer is filled in by ``_deep_repair_offer`` after the final
    stage-1 measurement. Returns ``(ms, after)``."""
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    tiers_run = []
    cad = None
    dmg = None
    cur_holes = boundary_loop_stats(ms.current_mesh().vertex_matrix(),
                                    ms.current_mesh().face_matrix())[0]
    cur_nm = int(after.get('non_two_manifold_edges', 0))
    if mode == 'local':
        if cur_holes > 0 or cur_nm > 0:
            ms, after, local_rep = _local_remesh_tier(ml, ms, after)
            tiers_run.append('local')
        else:
            local_rep = None
    else:
        local_rep = None
    if mode in (None, 'full'):
        # replace_ftetwild engines REPLACE the built-in fTetWild tier; they
        # receive the original input, exactly like fTetWild. With a custom
        # chain.toml that omits 'ftetwild' the built-in tier is skipped too.
        replace_names = engine_placement_order(engines, engine_chain,
                                               'replace_ftetwild')
        if replace_names:
            _entries = []
            _cv, _ct, _adopted = run_engines(
                ml, engines, engine_chain, 'replace_ftetwild', tmpdir, v, t,
                cur_holes, cur_nm, spec, _entries)
            _record_engine_entries(stats, _entries)
            if _adopted:
                ms = ml.MeshSet()
                ms.add_mesh(ml.Mesh(vertex_matrix=_cv, face_matrix=_ct))
                after = ms.apply_filter('get_topological_measures')
                tiers_run.append('replace_ftetwild')
        elif (not engine_chain) or ('ftetwild' in engine_chain):
            # Graft (shell wrap) is tried BEFORE the fTetWild tier. When it is
            # reload-watertight and its healthy fidelity passes, it is adopted
            # and fTetWild is skipped. When it closes the mesh but fidelity
            # fails, fTetWild is also tried and the watertight result with the
            # lower healthy deviation wins.
            #
            # CAD guard hook: the geometric CAD/machined-vs-organic score is
            # recorded for every input.  The ladder keeps Graft first for ALL
            # inputs (which satisfies the guard: a CAD-like part gets the
            # verbatim Graft hybrid before Dressing and Dressing only runs when
            # damage remains); the calibrated detector will drive the ordering.
            cad = cad_likeness(v, t)
            graft_ms = graft_after = None
            graft_dev = None
            graft_ok = False
            # Only run the last-resort Graft when stage 1 still leaves holes or
            # non-manifold edges (the same trigger fTetWild's 'auto' uses).  A
            # closed stage-1 result is already the final mesh: Graft must not
            # run on it, or already-watertight repairs would change output and
            # pay an unnecessary tier.
            if graft in (True, 'auto') and (cur_holes > 0 or cur_nm > 0):
                _g_ms, _g_after = graft_tier(ml, ms, after, stats, v, t, tmpdir,
                                             graft=True,
                                             intensity=(getattr(spec, 'base', None)
                                                        or getattr(spec, 'name', None)),
                                             spec=spec)
                _g = stats.get('graft') or {}
                if _g.get('adopted'):
                    if _g.get('fidelity_ok') is not False:
                        ms, after = _g_ms, _g_after
                        graft_ok = True
                        tiers_run.append('graft')
                    else:
                        graft_ms, graft_after = _g_ms, _g_after
                        graft_dev = _g.get('hausdorff_healthy')

            # Dressing (#16) sits AFTER Graft and BEFORE fTetWild: its
            # volumetric skin targets the whole-shell folds and heavy
            # self-intersections Graft's hybrid leaves behind.  When the switch
            # is on it is an Auto fallback, tried whenever the result the ladder
            # now holds (after Graft) still has holes, non-manifold edges or
            # exact self-intersections; an explicit force runs regardless.
            dressing_ok = False
            _dh = _dnm = 0
            _dsi = None
            if dressing == 'auto':
                _dh, _dnm, _dsi = _dressing_damage(ms, after,
                                                   measure_si=(si_mode != 'off'))
                dmg = {'holes': _dh, 'non_manifold': _dnm,
                       'self_intersections': _dsi}
            if _dressing_wanted(dressing, _dh, _dnm, _dsi, si_mode):
                _d_ms, _d_after = dressing_tier(
                    ml, ms, after, stats, v, t, tmpdir, dressing=True,
                    intensity=(getattr(spec, 'base', None)
                               or getattr(spec, 'name', None)),
                    drain=dressing_drain, defects=dressing_defects,
                    r_max_scale=dressing_rmax_scale,
                    sigma_scale=dressing_sigma_scale,
                    force_adopt=dressing_force_adopt)
                if (stats.get('dressing') or {}).get('adopted'):
                    ms, after = _d_ms, _d_after
                    dressing_ok = True
                    tiers_run.append('dressing')

            if dressing_ok:
                stats['experimental_ftetwild'] = False
                if graft_ms is not None:
                    stats['graft']['adopted'] = False
                    stats['graft']['reason'] = 'superseded by Dressing'
            elif graft_ok:
                stats['experimental_ftetwild'] = False
            else:
                ms, after = _ftetwild_tier(ml, ms, after, stats, v, t, tmpdir,
                                           ftetwild, spec=spec)
                if (stats.get('experimental_ftetwild') or {}).get('ran'):
                    tiers_run.append('ftetwild')
                if graft_ms is not None:
                    ft_dev = (stats.get('experimental_ftetwild')
                              or {}).get('hausdorff_rel')
                    ft_adopted = bool((stats.get('experimental_ftetwild')
                                       or {}).get('adopted'))
                    if (not ft_adopted
                            or (ft_dev is not None and graft_dev is not None
                                and ft_dev < graft_dev)):
                        stats['graft']['adopted'] = False
                        stats['graft']['reason'] = (
                            'superseded by fTetWild (lower healthy deviation)')
                        tiers_run.append('graft')
                    else:
                        ms, after = graft_ms, graft_after
                        stats['graft']['adopted'] = True
                        stats['graft']['preferred_over_ftetwild'] = True
                        if 'ftetwild' in tiers_run:
                            tiers_run.remove('ftetwild')
                        tiers_run.append('graft')
    else:
        # mode 'off'/'local': Dressing is run whenever it is explicitly enabled
        # (method #16 sets deep_repair='off'), regardless of remaining holes, so
        # an explicit choice is always honoured; the auto 'full' ladder stays
        # conservative and only tries it when stage 1 left damage.
        if dressing is True:
            _d_ms, _d_after = dressing_tier(
                ml, ms, after, stats, v, t, tmpdir, dressing=True,
                intensity=(getattr(spec, 'base', None)
                           or getattr(spec, 'name', None)),
                drain=dressing_drain, defects=dressing_defects,
                r_max_scale=dressing_rmax_scale,
                sigma_scale=dressing_sigma_scale,
                force_adopt=dressing_force_adopt)
            if (stats.get('dressing') or {}).get('adopted'):
                ms, after = _d_ms, _d_after
                tiers_run.append('dressing')
        stats['experimental_ftetwild'] = False
    if mode is not None:
        stats['deep_repair'] = {
            'mode': mode,
            'si_mode': si_mode,
            'holes_before': int(cur_holes),
            'nm_before': cur_nm,
            'tiers_run': tiers_run,
            'local': local_rep,
            'ftetwild': stats.get('experimental_ftetwild') or None,
            'graft': stats.get('graft') or None,
            'dressing': stats.get('dressing') or None,
            'cad_likeness': cad,
            'dressing_damage': dmg,
            'available': None,
        }
    return ms, after


def _deep_repair_offer(stats, holes, nm, n_faces, spec=None):
    """Fill ``deep_repair.available`` and ``final_tier`` once the final
    stage-1 counts are known: when holes or non-manifold edges remain, list
    the tiers this mode did not run (fTetWild only when installed) with a
    rough time estimate."""
    if spec is None:
        spec = triage.PRESETS[triage.DEFAULT_INTENSITY]
    dr = stats.get('deep_repair')
    if not dr:
        return
    final = 'stage1'
    if (dr.get('ftetwild') or {}).get('adopted'):
        final = 'ftetwild'
    elif (stats.get('graft') or {}).get('adopted'):
        final = 'graft'
    elif (stats.get('dressing') or {}).get('adopted'):
        final = 'dressing'
    elif (dr.get('local') or {}).get('adopted'):
        final = 'local'
    dr['final_tier'] = final
    if holes == 0 and nm == 0:
        return
    within_cap = (spec.ftetwild_max_faces is None
                  or n_faces <= spec.ftetwild_max_faces)
    tiers = []
    if dr['mode'] == 'off':
        tiers.append('local')
    if (dr['mode'] in ('off', 'local') and ftetwild_available()
            and within_cap):
        tiers.append('ftetwild')
    if tiers:
        dr['available'] = {
            'holes_remaining': int(holes),
            'nm_remaining': int(nm),
            'tiers': tiers,
            'estimate_s': estimate_deep_repair_time(
                n_faces, holes + nm, tiers, spec.ftetwild_timeout),
        }


def maybe_run_stage2(report, verts, tris, tmpdir):
    """Run stage 2 (manifold3d) when the mesh is watertight and the bridge is
    available; shared by the single-mesh path and per-object 3MF repair.

    The gate mirrors the historical single-mesh behaviour: stage 2 only
    applies to a mesh that stage 1 closed (two-manifold with no remaining
    holes) AND the bridge exists. ``run_stage2`` itself handles the fixed
    venv subprocess, the in-process fallback, or an explicit "skipped"
    report. Sets ``report['stage2']`` when stage 2 was attempted and returns
    ``(new_verts, new_tris)`` — the manifold3d-rebuilt mesh when it produced
    output, the input arrays unchanged otherwise."""
    if (report.get('stage1', {}).get('two_manifold')
            and report.get('stage1', {}).get('holes_remaining', 0) == 0
            and os.path.exists(BRIDGE)):
        inter = os.path.join(tmpdir, 'stage1.obj')
        out_obj = os.path.join(tmpdir, 'stage2.obj')
        write_obj(inter, verts, tris)
        mrep, _ = run_stage2(inter, out_obj)
        if mrep is not None:
            report['stage2'] = mrep
            if 'error' not in mrep and os.path.exists(out_obj):
                return read_obj(out_obj)
    return verts, tris


def save_mesh(out_path, verts, tris):
    import pymeshlab as ml
    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(verts, np.float32),
                        face_matrix=np.asarray(tris, np.int32)))
    ms.save_current_mesh(out_path)


def _geom_change_pct(s1):
    """Geometry-change magnitude % for a stage1 dict, or None.

    ``max(|volume_change_percent|, |surface_area_change_percent|)`` — the
    worst geometric distortion. Reuses metrics the repair already computed
    (no recomputation)."""
    vals = []
    for key in ('volume_change_percent', 'surface_area_change_percent'):
        v = s1.get(key)
        if isinstance(v, (int, float)) and not isinstance(v, bool):
            vals.append(abs(float(v)))
    return max(vals) if vals else None


def _budget_message(budget):
    """Human-readable 'metric value (budget)' list for an exceeded budget."""
    parts = []
    for e in budget.get('exceeded', []):
        if e['metric'] == 'geometry_change':
            parts.append('geometry change %.2f%% (budget %.2f%%)'
                         % (e['value'], e['budget']))
        elif e['metric'] == 'risk':
            parts.append('risk %d (budget %d)' % (e['value'], e['budget']))
    return '; '.join(parts)


def budget_check(result, max_geom_change=None, max_risk=None):
    """Compare the repair's actual geometry change / risk against budgets.

    Both metrics are reused, never recomputed:
      - geometry change % = max(|volume|, |surface area|) change (stage1)
      - repair risk score (0-100) = repair_score.compute_scores() output
    For multi-object 3MF the WORST object drives each metric (per-object
    stage1 / repair_risk values); the report still carries the full per-object
    numbers.

    Returns None when no budget is set (no behaviour change); otherwise:
      {'geometry_change_pct': float|None, 'risk_score': int|None,
       'max_geometry_change_pct': float|None, 'max_risk_score': float|None,
       'within_budget': bool, 'exceeded': [{metric, value, budget}, ...]}.
    """
    if max_geom_change is None and max_risk is None:
        return None
    reports = result.get('object_reports')
    if reports:
        geoms, risks = [], []
        for rep in reports:
            g = _geom_change_pct(rep.get('stage1') or {})
            if g is not None:
                geoms.append(g)
            r = rep.get('repair_risk')
            if isinstance(r, (int, float)) and not isinstance(r, bool):
                risks.append(float(r))
        geom = max(geoms) if geoms else None
        risk = max(risks) if risks else None
    else:
        geom = _geom_change_pct(result.get('stage1') or {})
        risk = result.get('repair_risk')
        if not isinstance(risk, (int, float)) or isinstance(risk, bool):
            risk = None
        elif risk is not None:
            risk = float(risk)

    exceeded = []
    if max_geom_change is not None and geom is not None \
            and geom > float(max_geom_change):
        exceeded.append({'metric': 'geometry_change',
                         'value': round(geom, 2),
                         'budget': float(max_geom_change)})
    if max_risk is not None and risk is not None and risk > float(max_risk):
        exceeded.append({'metric': 'risk',
                         'value': int(round(risk)),
                         'budget': float(max_risk)})
    return {
        'geometry_change_pct': None if geom is None else round(geom, 2),
        'risk_score': None if risk is None else int(round(risk)),
        'max_geometry_change_pct':
            float(max_geom_change) if max_geom_change is not None else None,
        'max_risk_score': float(max_risk) if max_risk is not None else None,
        'within_budget': not exceeded,
        'exceeded': exceeded,
    }


def _confirm_budget_save(budget):
    """Prompt the user to confirm saving despite an exceeded budget.

    Only prompts on an interactive terminal (stdin is a TTY). Non-interactive
    callers (the GUI subprocess, scripts, file-manager integration) never
    hang and return False so the output is declined instead of saved
    silently."""
    if not sys.stdin.isatty():
        return False
    try:
        print('WARNING: the repair exceeded your budget (%s).'
              % _budget_message(budget), file=sys.stderr)
        answer = input('Save anyway? [y/N] ').strip().lower()
    except (EOFError, KeyboardInterrupt):
        return False
    return answer in ('y', 'yes')


def scan_bad_coordinates(path):
    """Return a description of NaN/Inf coordinates in STL/OBJ files, or None."""
    ext = os.path.splitext(path)[1].lower()
    if ext == '.3mf':
        # 3MF meshes are parsed as XML (parse_3mf_meshes); NaN/Inf vertex
        # attributes simply don't match the numeric regex and are skipped, so
        # there is nothing to scan here.
        return None
    if ext == '.obj':
        with open(path, errors='replace') as f:
            for line in f:
                if line.startswith('v '):
                    for tok in line.split()[1:4]:
                        try:
                            if not math.isfinite(float(tok)):
                                return 'NaN or infinite coordinates'
                        except ValueError:
                            pass
        return None
    with open(path, 'rb') as f:
        head = f.read(80)
        if head[:5] == b'solid':
            f.seek(0)
            for line in f:
                if line.strip().startswith(b'vertex'):
                    for tok in line.split()[1:4]:
                        try:
                            if not math.isfinite(float(tok)):
                                return 'NaN or infinite coordinates'
                        except ValueError:
                            pass
            return None
        buf = f.read(4)
        if len(buf) < 4:
            return 'malformed STL (no triangle count)'
        n = struct.unpack('<I', buf)[0]
        # A valid binary STL is exactly 84 + n*50 bytes; a mismatch means a
        # bad header count or a cut-off file, which pymeshlab hangs on instead
        # of failing cleanly.
        size = os.path.getsize(path)
        if size != 84 + n * 50:
            return 'malformed STL (declared %d triangles, file size mismatch)' % n
        return _scan_bin_stl(f, n)
    return None


def _scan_bin_stl(f, n):
    """Bulk-read the n 50-byte binary STL records after the 84-byte header and
    report the FIRST non-finite vertex coordinate (only the 3 vertex vectors,
    offsets 12..48 — normals and the 2-byte attribute are not checked, exactly
    as before). Returns an error message string or None."""
    dt = np.dtype([('normal', '<f4', (3,)),
                   ('verts', '<f4', (3, 3)),
                   ('attr', '<u2')])
    data = np.fromfile(f, dtype=dt, count=n)
    if data.shape[0] != n:
        return 'malformed STL (truncated data)'
    if not np.isfinite(data['verts']).all():
        return 'NaN or infinite coordinates'
    return None


def obj_has_material_refs(path):
    """True when an OBJ file references materials/textures (mtllib/usemtl).

    The repair pipeline rebuilds the mesh (verts + tris only), so any
    material/texture references in the input are not preserved in the
    repaired output. This lets the report surface that loss explicitly
    instead of dropping it silently. Only meaningful for .obj inputs."""
    try:
        with open(path, errors='replace') as f:
            for line in f:
                s = line.lstrip().lower()
                if s.startswith(('mtllib ', 'usemtl ')):
                    return True
    except OSError:
        return False
    return False


def run_after_stage2_engines(ml, report, new_v, new_t, tmpdir, engines,
                             engine_chain, spec):
    """Run after_stage2 engines on the stage-2 result and adopt under the
    shared guard. Returns the (possibly replaced) ``(new_v, new_t)`` and
    appends one report entry per engine to ``report['engines']``."""
    if not engine_placement_order(engines, engine_chain, 'after_stage2'):
        return new_v, new_t
    entries = []
    base_holes = boundary_loop_stats(new_v, new_t)[0]
    base_ms = ml.MeshSet()
    base_ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(new_v, np.float32),
                             face_matrix=np.asarray(new_t, np.int32)))
    base_nm = base_ms.apply_filter('get_topological_measures').get(
        'non_two_manifold_edges', 0)
    cand_v, cand_t, adopted = run_engines(
        ml, engines, engine_chain, 'after_stage2', tmpdir, new_v, new_t,
        base_holes, base_nm, spec, entries)
    _record_engine_entries(report, entries)
    if adopted:
        return cand_v, cand_t
    return new_v, new_t


def repair_file(src, out, tmpdir, mode='auto', profile=None, engine='experimental',
                join_components=False, autorefine=False, ftetwild=False,
                indirect_autorefine=False, extra_features=False, deep_repair=None,
                triage_spec=None, engines=None, engine_chain=None,
                closing=None, proxy_template=False, repeat=None,
                repeat_source=None, repeat_target=None,
                 wall_thicken=False, wall_min_thickness=None, graft=False,
                 flap=False,
                 dressing=False, dressing_drain=None, dressing_defects=None,
                 dressing_rmax_scale=None, dressing_sigma_scale=None,
                 si_mode=None, dressing_force_adopt=False):
    """Repair a single STL/OBJ/3MF file. Returns the report dict."""
    import pymeshlab as ml

    bad_coords = scan_bad_coordinates(src)
    if bad_coords:
        raise ValueError('input mesh contains %s' % bad_coords)

    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    verts = np.asarray(load_ms.current_mesh().vertex_matrix(), dtype=np.float32)
    tris = np.asarray(load_ms.current_mesh().face_matrix(), dtype=np.int32)

    ext = os.path.splitext(src)[1].lower()
    declared_unit = None
    if ext == '.3mf':
        units = read_3mf_units(src)
        if units:
            declared_unit = next(iter(units.values()))

    def _run(flap_value):
        """Repair the loaded mesh with Flap on/off, through P-HONEST."""
        report, new_v, new_t = repair_mesh_from_arrays(
            verts, tris, tmpdir, mode=mode, profile=profile, engine=engine,
            declared_unit=declared_unit, join_components=join_components,
            autorefine=autorefine, ftetwild=ftetwild,
            indirect_autorefine=indirect_autorefine,
            extra_features=extra_features, deep_repair=deep_repair,
            triage_spec=triage_spec, engines=engines, engine_chain=engine_chain,
            closing=closing, proxy_template=proxy_template, repeat=repeat,
            repeat_source=repeat_source, repeat_target=repeat_target,
            wall_thicken=wall_thicken, wall_min_thickness=wall_min_thickness,
            graft=graft, flap=flap_value,
            dressing=dressing, dressing_drain=dressing_drain,
            dressing_defects=dressing_defects,
            dressing_rmax_scale=dressing_rmax_scale,
            dressing_sigma_scale=dressing_sigma_scale, si_mode=si_mode,
            dressing_force_adopt=dressing_force_adopt)
        report['_fp'] = history.mesh_fingerprint(verts, tris)
        report['defects'] = detect_defects(verts, tris)
        if ext == '.obj':
            report['material_discarded'] = obj_has_material_refs(src)

        # Stage 2 applies to watertight results; shared helper keeps the single-
        # mesh path and per-object 3MF repair on the same code (same gate, same
        # OBJ round-trip, same explicit skip reporting).
        new_v, new_t = maybe_run_stage2(report, new_v, new_t, tmpdir)

        if engines:
            new_v, new_t = run_after_stage2_engines(
                ml, report, new_v, new_t, tmpdir, engines, engine_chain,
                triage_spec or triage.resolve_intensity(None))

        # P-WELD: reload-safe final pass. STL re-welds exactly-coincident
        # float32 vertices on load, so a clean index-topology mesh can reload
        # non-manifold (the stage-2 / closing seam case). Separate those
        # coincidences (or, when the welded mesh is itself repairable, repair
        # it) and adopt only a strictly reload-watertight result; never make
        # the output worse.
        if ext == '.stl':
            new_v, new_t, _pwel = p_weld_final(ml, new_v, new_t)
            if _pwel is not None:
                report['p_weld'] = _pwel

        # Localized exact self-union of residual SI clusters (experimental, OFF
        # by default). Runs on the post-Stage-2 + post-P-WELD mesh; a no-op that
        # returns the input arrays unchanged when disabled or when no cluster is
        # accepted.
        if _should_run_local_exact(report, triage_spec):
            _lse_spec = triage_spec or triage.resolve_intensity(None)
            new_v, new_t, _lse = local_exact.local_exact_self_union(
                ml, new_v, new_t, time_budget=_local_exact_budget(_lse_spec))
            if _lse.get('ran'):
                report['local_exact'] = _lse

        # Flap debris cleanup (only when Flap was adopted): drop the flat,
        # effectively zero-volume closed shells the repair left behind, so a
        # Flap-ON result is never worse than flap-OFF on the part count. Solid
        # parts are never dropped and the cleanup is adopted only when the
        # reload holes/non-manifold count does not rise; every dropped
        # component is reported. See drop_flap_debris.
        if (report.get('flap') or {}).get('adopted'):
            _cl_v, _cl_t, _cl_info = drop_flap_debris(verts, tris, new_v, new_t)
            if _cl_info:
                _bh, _bnm = reload_strict_holes_nm(new_v, new_t)
                _ch, _cnm = reload_strict_holes_nm(_cl_v, _cl_t)
                if _ch <= _bh and _cnm <= _bnm:
                    new_v, new_t = _cl_v, _cl_t
                    report['flap']['debris_cleanup'] = _cl_info

        # P-HONEST: the verdict must describe the mesh actually saved, not the
        # in-memory index topology. The final mesh is chosen above; judge it in
        # the save/reload-equivalent form and let that verdict win.
        enforce_reload_verdict(report, new_v, new_t)
        return report, new_v, new_t

    report, new_v, new_t = _run(flap)

    # HIGH-1 safety net: the Flap adoption gate is topological-only and decided
    # on the pre-chain mesh, so it cannot guarantee the FINAL result beats
    # flap-OFF. When Flap was adopted and the final mesh is not strict-
    # watertight, or has more parts than the input, re-run this file ONCE with
    # Flap OFF and keep the better result. Rare (never fires on the 41-sample
    # regression, where Flap-ON is 41/41 watertight) and it reuses the same
    # code path, so it cannot diverge from a plain flap-OFF repair.
    if flap and (report.get('flap') or {}).get('adopted'):
        try:
            on_reason, on_metrics = _flap_final_needs_off(
                verts, tris, new_v, new_t)
        except Exception:  # noqa: BLE001 - the net never crashes a repair
            on_reason, on_metrics = None, None
        if on_reason:
            on_flap = dict(report['flap'])
            fallback = {'ran': False, 'used': 'on', 'reason': on_reason,
                        'on': on_metrics}
            use_off = False
            try:
                off_report, off_v, off_t = _run(False)
                _, off_metrics = _flap_final_needs_off(
                    verts, tris, off_v, off_t)
                fallback['ran'] = True
                fallback['off'] = off_metrics
                use_off = _flap_off_preferred(
                    (on_reason, on_metrics), (None, off_metrics))
            except Exception as e:  # noqa: BLE001 - the net never crashes
                fallback['error'] = str(e)
            fallback['used'] = 'off' if use_off else 'on'
            if use_off:
                on_flap['adopted_final'] = False
                on_flap['final_fallback'] = fallback
                off_report['flap'] = on_flap
                report, new_v, new_t = off_report, off_v, off_t
            else:
                report['flap']['final_fallback'] = fallback

    save_mesh(out, new_v, new_t)
    return report


def parse_3mf_meshes(path):
    """Return {model_name: [(verts, tris, (start,end))]} for every <mesh> block."""
    out = {}
    with zipfile.ZipFile(path) as z:
        for name in z.namelist():
            if not name.endswith('.model'):
                continue
            xml = _read_zip_entry(z, name).decode('utf-8', errors='replace')
            blocks = []
            for m in re.finditer(r'<mesh>.*?</mesh>', xml, re.S):
                block = m.group(0)
                vs = np.array([
                    list(map(float, v)) for v in re.findall(
                        r'<vertex\s+x="(-?[\d.eE+-]+)"\s+y="(-?[\d.eE+-]+)"\s+z="(-?[\d.eE+-]+)"\s*/>', block)
                ], dtype=np.float32)
                ts = np.array([
                    list(map(int, t)) for t in re.findall(
                        r'<triangle\s+v1="(\d+)"\s+v2="(\d+)"\s+v3="(\d+)"[^>]*/>', block)
                ], dtype=np.int32)
                if len(vs) and len(ts):
                    blocks.append((vs, ts, m.span()))
            if blocks:
                out[name] = blocks
    return out


def build_mesh_block(verts, tris):
    lines = ['<mesh>', '    <vertices>']
    for v in verts:
        lines.append('     <vertex x="%.7g" y="%.7g" z="%.7g"/>' % (v[0], v[1], v[2]))
    lines.append('    </vertices>')
    lines.append('    <triangles>')
    for t in tris:
        lines.append('     <triangle v1="%d" v2="%d" v3="%d"/>' % (t[0], t[1], t[2]))
    lines.append('    </triangles>')
    lines.append('</mesh>')
    return '\n'.join(lines)


# Decompressed-size cap for any single 3MF archive entry (FAZ14 zip-bomb
# guard). A crafted 3MF can declare a tiny compressed size with a huge
# uncompressed size; we check zipfile's header `file_size` BEFORE reading the
# entry, so a bomb fails with a controlled error instead of exhausting memory.
_3MF_MAX_ENTRY_BYTES = 1 << 30  # 1 GiB


def _read_zip_entry(z, name):
    """Read one 3MF zip entry, bounded by the decompressed-size cap.

    Checks the zip header's declared ``file_size`` BEFORE reading so a crafted
    entry (tiny compressed, huge declared uncompressed) fails with a controlled
    ValueError instead of exhausting memory (FAZ14 zip-bomb guard)."""
    info = z.getinfo(name)
    if info.file_size > _3MF_MAX_ENTRY_BYTES:
        raise ValueError(
            '3MF entry "%s" declares %d bytes uncompressed '
            '(> %d): suspicious compression ratio / oversized input.'
            % (name, info.file_size, _3MF_MAX_ENTRY_BYTES))
    return z.read(name)


def repair_3mf(src, out, tmpdir, mode='auto', profile=None, engine='experimental',
               join_components=False, autorefine=False, ftetwild=False,
               indirect_autorefine=False, extra_features=False, deep_repair=None,
               triage_spec=None, engines=None, engine_chain=None,
               closing=None, proxy_template=False, repeat=None,
               repeat_source=None, repeat_target=None,
               wall_thicken=False, wall_min_thickness=None, graft=False,
               flap=False,
               dressing=False, dressing_drain=None, dressing_defects=None,
               dressing_rmax_scale=None, dressing_sigma_scale=None,
               si_mode=None, dressing_force_adopt=False):
    """Repair every object mesh in a 3MF archive, preserving structure.

    Per-object Stage 2: any object that stage 1 closes (two-manifold with no
    remaining holes) is passed through the shared ``maybe_run_stage2`` helper,
    so a closed object gets a manifold3d watertight rebuild exactly like a
    single-mesh file (the helper sets the per-object ``stage2`` report and
    returns the rebuilt arrays). Objects that remain open after stage 1 are
    written as stage-1 output, mirroring the single-mesh behaviour.
    """
    import pymeshlab as ml
    meshes = parse_3mf_meshes(src)
    if not meshes:
        return {'error': 'no mesh objects found in 3MF'}

    with zipfile.ZipFile(src) as z:
        items = []
        for name in z.namelist():
            items.append((name, _read_zip_entry(z, name)))

    units = read_3mf_units(src)
    reports = []
    cache = {}
    for model_name, blocks in meshes.items():
        declared = units.get(model_name, 'millimeter')
        for idx, (verts, tris, span) in enumerate(blocks):
            key = (verts.tobytes(), tris.tobytes())
            if key in cache:
                # byte-identical geometry -> reuse the repair AND its report;
                # each object still gets its own per-object report (incl. the
                # stage2 outcome, which is a pure function of the geometry).
                new_v, new_t, rep = cache[key]
                reports.append(dict(rep))
            else:
                rep, new_v, new_t = repair_mesh_from_arrays(
                    verts, tris, tmpdir, mode=mode, profile=profile,
                    engine=engine, declared_unit=declared,
                    join_components=join_components,
                    autorefine=autorefine,
                    ftetwild=ftetwild,
                    indirect_autorefine=indirect_autorefine,
                    extra_features=extra_features,
                    deep_repair=deep_repair,
                    triage_spec=triage_spec, engines=engines,
                    engine_chain=engine_chain,
                    closing=closing, proxy_template=proxy_template,
                    repeat=repeat, repeat_source=repeat_source,
                    repeat_target=repeat_target, wall_thicken=wall_thicken,
                    wall_min_thickness=wall_min_thickness, graft=graft,
                    flap=flap,
                    dressing=dressing, dressing_drain=dressing_drain,
                    dressing_defects=dressing_defects,
                    dressing_rmax_scale=dressing_rmax_scale,
                    dressing_sigma_scale=dressing_sigma_scale,
                    si_mode=si_mode,
                    dressing_force_adopt=dressing_force_adopt)
                rep['defects'] = detect_defects(verts, tris)
                rep['_fp'] = history.mesh_fingerprint(verts, tris)
                # Per-object stage 2 first: closed objects get a watertight
                # rebuild (same helper/gate as the single-mesh path), and the
                # confidence/health/risk below then see the stage2 outcome.
                new_v, new_t = maybe_run_stage2(rep, new_v, new_t, tmpdir)
                if engines:
                    new_v, new_t = run_after_stage2_engines(
                        ml, rep, new_v, new_t, tmpdir, engines, engine_chain,
                        triage_spec or triage.resolve_intensity(None))
                if _should_run_local_exact(rep, triage_spec):
                    _lse_spec = triage_spec or triage.resolve_intensity(None)
                    new_v, new_t, _lse = local_exact.local_exact_self_union(
                        ml, new_v, new_t,
                        time_budget=_local_exact_budget(_lse_spec))
                    if _lse.get('ran'):
                        rep['local_exact'] = _lse
                # Flap debris cleanup (only when Flap was adopted); see the
                # single-mesh path and drop_flap_debris.
                if (rep.get('flap') or {}).get('adopted'):
                    _cl_v, _cl_t, _cl_info = drop_flap_debris(
                        verts, tris, new_v, new_t)
                    if _cl_info:
                        _bh, _bnm = reload_strict_holes_nm(new_v, new_t)
                        _ch, _cnm = reload_strict_holes_nm(_cl_v, _cl_t)
                        if _ch <= _bh and _cnm <= _bnm:
                            new_v, new_t = _cl_v, _cl_t
                            rep['flap']['debris_cleanup'] = _cl_info
                # P-HONEST: judge the per-object mesh actually written into the
                # archive (reload-equivalent form) before the confidence/score
                # below, so those see the honest verdict too.
                enforce_reload_verdict(rep, new_v, new_t)
                _rc = repair_confidence(rep)
                rep['repair_confidence'] = _rc['score']
                rep['repair_confidence_label'] = _rc['label']
                rep['repair_confidence_factors'] = _rc['factors']
                _rs = repair_score.compute_scores(rep)
                rep['repair_health'] = _rs['health']
                rep['repair_health_factors'] = _rs['health_factors']
                rep['repair_risk'] = _rs['risk']
                rep['repair_risk_factors'] = _rs['risk_factors']
                rep['repair_status'] = _rs['status']
                rep['repair_status_code'] = _rs['status_code']
                reports.append(rep)
                cache[key] = (new_v, new_t, rep)

            for i, (fname, fdata) in enumerate(items):
                if fname != model_name:
                    continue
                xml = fdata.decode('utf-8', errors='replace')
                rebuilt = list(xml)
                rebuilt[span[0]:span[1]] = build_mesh_block(new_v, new_t)
                items[i] = (fname, ''.join(rebuilt).encode('utf-8'))

    with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
        for fname, fdata in items:
            z.writestr(fname, fdata)

    agg = {'objects': len(meshes), 'object_reports': reports}
    if reports:
        # backward compat: the top-level stage1 stays object-0's (classification
        # now evaluates object_reports directly for multi-object files).
        agg['stage1'] = reports[0].get('stage1', {})
        agg['objects_watertight'] = sum(
            1 for r in reports
            if r.get('stage1', {}).get('two_manifold')
            and r.get('stage1', {}).get('holes_remaining', 0) == 0
            and bool(r.get('stage2', {}).get('ok')))
        agg['objects_stage2_ok'] = sum(
            1 for r in reports if bool(r.get('stage2', {}).get('ok')))
        s2 = next((r['stage2'] for r in reports if 'stage2' in r), None)
        if s2 is not None:
            agg['stage2'] = s2
        # Flap aggregate (only when Flap was requested, so an OFF multi-object
        # file keeps the pre-flap aggregate byte-identical): lets a 3MF batch be
        # scanned for the Flap outcome without walking object_reports.
        _flaps = [r['flap'] for r in reports
                  if isinstance(r.get('flap'), dict)]
        if _flaps:
            agg['flap'] = {
                'objects': len(_flaps),
                'ran': sum(1 for f in _flaps if f.get('ran')),
                'adopted': sum(1 for f in _flaps if f.get('adopted')),
                'final_fallback_off': sum(
                    1 for f in _flaps
                    if (f.get('final_fallback') or {}).get('used') == 'off'),
            }
    return agg


def load_meshes(src):
    """Load a mesh file as a list of (model_name|None, verts, tris).

    STL/OBJ give one entry with ``model_name`` None; a 3MF yields one entry
    per object (or an empty list when it has no mesh objects). Shared by
    validate and dry-run so both see exactly what repair would see."""
    import pymeshlab as ml
    ext = os.path.splitext(src)[1].lower()
    if ext == '.3mf':
        out = []
        for name, blocks in parse_3mf_meshes(src).items():
            for vs, ts, _span in blocks:
                out.append((name, np.asarray(vs, dtype=np.float32),
                            np.asarray(ts, dtype=np.int32)))
        return out
    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    m = load_ms.current_mesh()
    return [(None, np.asarray(m.vertex_matrix(), dtype=np.float32),
             np.asarray(m.face_matrix(), dtype=np.int32))]


def validate_mesh_from_arrays(verts, tris, engine='experimental',
                              declared_unit=None, si_mode=None):
    """Analyze one mesh WITHOUT repairing it.

    Combines defects.detect(), the mesh classifier and cheap pymeshlab
    measures (self-intersecting faces, connected components) into a
    read-only report. Never writes anything. ``declared_unit`` feeds the
    non-blocking unit-warning heuristic (3MF only). ``si_mode`` selects the
    self-intersection policy; under 'off' the count is reported as None
    ('not measured') and is not required by the watertight predicate."""
    import pymeshlab as ml
    si_mode = resolve_si_mode(si_mode)
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    cls, classifier_engine = classify_with_engine(verts, tris, engine)
    d = detect_defects(verts, tris)
    holes = d['holes']
    nm = d['non_manifold']

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    topo = ms.apply_filter('get_topological_measures')
    if si_mode == 'off':
        self_intersections = None
    else:
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        self_intersections = int(ms.current_mesh().face_selection_array().sum())

    vol = signed_volume(v, t)
    bridge = bool(os.path.exists(BRIDGE))
    validation = {
        'vertices': int(v.shape[0]),
        'faces': int(t.shape[0]),
        'holes': holes,
        'non_manifold': nm,
        'self_intersections': self_intersections,
        'connected_components': int(topo.get('connected_components_number', 1)),
        'watertight': bool(len(holes) == 0 and len(nm) == 0
                           and self_intersections in (0, None)),
        'signed_volume': round(float(vol), 6),
        'surface_area': round(surface_area(v, t), 3),
        'orientation': 'inverted' if vol < 0 else 'consistent',
    }
    _u = check_units(v, declared_unit)
    if _u:
        validation['unit_warning'] = True
        validation['unit_hint'] = _u['unit_hint']
    est = estimate_confidence_pre_repair({
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'stage2_bridge_available': bridge,
        'holes': len(holes),
        'non_manifold': len(nm),
        'self_intersections': self_intersections,
    })
    return {'validation': validation,
            'detected_type': cls['type'],
            'detected_confidence': cls['confidence'],
            'classifier_engine': classifier_engine,
            'stage2_bridge_available': bridge,
            'estimated_confidence': est['score'],
            'estimated_confidence_label': est['label'],
            'estimated_confidence_factors': est['factors']}


def validate_file(src, engine='experimental', si_mode=None):
    """Validate a mesh file without repairing it. Returns the report dict;
    a hard error (missing/malformed input) is a dict with an 'error' key."""
    result = {'input': src}
    try:
        if not os.path.exists(src):
            raise ValueError('file not found: %s' % src)
        bad_coords = scan_bad_coordinates(src)
        if bad_coords:
            raise ValueError('input mesh contains %s' % bad_coords)
        meshes = load_meshes(src)
        if not meshes:
            raise ValueError('no mesh objects found in 3MF')
        units = read_3mf_units(src) if os.path.splitext(src)[1].lower() == '.3mf' else {}
        reports = []
        for name, vs, ts in meshes:
            declared = units.get(name) if name is not None else None
            rep = validate_mesh_from_arrays(vs, ts, engine, declared_unit=declared,
                                            si_mode=si_mode)
            if name is not None:
                rep['model'] = name
            reports.append(rep)
        if len(reports) == 1:
            result.update(reports[0])
        else:
            result['objects'] = len(reports)
            result['object_reports'] = reports
            result['detected_type'] = reports[0].get('detected_type')
            result['detected_confidence'] = reports[0].get('detected_confidence')
    except Exception as e:
        result['error'] = 'validation failed: %s' % e
    return result


def dry_run_mesh_from_arrays(verts, tris, mode='auto', profile=None, engine='experimental',
                             declared_unit=None, triage_spec=None, si_mode=None,
                             flap=False):
    """Report what a repair WOULD do for one mesh, without doing it.

    Detects the type, resolves the mode/thresholds and counts the holes /
    debris / self-intersections. The debris count reuses the same
    meshing_remove_connected_component_by_face_number filter the repair
    chain runs, applied to an in-memory scratch mesh - nothing is written.
    ``declared_unit`` feeds the non-blocking unit-warning heuristic
    (3MF only). ``triage_spec`` is the resolved intensity preset (Balanced
    when None); Stage 1 thresholds are unaffected by it. ``flap`` is reported
    as ``flap_step`` in the plan so --dry-run shows the pre-pass (no work is
    done)."""
    import pymeshlab as ml
    si_mode = resolve_si_mode(si_mode)
    v = np.asarray(verts, dtype=np.float32)
    t = np.asarray(tris, dtype=np.int32)
    if len(t) == 0 or len(v) == 0:
        raise ValueError('input mesh is empty (no triangles)')
    if not np.isfinite(v).all():
        raise ValueError('input mesh contains NaN or infinite coordinates')

    cls, classifier_engine = classify_with_engine(verts, tris, engine)
    d = detect_defects(verts, tris)
    holes = d['holes']
    nm = d['non_manifold']
    params, tuning = resolve_mode_params(mode, cls['type'], cls['confidence'],
                                         profile=profile)
    # keep --dry-run's would_apply in sync with the real repair: the mesh-
    # sensitive maxholesize raise must be visible in the plan too.
    params['maxholesize'] = max(params['maxholesize'],
                                2 * boundary_loop_stats(verts, tris)[1])

    ms = ml.MeshSet()
    ms.add_mesh(ml.Mesh(vertex_matrix=v, face_matrix=t))
    topo = ms.apply_filter('get_topological_measures')
    # "found" counts describe the INPUT mesh (like defects.detect does), so
    # self-intersections are measured before the debris-removal step below.
    if si_mode == 'off':
        self_intersections = None
    else:
        ms.apply_filter('compute_selection_by_self_intersections_per_face')
        self_intersections = int(ms.current_mesh().face_selection_array().sum())
        ms.apply_filter('set_selection_none')
    before_faces = ms.current_mesh().face_number()
    ms.apply_filter('meshing_remove_connected_component_by_face_number',
                    mincomponentsize=params['mincomponentsize'], removeunref=True)
    debris_faces = max(before_faces - ms.current_mesh().face_number(), 0)

    est = estimate_confidence_pre_repair({
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'tuning_applied': tuning,
        'mode': mode,
        'holes': len(holes),
        'non_manifold': len(nm),
        'self_intersections': self_intersections,
        'stage2_bridge_available': bool(os.path.exists(BRIDGE)),
    })

    result = {
        'repair_mode': mode,
        'repair_profile': profile,
        'detected_type': cls['type'],
        'detected_confidence': cls['confidence'],
        'classifier_engine': classifier_engine,
        'tuning_applied': tuning,
        'would_apply': params,
        'holes_found': len(holes),
        'largest_hole_diameter': round(max((h['diameter'] for h in holes), default=0.0), 4),
        'non_manifold_regions': len(nm),
        'debris_faces_removable': debris_faces,
        'self_intersections': self_intersections,
        'connected_components': int(topo.get('connected_components_number', 1)),
        'stage2_bridge_available': bool(os.path.exists(BRIDGE)),
        'estimated_confidence': est['score'],
        'estimated_confidence_label': est['label'],
        'estimated_confidence_factors': est['factors'],
        'flap_step': bool(flap),
    }
    result.update(triage.triage_report_fields(triage_spec))
    _u = check_units(v, declared_unit)
    if _u:
        result['unit_warning'] = True
        result['unit_hint'] = _u['unit_hint']
    return result


def dry_run_file(src, mode='auto', profile=None, engine='experimental',
                 triage_spec=None, si_mode=None, flap=False):
    """Dry-run one mesh file. Returns the report dict; a hard error is a
    dict with an 'error' key. Never writes any output file."""
    result = {'input': src}
    try:
        if not os.path.exists(src):
            raise ValueError('file not found: %s' % src)
        bad_coords = scan_bad_coordinates(src)
        if bad_coords:
            raise ValueError('input mesh contains %s' % bad_coords)
        meshes = load_meshes(src)
        if not meshes:
            raise ValueError('no mesh objects found in 3MF')
        units = read_3mf_units(src) if os.path.splitext(src)[1].lower() == '.3mf' else {}
        reports = []
        for name, vs, ts in meshes:
            declared = units.get(name) if name is not None else None
            rep = dry_run_mesh_from_arrays(vs, ts, mode, profile, engine,
                                           declared_unit=declared,
                                           triage_spec=triage_spec,
                                           si_mode=si_mode, flap=flap)
            if name is not None:
                rep['model'] = name
            reports.append(rep)
        if len(reports) == 1:
            result.update(reports[0])
        else:
            result['objects'] = len(reports)
            result['object_reports'] = reports
    except Exception as e:
        result['error'] = 'dry run failed: %s' % e
    return result


def human_defects(r):
    """Render the defect list for --human mode (only with --defects)."""
    d = r.get('defects')
    if not d:
        return None
    lines = ['', 'Defects (input):']
    holes = d.get('holes', [])
    nm = d.get('non_manifold', [])
    if not holes and not nm:
        lines.append('  none')
    for h in holes:
        c = h['centroid']
        lines.append('  hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f mm, %d verts'
                     % (c[0], c[1], c[2], h['diameter'], h['vertices']))
    for r_ in nm:
        c = r_['centroid']
        lines.append('  non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces'
                     % (c[0], c[1], c[2], r_['faces']))
    return '\n'.join(lines)


def human_report(r, show_defects=False, show_diff=False):
    if 'error' in r and 'stage1' not in r:
        return 'ERROR: %s' % r['error']
    s1 = r.get('stage1', {})
    lines = []
    lines.append('Input : %s' % r.get('input'))
    lines.append('Output: %s' % r.get('output'))
    lines.append('Mode  : %s' % r.get('repair_mode', 'auto'))
    if r.get('unit_warning'):
        lines.append('WARNING: %s' % r.get('unit_hint'))
    if r.get('status') == 'budget_declined':
        lines.append('ERROR: %s' % r.get('error'))
    if r.get('classifier_engine') is not None:
        lines.append('Classifier: %s' % r['classifier_engine'])
    if r.get('repair_profile'):
        lines.append('Profile: %s' % r['repair_profile'])
    lines.append('Intensity: %s' % r.get('triage_intensity', 'balanced'))
    dt = r.get('detected_type')
    if dt:
        conf = r.get('detected_confidence', 0.0)
        tuning = r.get('tuning_applied')
        line = 'Type  : %s (confidence %.2f)' % (dt, conf)
        if tuning is not None:
            line += ' - tuned thresholds' if tuning else ' - default thresholds (below confidence gate)'
        lines.append(line)
    rc = r.get('repair_confidence')
    if rc is not None:
        lines.append('Confidence: %d/100 (%s)'
                     % (rc, str(r.get('repair_confidence_label', '?')).title()))
    rh = r.get('repair_health')
    rk = r.get('repair_risk')
    if rh is not None or rk is not None:
        lines.append('Health: %s/100   Risk: %s/100   Status: %s' % (
            'n/a' if rh is None else rh,
            'n/a' if rk is None else rk,
            r.get('repair_status', 'n/a')))
    b = r.get('budget')
    if b is not None:
        lines.append('Budget: geometry change %s%% (limit %s) · risk %s '
                     '(limit %s) · %s' % (
                         'n/a' if b.get('geometry_change_pct') is None
                         else b.get('geometry_change_pct'),
                         'n/a' if b.get('max_geometry_change_pct') is None
                         else b.get('max_geometry_change_pct'),
                         'n/a' if b.get('risk_score') is None
                         else b.get('risk_score'),
                         'n/a' if b.get('max_risk_score') is None
                         else b.get('max_risk_score'),
                         'EXCEEDED' if not b.get('within_budget')
                         else 'within budget'))
    if r.get('material_discarded'):
        lines.append('Material: input OBJ has mtllib/usemtl references which '
                     'are not preserved in the repaired output.')
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed            : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining         : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed: %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed           : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components    : %d' % s1.get('components', 0))
    lines.append('  Two-manifold            : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    pv = r.get('pinched_vertices_split')
    if pv:
        lines.append('  Pinched vertices split  : %d -> %d (%d pass(es))%s'
                     % (pv.get('before', 0), pv.get('after', 0), pv.get('passes', 0),
                        '' if pv.get('adopted') else ', not kept'))
    pw = r.get('p_weld')
    if pw:
        state = ('%s adopted' % pw.get('method')) if pw.get('applied') \
            else 'no improvement (kept reload-welded state)'
        lines.append('  Reload-safe final pass  : holes %d -> %d, non-manifold %d -> %d (%s)'
                     % (pw.get('holes_before', 0), pw.get('holes_after', 0),
                        pw.get('non_manifold_before', 0),
                        pw.get('non_manifold_after', 0), state))
    le_r = r.get('local_exact')
    if le_r and le_r.get('ran'):
        if le_r.get('applied'):
            state = '%d/%d cluster(s) accepted' % (le_r.get('accepted', 0),
                                                   le_r.get('clusters', 0))
        else:
            state = 'no cluster accepted (%s)' % (le_r.get('reason') or '?')
        lines.append('  Local exact self-union : SI %d -> %s, %s in %.2fs'
                     % (le_r.get('si_before', 0), le_r.get('si_after'),
                        state, le_r.get('seconds', 0)))
    if r.get('extreme_passes_applied'):
        lines.append('  Extreme passes          : applied (%d self-intersecting face(s) removed)'
                     % r.get('self_intersections_removed', 0))
    elif r.get('repair_mode') == 'extreme':
        lines.append('  Extreme passes          : none needed (no self-intersections)')
    ar_r = r.get('experimental_autorefine')
    if ar_r:
        if ar_r.get('skipped'):
            lines.append('  Autorefine (experiment) : skipped (no triangles)')
        elif 'error' in ar_r:
            lines.append('  Autorefine (experiment) : error (%s)' % ar_r['error'][:60])
        else:
            conv = 'yes' if ar_r.get('converged') else ('no (capped)' if ar_r.get('capped') else 'no')
            adopted = ' adopted' if ar_r.get('adopted') else ' NOT adopted (kept stage-1 output)'
            lines.append('  Autorefine (experiment) : SI pairs %d -> %d, %d pass(es), '
                         'converged=%s%s' % (ar_r.get('si_before', 0),
                                             ar_r.get('si_after', 0),
                                             ar_r.get('iterations', 0), conv, adopted))
    ft_r = r.get('experimental_ftetwild')
    if ft_r:
        if ft_r.get('reject_reason') == 'too_large':
            lines.append('  fTetWild fallback : skipped (%d faces, limit %d)'
                         % (ft_r.get('input_faces', 0), ft_r.get('max_faces', 0)))
        elif ft_r.get('ran') and 'error' in ft_r:
            lines.append('  fTetWild fallback : error (%s)'
                         % ft_r['error'][:60])
        elif ft_r.get('ran'):
            if ft_r.get('adopted') and ft_r.get('shape_changed'):
                adopted = (' adopted, SHAPE CHANGED (Hausdorff %.1f%% of the '
                           'diagonal)' % (100 * (ft_r.get('hausdorff_rel') or 0)))
            elif ft_r.get('adopted'):
                adopted = ' adopted'
            else:
                adopted = ' NOT adopted (kept stage-1 output)'
            pp = ' + manifold3d post-process' if ft_r.get('manifold_postprocessed') else ''
            if ft_r.get('adopted_attempt') == 'optimize':
                pp += ' (second attempt, optimisation on)'
            if ft_r.get('ftetwild_decimated'):
                pp += ', decimated %d->%d faces' % (
                    ft_r.get('ftetwild_faces_raw', 0),
                    ft_r.get('ftetwild_faces_decimated',
                             ft_r.get('ftetwild_faces_final', 0)))
            lines.append('  fTetWild fallback : %d faces in %.2fs%s, '
                         'holes=%s non-manifold=%s%s' % (
                             ft_r.get('output_faces', 0), ft_r.get('time', 0), pp,
                             ft_r.get('output_holes'), ft_r.get('output_non_manifold'),
                             adopted))
    fl_r = r.get('flap')
    if fl_r and fl_r.get('ran'):
        if fl_r.get('adopted'):
            state = ' adopted'
        else:
            state = ' NOT adopted (kept stage-1 output)'
        lines.append('  Flap (surface fill)     : filled %s/%s loops, '
                     '%s patch faces in %.2fs, max original move %.3g%s'
                     % (fl_r.get('loops_filled'), fl_r.get('loops_found'),
                        fl_r.get('patch_faces'), fl_r.get('time_s') or 0,
                        fl_r.get('max_orig_vertex_move_mm') or 0, state))
    gr = r.get('graft')
    if gr and gr.get('ran'):
        if gr.get('adopted'):
            state = ' adopted%s' % (
                '' if gr.get('fidelity_ok') is not False
                else ' (LOW FIDELITY: %s)' % (gr.get('warnings') or [{}])[0].get(
                    'message_en', 'detail loss'))
        else:
            state = ' NOT adopted (kept stage-1 output)'
        lines.append('  Graft (shell wrap)      : %s faces in %.2fs, r=%s, %s%s'
                     % (gr.get('faces', 0), gr.get('seconds', 0),
                        gr.get('r_used'), gr.get('mode'), state))
    dr_r = r.get('dressing')
    if dr_r and dr_r.get('ran'):
        if dr_r.get('adopted'):
            state = ' adopted%s' % (
                '' if dr_r.get('fidelity_ok') is not False
                else ' (LOW FIDELITY: %.2f%% of diag)'
                % (100.0 * (dr_r.get('hausdorff_rel_max') or 0)))
        else:
            state = ' NOT adopted (%s)' % (dr_r.get('reason') or '?')
        _dd = dr_r.get('drain') or {}
        _drain_txt = ('drain=%s' % _dd.get('mode')
                      if _dd and _dd.get('drained')
                      else 'drain=off')
        lines.append('  Dressing (viscosity)    : %s faces in %.2fs, '
                     'voxel=%s, r=%s, %s, holes=%s nm=%s%s'
                     % (dr_r.get('faces') or dr_r.get('faces_coat', 0),
                        dr_r.get('seconds', 0), dr_r.get('voxel'),
                        dr_r.get('r_base'), _drain_txt, dr_r.get('holes'),
                        dr_r.get('non_manifold'), state))
    eng_r = r.get('engines')
    if eng_r:
        lines.append('')
        lines.append('External engines:')
        for e in eng_r:
            if e.get('adopted'):
                if e.get('shape_changed'):
                    state = ('adopted, SHAPE CHANGED (Hausdorff %.1f%% of the '
                             'diagonal)' % (100 * (e.get('hausdorff_rel') or 0)))
                else:
                    state = 'adopted'
            else:
                state = 'NOT adopted (%s)' % (e.get('reject_reason') or '?')
            lines.append('  %s [%s] : %s, rc=%s, %.2fs' % (
                e.get('name'), e.get('placement'), state, e.get('rc'),
                e.get('time', 0)))
    dr = r.get('deep_repair')
    if dr:
        lr = dr.get('local')
        if lr:
            if 'error' in lr:
                lines.append('  Local re-mesh (experiment) : error (%s)' % lr['error'][:60])
            else:
                res = ('adopted' if lr.get('adopted')
                       else 'NOT adopted (%s)' % lr.get('reject_reason', '?'))
                lines.append('  Local re-mesh (experiment) : %d region(s), %d face(s) removed, %s added, '
                             'holes %s -> %s, non-manifold %s -> %s, %s' % (
                                 lr.get('regions', 0), lr.get('faces_removed', 0),
                                 lr.get('faces_added', '-'), lr.get('holes_before'),
                                 lr.get('holes_after', '-'), lr.get('nm_before'),
                                 lr.get('nm_after', '-'), res))
        av = dr.get('available')
        if av:
            est = ', '.join('%s ~%ss' % (k, v) for k, v in av['estimate_s'].items())
            lines.append('  Deep repair available : %d hole(s), %d non-manifold '
                         'edge(s) left; %s' % (av['holes_remaining'],
                                               av['nm_remaining'], est))
    cl_r = r.get('closing')
    if cl_r:
        if not cl_r.get('ran'):
            lines.append('  Closing (method %s) : not run (%s)'
                         % (cl_r.get('tier'), cl_r.get('reason')))
        else:
            state = ('adopted' if cl_r.get('adopted')
                     else 'NOT adopted (%s)' % (cl_r.get('reason') or '?'))
            note = (' - %s' % '; '.join(cl_r.get('notes') or [])
                    if cl_r.get('notes') else '')
            lines.append('  Closing (method %s) : %s faces, holes=%s '
                         'non-manifold=%s, %s%s' % (
                             cl_r.get('tier'), cl_r.get('faces'),
                             cl_r.get('holes'), cl_r.get('non_manifold'),
                             state, note))
    rp_r = r.get('repeat')
    if rp_r:
        if not rp_r.get('ran'):
            lines.append('  Repeat repair (method %s) : not run (%s)'
                         % (rp_r.get('mode'), rp_r.get('reason')))
        else:
            state = ('transplanted' if rp_r.get('adopted')
                     else 'NOT adopted (%s)' % (rp_r.get('reason') or '?'))
            lines.append('  Repeat repair (method %s) : pattern=%s, %s '
                         'position(s) repaired, %s' % (
                             rp_r.get('mode'), rp_r.get('pattern_type'),
                             rp_r.get('positions_repaired'), state))
    ia_r = r.get('experimental_indirect_autorefine')
    if ia_r:
        if ia_r.get('skipped'):
            lines.append('  Indirect autorefine (experiment) : skipped (%s)'
                         % (ia_r.get('error') or 'no triangles')[:60])
        elif 'error' in ia_r:
            lines.append('  Indirect autorefine (experiment) : error (%s)'
                         % ia_r['error'][:60])
        else:
            adopted = ' adopted' if ia_r.get('adopted') else ' NOT adopted (kept stage-1 output)'
            lines.append('  Indirect autorefine (experiment) : SI pairs %d -> %d, '
                         'faces %d -> %d%s' % (ia_r.get('si_before', 0),
                                               ia_r.get('si_after', 0),
                                               ia_r.get('faces_before', 0),
                                               ia_r.get('faces_after', 0),
                                               adopted))
    if show_diff:
        lines.append('  Vertices                : %s -> %s' % (
            s1.get('vertices_before', 0), s1.get('vertices_after', 0)))
        lines.append('  Faces                   : %s -> %s' % (
            s1.get('faces_before', 0), s1.get('faces_after', 0)))
        if s1.get('surface_area_change_percent') is not None:
            lines.append('  Surface area change     : %s%%' % s1.get('surface_area_change_percent'))
    if s1.get('volume_change_percent') is not None:
        lines.append('  Volume change          : %s%%' % s1.get('volume_change_percent'))
    if s1.get('volume_warning'):
        lines.append('')
        lines.append('  WARNING: %s' % s1['volume_warning'])
    if 'stage2' in r:
        s2 = r['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            if s2['error'].startswith('Stage 2 skipped'):
                lines.append('  SKIPPED: %s' % s2['error'])
            else:
                lines.append('  ERROR: %s' % s2['error'])
        else:
            for k, v in s2.items():
                lines.append('  %s: %s' % (k, v))
    if 'objects' in r:
        lines.append('')
        lines.append('3MF objects repaired: %d' % r['objects'])
        if r.get('objects_watertight') is not None:
            lines.append('Objects watertight: %d/%d' % (
                r.get('objects_watertight'), len(r.get('object_reports', []))))
        for i, rep in enumerate(r.get('object_reports', [])):
            s1o = rep.get('stage1', {})
            s2o = rep.get('stage2') or {}
            if s2o.get('ok'):
                label = 'watertight'
            elif s1o.get('two_manifold') and s1o.get('holes_remaining', 0) == 0:
                label = ('stage 2 skipped'
                         if s2o.get('error', '').startswith('Stage 2 skipped')
                         else 'stage 2 error')
            else:
                label = 'partial'
            lines.append('  object %d: %s (%d hole(s) remaining, two-manifold=%s)' % (
                i, label,
                s1o.get('holes_remaining', 0), 'YES' if s1o.get('two_manifold') else 'NO'))
            if rep.get('unit_warning'):
                lines.append('    WARNING: %s' % rep.get('unit_hint'))
            if show_diff:
                lines.append('    vertices %s -> %s, faces %s -> %s, surface %s%%' % (
                    s1o.get('vertices_before', 0), s1o.get('vertices_after', 0),
                    s1o.get('faces_before', 0), s1o.get('faces_after', 0),
                    s1o.get('surface_area_change_percent', 0)))
            if show_defects:
                ds = human_defects(rep)
                if ds:
                    lines.append(ds)
    elif show_defects:
        ds = human_defects(r)
        if ds:
            lines.append(ds)
    for _sug in r.get('suggestions', []):
        if _sug.get('method') == 'dressing':
            lines.append('')
            lines.append(_dressing_suggestion_text(_sug))
    return '\n'.join(lines)


def human_validate(r, show_defects=False):
    """Render a validate report for --human mode."""
    if 'error' in r and 'validation' not in r:
        return 'ERROR: %s' % r['error']
    lines = ['Input : %s' % r.get('input')]
    if 'object_reports' in r:
        lines.append('3MF objects validated: %d' % r.get('objects', 0))
        for i, rep in enumerate(r.get('object_reports', [])):
            v = rep.get('validation', {})
            lines.append('  object %d (%s): %d verts, %d faces, %d hole(s), '
                         '%d non-manifold region(s), %d self-intersection(s), watertight=%s' % (
                i, rep.get('model', '?'), v.get('vertices', 0), v.get('faces', 0),
                len(v.get('holes', [])), len(v.get('non_manifold', [])),
                v.get('self_intersections', 0), 'YES' if v.get('watertight') else 'NO'))
            if v.get('unit_warning'):
                lines.append('    WARNING: %s' % v.get('unit_hint'))
        return '\n'.join(lines)
    v = r.get('validation', {})
    if v.get('unit_warning'):
        lines.append('WARNING: %s' % v.get('unit_hint'))
    lines.append('Type  : %s (confidence %.2f)' % (
        r.get('detected_type', '?'), r.get('detected_confidence', 0.0)))
    if r.get('classifier_engine') is not None:
        lines.append('Classifier: %s' % r['classifier_engine'])
    ec = r.get('estimated_confidence')
    if ec is not None:
        lines.append('Estimated confidence: %d/100 (%s) — actual result may differ after repair'
                     % (ec, str(r.get('estimated_confidence_label', '?')).title()))
    lines.append('')
    lines.append('Validation:')
    lines.append('  Vertices            : %d' % v.get('vertices', 0))
    lines.append('  Faces               : %d' % v.get('faces', 0))
    lines.append('  Holes               : %d' % len(v.get('holes', [])))
    lines.append('  Non-manifold regions: %d' % len(v.get('non_manifold', [])))
    lines.append('  Self-intersections  : %d' % v.get('self_intersections', 0))
    lines.append('  Connected components: %d' % v.get('connected_components', 0))
    lines.append('  Watertight          : %s' % ('YES' if v.get('watertight') else 'NO'))
    lines.append('  Signed volume       : %.6f' % v.get('signed_volume', 0.0))
    lines.append('  Surface area        : %.3f' % v.get('surface_area', 0.0))
    lines.append('  Orientation         : %s' % v.get('orientation', '?'))
    if show_defects:
        for h in v.get('holes', []):
            c = h['centroid']
            lines.append('    hole: centroid=(%.3f, %.3f, %.3f), diameter=%.3f, %d verts'
                         % (c[0], c[1], c[2], h['diameter'], h['vertices']))
        for nm in v.get('non_manifold', []):
            c = nm['centroid']
            lines.append('    non-manifold: centroid=(%.3f, %.3f, %.3f), %d faces'
                         % (c[0], c[1], c[2], nm['faces']))
    return '\n'.join(lines)


def human_dry_run(r):
    """Render a --dry-run report for --human mode."""
    if 'error' in r and 'repair_mode' not in r:
        return 'ERROR: %s' % r['error']
    lines = ['Input : %s' % r.get('input'),
             'Dry-run: no output file will be written']
    if 'object_reports' in r:
        lines.append('3MF objects: %d' % r.get('objects', 0))
        for i, rep in enumerate(r.get('object_reports', [])):
            lines.append('  object %d (%s): mode=%s, type=%s, holes=%d, '
                         'non-manifold=%d, debris=%d face(s), self-intersections=%d' % (
                i, rep.get('model', '?'), rep.get('repair_mode', '?'),
                rep.get('detected_type', '?'), rep.get('holes_found', 0),
                rep.get('non_manifold_regions', 0), rep.get('debris_faces_removable', 0),
                rep.get('self_intersections', 0)))
            if rep.get('unit_warning'):
                lines.append('    WARNING: %s' % rep.get('unit_hint'))
        return '\n'.join(lines)
    lines.append('Mode  : %s' % r.get('repair_mode', 'auto'))
    if r.get('unit_warning'):
        lines.append('WARNING: %s' % r.get('unit_hint'))
    lines.append('Type  : %s (confidence %.2f)' % (
        r.get('detected_type', '?'), r.get('detected_confidence', 0.0)))
    if r.get('classifier_engine') is not None:
        lines.append('Classifier: %s' % r['classifier_engine'])
    pa = r.get('would_apply', {})
    tuning = 'tuned thresholds' if r.get('tuning_applied') else 'default thresholds'
    lines.append('Tuning: %s' % tuning)
    ec = r.get('estimated_confidence')
    if ec is not None:
        lines.append('Estimated confidence: %d/100 (%s) — actual result may differ after repair'
                     % (ec, str(r.get('estimated_confidence_label', '?')).title()))
    lines.append('Would apply: mincomponentsize=%d, maxholesize=%d' % (
        pa.get('mincomponentsize', 0), pa.get('maxholesize', 0)))
    lines.append('Found  : %d hole(s) (largest %.3f), %d non-manifold region(s), '
                 '%d self-intersection(s), %d debris face(s) (< mincomponentsize)' % (
        r.get('holes_found', 0), r.get('largest_hole_diameter', 0.0),
        r.get('non_manifold_regions', 0), r.get('self_intersections', 0),
        r.get('debris_faces_removable', 0)))
    s2 = r.get('stage2_bridge_available')
    lines.append('Stage 2: %s' % (
        'would run if stage 1 closes the mesh (bridge available)' if s2
        else 'not available on this system'))
    if r.get('flap_step'):
        lines.append('Flap  : would run first (pre-pass on the input)')
    return '\n'.join(lines)


def process_file(src, human, mode='auto', profile=None, no_history=False,
                 out=None, engine='experimental', max_geom_change=None,
                 max_risk=None, force=False, join_components=False,
                 autorefine=False, ftetwild=False, indirect_autorefine=False,
                 extra_features=False, deep_repair=None, triage_spec=None,
                 engines=None, engine_chain=None, engine_warnings=None,
                 methods=None, engine_filter=None, repeat_source=None,
                 repeat_target=None, wall_min_thickness=None, graft=False,
                 flap=False,
                 dressing=False, dressing_drain=None, dressing_defects=None,
                 dressing_rmax_scale=None, dressing_sigma_scale=None,
                 si_mode=None, dressing_force_adopt=False):
    """Repair one file. Returns (result_dict, category).

    ``max_geom_change``/``max_risk`` (optional repair budgets) gate the save:
    when the actual geometry change % / repair risk exceeds a budget, the
    temp output is only moved into place after explicit confirmation (an
    interactive ``[y/N]`` prompt on a TTY) or ``force``. Without a budget or
    within budget the save is unconditional (reporting the numbers).
    ``join_components`` enables the experimental join-small-components
    prototype (flag-gated). ``autorefine`` enables the experimental
    self-intersection subdivision prototype (flag-gated). ``ftetwild`` enables
    the experimental fTetWild fallback tier (flag-gated). ``indirect_autorefine``
    enables the experimental exact indirect-predicate arrangement-lite split
    (flag-gated).
    ``triage_spec`` is the resolved intensity preset (sutura/triage.py),
    forwarded to the repair; None resolves to Balanced.
    ``engines``/``engine_chain``/``engine_warnings`` are the ``load_engine_run``
    output; when the first two are None the engines are loaded here (once per
    call). Load warnings are recorded in the report and never fail a repair.
    ``methods`` is the optional user-tagged method list (``--methods``); None
    means auto mode (today's default pipeline first, ranked fallbacks only when
    it fails), implemented by ``sutura/methods.py``.
    ``engine_filter`` is the optional list of external-engine names
    (``--engines``): when given, only those engines run (at their configured
    placement); None keeps the default of running every enabled engine.
    """
    if not os.path.exists(src):
        return ({'input': src, 'error': 'file not found: %s' % src}, 'error')

    if engines is None and engine_chain is None:
        engines, engine_chain, engine_warnings = load_engine_run()
    if engine_filter is not None:
        _want = set(engine_filter)
        engines = {n: c for n, c in (engines or {}).items() if n in _want}
        engine_chain = [n for n in (engine_chain or []) if n in _want]

    stem, ext = os.path.splitext(src)
    if out is None:
        out = stem + '_fixed' + ext

    tmpdir = tempfile.mkdtemp(prefix='sutura-')
    tmp_out = os.path.join(tmpdir, 'out' + ext)
    result = {'input': src, 'output': out}
    if engine_warnings:
        result['engine_warnings'] = list(engine_warnings)
    t0 = time.perf_counter()
    try:
        # Method registry / exec policy (sutura/methods.py): with methods=None
        # this is auto mode -- it runs today's default pipeline first and only
        # escalates to ranked methods when that baseline is not strict-
        # watertight, so an untagged repair stays byte-identical to today.
        result.update(method_registry.repair_with_methods(
            src, tmp_out, tmpdir, methods=methods,
            # An explicit --engines tag ("only these engines") must not silently
            # auto-escalate to extra repair methods; --methods is unaffected.
            auto_escalation=(engine_filter is None),
            mode=mode, profile=profile,
            engine=engine, join_components=join_components,
            autorefine=autorefine, ftetwild=ftetwild,
            indirect_autorefine=indirect_autorefine,
            extra_features=extra_features, deep_repair=deep_repair,
            triage_spec=triage_spec, engines=engines,
            engine_chain=engine_chain, repeat_source=repeat_source,
            repeat_target=repeat_target, wall_min_thickness=wall_min_thickness,
            graft=graft, flap=flap,
            dressing=dressing, dressing_drain=dressing_drain,
            dressing_defects=dressing_defects,
            dressing_rmax_scale=dressing_rmax_scale,
            dressing_sigma_scale=dressing_sigma_scale,
            si_mode=si_mode,
            dressing_force_adopt=dressing_force_adopt))

        # Post-repair confidence + Health/Risk scores (needed for the budget
        # gating below). Multi-object 3MF carries these per object already.
        # Fail-silent: repair_score.compute_scores never raises.
        if 'object_reports' not in result:
            _rc = repair_confidence(result)
            result['repair_confidence'] = _rc['score']
            result['repair_confidence_label'] = _rc['label']
            result['repair_confidence_factors'] = _rc['factors']
            try:
                _rs = repair_score.compute_scores(result)
                result['repair_health'] = _rs['health']
                result['repair_health_factors'] = _rs['health_factors']
                result['repair_risk'] = _rs['risk']
                result['repair_risk_factors'] = _rs['risk_factors']
                result['repair_status'] = _rs['status']
                result['repair_status_code'] = _rs['status_code']
            except Exception:
                pass

        # Repair budget: compare actual geometry change + risk against the
        # user's budgets; an exceeded budget requires confirmation before the
        # temp output is moved into place (or --force). Never saved silently.
        budget = budget_check(result, max_geom_change, max_risk)
        if budget is not None:
            result['budget'] = budget
        if budget is not None and not budget['within_budget']:
            if force or _confirm_budget_save(budget):
                os.replace(tmp_out, out)
            else:
                # Unambiguous, machine-detectable outcome: distinct from a
                # generic 'error' (corrupt file, crash) so the GUI can offer
                # a --force re-run without wrongly offering it for other
                # hard failures.
                result['status'] = 'budget_declined'
                result['error'] = (
                    'Save declined: the repair exceeded your budget (%s). '
                    'No output was written; re-run with --force to save '
                    'anyway.' % _budget_message(budget))
                if 'object_reports' not in result:
                    _rc = repair_confidence(result)
                    result['repair_confidence'] = _rc['score']
                    result['repair_confidence_label'] = _rc['label']
                    result['repair_confidence_factors'] = _rc['factors']
        elif os.path.exists(tmp_out):
            os.replace(tmp_out, out)
    except ExtremeRemovedAllError as e:
        result['error'] = str(e)
        result['extreme_removed_object'] = True
    except Exception as e:
        result['error'] = 'repair failed: %s' % e
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
    elapsed_ms = int((time.perf_counter() - t0) * 1000)

    category, issues, _summary = classify(result)
    result['category'] = category
    result['issues'] = issues
    # The Dressing opt-in hint is kept SEPARATE from the issue codes: it is a
    # suggestion (a next action), not a defect in this result, so the category
    # and the batch issue_counts stay unchanged. Only set when non-empty.
    _suggestions = _dressing_suggestions(result)
    if _suggestions:
        result['suggestions'] = _suggestions

    _fps = history.pop_fingerprints(result)
    if not no_history:
        history.write(result, VERSION, elapsed_ms, _fps)
    return result, category


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
        print(json.dumps({'error': "engines: subcommand must be 'list' or 'check'"}))
        return 1
    try:
        import engines as _engines
    except Exception as e:  # noqa: BLE001
        print(json.dumps({'error': 'engines module unavailable: %s' % e}))
        return 1
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
        print(json.dumps({'error': "ftetwild: subcommand must be "
                                   "'status', 'install' or 'uninstall'"}))
        return 1
    try:
        import ftetwild_manager as mgr
    except Exception as e:  # noqa: BLE001
        print(json.dumps({'error': 'ftetwild manager unavailable: %s' % e}))
        return 1

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
            print(json.dumps({'error': 'install cancelled (use --yes to confirm)'}))
            return 1
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
        print(json.dumps({'error': 'uninstall cancelled (use --yes to confirm)'}))
        return 1
    res = mgr.uninstall(progress_cb=lambda s: print(s, file=sys.stderr))
    print(json.dumps({'result': res}, ensure_ascii=False))
    return 0 if res.get('ok') else 1


def main():
    import argparse
    parser = argparse.ArgumentParser(
        prog='sutura',
        description='Sutura Triage Engine: staged robust mesh repair for STL, OBJ, and 3MF files.',
        epilog=(
            'subcommands (first argument):\n'
            '  validate FILE          read-only mesh validation (no repair, no output file)\n'
            '  clear-cache            clear content-addressed Chart cache (~/.cache/sutura/)\n'
            '  clear-learning         reset learning-triage history (triage_learning.json)\n'
            '  export-history         view or export anonymous technical usage history\n'
            '  engines list|check     inspect configured third-party repair engines\n'
            '  ftetwild status|install|uninstall  manage optional fTetWild fallback extra\n'
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('files', nargs='*', metavar='FILE',
                        help='input mesh file(s)')
    parser.add_argument('-o', '--output', metavar='OUTPUT',
                        help='output file (only valid with a single input)')
    parser.add_argument('--human', action='store_true',
                        help='print a human-readable report')
    parser.add_argument('--defects', action='store_true',
                        help='with --human, also list input defects (holes / '
                             'non-manifold regions). JSON always includes defects.')
    parser.add_argument('--diff', action='store_true',
                        help='with --human, also show before/after geometry '
                             'diff (vertices, faces, surface area change). '
                             'JSON always includes these fields.')
    parser.add_argument('--mode', choices=REPAIR_MODES, default='auto',
                        help='repair mode: low/medium/aggressive/extreme use '
                             'fixed Stage 1 thresholds; auto (default) uses '
                             'the mesh classifier + confidence gate.')
    parser.add_argument('--classifier-engine', choices=CLASSIFIER_ENGINES,
                        default=None,
                        help='mesh classifier engine: experimental (default) '
                             'is the RANSAC + trained-head engine; classic '
                             'selects the original heuristic. Also selectable '
                             'via SUTURA_CLASSIFIER_ENGINE.')
    parser.add_argument('--profile', choices=REPAIR_PROFILES, default=None,
                        help='named Stage 1 threshold preset: mechanical, '
                             'organic, scan, miniature or fast. Only effective '
                             'with mode auto; an explicit fixed mode wins.')
    parser.add_argument('--intensity', default=None, metavar='NAME',
                        help='Triage Engine intensity for the effort after '
                             'Stage 1: a built-in preset (quick, balanced '
                             '(default; byte-identical to the historical '
                             'defaults), thorough, extreme) or a named user '
                             'profile from ~/.config/sutura/profiles.json. '
                             'Stage 1 is NOT affected (use --mode/--profile). '
                             'Also from SUTURA_INTENSITY or the "intensity" '
                             'key of ~/.config/sutura/config.json. See '
                             '--list-intensities.')
    parser.add_argument('--list-intensities', action='store_true',
                        help='list the built-in intensity presets and the '
                             'user profiles with their effective values, then '
                             'exit (no repair)')
    parser.add_argument('--list-methods', action='store_true',
                        help='list the repair methods (num, id, family, '
                             'availability) and exit (no repair)')
    parser.add_argument('--analyze', action='store_true',
                        help='analyze the input object(s) -- defects, mesh '
                             'type, self-intersection load -- and print the '
                             'ranked method recommendations plus the external '
                             'engines. Writes no output file.')
    parser.add_argument('--methods', default=None, metavar='LIST',
                        help='tag the repair methods to try, in order, e.g. '
                             '--methods 2,3,5. Without it the repair is auto: '
                             "today's default pipeline first, with ranked "
                             'fallbacks only when it fails. BREAKING (0.7.0): '
                             "in 0.6.0 '13'/'graft' meant the manual "
                             "picked-points method; that is now #12 "
                             "'transplant_plus', and '13'/'graft' is the new "
                             'Graft (shell wrap).')
    parser.add_argument('--engines', default=None, metavar='LIST',
                        help='tag the configured external engines to run, e.g. '
                             '--engines meshfix,my-engine. When given, only '
                             'those enabled engines run (at their configured '
                             'placement); without it every enabled engine runs '
                             'as configured. Engines are never mixed into the '
                             'method ranking (see `engines list`).')
    parser.add_argument('--repeat-source', default=None, metavar='X,Y,Z',
                        help='method 12 (repeat_manual): the 3D point on the '
                             'healthy repeated element to copy from.')
    parser.add_argument('--wall-min-thickness', type=float, default=None,
                        metavar='T',
                        help='target minimum wall thickness for the opt-in '
                             'Wall Thicken method (#15); default 1%% of the '
                             'bounding-box diagonal. Ignored unless #15 is '
                             'tagged via --methods.')
    parser.add_argument('--repeat-target', default=None, metavar='X,Y,Z',
                        help='method 12 (repeat_manual): the 3D point on the '
                             'damaged repeated element to replace.')
    parser.add_argument('--json', action='store_true',
                        help='force machine-readable JSON output (the default '
                             'format; overrides --human)')
    parser.add_argument('--no-learning-triage', action='store_true',
                        help='disable the bounded local-history ranking bonus '
                             '(learning triage) for this run')
    parser.add_argument('--no-history', action='store_true',
                        help='do not write the anonymous usage history record '
                             '(mesh geometry + repair results only, never file '
                             'names or paths)')
    parser.add_argument('--no-cache', action='store_true',
                        help='disable content-addressed Sutura Chart cache (~/.cache/sutura/)')
    parser.add_argument('--max-geometry-change', type=float, default=None,
                        metavar='PCT',
                        help='repair budget: warn/block when the actual geometry '
                             'change (max of |volume| and |surface area| change '
                             '%%) exceeds PCT. On a TTY you are prompted to '
                             'confirm; non-interactive runs decline the save '
                             'unless --force is given (0 disables the budget)')
    parser.add_argument('--max-risk', type=float, default=None, metavar='SCORE',
                        help='repair budget: warn/block when the repair risk '
                             'score (0-100) exceeds SCORE. Same confirmation '
                             'behaviour as --max-geometry-change (0 disables)')
    parser.add_argument('--force', action='store_true',
                        help='with --max-geometry-change/--max-risk: save the '
                             'output even when the budget is exceeded, without '
                             'asking')
    parser.add_argument('--last', type=int, default=0, metavar='N',
                        help='export-history: only the last N records')
    parser.add_argument('--clear', action='store_true',
                        help='export-history: clear the history file')
    parser.add_argument('--summary-only', action='store_true',
                        help='export-history: print only the summary')
    parser.add_argument('--dry-run', action='store_true',
                        help='do not repair: report what would be done and '
                             'write no output file')
    parser.add_argument('--yes', action='store_true',
                        help='ftetwild install|uninstall: confirm without an '
                             'interactive prompt')
    parser.add_argument('--experimental-join-components', action='store_true',
                        help='experimental prototype: move small connected '
                             'components onto the nearest larger component '
                             'instead of deleting them (changes geometry; '
                             'NOT the default behaviour, evaluation only)')
    parser.add_argument('--experimental-autorefine', action='store_true',
                        help='experimental prototype: resolve self-'
                             'intersections by subdividing the intersecting '
                             'triangles along their intersection segments '
                             '(Lazard & Valque 2025) instead of deleting '
                             'faces. NEVER deletes input faces; on moderate-'
                             'SI meshes it reduces SI, on dense-SI scans it '
                             'is limited by float64 construction (see '
                             'docs/alpha-wrap-feasibility-2026-09.md). NOT '
                             'the default behaviour, evaluation only')
    parser.add_argument('--no-fallback-ftetwild', action='store_true',
                        help='disable the fTetWild fallback tier. By default, '
                             'when the optional fTetWild extra is installed '
                             '(SUTURA_WITH_FTETWILD=1) and the stage-1 chain '
                             'still leaves holes or non-manifold edges, the '
                             'ORIGINAL input is tetrahedralized and its '
                             'boundary extracted as a watertight, SI-free '
                             'surface (fTetWild via pytetwild, MPL-2.0); it is '
                             'adopted only when no worse on holes+non-manifold '
                             'than the stage-1 result. Without the extra this '
                             'flag changes nothing')
    parser.add_argument('--experimental-fallback-ftetwild', action='store_true',
                        help='run the fTetWild fallback tier also when the '
                             'stage-1 result is closed but still '
                             'self-intersects (slow: up to %d s per mesh; '
                             'remeshes such meshes), and report an explicit '
                             'skip when fTetWild is not installed' % FTETWILD_TIMEOUT)
    parser.add_argument('--ftetwild-optimize', action='store_true',
                        help='let fTetWild also optimise the quality of its '
                             'tetrahedra (off by default: only the boundary '
                             'surface is used, and the optimisation mostly '
                             'lengthens repairs of complex parts). Default from '
                             '%s or the "ftetwild_optimize" key of '
                             '~/.config/sutura/config.json' % FTETWILD_OPTIMIZE_ENV)
    parser.add_argument('--no-graft', action='store_true',
                        help='disable the Graft (shell wrap) tier (#13). By '
                             'default the deep-repair ladder tries Graft '
                             'before fTetWild on the original input and adopts '
                             'its reload-watertight result; when Graft reports '
                             'fidelity_ok=false, fTetWild is also tried and '
                             'the lower healthy-deviation result wins')
    parser.add_argument('--experimental-graft', action='store_true',
                        help='force the Graft (shell wrap) tier (#13) even when '
                             'the deep-repair ladder would not run it (e.g. '
                             'with --deep-repair off). Primarily for its own '
                             'evaluation; the auto path (default) already '
                             'prefers Graft over fTetWild')
    parser.add_argument('--flap', action='store_true',
                        help='run the Flap surface-based hole filler as the '
                             'FIRST Stage-1 step (a pre-pass on the input, '
                             'before the VCG chain): every open boundary loop '
                             'is covered with a minimum-area triangulation '
                             'refined to the surrounding edge length and faired '
                             'with a thin-plate solve that continues the '
                             'neighbouring surface. Original triangles are kept '
                             'verbatim and the result is adopted only when the '
                             'reload-honest holes + non-manifold count does not '
                             'worsen; repair_file re-runs with Flap OFF if the '
                             'final mesh still ends up worse. On by default; '
                             'env: SUTURA_FLAP / SUTURA_FLAP_DEFAULT')
    parser.add_argument('--no-flap', action='store_true',
                        help='disable the Flap hole filler (it is on by default)')
    parser.add_argument('--no-dressing', action='store_true',
                        help='disable the Dressing (viscosity coat) tier (#16); '
                             'this is the default (Dressing is opt-in)')
    parser.add_argument('--experimental-dressing', action='store_true',
                        help='enable the Dressing (viscosity coat) tier (#16): '
                             'extract a variable-thickness isosurface of the '
                             'generalized-winding signed field (2-manifold and '
                             'self-intersection-free by construction). Opt-in '
                             'until measured on the corpus; tried after Graft '
                             'and before fTetWild')
    parser.add_argument('--dressing-drain', default=None,
                        help='Dressing (#16) healthy-region erode-back: a mode '
                             '(none, half, full, deep), an explicit delta_r in '
                             'mm, or unset for the intensity preset default '
                             '(Quick off; Balanced/Thorough full; Extreme deep). '
                             'Applied in the level set (F = s - r + delta_r), '
                             'never by vertex projection')
    parser.add_argument('--dressing-defects', choices=DRESSING_DEFECT_SETS,
                        default=None,
                        help='Dressing (#16) viscosity-mask defect set: "all" '
                             '(default) = holes + non-manifold + '
                             'self-intersections, "holes_nm" = holes + '
                             'non-manifold only (self-intersections are '
                             'ignored). Env: SUTURA_DRESSING_DEFECTS')
    parser.add_argument('--dressing-rmax-scale', type=float, default=None,
                        metavar='F',
                        help='Dressing (#16) multiplier on the resolved r_max '
                             '(bridging radius); default 1.0. A value < 1 gives '
                             'a narrower coat. Env: SUTURA_DRESSING_RMAX_SCALE')
    parser.add_argument('--dressing-sigma-scale', type=float, default=None,
                        metavar='F',
                        help='Dressing (#16) multiplier on the resolved sigma '
                             '(defect-influence width); default 1.0. Env: '
                             'SUTURA_DRESSING_SIGMA_SCALE')
    parser.add_argument('--dressing-force-adopt', action='store_true',
                        help='Dressing (#16) skip the shape-preservation '
                             'gates (volume / component count / healthy-surface '
                             'normal angle / coat fidelity) and adopt a '
                             'gate-rejected coat that is still strictly '
                             'watertight and self-intersection-free. The shape '
                             'may deform; offered by the non-watertight '
                             'suggestion. Implies --experimental-dressing')
    parser.add_argument('--deep-repair', choices=DEEP_REPAIR_MODES, default=None,
                        help='deep-repair ladder after the fast repair, when '
                             'holes or non-manifold edges remain: "full" '
                             '(default) runs the fTetWild tier as before, '
                             '"local" re-meshes only the damaged regions '
                             '(experimental prototype, the rest of the '
                             'surface is kept unchanged), "off" runs no tier '
                             'and only reports what is available with a '
                             'rough time estimate. Default from %s or the '
                             '"deep_repair" key of ~/.config/sutura/config.json'
                             % DEEP_REPAIR_ENV)
    parser.add_argument('--si-mode', choices=SI_MODES, default=None,
                        help='self-intersection policy: "report" (default) '
                             'measures and reports self-intersections but '
                             'never escalates the deep-repair ladder or fails '
                             'a result on self-intersections alone (holes and '
                             'non-manifold edges always count); "repair" treats '
                             'a residual self-intersection count as damage and '
                             'runs the automatic Dressing fallback; "off" '
                             'skips the exact self-intersection classifier and '
                             'reports it as not measured. Explicit '
                             '--experimental-fallback-ftetwild / '
                             '--experimental-dressing still act on '
                             'self-intersections. Env: %s' % SI_MODE_ENV)
    parser.add_argument('--experimental-indirect-autorefine', action='store_true',
                        help='experimental prototype: exact arrangement-lite '
                             'self-intersection split via the rust/sutura-geom '
                             'extension (indirect predicates: broad phase + '
                             'exact triangle-triangle classifier + per-triangle '
                             '2D CDT + exact-rational welding). Resolves '
                             'proper intersections by subdivision instead of '
                             'face deletion. Adopted only when no worse on '
                             'holes+non-manifold than the stage-1 result '
                             '(same guard as --experimental-autorefine). NOT '
                             'the default behaviour, evaluation only')
    parser.add_argument('--experimental-edge-tiebreak', action='store_true',
                        help='experimental opt-in: use the 11-feature '
                             'classifier head (base features + the five strong '
                             'FAZ10 scan signals). NOT the default; the gain is '
                             'marginal (1 mesh on the 71-mesh labeled set).')
    parser.add_argument('--version', action='version', version='%(prog)s ' + VERSION)
    args = parser.parse_args()

    # --list-intensities: print the presets and user profiles, then exit.
    if args.list_intensities:
        print(triage.format_intensities())
        sys.exit(0)

    # --list-methods: print the method registry, then exit (no repair).
    if args.list_methods:
        if args.json:
            print(json.dumps(method_registry.methods_json(), ensure_ascii=False))
        else:
            print(method_registry.format_methods())
        sys.exit(0)

    # --intensity accepts a built-in preset OR a user profile; argparse cannot
    # know the profiles at parser-construction time, so validate here.
    _profiles = triage.load_profiles()
    if (args.intensity is not None and args.intensity not in triage.PRESETS
            and args.intensity not in _profiles):
        _valid = list(triage.INTENSITIES) + sorted(_profiles)
        parser.error("argument --intensity: invalid choice: %r (choose from %s)"
                     % (args.intensity, ', '.join(map(repr, _valid))))

    files = args.files
    out = args.output
    human = args.human and not args.json
    show_defects = args.defects
    show_diff = args.diff
    mode = args.mode
    dry_run = args.dry_run
    engine = resolve_classifier_engine(args.classifier_engine)
    si_mode = resolve_si_mode(args.si_mode)
    triage_spec = triage.resolve_intensity(args.intensity,
                                           user_profiles=_profiles)

    # --methods: tag the repair methods (validated against the registry).
    methods_sel = None
    if args.methods:
        parts = [p.strip() for p in args.methods.split(',') if p.strip()]
        if not parts:
            parser.error('argument --methods: empty list')
        nums = []
        for part in parts:
            m = method_registry.get_method(part)
            if m is None:
                parser.error('argument --methods: unknown method %r '
                             '(see --list-methods)' % part)
            nums.append(m.num)
        methods_sel = nums

    # --repeat-source / --repeat-target: the 3D points method 12 transplants
    # between. Validated as "X,Y,Z" and required together when 12 is tagged.
    def _parse_point(text, flag):
        parts = [p.strip() for p in text.split(',')]
        if len(parts) != 3:
            parser.error('argument %s: expected X,Y,Z' % flag)
        try:
            return tuple(float(p) for p in parts)
        except ValueError:
            parser.error('argument %s: coordinates must be numbers' % flag)

    repeat_source = (_parse_point(args.repeat_source, '--repeat-source')
                     if args.repeat_source else None)
    repeat_target = (_parse_point(args.repeat_target, '--repeat-target')
                     if args.repeat_target else None)
    if methods_sel is not None and 12 in methods_sel:
        if repeat_source is None or repeat_target is None:
            parser.error('--methods 12 requires --repeat-source X,Y,Z and '
                         '--repeat-target X,Y,Z')

    # --engines: tag specific external engines (names validated against the
    # configured engines below, after they are loaded).
    engines_sel = None
    if args.engines:
        parts = [p.strip() for p in args.engines.split(',') if p.strip()]
        if not parts:
            parser.error('argument --engines: empty list')
        engines_sel = parts

    # 'engines list|check' and 'ftetwild status|install|uninstall' are
    # subcommands, dispatched before any repair (like validate/export-history).
    if files and files[0] == 'engines':
        if out is not None or dry_run:
            print(json.dumps({'error': 'engines takes no -o/--dry-run'}))
            sys.exit(1)
        sys.exit(run_engines_cli(files[1:], human=human))
    if files and files[0] == 'ftetwild':
        if out is not None:
            print(json.dumps({'error': 'ftetwild takes no -o'}))
            sys.exit(1)
        sys.exit(run_ftetwild_cli(files[1:], dry_run=dry_run, yes=args.yes,
                                  human=human))

    # Repair budgets: validate the ranges, then 0 / unset both mean "no
    # budget" (disabled), matching the GUI's 0 = no limit convention.
    if args.max_geometry_change is not None and args.max_geometry_change < 0:
        print(json.dumps({'error': '--max-geometry-change must be >= 0'}))
        sys.exit(1)
    if args.max_risk is not None and not (0 <= args.max_risk <= 100):
        print(json.dumps({'error': '--max-risk must be between 0 and 100'}))
        sys.exit(1)
    max_geom_change = args.max_geometry_change or None
    max_risk = args.max_risk or None

    if len(files) > 1 and out is not None:
        print(json.dumps({'error': '-o cannot be used with multiple input files'}))
        sys.exit(1)

    if getattr(args, 'no_learning_triage', False):
        os.environ[history.LEARNING_ENV] = '0'

    if getattr(args, 'no_cache', False):
        try:
            from sutura_engine import chart as cache
            cache.set_cache_enabled(False)
        except Exception:
            pass

    # 'clear-cache' as the first positional argument clears ~/.cache/sutura/
    if files and files[0] == 'clear-cache':
        try:
            from sutura_engine import chart as cache
            freed = cache.clear_cache()
            if human:
                print('Cache cleared: %s freed' % cache._human_bytes(freed))
            else:
                print(json.dumps({'cleared': True, 'freed_bytes': freed, 'freed_human': cache._human_bytes(freed)}))
        except Exception as e:
            print(json.dumps({'error': 'clear-cache failed: %s' % e}))
            sys.exit(1)
        sys.exit(0)

    # 'clear-learning' as the first positional argument resets the local
    # learning-triage counts (the bounded ranking bonus) without touching the
    # usage history.
    if files and files[0] == 'clear-learning':
        if len(files) > 1 or out is not None or dry_run or human:
            print(json.dumps({'error': 'clear-learning takes no positional '
                                       'arguments'}))
            sys.exit(1)
        removed = history.clear_learning()
        print(json.dumps({'cleared': removed}))
        sys.exit(0)

    # 'export-history' as the first positional argument prints the anonymous
    # usage history (summary + full JSON array) instead of repairing.
    if files and files[0] == 'export-history':
        if len(files) > 1 or out is not None or human or dry_run:
            print(json.dumps({'error': 'export-history takes no positional '
                                       'arguments (flags: --last N, --clear, '
                                       '--summary-only)'}))
            sys.exit(1)
        history.export_history(last=args.last,
                               summary_only=args.summary_only,
                               clear=args.clear)
        sys.exit(0)

    # 'validate' as the first positional argument switches to validate-only
    # mode: analyze the mesh(es) without repairing or writing anything.
    if files and files[0] == 'validate':
        targets = files[1:]
        if not targets:
            print(json.dumps({'error': 'validate requires at least one input file'}))
            sys.exit(1)
        if out is not None:
            print(json.dumps({'error': '-o is not valid with validate'}))
            sys.exit(1)
        if dry_run:
            print(json.dumps({'error': '--dry-run is not valid with validate'}))
            sys.exit(1)
        if methods_sel is not None:
            print(json.dumps({'error': '--methods is not valid with validate'}))
            sys.exit(1)
        if engines_sel is not None:
            print(json.dumps({'error': '--engines is not valid with validate'}))
            sys.exit(1)
        results = [validate_file(f, engine, si_mode=si_mode) for f in targets]
        nerr = sum(1 for r in results if 'error' in r)
        if len(targets) == 1:
            result = results[0]
            if human:
                print(human_validate(result, show_defects=show_defects))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(human_validate(result, show_defects=show_defects))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    # --analyze: read-only per-object analysis + ranked method recommendations.
    if args.analyze:
        if out is not None:
            print(json.dumps({'error': '-o is not valid with --analyze'}))
            sys.exit(1)
        if dry_run:
            print(json.dumps({'error': '--analyze is not valid with --dry-run'}))
            sys.exit(1)
        if methods_sel is not None:
            print(json.dumps({'error': '--methods is not valid with --analyze'}))
            sys.exit(1)
        if engines_sel is not None:
            print(json.dumps({'error': '--engines is not valid with --analyze'}))
            sys.exit(1)
        if not files:
            print(json.dumps({'error': '--analyze requires at least one input file'}))
            sys.exit(1)
        results = [method_registry.analyze_report(
            f, engine, args.experimental_edge_tiebreak) for f in files]
        nerr = sum(1 for r in results if 'error' in r)
        if len(files) == 1:
            result = results[0]
            if human:
                print(method_registry.format_analyze_human(result))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(method_registry.format_analyze_human(result))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    # --dry-run: analyze and report the plan, write no output file at all.
    if dry_run:
        if out is not None:
            print(json.dumps({'error': '-o is not valid with --dry-run'}))
            sys.exit(1)
        if methods_sel is not None:
            print(json.dumps({'error': '--methods is not valid with --dry-run'}))
            sys.exit(1)
        if engines_sel is not None:
            print(json.dumps({'error': '--engines is not valid with --dry-run'}))
            sys.exit(1)
        results = [dry_run_file(f, mode=mode, profile=args.profile, engine=engine,
                                triage_spec=triage_spec, si_mode=si_mode,
                                flap=resolve_flap(no_flap=args.no_flap,
                                                  force=args.flap))
                   for f in files]
        nerr = sum(1 for r in results if 'error' in r)
        if len(files) == 1:
            result = results[0]
            if human:
                print(human_dry_run(result))
            else:
                print(json.dumps(result, ensure_ascii=False))
            sys.exit(0 if nerr == 0 else 1)
        if human:
            for result in results:
                print(human_dry_run(result))
                print()
        else:
            print(json.dumps({'files': results}, ensure_ascii=False))
        sys.exit(0 if nerr == 0 else 1)

    if args.ftetwild_optimize:
        # read by ftetwild_params() inside the fTetWild tier
        os.environ[FTETWILD_OPTIMIZE_ENV] = '1'
    # An explicit deep-repair / fTetWild flag always wins over the intensity
    # preset; otherwise the preset supplies the deep-repair tier defaults.
    if (args.deep_repair is not None or args.no_fallback_ftetwild
            or args.experimental_fallback_ftetwild):
        _dr_mode, _ft_arg = resolve_deep_repair_flags(
            args.deep_repair, args.no_fallback_ftetwild,
            args.experimental_fallback_ftetwild)
    else:
        _dr_mode = triage_spec.deep_repair
        _ft_arg = 'auto' if triage_spec.ftetwild_enabled else False
    # Graft (#13): CLI flag > env > 'auto' (tried by the deep-repair ladder
    # before fTetWild). --experimental-graft forces it outside the ladder.
    _graft_arg = resolve_graft(no_graft=args.no_graft,
                               force=args.experimental_graft)
    # Flap: on by default; --no-flap disables it, --flap forces it, SUTURA_FLAP
    # may set 0/1 and SUTURA_FLAP_DEFAULT overrides the built-in default.
    _flap_arg = resolve_flap(no_flap=args.no_flap, force=args.flap)
    # Dressing (#16): the single default switch (DRESSING_DEFAULT_ENABLED /
    # SUTURA_DRESSING_DEFAULT) decides; --no-dressing disables and
    # --experimental-dressing forces.
    _dressing_arg = resolve_dressing(no_dressing=args.no_dressing,
                                     force=args.experimental_dressing)
    # --dressing-force-adopt can only act on a coat, so it implies enabling
    # Dressing even when --no-dressing / the default switch would disable it.
    if args.dressing_force_adopt:
        _dressing_arg = True
    _dressing_drain = resolve_dressing_drain(args.dressing_drain)
    _dressing_defects = resolve_dressing_defects(args.dressing_defects)
    _dressing_rmax_scale = resolve_dressing_rmax_scale(args.dressing_rmax_scale)
    _dressing_sigma_scale = resolve_dressing_sigma_scale(
        args.dressing_sigma_scale)
    # External engines are loaded ONCE for the whole batch; broken configs
    # only warn and never fail a repair.
    _engines, _engine_chain, _engine_warnings = load_engine_run()
    if engines_sel is not None:
        _unknown = [n for n in engines_sel if n not in _engines]
        if _unknown:
            parser.error('argument --engines: unknown engine(s): %s '
                         '(see `sutura engines list`)' % ', '.join(_unknown))
    results = [process_file(f, human, mode=mode, profile=args.profile,
                            no_history=args.no_history, engine=engine,
                            out=out, max_geom_change=max_geom_change,
                            max_risk=max_risk, force=args.force,
                            join_components=args.experimental_join_components,
                            autorefine=args.experimental_autorefine,
                            ftetwild=_ft_arg, deep_repair=_dr_mode,
                            graft=_graft_arg, flap=_flap_arg,
                            dressing=_dressing_arg,
                            dressing_drain=_dressing_drain,
                            dressing_defects=_dressing_defects,
                            dressing_rmax_scale=_dressing_rmax_scale,
                            dressing_sigma_scale=_dressing_sigma_scale,
                            si_mode=si_mode,
                            dressing_force_adopt=args.dressing_force_adopt,
                            indirect_autorefine=args.experimental_indirect_autorefine,
                            extra_features=args.experimental_edge_tiebreak,
                            triage_spec=triage_spec, engines=_engines,
                            engine_chain=_engine_chain,
                            engine_warnings=_engine_warnings,
                            methods=methods_sel,
                            engine_filter=engines_sel,
                            repeat_source=repeat_source,
                            repeat_target=repeat_target,
                            wall_min_thickness=args.wall_min_thickness) for f in files]
    ok = sum(1 for _, c in results if c == 'watertight')
    warnings = sum(1 for _, c in results if c == 'warning')
    errors = sum(1 for _, c in results if c == 'error')

    # issue counts across the whole batch (e.g. volume_warning: 3)
    issue_counts = {}
    for r, _ in results:
        for code in r.get('issues', []):
            issue_counts[code] = issue_counts.get(code, 0) + 1

    if len(files) == 1:
        result, category = results[0]
        if human:
            print(human_report(result, show_defects=show_defects, show_diff=show_diff))
        else:
            print(json.dumps(result, ensure_ascii=False))
        sys.exit(0 if category != 'error' else 1)

    if human:
        for result, category in results:
            print(human_report(result, show_defects=show_defects, show_diff=show_diff))
            print()
        print('Summary: %d file(s), %d watertight, %d with warnings, %d failed.'
              % (len(files), ok, warnings, errors))
        for code, n in issue_counts.items():
            print('  - %s: %d dosya' % (issue_label(code), n))
    else:
        payload = {
            'files': [r for r, _ in results],
            'summary': {
                'total': len(files), 'ok': ok, 'warning': warnings, 'error': errors,
                'issue_counts': issue_counts,
            },
        }
        print(json.dumps(payload, ensure_ascii=False))
    sys.exit(0 if errors == 0 else 1)


if __name__ == '__main__':
    main()