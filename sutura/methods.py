"""Repair method registry, ranking and the tagged-repair execution policy (P0).

This module is the single source of truth for the user-facing repair methods
(``--list-methods`` / ``--methods``) and for the per-object analysis driven
recommendation engine (``--analyze``). Methods are thin wrappers over the
existing pipeline: ``RepairMethod.run`` calls
``repair.repair_mesh_from_arrays`` with the method's kwargs, and the file-level
orchestrator ``repair_with_methods`` reuses ``repair.repair_file`` /
``repair.repair_3mf`` unchanged, so no chain logic is duplicated.

Design rules (see AGENTS.md / the P0 brief):
  * ``num`` is a stable user-facing integer and is NEVER derived from rank.
  * New methods are added from P1/P2/P4/P5 modules via ``register_method``;
    the fixed 1..12 list below is the P0 baseline and must not be reordered.
  * With ``methods=None`` (auto) the untagged repair runs today's default
    pipeline first and, when the result is already strict-watertight and
    passes the shape guard, the output is byte-identical to today.

External third-party engines (``sutura/engines.py``) are reported separately by
``external_engines`` and are NEVER mixed into the method ranking.

Dependency note: ``repair`` is imported lazily via ``_repair_mod`` so this
module can be imported by ``repair.py`` itself (no circular import) and so a
run of ``repair.py`` as ``__main__`` does not get re-executed as a module.
"""
import os
import sys
import time
import shutil
import importlib.util
from collections import namedtuple
from dataclasses import dataclass, field
from typing import Callable, Optional

import templates
import object_analysis
import classification

# --- execution policy constants (documented) --------------------------------

# Auto mode tries at most this many extra methods after the baseline.
AUTO_MAX_ATTEMPTS = 3
# Wall-clock budget for the untagged auto ranked fallback: after the baseline
# attempt, extra methods may run for at most
# min(max(AUTO_FALLBACK_MIN_S,
#         AUTO_FALLBACK_BUDGET_FACTOR * baseline_seconds),
#     AUTO_FALLBACK_MAX_S).
# When the budget is exceeded the fallback stops starting further methods
# (a method already running is not interrupted), keeps the best candidate and
# records the "fallback budget reached" note. The budget is relative to the
# baseline so a slow input does not get a fast cutoff and a trivial input still
# gets the minimum, while AUTO_FALLBACK_MAX_S keeps a slow baseline from
# granting an unbounded fallback window. Explicit user tags (``--methods``) are
# NEVER budgeted.
AUTO_FALLBACK_BUDGET_FACTOR = 2.0
AUTO_FALLBACK_MIN_S = 30.0
# Hard ceiling on the fallback budget: even when the baseline itself was slow,
# the extra methods never get more than this many seconds. A baseline that
# already exceeded the ceiling on its own skips the ranked fallback entirely
# (the work is done, only the report note remains).
AUTO_FALLBACK_MAX_S = 120.0
# A geometry-inventing method (fTetWild, Poisson, ...) is tried automatically
# ONLY when its recommendation score clears this bar: inventing a back surface
# is a bigger geometry change than cleaning an existing one.
AUTO_INVENT_MIN_SCORE = 0.8
# One-sided (input -> output) Hausdorff guard for geometry-inventing methods,
# relative to the input bbox diagonal. Same value the fTetWild tier uses.
GENERATIVE_MAX_HAUSDORFF_REL = 0.01
# A method in a template's preferred list gets this score multiplier.
PREFERENCE_BOOST = 1.25

Recommendation = namedtuple(
    'Recommendation', 'num id name score reason template reason_key reason_args')
# Backward-compatible defaults: older 6-field constructions stay valid.
Recommendation.__new__.__defaults__ = (None, ())


# --- registry ---------------------------------------------------------------

@dataclass(frozen=True)
class RepairMethod:
    num: int
    id: str
    name: str
    description: str
    family: str
    invents_geometry: bool = False
    needs_user_input: bool = False
    kwargs: dict = field(default_factory=dict, compare=False)
    available_fn: Optional[Callable] = field(default=None, compare=False,
                                             repr=False)
    # Shape-guard direction for an invents_geometry method. False (default)
    # uses the P0 output -> input Hausdorff (does the output stray from the
    # input? -- right for an envelope like fTetWild). True uses the
    # input -> output direction (is every original surface point still covered?
    # -- right for the closing methods, whose whole point is to invent a back
    # surface that is BY DESIGN far from the open input).
    guard_input_to_output: bool = field(default=False, compare=False,
                                        repr=False)
    # True when the method's own tier already enforces the shape guard (e.g.
    # the repeated-element transplant measures the untouched geometry with
    # ``hausdorff_outside``); ``_evaluate`` then skips the generic one-sided
    # Hausdorff, which would wrongly penalize the intended local replacement.
    self_guarded: bool = field(default=False, compare=False, repr=False)

    def available(self):
        """``(bool, reason|None)`` availability, never raising."""
        if self.available_fn is None:
            return True, None
        try:
            ok, reason = self.available_fn()
            return bool(ok), (reason if not ok else None)
        except Exception as e:  # noqa: BLE001 - availability never crashes
            return False, 'availability check failed: %s' % e

    def score(self, analysis):
        """``(0..1, reason, reason_key, reason_args)`` for the analyzed object.

        ``reason`` is the English text (CLI / fallback); ``reason_key`` +
        ``reason_args`` let the GUI localize the same sentence."""
        return _score(self.id, analysis)

    def run(self, verts, tris, tmpdir, ctx):
        """Array-level thin wrapper over the existing pipeline.

        Returns the repair report with the output arrays attached under the
        transient ``_verts``/``_tris`` keys (the file-level orchestrator pops
        them). ``ctx`` is the base ``repair_mesh_from_arrays`` kwargs dict; the
        method's ``kwargs`` override it.
        """
        repair = _repair_mod()
        merged = dict(ctx or {})
        merged.update(self.kwargs)
        mode = merged.pop('mode', 'auto')
        profile = merged.pop('profile', None)
        engine = merged.pop('engine', 'experimental')
        report, out_v, out_t = repair.repair_mesh_from_arrays(
            verts, tris, tmpdir, mode=mode, profile=profile, engine=engine,
            **merged)
        out_v, out_t = repair.maybe_run_stage2(report, out_v, out_t, tmpdir)
        # P-HONEST: same reload-honest verdict at this API exit point as the
        # file-level orchestrator applies, so a caller that saves out_v/out_t
        # cannot get a watertight claim the saved mesh would not honour.
        repair.enforce_reload_verdict(report, out_v, out_t)
        report = dict(report)
        report['_verts'] = out_v
        report['_tris'] = out_t
        return report


