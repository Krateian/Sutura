# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""engines tier (P1a split). Moved verbatim from sutura/repair.py; no logic change."""
import os

import numpy as np

from sutura_engine import health as method_health
from sutura_engine.pipeline.presets import FTETWILD_MAX_HAUSDORFF_REL
from sutura_engine.pipeline.utils import _hausdorff_rel, boundary_loop_stats


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
        with method_health.tier_guard('engine:' + name, placement) as _g:
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
        if _g.error is not None:
            entry['reject_reason'] = 'error: %s' % _g.error
        entries.append(entry)
    return None, None, False


def _record_engine_entries(stats, entries):
    """Append engine entries to ``stats['engines']`` and flag shape changes."""
    if not entries:
        return
    stats.setdefault('engines', []).extend(entries)
    if any(e.get('shape_changed') for e in entries):
        stats['shape_changed'] = True


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
