#!/usr/bin/env python3
"""Sutura - two-stage STL/3MF mesh repair engine.

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
from collections import defaultdict
import numpy as np

from classification import classify, issue_label
from confidence import (estimate_confidence_pre_repair, repair_confidence)
from defects import detect as detect_defects
from mesh_classifier import classify_mesh
import repair_score
import history
import triage

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

VERSION = "0.4.2"


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
        ('meshing_close_holes', {'maxholesize': maxholesize}),
        # --experimental-join-components replaces this removal step with a
        # join (small components are moved onto the nearest larger component
        # instead of being deleted) -- prototype, flag-gated, see
        # join_small_components.
        ('meshing_remove_connected_component_by_face_number',
         {'mincomponentsize': mincomponentsize, 'removeunref': True}),
        ('meshing_remove_unreferenced_vertices', {}),
        ('meshing_re_orient_faces_coherently', {}),
        ('meshing_close_holes', {'maxholesize': maxholesize}),
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
        ('meshing_close_holes', {'maxholesize': maxholesize}),
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


def repair_mesh_from_arrays(verts, tris, tmpdir, mode='auto', profile=None,
                            engine='experimental', declared_unit=None,
                            join_components=False, autorefine=False,
                            ftetwild=False, indirect_autorefine=False,
                            extra_features=False, deep_repair=None,
                            triage_spec=None):
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
    ``triage_spec`` is the resolved intensity preset (sutura/triage.py) that
    supplies the post-Stage-1 knobs (deep-repair tier timeout/size cap,
    decimation ladder, Hausdorff sample count); None resolves to Balanced
    (the pre-triage behaviour). It never changes Stage 1.
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
    stats['triage_intensity'] = triage_spec.name

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
    ms, after = deep_repair_ladder(ml, ms, after, stats, v, t, tmpdir,
                                   mode=deep_repair, ftetwild=ftetwild,
                                   spec=triage_spec)

    # A closed result with no non-manifold edge can still have pinched
    # ("bowtie") vertices, e.g. when the final close_holes fan-fills a hole at
    # a vertex shared by two boundary loops; stage 2 then never runs and a
    # strictly watertight mesh is reported as a warning (thingi10k_145065).
    ms, after, stats['pinched_vertices_split'] = _split_pinched_vertices(ml, ms, after)

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


def deep_repair_ladder(ml, ms, after, stats, v, t, tmpdir, mode=None,
                       ftetwild=False, spec=None):
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
        ms, after = _ftetwild_tier(ml, ms, after, stats, v, t, tmpdir, ftetwild,
                                   spec=spec)
        if (stats.get('experimental_ftetwild') or {}).get('ran'):
            tiers_run.append('ftetwild')
    else:
        stats['experimental_ftetwild'] = False
    if mode is not None:
        stats['deep_repair'] = {
            'mode': mode,
            'holes_before': int(cur_holes),
            'nm_before': cur_nm,
            'tiers_run': tiers_run,
            'local': local_rep,
            'ftetwild': stats.get('experimental_ftetwild') or None,
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


def repair_file(src, out, tmpdir, mode='auto', profile=None, engine='experimental',
                join_components=False, autorefine=False, ftetwild=False,
                indirect_autorefine=False, extra_features=False, deep_repair=None,
                triage_spec=None):
    """Repair a single STL/OBJ/3MF file. Returns the report dict."""
    import pymeshlab as ml

    bad_coords = scan_bad_coordinates(src)
    if bad_coords:
        raise ValueError('input mesh contains %s' % bad_coords)

    load_ms = ml.MeshSet()
    load_ms.load_new_mesh(src)
    verts = np.asarray(load_ms.current_mesh().vertex_matrix(), dtype=np.float32)
    tris = np.asarray(load_ms.current_mesh().face_matrix(), dtype=np.int32)

    declared_unit = None
    if os.path.splitext(src)[1].lower() == '.3mf':
        units = read_3mf_units(src)
        if units:
            declared_unit = next(iter(units.values()))

    report, new_v, new_t = repair_mesh_from_arrays(
        verts, tris, tmpdir, mode=mode, profile=profile, engine=engine,
        declared_unit=declared_unit, join_components=join_components,
        autorefine=autorefine, ftetwild=ftetwild,
        indirect_autorefine=indirect_autorefine,
        extra_features=extra_features, deep_repair=deep_repair,
        triage_spec=triage_spec)
    report['_fp'] = history.mesh_fingerprint(verts, tris)
    report['defects'] = detect_defects(verts, tris)
    if os.path.splitext(src)[1].lower() == '.obj':
        report['material_discarded'] = obj_has_material_refs(src)

    # Stage 2 applies to watertight results; shared helper keeps the single-
    # mesh path and per-object 3MF repair on the same code (same gate, same
    # OBJ round-trip, same explicit skip reporting).
    new_v, new_t = maybe_run_stage2(report, new_v, new_t, tmpdir)

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
               triage_spec=None):
    """Repair every object mesh in a 3MF archive, preserving structure.

    Per-object Stage 2: any object that stage 1 closes (two-manifold with no
    remaining holes) is passed through the shared ``maybe_run_stage2`` helper,
    so a closed object gets a manifold3d watertight rebuild exactly like a
    single-mesh file (the helper sets the per-object ``stage2`` report and
    returns the rebuilt arrays). Objects that remain open after stage 1 are
    written as stage-1 output, mirroring the single-mesh behaviour.
    """
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
                    triage_spec=triage_spec)
                rep['defects'] = detect_defects(verts, tris)
                rep['_fp'] = history.mesh_fingerprint(verts, tris)
                # Per-object stage 2 first: closed objects get a watertight
                # rebuild (same helper/gate as the single-mesh path), and the
                # confidence/health/risk below then see the stage2 outcome.
                new_v, new_t = maybe_run_stage2(rep, new_v, new_t, tmpdir)
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


def validate_mesh_from_arrays(verts, tris, engine='experimental', declared_unit=None):
    """Analyze one mesh WITHOUT repairing it.

    Combines defects.detect(), the mesh classifier and cheap pymeshlab
    measures (self-intersecting faces, connected components) into a
    read-only report. Never writes anything. ``declared_unit`` feeds the
    non-blocking unit-warning heuristic (3MF only)."""
    import pymeshlab as ml
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
        'watertight': bool(len(holes) == 0 and len(nm) == 0 and self_intersections == 0),
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


def validate_file(src, engine='experimental'):
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
            rep = validate_mesh_from_arrays(vs, ts, engine, declared_unit=declared)
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
                             declared_unit=None, triage_spec=None):
    """Report what a repair WOULD do for one mesh, without doing it.

    Detects the type, resolves the mode/thresholds and counts the holes /
    debris / self-intersections. The debris count reuses the same
    meshing_remove_connected_component_by_face_number filter the repair
    chain runs, applied to an in-memory scratch mesh - nothing is written.
    ``declared_unit`` feeds the non-blocking unit-warning heuristic
    (3MF only). ``triage_spec`` is the resolved intensity preset (Balanced
    when None); Stage 1 thresholds are unaffected by it."""
    import pymeshlab as ml
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
        'triage_intensity': (
            triage_spec.name if triage_spec is not None
            else triage.DEFAULT_INTENSITY),
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
    }
    _u = check_units(v, declared_unit)
    if _u:
        result['unit_warning'] = True
        result['unit_hint'] = _u['unit_hint']
    return result


def dry_run_file(src, mode='auto', profile=None, engine='experimental',
                 triage_spec=None):
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
                                           triage_spec=triage_spec)
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
    return '\n'.join(lines)


def process_file(src, human, mode='auto', profile=None, no_history=False,
                 out=None, engine='experimental', max_geom_change=None,
                 max_risk=None, force=False, join_components=False,
                 autorefine=False, ftetwild=False, indirect_autorefine=False,
                 extra_features=False, deep_repair=None, triage_spec=None):
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
    """
    if not os.path.exists(src):
        return ({'input': src, 'error': 'file not found: %s' % src}, 'error')

    stem, ext = os.path.splitext(src)
    if out is None:
        out = stem + '_fixed' + ext

    tmpdir = tempfile.mkdtemp(prefix='sutura-')
    tmp_out = os.path.join(tmpdir, 'out' + ext)
    result = {'input': src, 'output': out}
    t0 = time.perf_counter()
    try:
        if ext.lower() == '.3mf' and len(parse_3mf_meshes(src)) > 1:
            result.update(repair_3mf(src, tmp_out, tmpdir, mode=mode,
                                     profile=profile, engine=engine,
                                     join_components=join_components,
                                     autorefine=autorefine,
                                     ftetwild=ftetwild,
                                     indirect_autorefine=indirect_autorefine,
                                     extra_features=extra_features,
                                     deep_repair=deep_repair,
                                     triage_spec=triage_spec))
        else:
            result.update(repair_file(src, tmp_out, tmpdir, mode=mode,
                                      profile=profile, engine=engine,
                                      join_components=join_components,
                                      autorefine=autorefine,
                                      ftetwild=ftetwild,
                                      indirect_autorefine=indirect_autorefine,
                                      extra_features=extra_features,
                                      deep_repair=deep_repair,
                                      triage_spec=triage_spec))

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

    _fps = history.pop_fingerprints(result)
    if not no_history:
        history.write(result, VERSION, elapsed_ms, _fps)
    return result, category


