# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""fTetWild fallback tier (P1a split).

Tetrahedralizes the original surface (via the ftetwild_bridge), extracts a
watertight SI-free boundary, decimates a wastefully dense boundary and
Manifold-post-processes a pinched one. Includes the availability probe and
the bridge runner. Moved verbatim from sutura/repair.py; no logic change.
"""
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time

import numpy as np

import topology
import triage
from sutura_engine import health as method_health
from sutura_engine.pipeline.mesh_io import FTETWILD_BRIDGE, read_obj, write_obj
from sutura_engine.pipeline.presets import (
    DECIMATE_HD_MARGIN, DENSE_QUALITY, FTETWILD_ABORT_MESSAGE,
    FTETWILD_MAX_HAUSDORFF_REL, FTETWILD_RETRY_PARAMS, FTETWILD_TIMEOUT,
    HAUSDORFF_SAMPLES, VENV311, ftetwild_params, resolve_ftetwild_optimize,
)
from sutura_engine.pipeline.stage2 import run_stage2
from sutura_engine.pipeline.utils import (
    _hausdorff_rel, _referenced_only, boundary_loop_stats,
)


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


def _ftetwild_manifold_postprocess(ml, tmpdir, cand_v, cand_t, cand_ms, cand_after,
                                   cand_holes, cand_nm):
    """Manifold post-process of the fTetWild boundary.

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
            m3_after = topology.mesh_topology(m3_v, m3_t)
            m3_holes = boundary_loop_stats(m3_v, m3_t)[0]
            m3_nm = m3_after.get('non_two_manifold_edges', 0)
            if m3_holes <= cand_holes and m3_nm <= cand_nm:
                return m3_v, m3_t, m3_ms, m3_after, m3_holes, m3_nm, True
    return cand_v, cand_t, cand_ms, cand_after, cand_holes, cand_nm, False


def _ftetwild_candidate(ml, tmpdir, cand_v, cand_t):
    """Measure a boundary candidate and run the Manifold post-process on it.

    Returns ``(verts, tris, meshset, topo, holes, nm, postprocessed)``; the
    same measurement/post-process the tier always applied to a single fTetWild
    boundary."""
    cand_ms = ml.MeshSet()
    cand_ms.add_mesh(ml.Mesh(vertex_matrix=np.asarray(cand_v, np.float32),
                             face_matrix=np.asarray(cand_t, np.int32)))
    cand_after = topology.mesh_topology(cand_v, cand_t)
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
                     # AFTER the in-tier Manifold post-process (which can
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
    holes/non-manifold guard, one retry with FTETWILD_RETRY_PARAMS runs -- a
    DETERMINISTIC rule (never a function of the elapsed wall clock).
    ``attempts`` lists every run, ``adopted_attempt`` names the adopted one;
    the top-level report fields describe the adopted (or else the last)
    attempt. ``FTETWILD_TIMEOUT`` is only an outer all-or-nothing abort checked
    between attempts: when it trips, the tier is abandoned and the previous
    stage result is kept (``aborted=True`` / ``reject_reason='aborted_time'``),
    never a partial attempt. An adopted result far from the input is flagged
    (``shape_changed``), not rejected."""
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
            with method_health.tier_guard('ftetwild', 'tier') as _g:
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
                aborted = False
                for tag, params in plan:
                    if attempts:
                        last = attempts[-1]
                        # an Extreme dense-failure retry already ran the
                        # optimising attempt inside _ftetwild_attempt
                        if (tag == 'optimize'
                                and (last.get('optimize_retry') or {}).get('ran')):
                            break
                        # retry only when the first boundary exists but fails
                        # the holes/non-manifold guard; DETERMINISTIC (never a
                        # function of the elapsed wall clock)
                        if last.get('reject_reason') != 'holes_nm':
                            break
                    # Outer wall-clock abort (all-or-nothing): the budget bounds
                    # the WHOLE ladder.  When it is spent the tier is abandoned
                    # and the previous stage result is kept, never a partial
                    # attempt; a single attempt still gets the remaining budget
                    # so it cannot run unbounded.
                    remaining = spec.ftetwild_timeout - (
                        time.perf_counter() - t_start)
                    if remaining <= 0.0:
                        aborted = True
                        break
                    att, cand = _ftetwild_attempt(ml, tmpdir, inter, tag,
                                                  params, remaining, v, t, spec)
                    attempts.append(att)
                    if att.get('error') == 'timeout':
                        aborted = True
                        break
                    if cand is None:
                        continue
                    if cand[4] <= cur_holes and cand[5] <= cur_nm:
                        chosen = (att, cand)
                        break
                    att['reject_reason'] = 'holes_nm'
                if attempts:
                    final = chosen[0] if chosen else attempts[-1]
                    ft_rep.update(
                        {k: val for k, val in final.items() if k != 'attempt'})
                ft_rep['attempts'] = attempts
                ft_rep['adopted'] = chosen is not None
                if aborted:
                    ft_rep['adopted'] = False
                    ft_rep['aborted'] = True
                    ft_rep['reject_reason'] = 'aborted_time'
                    ft_rep['reason'] = FTETWILD_ABORT_MESSAGE % (
                        ('%g s' % float(spec.ftetwild_timeout))
                        if spec.ftetwild_timeout else 'time')
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
            if _g.error is not None:
                ft_rep['error'] = str(_g.error)
            stats['experimental_ftetwild'] = ft_rep
    return ms, after
