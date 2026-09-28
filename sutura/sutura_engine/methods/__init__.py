# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura repair methods registry, ranking, and discovery.

Provides:
  - RepairMethod protocol & concrete implementation
  - Method registration and auto-discovery of builtin method modules (1..12)
  - Method recommendation and ranking based on object analysis & templates
  - Text & JSON formatting helpers for CLI inspection
"""
import importlib
import os
import pkgutil
from typing import Any, Callable, Dict, List, Optional, Tuple

from sutura_engine.analysis import SCORE_TEMPLATES, to_dict, analyze_file, combine
from sutura_engine.methods.availability import (
    always_available,
    closing_available,
    ftetwild_available,
    indirect_available,
    proxy_available,
    repeat_available,
)
from sutura_engine.methods.protocol import (
    MethodResult,
    Recommendation,
    RepairContext,
    RepairMethod,
    RepairMethodProtocol,
    _repair_mod,
)

PREFERENCE_BOOST = 1.25

# Compatibility aliases
_always = always_available
_indirect_available = indirect_available
_ftetwild_available = ftetwild_available
_closing_available = closing_available
_proxy_available = proxy_available
_repeat_available = repeat_available


def _not_implemented(phase: str) -> Callable[[], Tuple[bool, str]]:
    def check() -> Tuple[bool, str]:
        return False, 'not implemented yet (%s)' % phase
    return check


# --- Registry ---------------------------------------------------------------

_METHODS: Dict[int, RepairMethod] = {}


def register_method(method: Any, replace: bool = False) -> Any:
    """Register a repair method. Raises on a num clash by default."""
    num = int(getattr(method, 'num'))
    if not replace and num in _METHODS:
        raise ValueError('method num %d is already registered' % num)
    _METHODS[num] = method
    return method


def all_methods() -> List[RepairMethod]:
    """Every registered method, ordered by num (available and unavailable alike)."""
    return [_METHODS[num] for num in sorted(_METHODS)]


def get_method(num: Any) -> Optional[RepairMethod]:
    """The registered method for ``num``, or None (never raises)."""
    try:
        return _METHODS.get(int(num))
    except (TypeError, ValueError):
        return None


def available_methods() -> List[RepairMethod]:
    """All registered methods that report available=True."""
    return [m for m in all_methods() if m.available()[0]]


def unregister_method(num: Any) -> Optional[RepairMethod]:
    """Remove a method (test/extension cleanup)."""
    try:
        return _METHODS.pop(int(num), None)
    except (TypeError, ValueError):
        return None


# --- Built-in Auto-Discovery ------------------------------------------------

def discover_builtin_methods() -> None:
    """Discover and register all builtin methods in sutura_engine.methods.builtin."""
    import sutura_engine.methods.builtin as builtin_pkg
    pkg_path = os.path.dirname(builtin_pkg.__file__)
    modules = []
    for _, modname, ispkg in pkgutil.iter_modules([pkg_path]):
        if not ispkg and modname.startswith('m'):
            modules.append(modname)
    for modname in sorted(modules):
        try:
            mod = importlib.import_module(f'sutura_engine.methods.builtin.{modname}')
            m = getattr(mod, 'METHOD', None)
            if m is not None:
                register_method(m, replace=True)
        except Exception:
            pass


# --- Scoring ----------------------------------------------------------------

def _a(analysis: Any, name: str, default: float = 0.0) -> Any:
    value = getattr(analysis, name, default)
    return default if value is None else value


def _clamp01(value: Any) -> float:
    try:
        value = float(value)
    except (TypeError, ValueError):
        return 0.0
    return max(0.0, min(1.0, value))


def _score(method_id: str, analysis: Any) -> Tuple[float, str, str, Tuple[Any, ...]]:
    """Readable per-method suitability ``(score, reason, reason_key, args)``."""
    holes = _a(analysis, 'boundary_loops')
    nm = _a(analysis, 'non_manifold_edges')
    si = _a(analysis, 'self_intersections')
    components = _a(analysis, 'components', 1)
    largest = _a(analysis, 'largest_loop_ratio')
    open_ratio = _a(analysis, 'open_area_ratio')

    if method_id == 'fast':
        penalty = (0.5 * min(nm / 5.0, 1.0) + 0.3 * min(holes / 5.0, 1.0)
                   + 0.2 * min(si / 50.0, 1.0))
        light = penalty < 0.3
        return (_clamp01(1.0 - penalty),
                'light defect load' if light else 'defects may need a deeper tier',
                'rec_reason_fast_light' if light else 'rec_reason_fast_deep', ())
    if method_id == 'deep_local':
        score = (0.75 if (nm <= 0 and holes <= 2 and largest <= 0.3)
                 else 0.6 * (1.0 - min(max(holes - 2, 0) / 5.0, 1.0))
                 * (1.0 - min(nm / 3.0, 1.0)))
        return (_clamp01(score), 'local re-mesh of small damaged regions',
                'rec_reason_deep_local', ())
    if method_id == 'deep_full':
        return (_clamp01(0.55 + 0.35 * min((holes + nm) / 5.0, 1.0)),
                'full deep-repair ladder', 'rec_reason_deep_full', ())
    if method_id == 'join_components':
        if components > 1:
            return _clamp01(min((components - 1) / 2.0, 1.0)), \
                '%d connected components' % int(components), \
                'rec_reason_components', (int(components),)
        return 0.0, 'single connected component', \
            'rec_reason_component_single', ()
    if method_id in ('autorefine', 'indirect_autorefine'):
        if si <= 0:
            return 0.0, 'no self-intersections', 'rec_reason_no_si', ()
        return (_clamp01(0.4 + 0.6 * min(si / 200.0, 1.0)),
                'self-intersections present (%d)' % int(si),
                'rec_reason_si', (int(si),))
    if method_id == 'ftetwild':
        score = (0.4 * _clamp01(open_ratio / 0.05)
                 + 0.3 * _clamp01((holes + nm) / 3.0)
                 + 0.3 * _clamp01(si / 200.0))
        return (_clamp01(score), 'large openings / heavy self-intersections',
                'rec_reason_ftetwild', ())
    if method_id == 'poisson_close':
        single = _clamp01(_a(analysis, 'single_side_score'))
        relief = _clamp01(_a(analysis, 'relief_score'))
        score = single * (1.0 - relief)
        if score <= 0:
            return 0.0, 'not a single-sided open scan', \
                'rec_reason_not_scan', ()
        return (_clamp01(score), 'single-sided open scan (%.2f)' % single,
                'rec_reason_poisson', (single,))
    if method_id == 'flat_back_close':
        relief = _clamp01(_a(analysis, 'relief_score'))
        if relief <= 0:
            return 0.0, 'no relief-like opening', 'rec_reason_not_relief', ()
        return (_clamp01(relief), 'relief / flat-back profile',
                'rec_reason_relief', ())
    if method_id == 'proxy_template':
        damage = holes + nm + max(components - 1, 0)
        if damage <= 0:
            return 0.0, 'no holes/non-manifold to reconstruct', \
                'rec_reason_no_damage', ()
        open_pen = min(open_ratio / 0.5, 1.0)
        si_pen = min(si / 200.0, 1.0)
        score = (0.45 * min(damage / 4.0, 1.0)
                 + 0.35 * (1.0 - open_pen)
                 + 0.20 * (1.0 - si_pen))
        return (_clamp01(score),
                'holes/non-manifold/debris with a mostly healthy surface',
                'rec_reason_proxy', ())
    if method_id in ('repeat_auto', 'repeat_manual'):
        rep = _clamp01(_a(analysis, 'repetition_score'))
        if rep <= 0:
            return 0.0, 'no repeated pattern detected', \
                'rec_reason_no_repeat', ()
        return (_clamp01(rep), 'repeated pattern (%.2f)' % rep,
                'rec_reason_repeat', (rep,))
    return 0.0, 'not implemented yet', 'rec_reason_not_implemented', ()


# --- Ranking ----------------------------------------------------------------

def rank_methods(analysis: Any, top_n: int = 6) -> List[Recommendation]:
    """Rank available methods for an analysis; returns ``[Recommendation]``."""
    out = []
    for method in all_methods():
        ok, _reason = method.available()
        if not ok or method.needs_user_input:
            continue
        base, method_reason, reason_key, reason_args = method.score(analysis)
        best_combined = None
        best_template = None
        best_score = base
        reason = method_reason
        for template in SCORE_TEMPLATES:
            conf = template.confidence(analysis)
            if conf <= 0:
                continue
            boost = (PREFERENCE_BOOST
                     if method.num in template.preferred else 1.0)
            combined = conf * min(1.0, base * boost)
            if best_combined is None or combined > best_combined:
                best_combined = combined
                best_template = template.id
                best_score = max(base, combined)
        if best_combined is not None:
            reason = '%s; template %s' % (method_reason, best_template)
        if best_score <= 0:
            continue
        out.append(Recommendation(method.num, method.id, method.name,
                                  round(_clamp01(best_score), 3), reason,
                                  best_template, reason_key, reason_args))
    out.sort(key=lambda r: (-r.score, r.num))
    return out[:top_n]


def external_engines() -> List[dict]:
    """Configured third-party engines, reported SEPARATELY from the ranking."""
    try:
        from sutura_engine.adapters import external_adapter as _engines
    except Exception:
        try:
            import engines as _engines
        except Exception:
            return []
    try:
        configs, _warnings = _engines.load_all_engines()
    except Exception:
        return []
    out = []
    for name in sorted(configs):
        cfg = configs[name]
        try:
            available = bool(_engines.is_engine_available(cfg))
        except Exception:
            available = False
        out.append({
            'name': cfg.name,
            'placement': cfg.placement,
            'enabled': bool(cfg.enabled),
            'available': available,
            'command': list(cfg.command or []),
        })
    return out


# --- Formatting -------------------------------------------------------------

def format_methods() -> str:
    lines = ['Repair methods:',
             '  num  id                    family      available  name']
    for method in all_methods():
        ok, reason = method.available()
        lines.append('  %-4d %-21s %-11s %-10s %s%s' % (
            method.num, method.id, method.family, 'yes' if ok else 'no',
            method.name, '' if ok else '  (%s)' % reason))
    return '\n'.join(lines)


def methods_json() -> List[dict]:
    out = []
    for method in all_methods():
        ok, reason = method.available()
        out.append({
            'num': method.num,
            'id': method.id,
            'name': method.name,
            'description': method.description,
            'family': method.family,
            'invents_geometry': method.invents_geometry,
            'needs_user_input': method.needs_user_input,
            'available': ok,
            'reason': reason,
        })
    return out


def format_recommendations(recs: List[Recommendation]) -> str:
    lines = []
    for r in recs:
        lines.append('  %-4d %-21s score=%.2f  %s' % (
            r.num, r.id, r.score, r.reason))
    return '\n'.join(lines) if lines else '  (none)'


def analyze_report(path: str, engine: str = 'experimental', extra_features: bool = False) -> dict:
    """CLI ``--analyze`` payload for one file: analysis + recommendations."""
    result: Dict[str, Any] = {'input': path}
    try:
        objs = analyze_file(path, engine=engine, extra_features=extra_features)
        if not objs:
            raise ValueError('no mesh objects found in 3MF')
        combined_obj = combine(objs)
        recs = [dict(r._asdict()) for r in rank_methods(combined_obj)]
        if len(objs) == 1:
            result['analysis'] = to_dict(objs[0])
            result['recommendations'] = recs
        else:
            result['objects'] = len(objs)
            result['object_analyses'] = [to_dict(o) for o in objs]
            result['recommendations'] = recs
        result['external_engines'] = external_engines()
    except Exception as e:
        result['error'] = 'analysis failed: %s' % e
    return result


def format_analyze_human(result: dict) -> str:
    if 'error' in result:
        return 'Analyze failed: %s' % result['error']
    lines = ['Input : %s' % result.get('input')]
    analyses = result.get('object_analyses') or [result.get('analysis', {})]
    for idx, a in enumerate(analyses):
        label = a.get('model') or ('object %d' % idx)
        lines.append('Object: %s' % label)
        lines.append('  faces=%s vertices=%s components=%s loops=%s '
                     'nm_edges=%s si=%s%s type=%s (%.2f)' % (
                         a.get('faces'), a.get('vertices'), a.get('components'),
                         a.get('boundary_loops'), a.get('non_manifold_edges'),
                         a.get('self_intersections'),
                         '~' if a.get('self_intersections_estimated') else '',
                         a.get('mesh_type'), float(a.get('type_confidence') or 0)))
        lines.append('  open_area_ratio=%s largest_loop_ratio=%s '
                     'repetition_score=%s' % (
                         a.get('open_area_ratio'), a.get('largest_loop_ratio'),
                         a.get('repetition_score')))
    lines.append('Recommended methods:')
    lines.append(format_recommendations([
        Recommendation(r['num'], r['id'], r['name'], r['score'], r['reason'],
                       r['template']) for r in result.get('recommendations', [])]))
    lines.append('External engines (separate from the ranking):')
    engines = result.get('external_engines') or []
    if not engines:
        lines.append('  (none configured)')
    for e in engines:
        lines.append('  %-16s placement=%-14s enabled=%-5s available=%s' % (
            e.get('name'), e.get('placement'), e.get('enabled'),
            e.get('available')))
    return '\n'.join(lines)


# Run initial discovery on module load
discover_builtin_methods()
