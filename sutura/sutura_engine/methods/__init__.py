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

from sutura_engine.diagnosis import SCORE_TEMPLATES, to_dict, analyze_file, combine
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

_METHOD_ALIASES: Dict[str, int] = {
    # 1: Quick Clean
    '1': 1, 'm01': 1, 'm1': 1, 'quick_clean': 1, 'fast': 1, 'quick': 1,
    # 2: Local Mend
    '2': 2, 'm02': 2, 'm2': 2, 'local_mend': 2, 'deep_local': 2, 'local': 2,
    # 3: Full Mend
    '3': 3, 'm03': 3, 'm3': 3, 'full_mend': 3, 'deep_full': 3, 'full': 3, 'deep': 3,
    # 4: Join
    '4': 4, 'm04': 4, 'm4': 4, 'join': 4, 'join_components': 4,
    # 5: Autorefine
    '5': 5, 'm05': 5, 'm5': 5, 'autorefine': 5,
    # 6: Exact Refine
    '6': 6, 'm06': 6, 'm6': 6, 'exact_refine': 6, 'indirect_autorefine': 6, 'exact': 6,
    # 7: fTetWild
    '7': 7, 'm07': 7, 'm7': 7, 'ftetwild': 7,
    # 8: Balloon
    '8': 8, 'm08': 8, 'm8': 8, 'balloon': 8, 'poisson_close': 8, 'poisson': 8,
    # 9: Backplate
    '9': 9, 'm09': 9, 'm9': 9, 'backplate': 9, 'flat_back_close': 9, 'flat_back': 9, 'relief': 9,
    # 10: Scaffold
    '10': 10, 'm10': 10, 'scaffold': 10, 'proxy_template': 10, 'proxy': 10,
    # 11: Transplant (Auto)
    '11': 11, 'm11': 11, 'transplant': 11, 'repeat_auto': 11, 'repeat': 11,
    # 12: Transplant+ (Manual) (graft and 13 are valid aliases)
    '12': 12, 'm12': 12, '13': 12, 'm13': 12, 'transplant_plus': 12, 'transplant+': 12,
    'graft': 12, 'repeat_manual': 12,
    # 14: Mirror Complete
    '14': 14, 'm14': 14, 'mirror_complete': 14, 'mirror': 14,
    # 15: Wall Thicken
    '15': 15, 'm15': 15, 'wall_thicken': 15, 'wall': 15, 'thicken': 15,
}


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


def get_method(num_or_name: Any) -> Optional[RepairMethod]:
    """The registered method for ``num_or_name`` (num, slug, or alias), or None."""
    if num_or_name is None:
        return None
    if isinstance(num_or_name, int):
        method = _METHODS.get(num_or_name)
        if method is not None:
            return method
        alias = _METHOD_ALIASES.get(str(num_or_name))
        return _METHODS.get(alias) if alias is not None else None
    key_str = str(num_or_name).strip()
    key_lower = key_str.lower()
    if key_lower in _METHOD_ALIASES:
        return _METHODS.get(_METHOD_ALIASES[key_lower])
    for m in _METHODS.values():
        if m.id == key_str or m.id.lower() == key_lower:
            return m
        disp = getattr(m, 'display_name', '')
        if disp and (disp == key_str or disp.lower() == key_lower):
            return m
    try:
        return _METHODS.get(int(key_str))
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
        if not ispkg and not modname.startswith('_'):
            if modname.startswith('m') and len(modname) > 3 and modname[1:3].isdigit():
                continue
            modules.append(modname)
    for modname in sorted(modules):
        try:
            mod = importlib.import_module(f'sutura_engine.methods.builtin.{modname}')
            methods = getattr(mod, 'METHODS', None)
            if methods is not None:
                for m in methods:
                    register_method(m, replace=True)
            else:
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


# Canonical scoring key per method number (the user-facing slug).
_CANONICAL_BY_NUM = {
    1: 'quick_clean', 2: 'local_mend', 3: 'full_mend', 4: 'join',
    5: 'autorefine', 6: 'exact_refine', 7: 'ftetwild', 8: 'balloon',
    9: 'backplate', 10: 'scaffold', 11: 'transplant', 12: 'transplant_plus',
    14: 'mirror_complete', 15: 'wall_thicken',
}