def main():
    import argparse
    parser = argparse.ArgumentParser(
        prog='sutura',
        description='Repair one or more STL/OBJ/3MF meshes. Output files get a "_fixed" suffix.')
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
    parser.add_argument('--intensity', choices=triage.INTENSITIES, default=None,
                        help='Triage Engine intensity preset for the effort '
                             'after Stage 1: quick (skip the deep-repair and '
                             'fTetWild tiers), balanced (default; byte-'
                             'identical to the historical defaults), thorough, '
                             'extreme. Stage 1 is NOT affected (use '
                             '--mode/--profile). Also from SUTURA_INTENSITY or '
                             'the "intensity" key of '
                             '~/.config/sutura/config.json')
    parser.add_argument('--no-history', action='store_true',
                        help='do not write the anonymous usage history record '
                             '(mesh geometry + repair results only, never file '
                             'names or paths)')
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
    files = args.files
    out = args.output
    human = args.human
    show_defects = args.defects
    show_diff = args.diff
    mode = args.mode
    dry_run = args.dry_run
    engine = resolve_classifier_engine(args.classifier_engine)
    triage_spec = triage.resolve_intensity(args.intensity)

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
        results = [validate_file(f, engine) for f in targets]
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

    # --dry-run: analyze and report the plan, write no output file at all.
    if dry_run:
        if out is not None:
            print(json.dumps({'error': '-o is not valid with --dry-run'}))
            sys.exit(1)
        results = [dry_run_file(f, mode=mode, profile=args.profile, engine=engine,
                                triage_spec=triage_spec) for f in files]
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
    results = [process_file(f, human, mode=mode, profile=args.profile,
                            no_history=args.no_history, engine=engine,
                            out=out, max_geom_change=max_geom_change,
                            max_risk=max_risk, force=args.force,
                            join_components=args.experimental_join_components,
                            autorefine=args.experimental_autorefine,
                            ftetwild=_ft_arg, deep_repair=_dr_mode,
                            indirect_autorefine=args.experimental_indirect_autorefine,
                            extra_features=args.experimental_edge_tiebreak,
                            triage_spec=triage_spec) for f in files]
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