_METHODS = {}


def register_method(method, replace=False):
    """Register a method (P1/P2/P4/P5 hook). Raises on a num clash by default."""
    if not replace and method.num in _METHODS:
        raise ValueError('method num %d is already registered' % method.num)
    _METHODS[method.num] = method
    return method


def all_methods():
    """Every registered method, ordered by num (available and unavailable alike)."""
    return [_METHODS[num] for num in sorted(_METHODS)]


def get_method(num):
    """The registered method for ``num``, or None (never raises).

    A defensive lookup: ``num`` may be ``None``/a non-integer when a report
    from an external CLI run is formatted (``method_used.num`` can be null).
    """
    try:
        return _METHODS.get(int(num))
    except (TypeError, ValueError):
        return None


def available_methods():
    return [m for m in all_methods() if m.available()[0]]


def unregister_method(num):
    """Remove a method (test/extension cleanup)."""
    return _METHODS.pop(int(num), None)


# --- availability helpers ---------------------------------------------------

def _always():
    return True, None


def _indirect_available():
    """The exact indirect-predicate tier needs the Rust extension + bridge."""
    try:
        if importlib.util.find_spec('sutura_geom') is None:
            return False, 'rust extension sutura_geom is not installed'
        if importlib.util.find_spec('indirect_bridge') is None:
            return False, 'indirect_bridge.py is not available'
        return True, None
    except Exception as e:  # noqa: BLE001
        return False, 'indirect autorefine unavailable: %s' % e


def _ftetwild_available():
    try:
        if _repair_mod().ftetwild_available():
            return True, None
        return False, 'fTetWild (pytetwild + pyvista) is not installed'
    except Exception as e:  # noqa: BLE001
        return False, 'fTetWild unavailable: %s' % e


def _closing_available():
    """Registry methods 8/9 need the standalone ``sutura/closing.py`` tier."""
    try:
        if importlib.util.find_spec('closing') is None:
            return False, 'closing.py is not available'
        return True, None
    except Exception as e:  # noqa: BLE001
        return False, 'closing unavailable: %s' % e