def _score(method_id: str, analysis: Any) -> Tuple[float, str, str, Tuple[Any, ...]]:
    """Readable per-method suitability ``(score, reason, reason_key, args)``.

    ``method_id`` accepts the canonical slug, a legacy slug or a numeric id
    (resolved through the registry aliases).
    """
    method = get_method(method_id)
    if method is not None:
        method_id = _CANONICAL_BY_NUM.get(method.num, str(method_id).lower())
    else:
        method_id = str(method_id).lower()
    holes = _a(analysis, 'boundary_loops')
    nm = _a(analysis, 'non_manifold_edges')
    si = _a(analysis, 'self_intersections')
    components = _a(analysis, 'components', 1)
    largest = _a(analysis, 'largest_loop_ratio')
    open_ratio = _a(analysis, 'open_area_ratio')

    if method_id == 'quick_clean':
        penalty = (0.5 * min(nm / 5.0, 1.0) + 0.3 * min(holes / 5.0, 1.0)
                   + 0.2 * min(si / 50.0, 1.0))
        light = penalty < 0.3
        return (_clamp01(1.0 - penalty),
                'light defect load' if light else 'defects may need a deeper tier',
                'rec_reason_fast_light' if light else 'rec_reason_fast_deep', ())
    if method_id == 'local_mend':
        score = (0.75 if (nm <= 0 and holes <= 2 and largest <= 0.3)
                 else 0.6 * (1.0 - min(max(holes - 2, 0) / 5.0, 1.0))
                 * (1.0 - min(nm / 3.0, 1.0)))
        return (_clamp01(score), 'local re-mesh of small damaged regions',
                'rec_reason_deep_local', ())
    if method_id == 'full_mend':
        return (_clamp01(0.55 + 0.35 * min((holes + nm) / 5.0, 1.0)),
                'full deep-repair ladder', 'rec_reason_deep_full', ())
    if method_id == 'join':
        if components > 1:
            return _clamp01(min((components - 1) / 2.0, 1.0)), \
                '%d connected components' % int(components), \
                'rec_reason_components', (int(components),)
        return 0.0, 'single connected component', \
            'rec_reason_component_single', ()
    if method_id in ('autorefine', 'exact_refine'):
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
    if method_id == 'balloon':
        single = _clamp01(_a(analysis, 'single_side_score'))
        relief = _clamp01(_a(analysis, 'relief_score'))
        score = single * (1.0 - relief)
        if score <= 0:
            return 0.0, 'not a single-sided open scan', \
                'rec_reason_not_scan', ()
        return (_clamp01(score), 'single-sided open scan (%.2f)' % single,
                'rec_reason_poisson', (single,))
    if method_id == 'backplate':
        relief = _clamp01(_a(analysis, 'relief_score'))
        if relief <= 0:
            return 0.0, 'no relief-like opening', 'rec_reason_not_relief', ()
        return (_clamp01(relief), 'relief / flat-back profile',
                'rec_reason_relief', ())
    if method_id == 'scaffold':
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
    if method_id in ('transplant', 'transplant_plus'):
        rep = _clamp01(_a(analysis, 'repetition_score'))
        if rep <= 0:
            return 0.0, 'no repeated pattern detected', \
                'rec_reason_no_repeat', ()
        return (_clamp01(rep), 'repeated pattern (%.2f)' % rep,
                'rec_reason_repeat', (rep,))
    if method_id == 'mirror_complete':
        single = _clamp01(_a(analysis, 'single_side_score'))
        relief = _clamp01(_a(analysis, 'relief_score'))
        score = single * (1.0 - relief)
        if score <= 0:
            return 0.0, 'not a single-sided open scan', \
                'rec_reason_not_scan', ()
        return (_clamp01(score), 'single-sided scan with a symmetry plane '
                '(%.2f)' % single, 'rec_reason_mirror', (single,))
    if method_id == 'wall_thicken':
        # Opt-in only (needs_user_input excludes it from the ranking); the
        # thicken decision is a shape change the user must make explicitly.
        return 0.0, 'opt-in thin-wall thicken', 'rec_reason_wall_optin', ()
    return 0.0, 'not implemented yet', 'rec_reason_not_implemented', ()


# --- Ranking ----------------------------------------------------------------

def rank_methods(analysis: Any, top_n: int = 6) -> List[Recommendation]:
    """Rank available methods for an analysis; returns ``[Recommendation]``."""
    # Learning triage (bounded): a small Bayesian bonus from the local
    # (template, method) success/time history. Loaded once per ranking; opt out
    # with SUTURA_LEARNING_TRIAGE=0 or the config key; resettable from the CLI
    # and the GUI Options.
    history_mod = None
    learning_stats = None
    try:
        import history as history_mod
        if history_mod.learning_enabled():
            learning_stats = history_mod.triage_stats()
        else:
            history_mod = None
    except Exception:
        history_mod = None

    out = []
    for method in all_methods():
        ok, _reason = method.available()
        if not ok or method.needs_user_input:
            continue
        base, method_reason, reason_key, reason_args = method.score(analysis)
        best_combined = None
        best_template = None
        best_score = base
        best_bonus = 0.0
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
            if learning_stats is not None:
                best_bonus = max(best_bonus, history_mod.learning_bonus(
                    template.id, method.id, stats=learning_stats))
        if best_combined is not None:
            reason = '%s; template %s' % (method_reason, best_template)
        if best_score <= 0:
            continue
        if best_bonus:
            best_score = min(1.0, best_score + best_bonus)
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
            'display_name': method.display_name,
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
            r.num, r.name, r.score, r.reason))
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