def _proxy_available():
    """Registry method 10 needs ``proxy_repair.py`` plus its deps."""
    try:
        for mod in ('proxy_repair', 'trimesh', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:  # noqa: BLE001
        return False, 'proxy repair unavailable: %s' % e


def _repeat_available():
    """Registry methods 11/12 need ``sutura/repeat_repair.py`` + its deps.

    The transplant itself uses in-process manifold3d CSG (isolated behind
    ``repeat_repair._execute_boolean_transplant``); detection and alignment run
    without it, so the module imports and the availability check does not
    require manifold3d. When a transplant is actually attempted on a host
    without manifold3d the tier reports a clean error rather than crashing."""
    try:
        for mod in ('repeat_repair', 'trimesh', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:  # noqa: BLE001
        return False, 'repeat repair unavailable: %s' % e


def _not_implemented(phase):
    def check():
        return False, 'not implemented yet (%s)' % phase
    return check


# --- fixed P0 methods (1..7) and placeholders (8..12) -----------------------

register_method(RepairMethod(
    1, 'fast', 'Fast cleanup',
    'Stage 1 + manifold3d rebuild; no deep repair and no fTetWild.',
    'clean', kwargs={'deep_repair': 'off', 'ftetwild': False}))
register_method(RepairMethod(
    2, 'deep_local', 'Local deep repair',
    "Deep-repair ladder 'local': re-mesh only the damaged regions.",
    'topology', kwargs={'deep_repair': 'local', 'ftetwild': False}))
register_method(RepairMethod(
    3, 'deep_full', 'Full deep repair',
    "Deep-repair ladder 'full' (today's default balanced behaviour).",
    'topology', kwargs={'deep_repair': 'full', 'ftetwild': 'auto'}))
register_method(RepairMethod(
    4, 'join_components', 'Join components',
    'Move small connected components onto the nearest larger component '
    'instead of deleting them.',
    'topology', kwargs={'join_components': True}))
register_method(RepairMethod(
    5, 'autorefine', 'Autorefine (SI)',
    'Resolve self-intersections by subdividing intersecting triangles '
    '(Lazard & Valque 2025); never deletes input faces.',
    'si', kwargs={'autorefine': True}))
register_method(RepairMethod(
    6, 'indirect_autorefine', 'Indirect autorefine (SI, exact)',
    'Exact arrangement-lite self-intersection split via the rust/sutura-geom '
    'extension (indirect predicates).',
    'si', kwargs={'indirect_autorefine': True},
    available_fn=_indirect_available))
register_method(RepairMethod(
    7, 'ftetwild', 'fTetWild envelope',
    'Tetrahedralize the original surface and extract a watertight, SI-free '
    'boundary (fTetWild). Estimates a back surface.',
    'envelope', invents_geometry=True, kwargs={'ftetwild': True},
    available_fn=_ftetwild_available))
register_method(RepairMethod(
    8, 'poisson_close', 'Poisson close',
    'Screened-Poisson surface reconstruction that closes the large opening '
    'of a single-sided scan (estimates the missing back surface).',
    'closing', invents_geometry=True, guard_input_to_output=True,
    kwargs={'closing': 'poisson', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=_closing_available))
register_method(RepairMethod(
    9, 'flat_back_close', 'Flat-back close',
    'Close a single dominant opening of a relief / plate with a flat back '
    'surface and side walls (the input surface is preserved exactly).',
    'closing', invents_geometry=True, guard_input_to_output=True,
    kwargs={'closing': 'flat_back', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=_closing_available))
register_method(RepairMethod(
    10, 'proxy_template', 'Proxy template match',
    'Rebuild a heavily broken mesh from a coarse watertight proxy, '
    're-projecting the healthy original regions (proxy_repair.py).',
    'template', invents_geometry=True, guard_input_to_output=True,
    kwargs={'proxy_template': True, 'deep_repair': 'off', 'ftetwild': False},
    available_fn=_proxy_available))
register_method(RepairMethod(
    11, 'repeat_auto', 'Repeat-aware auto',
    'Detect a rotational/translational/helical repeated pattern and transplant '
    'a healthy copy onto each damaged or missing one (repeat_repair.py).',
    'pattern', invents_geometry=True, self_guarded=True,
    kwargs={'repeat': 'auto', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=_repeat_available))
register_method(RepairMethod(
    12, 'repeat_manual', 'Repeat-aware manual',
    'Transplant the repeated element marked by the user (--repeat-source) onto '
    'the damaged one (--repeat-target); never runs automatically.',
    'pattern', invents_geometry=True, self_guarded=True,
    needs_user_input=True,
    kwargs={'repeat': 'manual', 'deep_repair': 'off', 'ftetwild': False},
    available_fn=_repeat_available))


# --- scoring ----------------------------------------------------------------

def _a(analysis, name, default=0.0):
    value = getattr(analysis, name, default)
    return default if value is None else value


def _clamp01(value):
    try:
        value = float(value)
    except (TypeError, ValueError):
        return 0.0
    return max(0.0, min(1.0, value))


def _score(method_id, analysis):
    """Readable per-method suitability ``(score, reason, reason_key, args)``.

    ``reason`` is the English sentence (used by the CLI and as the GUI
    fallback); ``reason_key`` is the i18n key and ``reason_args`` the format
    arguments so the GUI can localize the identical sentence (EN/TR)."""
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


# --- ranking ----------------------------------------------------------------

def rank_methods(analysis, top_n=6):
    """Rank available methods for an analysis; returns ``[Recommendation]``.

    The score combines the method's own ``score`` with the best-fitting
    template: ``template_confidence * min(1, method_score * boost)`` where
    ``boost`` rewards a method named in the template's preferred list. Methods
    that need user input (e.g. ``repeat_manual``) and unavailable methods are
    excluded; they remain visible through ``all_methods()``.
    """
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
        for template in templates.SCORE_TEMPLATES:
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


def external_engines():
    """Configured third-party engines, reported SEPARATELY from the ranking."""
    try:
        import engines as _engines
    except Exception:  # noqa: BLE001
        return []
    try:
        configs, _warnings = _engines.load_all_engines()
    except Exception:  # noqa: BLE001
        return []
    out = []
    for name in sorted(configs):
        cfg = configs[name]
        try:
            available = bool(_engines.is_engine_available(cfg))
        except Exception:  # noqa: BLE001
            available = False
        out.append({
            'name': cfg.name,
            'placement': cfg.placement,
            'enabled': bool(cfg.enabled),
            'available': available,
            'command': list(cfg.command or []),
        })
    return out


# --- low-level helpers ------------------------------------------------------

def _repair_mod():
    for name in ('repair', '__main__'):
        mod = sys.modules.get(name)
        if mod is not None and hasattr(mod, 'repair_mesh_from_arrays'):
            return mod
    import repair as _repair
    return _repair


def _load_objects(path):
    """[(model_name|None, verts, tris)] for a file (one per 3MF object)."""
    return _repair_mod().load_meshes(path)


def _safe_load(path):
    try:
        return _load_objects(path)
    except Exception:  # noqa: BLE001 - input arrays are best-effort
        return []


def one_sided_hausdorff(in_v, in_t, out_v, out_t, samples=None):
    """One-sided input -> output Hausdorff, relative to the input bbox diagonal.

    Returns ``(max_rel, mean_rel)`` (or ``(None, None)`` for an empty mesh).
    Reuses ``repair._hausdorff_rel`` (which samples the OUTPUT and measures the
    distance to the input), so a geometry-inventing method can be held to the
    same shape guard as the fTetWild tier without changing any existing guard.
    """
    import pymeshlab as ml
    repair = _repair_mod()
    if samples is None:
        samples = repair.HAUSDORFF_SAMPLES
    return repair._hausdorff_rel(ml, in_v, in_t, out_v, out_t, samples=samples)


def _worst_hausdorff(in_objs, out_objs):
    """Worst per-object one-sided Hausdorff, or None when not comparable."""
    if not in_objs or len(in_objs) != len(out_objs):
        return None
    worst = None
    for (_in_name, in_v, in_t), (_out_name, out_v, out_t) in zip(in_objs,
                                                                 out_objs):
        mx, _mean = one_sided_hausdorff(in_v, in_t, out_v, out_t)
        if mx is None:
            continue
        worst = mx if worst is None else max(worst, mx)
    return worst


def _worst_hausdorff_input_to_output(in_objs, out_objs):
    """Worst per-object input -> output Hausdorff, or None when not comparable.

    Samples the INPUT and measures the distance to the output (relative to the
    input bbox diagonal): does the output still cover every original surface
    point? Reuses ``closing.one_sided_hausdorff`` (the closing modules' own
    fidelity metric), so the guard is not duplicated. Used for the closing /
    proxy methods, whose invented back surface is far from the open input by
    design -- the output -> input direction would reject them all.
    """
    if not in_objs or len(in_objs) != len(out_objs):
        return None
    try:
        import closing as _closing
    except Exception:  # noqa: BLE001 - optional module
        return None
    worst = None
    for (_in_name, in_v, in_t), (_out_name, out_v, out_t) in zip(in_objs,
                                                                 out_objs):
        mx, _mean = _closing.one_sided_hausdorff(in_v, in_t, out_v, out_t)
        if mx is None:
            continue
        worst = mx if worst is None else max(worst, mx)
    return worst


def _strict_holes_nm(objs, weld=False):
    """Total holes + non-manifold regions over ``objs``.

    ``weld=True`` first converts each mesh into its STL save/reload-equivalent
    form (``repair.weld_reload_equivalent``): positions cast to float32 with
    exactly-coincident positions merged. A generative method's output can be
    index-watertight in memory yet non-manifold after the float32 round-trip
    (coincident seam vertices weld on reload), so the guard must judge the mesh
    the user will actually reload -- not the in-memory index topology."""
    import defects
    weld_fn = _repair_mod().weld_reload_equivalent if weld else None
    holes = 0
    nm = 0
    for _name, v, t in objs:
        if weld_fn is not None:
            v, t = weld_fn(v, t)
        d = defects.detect(v, t)
        holes += len(d['holes'])
        nm += len(d['non_manifold'])
    return holes, nm


def _worst_geom_change(result):
    reports = result.get('object_reports') if isinstance(result, dict) else None
    reps = reports if reports else [result]
    vals = []
    for rep in reps:
        s1 = (rep or {}).get('stage1') or {}
        for key in ('volume_change_percent', 'surface_area_change_percent'):
            value = s1.get(key)
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                vals.append(abs(float(value)))
    return max(vals) if vals else None


def _evaluate(result, path, multi, in_objs, method):
    """Evaluate one attempt against strict-watertight + shape guard."""
    rec = {'watertight': False, 'holes': None, 'non_manifold': None,
           'hausdorff_rel': None, 'geom_change_pct': None, 'reason': None}
    if not isinstance(result, dict) or 'error' in result:
        rec['reason'] = (result or {}).get('error', 'no result')
        return rec
    # 5.1: the same rule as the top-level verdict (classification.classify):
    # "watertight" requires stage 1 closed AND stage 2 (manifold3d) actually
    # ran and returned ok. A stage-1-closed mesh with stage 2 skipped/errored
    # is a warning, so a method must never be recorded as reach-watertight on
    # the index topology alone.
    category, _issues, _key = classification.classify(result)
    needs_objs = bool(method is not None and method.invents_geometry and in_objs
                      and not getattr(method, 'self_guarded', False))
    out_objs = None
    if category == 'watertight':
        # Every method is judged on the reload-equivalent mesh (P-HONEST): the
        # top-level verdict never claims watertight unless the SAVED mesh is
        # strict-watertight after the save/reload weld. ``repair_file`` /
        # ``repair_3mf`` already ran ``enforce_reload_verdict``, so a
        # watertight classify means the reload check passed -- no need to
        # re-read every output (an already-watertight result does no extra
        # work). A candidate that fails the reload check was downgraded by
        # enforce, so it does not classify watertight here.
        holes = nm = 0
    else:
        # Not watertight: measure the saved mesh so the reject reason and the
        # holes/non-manifold counts are the real reload-equivalent ones.
        try:
            out_objs = _load_objects(path)
        except Exception as e:  # noqa: BLE001
            rec['reason'] = 'cannot read output: %s' % e
            return rec
        holes, nm = _strict_holes_nm(out_objs, weld=True)
    rec['holes'] = holes
    rec['non_manifold'] = nm
    if holes or nm:
        rec['reason'] = 'holes=%d non-manifold=%d remain' % (holes, nm)
        return rec
    if category != 'watertight':
        rec['reason'] = ('stage 1 closed but stage 2 did not confirm the solid '
                         '(%s)' % (_key or 'warning'))
        return rec
    rec['watertight'] = True
    # A geometry-inventing method is held to a one-sided Hausdorff guard; a
    # cleaning/topology/SI method is not -- its own adopt/fallback guards
    # already protect the geometry, so strict watertightness is the guard here.
    # The direction depends on the method: the closing/proxy methods
    # (guard_input_to_output=True) are measured input -> output (is the
    # original surface still covered?), the envelope methods output -> input
    # (does the output stray from the input?). The geometry change is recorded.
    if needs_objs:
        if out_objs is None:
            try:
                out_objs = _load_objects(path)
            except Exception as e:  # noqa: BLE001
                rec['reason'] = 'cannot read output: %s' % e
                return rec
        if method.guard_input_to_output:
            hd = _worst_hausdorff_input_to_output(in_objs, out_objs)
        else:
            hd = _worst_hausdorff(in_objs, out_objs)
        rec['hausdorff_rel'] = hd
        if hd is not None and hd > GENERATIVE_MAX_HAUSDORFF_REL:
            rec['watertight'] = False
            rec['reason'] = ('shape changed (one-sided Hausdorff %.3f > %.3f)'
                             % (hd, GENERATIVE_MAX_HAUSDORFF_REL))
            return rec
    else:
        rec['geom_change_pct'] = _worst_geom_change(result)
    rec['reason'] = 'strict-watertight'
    return rec


def _attempt(src, out, tmpdir, kwargs, multi):
    repair = _repair_mod()
    try:
        if multi:
            return repair.repair_3mf(src, out, tmpdir, **kwargs)
        return repair.repair_file(src, out, tmpdir, **kwargs)
    except Exception as e:  # noqa: BLE001 - one method must not kill the run
        # ExtremeRemovedAllError is a terminal, user-facing condition that
        # process_file handles specially (its own issue code); never swallow it.
        if isinstance(e, getattr(repair, 'ExtremeRemovedAllError', ())):
            raise
        return {'input': src, 'output': out, 'error': 'repair failed: %s' % e}


def _baseline_num(kwargs):
    """Which registry method today's defaults correspond to."""
    mode = kwargs.get('deep_repair')
    if mode == 'local':
        return 2
    if mode == 'off':
        return 1
    return 3


def _record(method, evaluation, outcome):
    return {
        'num': method.num,
        'id': method.id,
        'name': method.name,
        'outcome': outcome,
        'watertight': bool(evaluation['watertight']),
        'holes': evaluation['holes'],
        'non_manifold': evaluation['non_manifold'],
        'hausdorff_rel': evaluation['hausdorff_rel'],
        'geom_change_pct': evaluation['geom_change_pct'],
        'reason': evaluation['reason'],
    }


def _analyze_objects(src, ctx):
    try:
        return object_analysis.analyze_file(
            src, engine=ctx.get('engine', 'experimental'),
            extra_features=ctx.get('extra_features', False))
    except Exception:  # noqa: BLE001 - analysis is best-effort
        return []


def _attach(result, method, tried, analysis_objects, recs, note=None,
            source=None, reached=True):
    result['method_used'] = {
        'num': method.num if method else None,
        'id': method.id if method else None,
        'name': method.name if method else None,
        'source': source,
    }
    if note:
        result['method_used']['note'] = note
    result['methods_tried'] = tried
    result['method_reached_watertight'] = bool(reached)
    if analysis_objects:
        result['analysis'] = [object_analysis.to_dict(o) for o in analysis_objects]
        result['recommendations'] = [dict(r._asdict()) for r in recs]


def _multi_note(multi):
    return None


def _is_obj_watertight(rep):
    s1 = (rep or {}).get('stage1', {})
    s2 = (rep or {}).get('stage2', {})
    return bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0 and bool(s2.get('ok'))


def _replace_3mf_meshes(in_3mf, out_3mf, mesh_list, tmpdir=None):
    """Write out_3mf with mesh blocks in .model entries replaced in order."""
    import re
    import zipfile
    mesh_iter = iter(mesh_list)
    repair = _repair_mod()
    with zipfile.ZipFile(in_3mf, 'r') as zin:
        items = []
        for name in zin.namelist():
            data = zin.read(name)
            if name.endswith('.model'):
                xml = data.decode('utf-8', errors='replace')
                def repl(_m):
                    v, t = next(mesh_iter)
                    return repair.build_mesh_block(v, t)
                new_xml = re.sub(r'<mesh>.*?</mesh>', repl, xml, flags=re.S)
                data = new_xml.encode('utf-8')
            items.append((name, data))
    tmp_target = os.path.join(tmpdir, 'updated_3mf.zip') if tmpdir else out_3mf + '.tmp'
    with zipfile.ZipFile(tmp_target, 'w', zipfile.ZIP_DEFLATED) as zout:
        for name, data in items:
            zout.writestr(name, data)
    os.replace(tmp_target, out_3mf)


def _auto_multi(src, out, tmpdir, ctx, base_method, result, tried):
    repair = _repair_mod()
    in_objs = _safe_load(src) if os.path.exists(src) else []
    analysis_objects = _analyze_objects(src, ctx)
    object_reports = result.get('object_reports', [])
    n_objs = len(object_reports)
    out_objs = _safe_load(out) if os.path.exists(out) else []

    final_meshes = []
    any_escalated = False

    for idx, rep in enumerate(object_reports):
        if _is_obj_watertight(rep):
            rep['method_used'] = {
                'num': base_method.num,
                'id': base_method.id,
                'name': base_method.name,
                'source': 'auto_baseline',
            }
            if idx < len(out_objs):
                final_meshes.append((out_objs[idx][1], out_objs[idx][2]))
            continue

        in_v = in_objs[idx][1] if idx < len(in_objs) else None
        in_t = in_objs[idx][2] if idx < len(in_objs) else None
        in_name = in_objs[idx][0] if idx < len(in_objs) else None

        if in_v is None or in_t is None or len(in_t) == 0:
            rep['method_used'] = {
                'num': base_method.num,
                'id': base_method.id,
                'name': base_method.name,
                'source': 'auto_baseline',
            }
            if idx < len(out_objs):
                final_meshes.append((out_objs[idx][1], out_objs[idx][2]))
            continue

        obj_analysis = analysis_objects[idx] if idx < len(analysis_objects) else None
        if obj_analysis is None:
            obj_analysis = object_analysis.analyze_mesh(in_v, in_t)

        recs = rank_methods(obj_analysis)
        attempts = 0
        adopted = False

        for rec in recs:
            method = get_method(rec.num)
            if method is None or method.num == base_method.num:
                continue
            ok, _reason = method.available()
            if not ok:
                continue
            if method.invents_geometry and rec.score < AUTO_INVENT_MIN_SCORE:
                continue
            if attempts >= AUTO_MAX_ATTEMPTS:
                break
            attempts += 1

            try:
                trial_rep = method.run(in_v, in_t, tmpdir, ctx)
                trial_v = trial_rep.pop('_verts')
                trial_t = trial_rep.pop('_tris')
            except Exception as e:
                tried.append({'num': method.num, 'id': method.id, 'name': method.name,
                              'outcome': 'rejected', 'watertight': False, 'holes': None,
                              'non_manifold': None, 'hausdorff_rel': None,
                              'geom_change_pct': None, 'reason': str(e)})
                continue

            trial_stl = os.path.join(tmpdir, 'trial_obj_%d_%d.stl' % (idx, method.num))
            try:
                repair.save_mesh(trial_stl, trial_v, trial_t)
            except Exception:
                pass

            evaluation = _evaluate(trial_rep, trial_stl, False, [(in_name, in_v, in_t)], method)
            tried.append(_record(method, evaluation, 'accepted'
                                 if evaluation['watertight'] else 'rejected'))

            if evaluation['watertight']:
                try:
                    trial_rep['defects'] = repair.detect_defects(trial_v, trial_t)
                    trial_rep['_fp'] = history.mesh_fingerprint(trial_v, trial_t)
                    _rc = repair.repair_confidence(trial_rep)
                    trial_rep['repair_confidence'] = _rc['score']
                    trial_rep['repair_confidence_label'] = _rc['label']
                    trial_rep['repair_confidence_factors'] = _rc['factors']
                    _rs = repair_score.compute_scores(trial_rep)
                    trial_rep['repair_health'] = _rs['health']
                    trial_rep['repair_health_factors'] = _rs['health_factors']
                    trial_rep['repair_risk'] = _rs['risk']
                    trial_rep['repair_risk_factors'] = _rs['risk_factors']
                    trial_rep['repair_status'] = _rs['status']
                    trial_rep['repair_status_code'] = _rs['status_code']
                except Exception:
                    pass

                mu = {
                    'num': method.num,
                    'id': method.id,
                    'name': method.name,
                    'source': 'auto_escalated',
                }
                if method.invents_geometry:
                    mu['note'] = 'back surface was estimated'
                trial_rep['method_used'] = mu

                object_reports[idx] = trial_rep
                final_meshes.append((trial_v, trial_t))
                adopted = True
                any_escalated = True
                break

        if not adopted:
            rep['method_used'] = {
                'num': base_method.num,
                'id': base_method.id,
                'name': base_method.name,
                'source': 'auto_baseline',
            }
            if idx < len(out_objs):
                final_meshes.append((out_objs[idx][1], out_objs[idx][2]))

    if any_escalated and len(final_meshes) == n_objs:
        _replace_3mf_meshes(out, out, final_meshes, tmpdir)

    result['object_reports'] = object_reports
    if object_reports:
        result['stage1'] = object_reports[0].get('stage1', {})
        s2 = next((r['stage2'] for r in object_reports if 'stage2' in r), None)
        if s2 is not None:
            result['stage2'] = s2
    result['objects_watertight'] = sum(
        1 for r in object_reports
        if r.get('stage1', {}).get('two_manifold')
        and r.get('stage1', {}).get('holes_remaining', 0) == 0
        and bool(r.get('stage2', {}).get('ok')))
    result['objects_stage2_ok'] = sum(
        1 for r in object_reports if bool(r.get('stage2', {}).get('ok')))
    all_watertight = (result['objects_watertight'] == n_objs)

    top_method = None
    if any_escalated:
        for r in object_reports:
            mu = r.get('method_used', {})
            if mu.get('source') == 'auto_escalated':
                top_method = get_method(mu.get('num'))
                break
    if top_method is None:
        top_method = base_method

    combined = object_analysis.combine(analysis_objects) if analysis_objects \
        else object_analysis.ObjectAnalysis()
    all_recs = rank_methods(combined)

    _attach(result, top_method, tried, analysis_objects, all_recs,
            note=None, source='auto_escalated' if any_escalated else 'auto_baseline',
            reached=all_watertight)
    return result


# --- execution policy -------------------------------------------------------

def repair_with_methods(src, out, tmpdir, methods=None, auto_escalation=True,
                         **kwargs):
    """Repair a file, optionally through an explicit list of method numbers.

    ``methods=None`` is auto: run today's default pipeline first (baseline);
    when it is strict-watertight and passes the shape guard the output is
    byte-identical to today. Only when it fails are ranked methods tried (at
    most ``AUTO_MAX_ATTEMPTS`` extras, geometry-inventing ones only above
    ``AUTO_INVENT_MIN_SCORE``), under the wall-clock budget
    ``min(max(AUTO_FALLBACK_MIN_S, AUTO_FALLBACK_BUDGET_FACTOR * baseline_s),
    AUTO_FALLBACK_MAX_S)``: once exceeded, no further method is started and the
    tried methods are reported with the "fallback budget reached" note. A
    baseline that already took longer than ``AUTO_FALLBACK_MAX_S`` skips the
    fallback entirely with the "fallback skipped: baseline too slow" note.
    ``methods=[2,3,5]`` tries exactly those, in order, is never budgeted, and
    keeps the best candidate when none reaches watertight.

    ``auto_escalation=False`` keeps the auto baseline but never escalates to
    extra methods (used when the user tagged only an external engine: the tags
    mean "exactly these", so the untagged method ranking must not run).
    """
    ctx = dict(kwargs)
    ext = os.path.splitext(src)[1].lower()
    multi = (ext == '.3mf'
             and len(_repair_mod().parse_3mf_meshes(src)) > 1)
    if methods is None:
        return _auto(src, out, tmpdir, ctx, multi, ext, bool(auto_escalation))
    return _explicit(src, out, tmpdir, ctx, multi, ext, list(methods))


def _auto_escalation_allowed(ctx):
    """Auto escalation only applies to the default deep-repair configuration.

    An explicit ``--deep-repair off|local`` (which is also how
    ``--no-fallback-ftetwild`` resolves) or a disabled fTetWild tier expresses
    the user's intent; in that case the untagged run keeps today's exact
    behaviour (no extra methods are tried), including the "deep repair
    available" offer.
    """
    return (ctx.get('deep_repair') in (None, 'full')
            and ctx.get('ftetwild') is not False)


def _auto(src, out, tmpdir, ctx, multi, ext, allow_escalation=True):
    base_method = get_method(_baseline_num(ctx))
    baseline_start = time.monotonic()
    result = _attempt(src, out, tmpdir, ctx, multi)
    baseline_s = time.monotonic() - baseline_start
    evaluation = _evaluate(result, out, multi, None, base_method)
    tried = [_record(base_method, evaluation, 'accepted'
                     if evaluation['watertight'] else 'rejected')]
    # A hard error (malformed input, crash) is returned as today; escalation
    # cannot turn it into a success.
    if evaluation['watertight'] or (isinstance(result, dict) and 'error' in result):
        _attach(result, base_method, tried, None, None, source='auto_baseline',
                reached=evaluation['watertight'])
        if multi and isinstance(result, dict) and 'object_reports' in result:
            for rep in result['object_reports']:
                rep['method_used'] = {
                    'num': base_method.num,
                    'id': base_method.id,
                    'name': base_method.name,
                    'source': 'auto_baseline',
                }
        return result
    # Escalation requires the default deep-repair configuration AND that the
    # user did not pin an external engine (tags mean "exactly these").
    if not allow_escalation or not _auto_escalation_allowed(ctx):
        _attach(result, base_method, tried, None, None, source='auto_baseline',
                reached=False)
        if multi and isinstance(result, dict) and 'object_reports' in result:
            for rep in result['object_reports']:
                rep['method_used'] = {
                    'num': base_method.num,
                    'id': base_method.id,
                    'name': base_method.name,
                    'source': 'auto_baseline',
                }
        return result
    # A baseline that already ran past the hard ceiling leaves no room for a
    # bounded fallback: skip the ranked methods entirely and record why.
    if baseline_s > AUTO_FALLBACK_MAX_S:
        _attach(result, base_method, tried, None, None,
                note='fallback skipped: baseline too slow',
                source='auto_baseline', reached=False)
        result['method_used']['fallback_skipped'] = True
        result['method_used']['baseline_s'] = round(baseline_s, 2)
        return result

    if multi and isinstance(result, dict) and 'object_reports' in result:
        return _auto_multi(src, out, tmpdir, ctx, base_method, result, tried)

    in_objs = _safe_load(src) if os.path.exists(src) else []
    analysis_objects = _analyze_objects(src, ctx)
    combined = object_analysis.combine(analysis_objects) if analysis_objects \
        else object_analysis.ObjectAnalysis()
    recs = rank_methods(combined)
    attempts = 0
    fallback_start = time.monotonic()
    fallback_budget = min(max(AUTO_FALLBACK_MIN_S,
                              AUTO_FALLBACK_BUDGET_FACTOR * baseline_s),
                          AUTO_FALLBACK_MAX_S)
    budget_reached = False
    for rec in recs:
        method = get_method(rec.num)
        if method is None or method.num == base_method.num:
            continue
        ok, _reason = method.available()
        if not ok:
            continue
        if method.invents_geometry and rec.score < AUTO_INVENT_MIN_SCORE:
            continue
        if attempts >= AUTO_MAX_ATTEMPTS:
            break
        if time.monotonic() - fallback_start >= fallback_budget:
            budget_reached = True
            break
        attempts += 1
        trial_out = os.path.join(tmpdir, 'method_%d%s' % (method.num, ext))
        trial = _attempt(src, trial_out, tmpdir, {**ctx, **method.kwargs},
                         multi)
        evaluation = _evaluate(trial, trial_out, multi, in_objs, method)
        tried.append(_record(method, evaluation, 'accepted'
                             if evaluation['watertight'] else 'rejected'))
        if evaluation['watertight']:
            shutil.copyfile(trial_out, out)
            note = ('back surface was estimated'
                    if method.invents_geometry else _multi_note(multi))
            _attach(trial, method, tried, analysis_objects, recs,
                    note=note, source='auto_escalated', reached=True)
            return trial
    # Nothing beat the baseline: keep the best candidate (the baseline output
    # and report are the only ones auto can adopt, since adopting requires a
    # strict-watertight result). When the wall-clock fallback budget was
    # reached, the tried methods are still reported with the budget note.
    note = ('fallback budget reached' if budget_reached else _multi_note(multi))
    _attach(result, base_method, tried, analysis_objects, recs,
            note=note, source='auto_baseline', reached=False)
    if budget_reached:
        result['method_used']['budget_reached'] = True
        result['method_used']['fallback_budget_s'] = round(fallback_budget, 2)
    return result


def _explicit(src, out, tmpdir, ctx, multi, ext, methods):
    in_objs = _safe_load(src) if os.path.exists(src) else []
    # An explicitly tagged run already knows which methods to execute, so the
    # (expensive) per-object analysis/ranking is not needed here: recommendations
    # come from ``--analyze`` / the GUI *Analyze* action. Skipping it keeps a
    # tagged run as quick as the plain pipeline.
    analysis_objects = []
    recs = []
    tried = []
    candidates = []
    for num in methods:
        method = get_method(num)
        if method is None:
            tried.append({'num': num, 'id': None, 'name': None,
                          'outcome': 'error', 'reason': 'unknown method'})
            continue
        ok, reason = method.available()
        if not ok:
            tried.append({'num': method.num, 'id': method.id,
                          'name': method.name, 'outcome': 'unavailable',
                          'reason': reason})
            continue
        trial_out = os.path.join(tmpdir, 'method_%d%s' % (method.num, ext))
        trial = _attempt(src, trial_out, tmpdir, {**ctx, **method.kwargs},
                         multi)
        evaluation = _evaluate(trial, trial_out, multi, in_objs, method)
        tried.append(_record(method, evaluation, 'accepted'
                             if evaluation['watertight'] else 'rejected'))
        candidates.append((evaluation, method, trial, trial_out))
        if evaluation['watertight']:
            shutil.copyfile(trial_out, out)
            note = ('back surface was estimated'
                    if method.invents_geometry else _multi_note(multi))
            _attach(trial, method, tried, analysis_objects, recs,
                    note=note, source='tagged', reached=True)
            return trial

    if candidates:
        def rank_key(cand):
            ev = cand[0]
            return (
                ev['holes'] if ev['holes'] is not None else (1 << 30),
                ev['non_manifold'] if ev['non_manifold'] is not None else (1 << 30),
                ev['hausdorff_rel'] if ev['hausdorff_rel'] is not None else 1e18,
            )
        evaluation, method, trial, trial_out = min(candidates, key=rank_key)
        shutil.copyfile(trial_out, out)
        for record in tried:
            if record.get('num') == method.num:
                record['outcome'] = 'best'
        _attach(trial, method, tried, analysis_objects, recs,
                note=_multi_note(multi), source='tagged', reached=False)
        return trial

    # Every requested method was unknown/unavailable: honest, explicit failure.
    result = {'input': src, 'output': out,
              'error': 'no requested method could run'}
    _attach(result, get_method(methods[0]) if methods else None, tried,
            analysis_objects, recs, source='tagged', reached=False)
    return result


# --- text/JSON formatting for the CLI ---------------------------------------

def format_methods():
    lines = ['Repair methods:',
             '  num  id                    family      available  name']
    for method in all_methods():
        ok, reason = method.available()
        lines.append('  %-4d %-21s %-11s %-10s %s%s' % (
            method.num, method.id, method.family, 'yes' if ok else 'no',
            method.name, '' if ok else '  (%s)' % reason))
    return '\n'.join(lines)


def methods_json():
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


def format_recommendations(recs):
    lines = []
    for r in recs:
        lines.append('  %-4d %-21s score=%.2f  %s' % (
            r.num, r.id, r.score, r.reason))
    return '\n'.join(lines) if lines else '  (none)'


def analyze_report(path, engine='experimental', extra_features=False):
    """CLI ``--analyze`` payload for one file: analysis + recommendations +
    a SEPARATE external-engines section."""
    result = {'input': path}
    try:
        objs = object_analysis.analyze_file(
            path, engine=engine, extra_features=extra_features)
        if not objs:
            raise ValueError('no mesh objects found in 3MF')
        combined = object_analysis.combine(objs)
        recs = [dict(r._asdict()) for r in rank_methods(combined)]
        if len(objs) == 1:
            result['analysis'] = object_analysis.to_dict(objs[0])
            result['recommendations'] = recs
        else:
            result['objects'] = len(objs)
            result['object_analyses'] = [object_analysis.to_dict(o) for o in objs]
            result['recommendations'] = recs
        result['external_engines'] = external_engines()
    except Exception as e:  # noqa: BLE001
        result['error'] = 'analysis failed: %s' % e
    return result


def format_analyze_human(result):
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
