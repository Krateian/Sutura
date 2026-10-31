# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura Triage Engine - auto escalation policy and intensity profiles.

Contains:
  1. Intensity presets and user profiles:
     Knobs for deep-repair ladder, fTetWild budget/size cap, dense decimation,
     and Hausdorff sample counts.
  2. Auto repair execution policy (triage):
     Budgeted escalation from baseline to ranked fallback methods.
"""
import dataclasses
from dataclasses import dataclass
import json
import os
import shutil
import sys
import tempfile
import time
from typing import Any, Callable, Dict, List, Optional, Tuple, Union

import classification
from sutura_engine import health as method_health
from sutura_engine.policy import AUTO as _AUTO
from sutura_engine.policy import GATES as _GATES
from sutura_engine.policy import INTENSITY as _INTENSITY_CFG
from sutura_engine.policy import SI_ESCALATION as _SI


def _repair_mod():
    """The canonical pipeline facade (single implementation instance)."""
    from sutura_engine import pipeline
    return pipeline

def _methods_mod():
    for name in ('methods', 'sutura.methods', 'sutura_engine.methods'):
        m = sys.modules.get(name)
        if m is not None and hasattr(m, 'all_methods'):
            return m
    from sutura_engine import methods
    return methods

def _analysis_mod():
    for name in ('sutura_engine.diagnosis', 'sutura_engine.analysis',
                 'object_analysis', 'sutura.object_analysis'):
        m = sys.modules.get(name)
        if m is not None and hasattr(m, 'analyze_file'):
            return m
    from sutura_engine import diagnosis
    return diagnosis

# --- Execution policy constants ---------------------------------------------

AUTO_MAX_ATTEMPTS = int(_AUTO['max_attempts'])
# The fallback ladder is COUNT-budgeted: at most AUTO_MAX_ATTEMPTS candidate
# methods are tried, each with its own deterministic tier budget.  The former
# elapsed-time budget (min/max seconds scaled by the baseline duration) decided
# from machine load and could skip a later method on a busy machine; it is gone.
# A very high wall-clock watchdog survives only as a batch safety net: when it
# trips it is reported explicitly, never silently treated as a normal ladder
# stop.  On an unloaded machine it is never reached.
AUTO_FALLBACK_WATCHDOG_S = float(_AUTO['fallback_watchdog_s'])
# Backward-compatible aliases (older configs/tests imported these names); they
# no longer influence method selection.
AUTO_FALLBACK_BUDGET_FACTOR = float(_AUTO['fallback_budget_factor'])
AUTO_FALLBACK_MIN_S = float(_AUTO['fallback_min_s'])
AUTO_FALLBACK_MAX_S = float(_AUTO['fallback_max_s'])
AUTO_INVENT_MIN_SCORE = float(_AUTO['invent_min_score'])
GENERATIVE_MAX_HAUSDORFF_REL = float(_AUTO['generative_max_hausdorff_rel'])
# SI escalation (auto path): after the final output is saved, when that file is
# reload-watertight but keeps residual self-intersections, try the configured
# candidate methods in order and keep the first result that is never-worse on
# the file-measured shape facts: SI strictly lower; holes, non-manifold and
# parts not higher; two-sided max/p95 within the sutura-verify tolerance.
# The work branch reuses the existing 120M graft/grid ceiling, not the generic
# 6M fallback cap, and no wall-clock limit.  A future surface-preserving SI
# candidate is selected by adding its method number to SI_ESCALATION_CANDIDATES
# (or ctx['si_escalation_methods']); the trigger and verdict stay unchanged.
SI_ESCALATION_METHOD_NUM = int(_SI['method_num'])
SI_ESCALATION_CANDIDATES = (SI_ESCALATION_METHOD_NUM,)
SI_ESCALATION_VOL_REL_TOL = float(_SI['vol_rel_tol'])
SI_ESCALATION_DIST_TOL_MIN_MM = float(_SI['dist_tol_min_mm'])
SI_ESCALATION_DIST_TOL_MAX_MM = float(_SI['dist_tol_max_mm'])
SI_ESCALATION_DIST_TOL_DIV = float(_SI['dist_tol_div'])

# Damage-channel pruning: skip fallback candidates that the J2 method
# scorecard proves futile for the diagnosed damage. ON by default; opt out
# with SUTURA_TRIAGE_PRUNE=0 (falsy). Direct diagnosis-v2 channels only.
AUTO_PRUNE_ENV = 'SUTURA_TRIAGE_PRUNE'

# --- Intensity Profiles -----------------------------------------------------

LadderEntry = Union[float, str]

INTENSITIES = ('quick', 'balanced', 'thorough', 'extreme')
DEFAULT_INTENSITY = _INTENSITY_CFG['default']
INTENSITY_ENV = 'SUTURA_INTENSITY'
INTENSITY_CONFIG = os.path.expanduser('~/.config/sutura/config.json')
PROFILES_CONFIG = os.path.expanduser('~/.config/sutura/profiles.json')
PROFILES_SCHEMA_VERSION = 1
HAUSDORFF_FLOOR = int(_GATES['tuning']['hausdorff_floor'])
DEEP_REPAIR_MODES = ('off', 'local', 'full')

_INVALID = object()


@dataclass(frozen=True)
class IntensitySpec:
    """Resolved post-Stage-1 knobs for one intensity preset."""
    name: str
    ftetwild_enabled: bool = True
    ftetwild_max_faces: Optional[int] = 300000
    ftetwild_timeout: float = 180.0
    graft_max_faces: Optional[int] = 2000000
    graft_timeout: float = 300.0
    ftetwild_optimize_retry_on_dense_fail: bool = False
    deep_repair: str = 'full'
    dense_target_ladder: tuple = (1.5, 3.0)
    dense_ratio: int = 4
    dense_min_faces: int = 20000
    ftetwild_hausdorff_samples: int = 200000
    base: Optional[str] = None
    overrides: dict = dataclasses.field(default_factory=dict, compare=False)


def _preset_from_policy(name):
    """Build one preset from the IntensitySpec schema defaults plus the policy
    overrides for ``name`` (a nullable cap uses the strict "none" sentinel)."""
    raw = dict(_INTENSITY_CFG.get(name) or {})
    for cap in ('ftetwild_max_faces', 'graft_max_faces'):
        if raw.get(cap) == 'none':
            raw[cap] = None
    if 'dense_target_ladder' in raw:
        raw['dense_target_ladder'] = tuple(raw['dense_target_ladder'])
    return dataclasses.replace(IntensitySpec(name=name), **raw)


_POLICY_PRESET_NAMES = tuple(k for k in _INTENSITY_CFG if k != 'default')
if _POLICY_PRESET_NAMES != INTENSITIES:
    raise ValueError('policy [intensity] presets %r != %r'
                     % (_POLICY_PRESET_NAMES, INTENSITIES))

PRESETS = {name: _preset_from_policy(name) for name in INTENSITIES}

_PROFILE_FIELDS = tuple(f.name for f in dataclasses.fields(IntensitySpec)
                        if f.name not in ('name', 'base', 'overrides'))


def _warn(msg: str) -> None:
    try:
        print('sutura: warning: %s' % msg, file=sys.stderr)
    except Exception:
        pass


def _coerce_ladder(value: Any) -> Any:
    if not isinstance(value, (list, tuple)):
        return _INVALID
    out = []
    for item in value:
        if item == 'threshold':
            out.append('threshold')
        elif isinstance(item, bool):
            return _INVALID
        elif isinstance(item, (int, float)) and float(item) > 0:
            out.append(float(item))
        else:
            return _INVALID
    return tuple(out)


def _coerce_field(field: str, value: Any) -> Any:
    if field in ('ftetwild_enabled', 'ftetwild_optimize_retry_on_dense_fail'):
        return value if isinstance(value, bool) else _INVALID
    if field in ('ftetwild_max_faces', 'graft_max_faces'):
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return _INVALID
        return int(value) if int(value) > 0 else None
    if field in ('ftetwild_timeout', 'graft_timeout'):
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return _INVALID
        return float(value) if float(value) > 0 else _INVALID
    if field == 'deep_repair':
        return value if value in DEEP_REPAIR_MODES else _INVALID
    if field == 'dense_target_ladder':
        return _coerce_ladder(value)
    if field in ('dense_ratio', 'dense_min_faces', 'ftetwild_hausdorff_samples'):
        if isinstance(value, bool) or not isinstance(value, int):
            return _INVALID
        if field == 'dense_ratio' and value < 1:
            return _INVALID
        return value if value >= 0 else _INVALID
    return _INVALID


def _normalize_entry(name: str, raw: Any) -> Optional[dict]:
    if not isinstance(raw, dict):
        return None
    if 'overrides' in raw:
        base = raw.get('base')
        overrides = raw.get('overrides')
    else:
        base = raw.get('base')
        overrides = {k: v for k, v in raw.items() if k != 'base'}
    if base not in PRESETS:
        if base is not None:
            _warn("profile %r: unknown base preset %r; using %r"
                  % (name, base, DEFAULT_INTENSITY))
        base = DEFAULT_INTENSITY
    if not isinstance(overrides, dict):
        _warn("profile %r: 'overrides' is not an object; ignored" % name)
        overrides = {}
    return {'base': base, 'overrides': dict(overrides)}


def _build_spec(name: str, base: str, overrides: dict, quiet: bool = False) -> Tuple[IntensitySpec, dict]:
    base = base if base in PRESETS else DEFAULT_INTENSITY
    applied = {}
    for key, value in overrides.items():
        if key not in _PROFILE_FIELDS:
            if not quiet:
                _warn("profile %r: unknown field %r ignored" % (name, key))
            continue
        coerced = _coerce_field(key, value)
        if coerced is _INVALID:
            if not quiet:
                _warn("profile %r: invalid value for %s (%r) ignored"
                      % (name, key, value))
            continue
        applied[key] = coerced
    floor = applied.get('ftetwild_hausdorff_samples')
    if floor is not None and floor < HAUSDORFF_FLOOR:
        if not quiet:
            _warn("profile %r: ftetwild_hausdorff_samples %d is below the "
                  "%d floor; clamped" % (name, floor, HAUSDORFF_FLOOR))
        applied['ftetwild_hausdorff_samples'] = HAUSDORFF_FLOOR
    spec = dataclasses.replace(PRESETS[base], name=name, base=base,
                               overrides=dict(applied), **applied)
    return spec, applied


def name_error(name: str, profiles: dict, exclude: Optional[str] = None) -> Optional[str]:
    if not isinstance(name, str) or not name.strip():
        return 'empty'
    candidate = name.strip()
    if candidate.lower() in PRESETS:
        return 'reserved'
    for existing in profiles:
        if existing == exclude:
            continue
        if existing.lower() == candidate.lower():
            return 'duplicate'
    return None


def unique_profile_name(profiles: dict, base: str) -> str:
    base = (base or 'profile').strip() or 'profile'
    if name_error(base, profiles) is None:
        return base
    index = 2
    while True:
        candidate = '%s %d' % (base, index)
        if name_error(candidate, profiles) is None:
            return candidate
        index += 1


def rename_profile(profiles: dict, old: str, new: str) -> Optional[str]:
    error = name_error(new, profiles, exclude=old)
    if error is not None:
        return error
    profiles[new.strip()] = profiles.pop(old)
    return None


def duplicate_profile(profiles: dict, src: str) -> Optional[str]:
    entry = profiles.get(src)
    if entry is None:
        return None
    new = unique_profile_name(profiles, '%s copy' % src)
    profiles[new] = {'base': entry.get('base', DEFAULT_INTENSITY),
                     'overrides': dict(entry.get('overrides', {}))}
    return new


def load_profiles(path: Optional[str] = None) -> dict:
    target = PROFILES_CONFIG if path is None else path
    try:
        with open(target, encoding='utf-8') as f:
            data = json.load(f)
    except FileNotFoundError:
        return {}
    except (OSError, ValueError) as exc:
        _warn('cannot read intensity profiles from %s (%s); using built-in '
              'presets only' % (target, exc))
        return {}
    if not isinstance(data, dict):
        _warn('intensity profiles file %s is not an object; using built-in '
              'presets only' % target)
        return {}
    version = data.get('schema_version', data.get('version'))
    if version != PROFILES_SCHEMA_VERSION:
        _warn('intensity profiles file %s has schema_version %r (expected '
              '%d); using built-in presets only'
              % (target, version, PROFILES_SCHEMA_VERSION))
        return {}
    raw = data.get('profiles')
    if not isinstance(raw, dict):
        _warn('intensity profiles file %s has no "profiles" object; using '
              'built-in presets only' % target)
        return {}
    out = {}
    seen = {}
    for name, entry in raw.items():
        if not isinstance(name, str) or not name.strip():
            _warn('ignoring a profile with an invalid name (%r)' % (name,))
            continue
        lower = name.lower()
        if lower in seen:
            _warn('ignoring duplicate profile name %r (case-insensitive clash '
                  'with %r)' % (name, seen[lower]))
            continue
        if lower in PRESETS:
            _warn('ignoring profile %r: it shadows a built-in preset' % name)
            continue
        seen[lower] = name
        normalized = _normalize_entry(name, entry)
        if normalized is not None:
            out[name] = normalized
    return out


def save_profiles(profiles: dict, path: Optional[str] = None) -> None:
    target = PROFILES_CONFIG if path is None else path
    directory = os.path.dirname(target) or '.'
    os.makedirs(directory, exist_ok=True)
    payload = {'schema_version': PROFILES_SCHEMA_VERSION,
               'profiles': profiles}
    fd, tmp = tempfile.mkstemp(dir=directory, prefix='.profiles-',
                               suffix='.tmp')
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            json.dump(payload, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, target)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def resolve_intensity_detail(cli_value=None, environ=None, config_path=None,
                             user_profiles=None, profiles_path=None):
    profiles = load_profiles(profiles_path) if user_profiles is None \
        else user_profiles
    path = INTENSITY_CONFIG if config_path is None else config_path
    config_value = None
    try:
        with open(path) as f:
            config_value = json.load(f).get('intensity')
    except (OSError, ValueError, AttributeError):
        pass
    env_value = (os.environ if environ is None else environ).get(INTENSITY_ENV)
    for value in (cli_value, env_value, config_value):
        if value in PRESETS:
            return PRESETS[value], {}
        if value in profiles:
            entry = _normalize_entry(value, profiles[value])
            if entry is not None:
                spec, applied = _build_spec(value, entry['base'],
                                            entry['overrides'])
                return spec, {'base': entry['base'], 'overrides': applied}
    return PRESETS[DEFAULT_INTENSITY], {}


def resolve_intensity(cli_value=None, environ=None, config_path=None,
                      user_profiles=None, profiles_path=None):
    return resolve_intensity_detail(cli_value, environ, config_path,
                                    user_profiles, profiles_path)[0]


def effective_spec(name: str, profiles: Optional[dict] = None, profiles_path: Optional[str] = None) -> Optional[IntensitySpec]:
    if name in PRESETS:
        return PRESETS[name]
    loaded = load_profiles(profiles_path) if profiles is None else profiles
    entry = loaded.get(name)
    if entry is None:
        return None
    normalized = _normalize_entry(name, entry)
    if normalized is None:
        return None
    spec, _ = _build_spec(name, normalized['base'], normalized['overrides'])
    return spec


def list_intensities(profiles: Optional[dict] = None, profiles_path: Optional[str] = None) -> List[dict]:
    loaded = load_profiles(profiles_path) if profiles is None else profiles
    rows = [{'name': n, 'kind': 'preset', 'base': None, 'spec': PRESETS[n]}
            for n in INTENSITIES]
    for name, entry in loaded.items():
        normalized = _normalize_entry(name, entry)
        if normalized is not None:
            spec, _ = _build_spec(name, normalized['base'], normalized['overrides'])
            rows.append({'name': name, 'kind': 'profile',
                         'base': normalized['base'], 'spec': spec})
    return rows


def triage_report_fields(spec: Optional[IntensitySpec]) -> dict:
    if spec is None:
        return {'triage_intensity': DEFAULT_INTENSITY}
    out = {'triage_intensity': spec.name}
    if spec.base:
        out['triage_profile_base'] = spec.base
        out['triage_overrides'] = dict(spec.overrides)
    return out


def _format_row(row: dict) -> str:
    spec = row['spec']
    cap = 'no-limit' if spec.ftetwild_max_faces is None \
        else str(spec.ftetwild_max_faces)
    ftetwild = ('on(cap=%s,timeout=%gs,optretry=%s)'
                % (cap, spec.ftetwild_timeout,
                   'yes' if spec.ftetwild_optimize_retry_on_dense_fail
                   else 'no')) if spec.ftetwild_enabled else 'off'
    ladder = ','.join(str(x) for x in spec.dense_target_ladder) or '-'
    gcap = 'no-limit' if spec.graft_max_faces is None \
        else str(spec.graft_max_faces)
    graft = 'graft(cap=%s,timeout=%gs)' % (gcap, spec.graft_timeout)
    base = ' [base=%s]' % spec.base if spec.base else ''
    return ('%-20s ftetwild=%s deep_repair=%s ladder=%s ratio=%d '
            'min_faces=%d hausdorff=%d %s%s'
            % (row['name'], ftetwild, spec.deep_repair, ladder,
               spec.dense_ratio, spec.dense_min_faces,
               spec.ftetwild_hausdorff_samples, graft, base))


def format_intensities(profiles: Optional[dict] = None, profiles_path: Optional[str] = None) -> str:
    rows = list_intensities(profiles, profiles_path)
    lines = ['Intensity presets (built-in, read-only):']
    for row in rows:
        if row['kind'] == 'preset':
            lines.append('  ' + _format_row(row))
    profiles_rows = [r for r in rows if r['kind'] == 'profile']
    lines.append('User profiles (base + effective values):')
    if not profiles_rows:
        lines.append('  (none)')
    for row in profiles_rows:
        lines.append('  ' + _format_row(row))
    return '\n'.join(lines)


def profile_overrides(base_spec: IntensitySpec, values: dict) -> dict:
    out = {}
    for field, new in values.items():
        if field not in _PROFILE_FIELDS:
            continue
        if new != getattr(base_spec, field):
            out[field] = new
    return out


# --- Execution policy helpers -----------------------------------------------

def _resolve_fn(name: str, default_fn: Callable) -> Callable:
    """Resolve a hook from sys.modules to honor test monkeypatching."""
    for mod_name in ('methods', 'sutura.methods'):
        mod = sys.modules.get(mod_name)
        if mod is not None and hasattr(mod, name):
            return getattr(mod, name)
    return default_fn


def _load_objects(path: str) -> list:
    """[(model_name|None, verts, tris)] for a file (one per 3MF object)."""
    from sutura_engine.pipeline.mesh_io import load_meshes
    return load_meshes(path)


def _safe_load(path: str) -> list:
    try:
        return _load_objects(path)
    except Exception:
        return []


def one_sided_hausdorff(in_v, in_t, out_v, out_t, samples=None):
    """One-sided input -> output Hausdorff, relative to the input bbox diagonal."""
    from sutura_engine.pipeline import utils
    if samples is None:
        samples = utils.HAUSDORFF_SAMPLES
    return utils._hausdorff_rel(None, in_v, in_t, out_v, out_t, samples=samples)


def _worst_hausdorff(in_objs, out_objs):
    """Worst per-object one-sided Hausdorff, or None when not comparable."""
    if not in_objs or len(in_objs) != len(out_objs):
        return None
    worst = None
    for (_in_name, in_v, in_t), (_out_name, out_v, out_t) in zip(in_objs, out_objs):
        mx, _mean = one_sided_hausdorff(in_v, in_t, out_v, out_t)
        if mx is None:
            continue
        worst = mx if worst is None else max(worst, mx)
    return worst


def _worst_hausdorff_input_to_output(in_objs, out_objs):
    """Worst per-object input -> output Hausdorff, or None when not comparable."""
    if not in_objs or len(in_objs) != len(out_objs):
        return None
    try:
        from sutura_engine.methods import closing as _closing
    except Exception:
        try:
            import closing as _closing
        except Exception:
            return None
    worst = None
    for (_in_name, in_v, in_t), (_out_name, out_v, out_t) in zip(in_objs, out_objs):
        mx, _mean = _closing.one_sided_hausdorff(in_v, in_t, out_v, out_t)
        if mx is None:
            continue
        worst = mx if worst is None else max(worst, mx)
    return worst


def _strict_holes_nm(objs, weld=False):
    """Total holes + non-manifold regions over ``objs``."""
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


def _adopt_guard_for_method(result, in_objs, out_objs, method,
                             out_holes, out_nm):
    """Central face/vertex + SI guard for one method attempt.

    Runs for every method, including ``self_guarded`` tiers: their own gate is
    still authoritative inside the tier, but the method output can add
    geometry in stage 2 or through a tier that is only loosely checked.  The
    policy is per method (see ``adopt_guard.policy_for``); Tamponade's plug
    faces are its slack, fTetWild's SI increase is a flag when holes/nm fall.
    """
    from sutura_engine.pipeline import adopt_guard

    policy = adopt_guard.policy_for(getattr(method, 'id', ''))
    slack_units = 0
    if getattr(method, 'id', '') == 'tamponade':
        tamp = result.get('tamponade') if isinstance(result, dict) else None
        if isinstance(tamp, dict):
            slack_units = int(tamp.get('plug_faces') or 0)
    improved = False
    if getattr(method, 'id', '') in ('ftetwild', 'graft'):
        in_holes, in_nm = _strict_holes_nm(in_objs, weld=True)
        improved = (out_holes < in_holes or out_nm < in_nm)
    in_faces = in_vertices = out_faces = out_vertices = 0
    for (_in_name, iv, it), (_out_name, ov, ot) in zip(in_objs, out_objs):
        in_faces += len(it)
        in_vertices += len(iv)
        out_faces += len(ot)
        out_vertices += len(ov)
        if len(in_objs) == len(out_objs):
            reason = adopt_guard.blowup_reason(
                len(it), len(iv), len(ot), len(ov), slack_units, policy)
            if reason is not None:
                fields = {'input_faces': len(it), 'input_vertices': len(iv),
                          'candidate_faces': len(ot),
                          'candidate_vertices': len(ov),
                          'slack_units': slack_units,
                          'guard_rejected': reason,
                          'guard_reason': reason,
                          'si_policy': policy.si_policy,
                          'si_checked': False, 'si_flagged': False,
                          'scope': 'object'}
                return adopt_guard.GuardResult(False, reason, fields)
    base_si = cand_si = None
    si_over_budget = False
    si_units = None
    si_budget = None
    try:
        from sutura_engine.pipeline import work_estimate as _we
        si_budget = _we.si_work_budget()
        si_units = _si_pair_work_units(in_objs) + _si_pair_work_units(out_objs)
        si_over_budget = not _we.fits_budget(si_units, si_budget)
    except Exception:  # noqa: BLE001 - an estimate must never block repair
        si_over_budget = False
    # The cap applies to the FINAL stage-1 scan (pipeline.py), never to the
    # adopt judge: a single candidate check is affordable and accepting an
    # unmeasured candidate could silently raise the self-intersection count.
    # Only the input baseline is memoised, so the per-candidate cost stays one
    # exact scan of the candidate.
    try:
        base_si = _base_si_count(in_objs, adopt_guard)
        cand_values = [adopt_guard.exact_si_count(ov, ot)
                       for _n, ov, ot in out_objs]
        if all(v is not None for v in cand_values):
            cand_si = sum(int(v) for v in cand_values)
    except Exception:
        base_si = cand_si = None
    guard = adopt_guard.check_candidate(
        in_faces, in_vertices, out_faces, out_vertices,
        slack_units=slack_units, base_si=base_si, candidate_si=cand_si,
        measure_si=False, improved_topology=improved, policy=policy)
    guard.fields['si_checked'] = (base_si is not None and cand_si is not None)
    if si_over_budget:
        guard.fields.update({
            'si_work_units': int(si_units),
            'si_work_budget': (None if si_budget is None else int(si_budget)),
            'si_candidate_over_budget': True,
        })
    return guard


# The input self-intersection count does not depend on the candidate, and the
# exact classifier is the expensive part.  Keep one entry per live object list
# (identity-checked, so a recycled ``id()`` can never reuse a stale count).
_BASE_SI_CACHE = {}


def _base_si_count(in_objs, adopt_guard):
    """Exact input SI count, memoised for the current object list."""
    key = id(in_objs)
    hit = _BASE_SI_CACHE.get(key)
    if hit is not None and hit[0] is in_objs:
        return hit[1]
    try:
        values = [adopt_guard.exact_si_count(iv, it)
                  for _n, iv, it in in_objs]
        count = (sum(int(v) for v in values)
                 if all(v is not None for v in values) else None)
    except Exception:  # noqa: BLE001 - unmeasurable SI is reported, not fatal
        count = None
    _BASE_SI_CACHE.clear()
    _BASE_SI_CACHE[key] = (in_objs, count)
    return count


def _si_pair_work_units(objs):
    """Deterministic exact-SI pair-work estimate for a list of meshes."""
    from sutura_engine.pipeline import work_estimate as _we
    from sutura_engine.pipeline.utils import boundary_loop_stats
    import topology
    total = 0
    for _name, verts, tris in objs or []:
        if len(tris) == 0:
            continue
        try:
            top = topology.mesh_topology(verts, tris)
            nm_e = int(top.get('non_two_manifold_edges', 0) or 0)
            nm_v = int(top.get('non_two_manifold_vertices', 0) or 0)
            holes = int(boundary_loop_stats(verts, tris)[0] or 0)
        except Exception:  # noqa: BLE001
            nm_e = nm_v = holes = 0
        total += _we.si_pair_work(len(tris), nm_e, nm_v, holes)
    return int(total)


def _evaluate(result, path, multi, in_objs, method):
    """Evaluate one attempt against strict-watertight + shape guard."""
    rec = {'watertight': False, 'holes': None, 'non_manifold': None,
           'hausdorff_rel': None, 'geom_change_pct': None, 'reason': None,
           'guard_rejected': None, 'guard_reason': None,
           'si_flagged': False, 'adopt_guard': None}
    if not isinstance(result, dict) or 'error' in result:
        rec['reason'] = (result or {}).get('error', 'no result')
        return rec
    category, _issues, _key = classification.classify(result)
    # Join (#4) moves whole components, so it can change the surface even though
    # it is not flagged ``invents_geometry`` (marking it so would also change
    # the fallback ranking and the "back surface was estimated" note).  Give it
    # the same one-sided Hausdorff gate here only, using the existing
    # GENERATIVE_MAX_HAUSDORFF_REL threshold.
    is_join = bool(method is not None and getattr(method, 'id', '') == 'join')
    shape_gated = bool(method is not None
                       and (getattr(method, 'invents_geometry', False) or is_join))
    needs_objs = bool(shape_gated and in_objs
                      and not getattr(method, 'self_guarded', False))
    out_objs = None
    if category == 'watertight':
        holes = nm = 0
    else:
        try:
            out_objs = _load_objects(path)
        except Exception as e:
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
    if method is not None and in_objs:
        if out_objs is None:
            try:
                out_objs = _load_objects(path)
            except Exception as e:
                rec['watertight'] = False
                rec['reason'] = 'cannot read output for adopt guard: %s' % e
                return rec
        if len(in_objs) == len(out_objs) and out_objs:
            guard = _adopt_guard_for_method(result, in_objs, out_objs,
                                            method, holes, nm)
            rec['adopt_guard'] = guard.fields
            rec['guard_rejected'] = guard.reason
            rec['guard_reason'] = guard.fields.get('guard_reason')
            rec['si_flagged'] = bool(guard.fields.get('si_flagged'))
            if not guard.ok:
                rec['watertight'] = False
                rec['reason'] = 'adopt guard: %s' % guard.reason
                return rec
    if is_join:
        # Preserve the non-gated reporting Join had before (the gate covers the
        # shape change; geom_change_pct stays an informational metric).
        rec['geom_change_pct'] = _worst_geom_change(result)
    if needs_objs:
        if out_objs is None:
            try:
                out_objs = _load_objects(path)
            except Exception as e:
                rec['reason'] = 'cannot read output: %s' % e
                return rec
        if method.guard_input_to_output:
            hd = _worst_hausdorff_input_to_output(in_objs, out_objs)
        else:
            hd = _worst_hausdorff(in_objs, out_objs)
        rec['hausdorff_rel'] = hd
        gen_max_hd = getattr(sys.modules.get('methods'), 'GENERATIVE_MAX_HAUSDORFF_REL', GENERATIVE_MAX_HAUSDORFF_REL)
        if hd is None:
            # C3: do not skip the gate silently.  The decision stays as before
            # (accept on an unmeasurable deviation), but the report says so.
            if is_join:
                rec['hausdorff_unknown'] = True
        elif hd > gen_max_hd:
            rec['watertight'] = False
            rec['reason'] = ('shape changed (one-sided Hausdorff %.3f > %.3f)'
                             % (hd, gen_max_hd))
            return rec
    else:
        if is_join:
            # No comparable input objects (e.g. an unreadable/multi-object
            # input): the gate cannot run, so flag it instead of skipping.
            rec['hausdorff_unknown'] = True
        rec['geom_change_pct'] = _worst_geom_change(result)
    rec['reason'] = 'strict-watertight'
    return rec


def _attempt(src, out, tmpdir, kwargs, multi):
    repair = _repair_mod()
    with method_health.tier_guard('pipeline', 'attempt') as _g:
        if multi:
            return repair.repair_3mf(src, out, tmpdir, **kwargs)
        return repair.repair_file(src, out, tmpdir, **kwargs)
    if _g.error is not None:
        if isinstance(_g.error, getattr(repair, 'ExtremeRemovedAllError', ())):
            raise _g.error
        return {'input': src, 'output': out,
                'error': 'repair failed: %s' % _g.error}


def _baseline_num(kwargs):
    mode = kwargs.get('deep_repair')
    if mode == 'local':
        return 2
    if mode == 'off':
        return 1
    return 3


def _record(method, evaluation, outcome, template=None, elapsed_ms=None):
    rec = {
        'num': method.num,
        'id': method.id,
        'name': method.name,
        'outcome': outcome,
        'watertight': bool(evaluation['watertight']),
        'holes': evaluation['holes'],
        'non_manifold': evaluation['non_manifold'],
        'hausdorff_rel': evaluation['hausdorff_rel'],
        'geom_change_pct': evaluation['geom_change_pct'],
        'guard_rejected': evaluation.get('guard_rejected'),
        'guard_reason': evaluation.get('guard_reason'),
        'si_flagged': bool(evaluation.get('si_flagged')),
        'adopt_guard': evaluation.get('adopt_guard'),
        'reason': evaluation['reason'],
        # Learning-triage inputs (anonymous: template + method slug only).
        'template': template,
        'elapsed_ms': int(elapsed_ms) if elapsed_ms is not None else None,
    }
    if evaluation.get('hausdorff_unknown'):
        rec['hausdorff_unknown'] = True
    return rec


def _analyze_objects(src, ctx):
    try:
        return _analysis_mod().analyze_file(
            src, engine=ctx.get('engine', 'experimental'),
            extra_features=ctx.get('extra_features', False))
    except Exception:
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
        result['analysis'] = [_analysis_mod().to_dict(o) for o in analysis_objects]
        result['recommendations'] = [dict(r._asdict()) for r in recs]


def _multi_note(multi):
    return None


def _is_obj_watertight(rep):
    s1 = (rep or {}).get('stage1', {})
    s2 = (rep or {}).get('stage2', {})
    return bool(s1.get('two_manifold')) and s1.get('holes_remaining', 0) == 0 and bool(s2.get('ok'))


def _replace_3mf_meshes(in_3mf, out_3mf, mesh_list, tmpdir=None,
                        attr_source=None):
    repair = _repair_mod()
    container = out_3mf if os.path.exists(out_3mf) else in_3mf
    return repair.rewrite_3mf_meshes(
        container, out_3mf, list(mesh_list),
        attr_source=attr_source or in_3mf)


def _auto_multi(src, out, tmpdir, ctx, base_method, result, tried):
    repair = _repair_mod()
    in_objs = _safe_load(src) if os.path.exists(src) else []
    analysis_objects = _analyze_objects(src, ctx)
    object_reports = result.get('object_reports', [])
    n_objs = len(object_reports)
    out_objs = _safe_load(out) if os.path.exists(out) else []

    eval_fn = _resolve_fn('_evaluate', _evaluate)
    rank_fn = _resolve_fn('rank_methods', _methods_mod().rank_methods)

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
            obj_analysis = _analysis_mod().analyze_mesh(
                in_v, in_t, engine=ctx.get('engine', 'experimental'),
                extra_features=ctx.get('extra_features', False))

        recs = rank_fn(obj_analysis)
        # NOTE: unlike single-mesh _auto, this multi-object path does NOT
        # re-order Graft (#13) before fTetWild (#7) nor apply the shared
        # healthy-deviation tie-break. The deep-repair ladder runs per object
        # in the baseline, so no in-repo fixture reaches #13/#7 here; the
        # divergence is documented rather than changed without a measurement
        # (see the C6 feasibility gate in P1b findings).
        attempts = 0
        adopted = False

        auto_invent_min_score = getattr(sys.modules.get('methods'), 'AUTO_INVENT_MIN_SCORE', AUTO_INVENT_MIN_SCORE)
        auto_max_attempts = getattr(sys.modules.get('methods'), 'AUTO_MAX_ATTEMPTS', AUTO_MAX_ATTEMPTS)
        for rec in recs:
            method = _methods_mod().get_method(rec.num)
            if method is None or method.num == base_method.num:
                continue
            ok, _reason = method.available()
            if not ok:
                continue
            if method.invents_geometry and rec.score < auto_invent_min_score:
                continue
            if attempts >= auto_max_attempts:
                break
            attempts += 1

            _t0 = time.monotonic()
            with method_health.tier_guard('method:%s' % method.id, 'run') as _gm:
                trial_rep = method.run(in_v, in_t, tmpdir, ctx)
                trial_v = trial_rep.pop('_verts')
                trial_t = trial_rep.pop('_tris')
            if _gm.error is not None:
                tried.append({'num': method.num, 'id': method.id, 'name': method.name,
                              'outcome': 'rejected', 'watertight': False, 'holes': None,
                              'non_manifold': None, 'hausdorff_rel': None,
                              'geom_change_pct': None, 'reason': str(_gm.error),
                              'template': rec.template,
                              'elapsed_ms': int((time.monotonic() - _t0) * 1000)})
                continue

            trial_stl = os.path.join(tmpdir, 'trial_obj_%d_%d.stl' % (idx, method.num))
            try:
                repair.save_mesh(trial_stl, trial_v, trial_t)
            except Exception:
                pass

            evaluation = eval_fn(trial_rep, trial_stl, False, [(in_name, in_v, in_t)], method)
            tried.append(_record(method, evaluation, 'accepted'
                                 if evaluation['watertight'] else 'rejected',
                                 template=rec.template,
                                 elapsed_ms=(time.monotonic() - _t0) * 1000))

            if evaluation['watertight']:
                try:
                    import history
                    import repair_score
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
        stats = _replace_3mf_meshes(
            out, out, final_meshes, tmpdir, attr_source=src)
        for rep, st in zip(object_reports, stats or []):
            rep['triangle_attributes'] = st
        # The top-level aggregate was built by the baseline repair_3mf before
        # escalation; refresh it from the now-final per-object stats so it can
        # never report stale counts.
        agg_attrs = repair.aggregate_triangle_attributes(object_reports)
        if agg_attrs is not None:
            result['triangle_attributes'] = agg_attrs

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
                top_method = _methods_mod().get_method(mu.get('num'))
                break
    if top_method is None:
        top_method = base_method

    combined = _analysis_mod().combine(analysis_objects) if analysis_objects \
        else _analysis_mod().ObjectAnalysis()
    all_recs = rank_fn(combined)

    _attach(result, top_method, tried, analysis_objects, all_recs,
            note=None, source='auto_escalated' if any_escalated else 'auto_baseline',
            reached=all_watertight)
    return result


def repair_with_methods(src: str, out: str, tmpdir: str, methods: Optional[list] = None,
                        auto_escalation: bool = True, **kwargs) -> dict:
    """Repair a file, optionally through an explicit list of method numbers."""
    ctx = dict(kwargs)
    ext = os.path.splitext(src)[1].lower()
    multi = (ext == '.3mf'
             and sum(len(v) for v in
                     _repair_mod().parse_3mf_meshes(src).values()) > 1)
    if methods is None:
        result = _auto(src, out, tmpdir, ctx, multi, ext,
                       bool(auto_escalation))
        if (auto_escalation and _auto_escalation_allowed(ctx)
                and ctx.get('si_mode') != 'off'):
            result = _si_escalate_final(
                src, out, tmpdir, ctx, multi, result, ext)
        elif isinstance(result, dict) and 'error' not in result:
            result['si_escalation'] = {
                'triggered': False, 'method': None, 'num': None,
                'name': None, 'candidates': [],
                'reason': 'disabled by user settings',
                'baseline': _si_metrics(result),
            }
        return result
    return _explicit(src, out, tmpdir, ctx, multi, ext, list(methods))


def _auto_escalation_allowed(ctx: dict) -> bool:
    return (ctx.get('deep_repair') in (None, 'full')
            and ctx.get('ftetwild') is not False)


def _graft_fidelity(result):
    """``(fidelity_ok, healthy_deviation, coverage_ok)`` from a Graft trial
    report, or ``(None, None, None)`` for a non-Graft / older report."""
    if not isinstance(result, dict):
        return None, None, None
    g = result.get('graft')
    if not isinstance(g, dict):
        return None, None, None
    return (g.get('fidelity_ok'), g.get('hausdorff_healthy'),
            g.get('coverage_ok'))


def _triage_prune_enabled(environ=None) -> bool:
    """Damage-channel pruning is ON by default; ``SUTURA_TRIAGE_PRUNE=0``
    (or false/no/off) restores the unpruned fallback order."""
    raw = (os.environ if environ is None else environ).get(AUTO_PRUNE_ENV)
    if raw is None:
        return True
    return str(raw).strip().lower() not in ('0', 'false', 'no', 'off')


def _prune_avoid_nums(analysis) -> set:
    """Fallback method numbers the diagnosed damage makes futile.

    Reads only the direct diagnosis-v2 channels: ``duplicate`` > 0 skips #4
    Join; ``interior`` > 0 (or an interior region) skips #11 Transplant and
    #15 Wall Thicken. #15 is already dropped by rank_methods
    (needs_user_input) and is kept here for defence. The hole_porosity/loops
    rule is deferred until the multi-defect calibration set exists.
    """
    d2 = getattr(analysis, 'diagnosis_v2', None)
    if not isinstance(d2, dict):
        return set()
    channels = d2.get('channels') or {}

    def _count(name):
        entry = channels.get(name)
        v = entry.get('count') if isinstance(entry, dict) else None
        return v if isinstance(v, (int, float)) else 0

    avoid = set()
    if _count('duplicate') > 0:
        avoid.add(4)
    has_interior = _count('interior') > 0 or any(
        isinstance(r, dict) and r.get('channel') == 'interior'
        for r in (d2.get('regions') or []))
    if has_interior:
        avoid.update({11, 15})
    return avoid


def _fallback_input_work(in_objs):
    """Deterministic damage counters of the original input (one pass).

    Returns ``{'faces', 'nm_edges', 'nm_vertices', 'holes'}``; a topology
    failure yields zeros and the caller's estimate falls back to the face
    count only.
    """
    counts = {'faces': 0, 'nm_edges': 0, 'nm_vertices': 0, 'holes': 0}
    for _name, verts, tris in in_objs or []:
        counts['faces'] += int(len(tris))
    try:
        import topology
        from sutura_engine.pipeline.utils import boundary_loop_stats
        for _name, verts, tris in in_objs or []:
            if len(tris) == 0:
                continue
            top = topology.mesh_topology(verts, tris)
            counts['nm_edges'] += int(
                top.get('non_two_manifold_edges', 0) or 0)
            counts['nm_vertices'] += int(
                top.get('non_two_manifold_vertices', 0) or 0)
            counts['holes'] += int(boundary_loop_stats(verts, tris)[0] or 0)
    except Exception:  # noqa: BLE001 - an estimate must never block repair
        pass
    return counts


def _fallback_grid_work(in_objs, ctx):
    """Graft grid work estimate summed over every input object, or 0.

    The damage counters already sum all objects; the grid estimate must use
    the same scope or a multi-object 3MF can be skipped on the first object's
    cost alone.
    """
    if not in_objs:
        return 0
    try:
        from sutura_engine import graft as _graft
        from sutura_engine.pipeline import work_estimate as _we
        intensity = (getattr(ctx.get('triage_spec'), 'base', None)
                     or ctx.get('intensity'))
        total = 0
        for _name, verts, tris in in_objs:
            if len(tris) == 0:
                continue
            cells, _voxel = _graft.estimate_grid_cells(
                verts, tris, intensity=intensity)
            counts = _fallback_input_work([(None, verts, tris)])
            total += int(_we.grid_work_with_holes(
                len(tris), cells, counts.get('holes', 0)))
        return total
    except Exception:  # noqa: BLE001 - estimate only
        return 0


def _attach_fallback_budget(result, work_budget, work_units, skipped):
    """Expose the deterministic fallback budget on the method report."""
    if not isinstance(result, dict):
        return
    mu = result.setdefault('method_used', {})
    mu['fallback_work_budget'] = (None if work_budget is None
                                  else int(work_budget))
    mu['fallback_work_units'] = int(work_units)
    if skipped:
        mu['fallback_skipped_budget'] = [dict(s) for s in skipped]


def _si_metrics(result):
    """Runtime SI/parts/deviation metrics from one repair report.

    Deviation is the existing pipeline/report measure:
    ``print_report.geometry.deviation_max_pct`` (pipeline/report.py
    ``_deviation_pct``, Rust BVH, 150k-face cap).  Volume is the same report's
    ``geometry.volume_change_pct`` (stage-1 percentage).
    """
    s1 = (result or {}).get('stage1') or {}
    geom = (((result or {}).get('print_report') or {}).get('geometry') or {})

    def _num(v):
        return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None

    vol = _num(geom.get('volume_change_pct'))
    if vol is None:
        vol = _num(s1.get('volume_change_percent'))
    return {
        'si': _num(s1.get('self_intersections_remaining')),
        'holes': _num(s1.get('holes_remaining')),
        'nm': _num(s1.get('non_manifold_edges_remaining')),
        'parts': _num(s1.get('components')),
        'dev_pct': _num(geom.get('deviation_max_pct')),
        'vol_pct': vol,
    }


def _exact_si_path(path):
    """Exact output SI count (the scorecard measure), or None.

    Uses the existing adopt-guard exact classifier; an unmeasurable count
    (None) means "do not trigger and keep the baseline".
    """
    try:
        from sutura_engine.pipeline import adopt_guard
        objs = _load_objects(path)
        vals = [adopt_guard.exact_si_count(v, t) for _n, v, t in objs]
        if not vals or any(v is None for v in vals):
            return None
        return int(sum(int(v) for v in vals))
    except Exception:  # noqa: BLE001 - unmeasurable => no trigger
        return None


def _si_candidate_nums(ctx):
    """Ordered SI-escalation candidate method numbers (default: Scaffold)."""
    raw = (ctx or {}).get('si_escalation_methods')
    if raw is None:
        return list(SI_ESCALATION_CANDIDATES)
    if isinstance(raw, (int, str)):
        raw = [raw]
    nums = []
    try:
        for item in raw:
            try:
                nums.append(int(item))
            except (TypeError, ValueError):
                continue
    except TypeError:
        return list(SI_ESCALATION_CANDIDATES)
    return nums or list(SI_ESCALATION_CANDIDATES)


def _si_tolerance_mm(in_objs):
    """The sutura-verify tolerance: clamp(bbox_diag/10000, 0.0125, 0.05) mm."""
    try:
        import numpy as np
        diag = 0.0
        for _name, verts, _tris in in_objs or []:
            v = np.asarray(verts, dtype=np.float64)
            if len(v):
                diag = max(diag, float(np.linalg.norm(v.max(0) - v.min(0))))
        if not (diag > 0.0):
            return None
        return min(max(diag / SI_ESCALATION_DIST_TOL_DIV,
                       SI_ESCALATION_DIST_TOL_MIN_MM),
                   SI_ESCALATION_DIST_TOL_MAX_MM)
    except Exception:  # noqa: BLE001 - unmeasurable => no adoption
        return None


def _si_two_sided_shape(in_objs, out_objs):
    """(max, p95) symmetric vertex-to-surface distance (mm), or None."""
    if not in_objs or not out_objs or len(in_objs) != len(out_objs):
        return None
    try:
        import numpy as np
        import sutura_geom
    except Exception:  # noqa: BLE001 - unmeasurable => no adoption
        return None
    vals = []
    max_d = 0.0
    try:
        for (_n, vi, ti), (_m, vo, to) in zip(in_objs, out_objs):
            if not (len(vi) and len(ti) and len(vo) and len(to)):
                return None
            vi = np.asarray(vi, dtype=np.float64)
            ti = np.asarray(ti, dtype=np.int32)
            vo = np.asarray(vo, dtype=np.float64)
            to = np.asarray(to, dtype=np.int32)
            _x, d_o2i, _f = sutura_geom.closest_points(vi, ti, vo)
            _x, d_i2o, _f = sutura_geom.closest_points(vo, to, vi)
            d_o2i = np.asarray(d_o2i, dtype=np.float64)
            d_i2o = np.asarray(d_i2o, dtype=np.float64)
            if len(d_o2i) == 0 or len(d_i2o) == 0:
                return None
            vals.append(d_o2i)
            vals.append(d_i2o)
            max_d = max(max_d, float(d_o2i.max()), float(d_i2o.max()))
        all_d = np.concatenate(vals)
        return (round(max_d, 4),
                round(float(np.percentile(all_d, 95)), 4))
    except Exception:  # noqa: BLE001 - unmeasurable => no adoption
        return None


def _si_file_facts(path, in_objs):
    """Reload-honest topology + input-relative shape facts for one output."""
    try:
        from defects import detect
        import topology
    except Exception:  # noqa: BLE001 - unmeasurable => no adoption
        return None
    try:
        out_objs = _load_objects(path)
    except Exception:  # noqa: BLE001 - unreadable => no adoption
        return None
    if not out_objs:
        return None
    holes = 0
    nm = 0
    parts = 0
    volume = 0.0
    have_volume = True
    try:
        for _name, verts, tris in out_objs:
            found = detect(verts, tris)
            holes += len(found['holes'])
            nm += len(found['non_manifold'])
            topo = topology.mesh_topology(verts, tris)
            parts += int(topo.get('connected_components_number') or 0)
            geom = topology.mesh_geometric_measures(verts, tris)
            vol = geom.get('mesh_volume')
            if isinstance(vol, (int, float)) and not isinstance(vol, bool):
                volume += float(vol)
            else:
                have_volume = False
    except Exception:  # noqa: BLE001 - unmeasurable => no adoption
        return None
    shape = _si_two_sided_shape(in_objs, out_objs)
    if shape is None:
        return None
    return {'holes': int(holes), 'nm': int(nm), 'parts': int(parts),
            'volume_mm3': round(volume, 6) if have_volume else None,
            'shape_max_mm': shape[0], 'shape_p95_mm': shape[1]}


def _si_verdict(base, cand, tol):
    """Never-worse SI-escalation rule; returns (ok, reason).

    ``tol`` is the sutura-verify distance tolerance in mm.  Both the two-sided
    max and p95 must stay within ``baseline + tol``; every other measure must
    be non-increasing, and SI must be strictly lower.
    """
    if base.get('si') is None:
        return False, 'baseline SI unmeasurable'
    if base['si'] <= 0:
        return False, 'baseline SI is zero'
    if cand.get('si') is None:
        return False, 'candidate SI unmeasurable'
    if cand['si'] >= base['si']:
        return False, 'SI not strictly lower'
    for key, label in (('holes', 'holes'), ('nm', 'non-manifold'),
                       ('parts', 'parts')):
        b, c = base.get(key), cand.get(key)
        if b is None:
            return False, 'baseline %s unmeasurable' % label
        if c is None:
            return False, 'candidate %s unmeasurable' % label
        if c > b:
            return False, '%s worse (%s -> %s)' % (label, b, c)
    if tol is None:
        return False, 'shape tolerance unmeasurable'
    for key, label in (('shape_max_mm', 'two-sided max'),
                       ('shape_p95_mm', 'two-sided p95')):
        b, c = base.get(key), cand.get(key)
        if b is None:
            return False, 'baseline %s unmeasurable' % label
        if c is None:
            return False, 'candidate %s unmeasurable' % label
        if c > b + tol:
            return False, '%s worse (%.4f -> %.4f mm > %.4f + %.4f)' % (
                label, b, c, b, tol)
    bv, cv = base.get('volume_mm3'), cand.get('volume_mm3')
    if bv is None or cv is None:
        # Both outputs are closed at this point; without their signed volumes
        # the never-worse claim cannot be made (the external harness may still
        # see a regression), so keep the baseline.
        return False, 'volume unmeasurable'
    if abs(bv) <= 1e-12:
        return False, 'baseline volume is zero'
    rel = abs(cv - bv) / abs(bv)
    if rel > SI_ESCALATION_VOL_REL_TOL:
        return False, 'volume change %.3f%% > %.2f%% (%.3f -> %.3f mm3)' % (
            rel * 100.0, SI_ESCALATION_VOL_REL_TOL * 100.0, bv, cv)
    return True, ('never-worse under the two-sided SI-escalation rule '
                  '(tol %.4f mm)' % tol)


def _si_escalation(src, out, tmpdir, ctx, multi, base_result, tried,
                   attempt_fn, eval_fn, ext, trial_src=None):
    """Try the SI candidates in order on the final saved output.

    Returns the adopted trial dict, or None with
    ``base_result['si_escalation']`` set to the audited outcome.  The trigger
    is the final reload-watertight output with exact SI > 0; the first
    candidate that passes the never-worse verdict wins.  Never raises into the
    repair path.
    """
    base = _si_metrics(base_result)
    base['si_stage1'] = base.get('si')
    field = {'triggered': False, 'method': None, 'num': None, 'name': None,
             'candidates': [], 'si_source': 'exact_output', 'baseline': base}
    if multi:
        field['reason'] = 'multi_object'
        base_result['si_escalation'] = field
        return None
    base['si'] = _exact_si_path(out)
    if base.get('si') is None:
        field['reason'] = 'baseline SI unmeasurable'
        base_result['si_escalation'] = field
        return None
    if base['si'] <= 0:
        field['reason'] = 'baseline SI is zero'
        base_result['si_escalation'] = field
        return None

    in_objs = _safe_load(src)
    tol = _si_tolerance_mm(in_objs)
    field['tolerance_mm'] = tol
    facts = _si_file_facts(out, in_objs)
    if facts is None:
        field['reason'] = 'final output unreadable or shape unmeasurable'
        base_result['si_escalation'] = field
        return None
    base.update(facts)
    if base.get('holes') or base.get('nm'):
        field['reason'] = 'baseline not watertight (holes=%s, nm=%s)' % (
            base.get('holes'), base.get('nm'))
        base_result['si_escalation'] = field
        return None
    if tol is None:
        field['reason'] = 'shape tolerance unmeasurable'
        base_result['si_escalation'] = field
        return None

    field['triggered'] = True
    field['reason'] = 'final output watertight with SI > 0'
    trial_src = src if trial_src is None else trial_src
    try:
        from sutura_engine.pipeline import work_estimate as _we
        counts = _fallback_input_work(in_objs)
        damage_units = _we.damage_work(
            counts['faces'], counts['nm_edges'], counts['nm_vertices'],
            counts['holes'])
        grid_units = _fallback_grid_work(in_objs, ctx)
        budget = _we.graft_grid_work_budget()
        field['work_budget'] = None if budget is None else int(budget)
        field['budget_scope'] = 'si_escalation_120m'
    except Exception as exc:  # noqa: BLE001 - estimate only
        field['outcome'] = 'error'
        field['reason'] = 'estimate: %s: %s' % (type(exc).__name__, exc)
        base_result['si_escalation'] = field
        return None

    for num in _si_candidate_nums(ctx):
        cand_field = {'num': num, 'id': None, 'name': None,
                      'outcome': None, 'reason': None}
        field['candidates'].append(cand_field)
        method = _methods_mod().get_method(num)
        if method is None:
            cand_field['reason'] = 'candidate method unavailable'
            continue
        cand_field['id'] = method.id
        cand_field['name'] = method.name
        ok, why = method.available()
        if not ok:
            cand_field['reason'] = 'candidate unavailable: %s' % why
            continue

        try:
            work_units = _we.candidate_work(
                method.id, damage=damage_units, grid=grid_units,
                faces=int(counts['faces']))
        except Exception as exc:  # noqa: BLE001 - estimate only
            cand_field['outcome'] = 'error'
            cand_field['reason'] = 'estimate: %s: %s' % (
                type(exc).__name__, exc)
            continue
        cand_field['work_units'] = int(work_units)
        cand_field['work_budget'] = (None if budget is None
                                     else int(budget))
        if budget is not None and not _we.fits_budget(work_units, budget):
            cand_field['outcome'] = 'skipped_budget'
            cand_field['reason'] = (
                'estimated %s work %d (budget %d) is above the '
                'SI-escalation cap' % (_we.work_class(method.id),
                                       int(work_units), int(budget)))
            continue

        trial_out = os.path.join(tmpdir, 'si_escalation_m%d%s' % (
            method.num, ext))
        _t0 = time.monotonic()
        try:
            trial = attempt_fn(trial_src, trial_out, tmpdir,
                               {**ctx, **method.kwargs}, multi)
            # Use method=None here: the generic generative d_in gate is not
            # part of the SI-escalation rule; this checks topology only.
            evaluation = eval_fn(trial, trial_out, multi, _safe_load(src),
                                 None)
            tried.append(_record(
                method, evaluation,
                'accepted' if evaluation.get('watertight') else 'rejected',
                elapsed_ms=(time.monotonic() - _t0) * 1000))
        except Exception as exc:  # noqa: BLE001 - the escalation never crashes
            cand_field['outcome'] = 'error'
            cand_field['reason'] = '%s: %s' % (type(exc).__name__, exc)
            cand_field['elapsed_ms'] = int((time.monotonic() - _t0) * 1000)
            continue

        cand = _si_metrics(trial)
        cand['si_stage1'] = cand.get('si')
        cand['si'] = _exact_si_path(trial_out)
        cand_facts = _si_file_facts(trial_out, in_objs)
        if cand_facts is not None:
            cand.update(cand_facts)
        cand_field['candidate'] = cand
        try:
            import hashlib
            with open(trial_out, 'rb') as _f:
                cand_field['candidate_sha256'] = hashlib.sha256(
                    _f.read()).hexdigest()
        except Exception:  # noqa: BLE001 - audit field only
            cand_field['candidate_sha256'] = None
        cand_field['elapsed_ms'] = int((time.monotonic() - _t0) * 1000)
        if not evaluation.get('watertight'):
            cand_field['outcome'] = 'rejected'
            cand_field['reason'] = 'candidate rejected: %s' % (
                evaluation.get('reason') or 'not watertight')
            continue
        ok, why = _si_verdict(base, cand, tol)
        if not ok:
            cand_field['outcome'] = 'rejected'
            cand_field['reason'] = why
            continue

        shutil.copyfile(trial_out, out)
        cand_field['outcome'] = 'adopted'
        cand_field['reason'] = why
        field.update({'method': method.id, 'num': method.num,
                      'name': method.name, 'outcome': 'adopted',
                      'reason': why})
        trial['si_escalation'] = field
        _attach(trial, method, tried, None, None, note='SI escalation',
                source='auto_si_escalated', reached=True)
        return trial

    field['outcome'] = 'rejected'
    last = next((c for c in reversed(field['candidates'])
                 if c.get('candidate') is not None), None)
    if last is None and field['candidates']:
        last = field['candidates'][-1]
    if last is not None:
        field['method'] = last.get('id')
        field['num'] = last.get('num')
        field['name'] = last.get('name')
        field['reason'] = last.get('reason') or 'no candidate adopted'
        field['candidate'] = last.get('candidate')
        field['candidate_sha256'] = last.get('candidate_sha256')
        field['elapsed_ms'] = last.get('elapsed_ms')
    base_result['si_escalation'] = field
    return None


def _si_escalate_final(src, out, tmpdir, ctx, multi, result, ext):
    """Run the SI-escalation checkpoint once on the final saved output."""
    if not isinstance(result, dict):
        return result
    if 'error' in result:
        result.setdefault('si_escalation', {
            'triggered': False, 'method': None, 'num': None, 'name': None,
            'candidates': [], 'reason': 'baseline error',
            'baseline': _si_metrics(result),
        })
        return result
    if not os.path.exists(out):
        result.setdefault('si_escalation', {
            'triggered': False, 'method': None, 'num': None, 'name': None,
            'candidates': [], 'reason': 'final output missing',
            'baseline': _si_metrics(result),
        })
        return result
    tried = result.get('methods_tried')
    if not isinstance(tried, list):
        tried = []
    trial = _si_escalation(
        src, out, tmpdir, ctx, multi, result, tried,
        _resolve_fn('_attempt', _attempt),
        _resolve_fn('_evaluate', _evaluate), ext)
    if trial is None:
        return result
    for key in ('fallback_work_budget', 'fallback_grid_work_budget',
                'fallback_work_input', 'fallback_work_estimate'):
        if key in result and key not in trial:
            trial[key] = result[key]
    if 'fallback_skipped_budget' in result \
            and 'fallback_skipped_budget' not in trial:
        trial['fallback_skipped_budget'] = result['fallback_skipped_budget']
    mu = trial.setdefault('method_used', {})
    for key, value in (result.get('method_used') or {}).items():
        if key.startswith('fallback_') and key not in mu:
            mu[key] = value
    return trial


def _auto(src: str, out: str, tmpdir: str, ctx: dict, multi: bool, ext: str,
          allow_escalation: bool = True) -> dict:
    attempt_fn = _resolve_fn('_attempt', _attempt)
    eval_fn = _resolve_fn('_evaluate', _evaluate)
    rank_fn = _resolve_fn('rank_methods', _methods_mod().rank_methods)

    # Imported lazily: sutura_engine.pipeline's package __init__ imports the
    # top-level ``triage`` shim, so a module-level import here would be circular.
    from sutura_engine.pipeline.order import (
        FTETWILD_METHOD_NUM, GRAFT_METHOD_NUM, ftetwild_supersedes_graft,
        order_graft_before_ftetwild,
    )

    base_method = _methods_mod().get_method(_baseline_num(ctx))
    baseline_start = time.monotonic()
    result = attempt_fn(src, out, tmpdir, ctx, multi)
    baseline_s = time.monotonic() - baseline_start
    evaluation = eval_fn(result, out, multi, None, base_method)
    tried = [_record(base_method, evaluation, 'accepted'
                     if evaluation['watertight'] else 'rejected',
                     elapsed_ms=baseline_s * 1000)]
    if evaluation['watertight'] or (isinstance(result, dict) and 'error' in result):
        if isinstance(result, dict) and 'si_escalation' not in result:
            result['si_escalation'] = {
                'triggered': False, 'method': 'scaffold',
                'num': SI_ESCALATION_METHOD_NUM,
                'reason': ('baseline error' if isinstance(result, dict)
                           and 'error' in result else 'not applicable'),
                'baseline': _si_metrics(result),
            }
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

    auto_watchdog_s = getattr(sys.modules.get('methods'),
                              'AUTO_FALLBACK_WATCHDOG_S',
                              AUTO_FALLBACK_WATCHDOG_S)
    auto_max_attempts = getattr(sys.modules.get('methods'), 'AUTO_MAX_ATTEMPTS', AUTO_MAX_ATTEMPTS)
    auto_invent_min_score = getattr(sys.modules.get('methods'), 'AUTO_INVENT_MIN_SCORE', AUTO_INVENT_MIN_SCORE)

    if multi and isinstance(result, dict) and 'object_reports' in result:
        return _auto_multi(src, out, tmpdir, ctx, base_method, result, tried)

    in_objs = _safe_load(src) if os.path.exists(src) else []
    analysis_objects = _analyze_objects(src, ctx)
    combined = _analysis_mod().combine(analysis_objects) if analysis_objects \
        else _analysis_mod().ObjectAnalysis()
    recs = rank_fn(combined)
    # Graft (#13) is the primary last-resort tier and must be attempted before
    # fTetWild (#7); the templates already prefer it, this guarantees the order
    # even without a template match (the tier order lives in pipeline.order).
    recs = order_graft_before_ftetwild(recs)
    avoid = _prune_avoid_nums(combined) if _triage_prune_enabled() else set()
    # Deterministic fallback work budget (no wall clock): the same input and
    # preset always produce the same skip/adopt decision.  ``SUTURA_FALLBACK_BUDGET=0``
    # disables the gate and restores the unbounded ladder.
    from sutura_engine.pipeline import work_estimate as _we
    from sutura_engine import trace as _trace
    work_budget = _we.fallback_work_budget()
    # Graft's whole-mesh morphology has its own calibrated cap (the explicit
    # Graft path uses the same value in ``graft.shell_wrap``); comparing it
    # with the generic 6 M fallback cap would disable the fallback tier above
    # a few thousand faces even when the explicit tier is allowed to run.
    grid_budget = _we.graft_grid_work_budget()
    _counts = _fallback_input_work(in_objs)
    damage_units = _we.damage_work(
        _counts['faces'], _counts['nm_edges'], _counts['nm_vertices'],
        _counts['holes'])
    grid_units = _fallback_grid_work(in_objs, ctx)
    budget_skips = []
    result['fallback_work_budget'] = (None if work_budget is None
                                      else int(work_budget))
    result['fallback_grid_work_budget'] = (None if grid_budget is None
                                           else int(grid_budget))
    result['fallback_work_input'] = {k: int(v) for k, v in _counts.items()}
    result['fallback_work_estimate'] = {
        'damage_units': int(damage_units),
        'grid_units': int(grid_units),
    }
    attempts = 0
    fallback_start = time.monotonic()
    watchdog_reached = False
    deferred_graft = None  # (healthy_dev, method, trial, trial_out, rec, cov_failed)
    for rec in recs:
        method = _methods_mod().get_method(rec.num)
        if method is None or method.num == base_method.num:
            continue
        if method.num in avoid:
            continue
        ok, _reason = method.available()
        if not ok:
            continue
        if method.invents_geometry and rec.score < auto_invent_min_score:
            continue
        if attempts >= auto_max_attempts:
            break
        work_units = _we.candidate_work(
            getattr(method, 'id', ''), damage=damage_units, grid=grid_units,
            faces=int(_counts['faces']))
        work_cls = _we.work_class(getattr(method, 'id', ''))
        method_budget = work_budget
        budget_scope = 'fallback'
        if work_cls == 'grid' and work_budget is not None:
            method_budget = grid_budget
            budget_scope = 'grid'
        if work_budget is not None and not _we.fits_budget(work_units,
                                                           method_budget):
            reason = ('estimated %s work %d (budget %d) is above the cap; '
                      'method not started, stage-1 result kept'
                      % (work_cls, int(work_units), int(method_budget)))
            budget_skips.append({
                'num': method.num, 'id': method.id, 'name': method.name,
                'outcome': 'skipped_budget', 'reason': reason,
                'work_class': work_cls,
                'work_units': int(work_units),
                'work_budget': int(method_budget),
                'budget_scope': budget_scope,
                'template': rec.template,
            })
            tried.append(budget_skips[-1])
            _trace.set_context(attempt='method_%d' % method.num)
            _trace.record_stage(
                stage='fallback_estimate', method='triage._auto', v_in=None,
                t_in=None, v_out=None, t_out=None,
                gate={'num': method.num, 'id': method.id,
                      'score': rec.score, 'work_class': work_cls,
                      'work_units': int(work_units),
                      'work_budget': int(method_budget),
                      'skipped_budget': True})
            continue
        # Very high out-of-band safety net only; the ladder itself is bounded
        # by the candidate count above.  A trip is surfaced explicitly.
        if (auto_watchdog_s and
                time.monotonic() - fallback_start >= float(auto_watchdog_s)):
            watchdog_reached = True
            break
        attempts += 1
        _t0 = time.monotonic()
        trial_out = os.path.join(tmpdir, 'method_%d%s' % (method.num, ext))
        _trace.set_context(attempt='method_%d' % method.num)
        _trace.record_stage(
            stage='fallback_estimate', method='triage._auto', v_in=None,
            t_in=None, v_out=None, t_out=None,
            gate={'num': method.num, 'id': method.id, 'score': rec.score,
                  'work_units': int(work_units),
                  'work_budget': (None if work_budget is None
                                  else int(work_budget)),
                  'skipped_budget': False})
        trial = attempt_fn(src, trial_out, tmpdir, {**ctx, **method.kwargs},
                           multi)
        evaluation = eval_fn(trial, trial_out, multi, in_objs, method)
        tried.append(_record(method, evaluation, 'accepted'
                             if evaluation['watertight'] else 'rejected',
                             template=rec.template,
                             elapsed_ms=(time.monotonic() - _t0) * 1000))
        _trace.record_stage(
            stage='fallback_attempt', method='triage._auto', v_in=None,
            t_in=None, v_out=None, t_out=None,
            gate={'num': method.num, 'id': method.id,
                  'watertight': bool(evaluation['watertight']),
                  'holes': evaluation['holes'],
                  'non_manifold': evaluation['non_manifold'],
                  'work_units': int(work_units),
                  'work_budget': (None if work_budget is None
                                  else int(work_budget))})
        if evaluation['watertight']:
            fav, hdev, cov = _graft_fidelity(trial)
            cov_failed = cov is False
            if method.num == GRAFT_METHOD_NUM and (fav is False or cov_failed):
                # Watertight but the healthy surface moved beyond tolerance or
                # the result no longer reproduces enough of the input surface
                # (coverage below threshold): keep it as a candidate and let
                # fTetWild be tried; the lower healthy deviation wins.
                if (deferred_graft is None
                        or (hdev is not None
                            and (deferred_graft[0] is None
                                 or hdev < deferred_graft[0]))):
                    deferred_graft = (hdev, method, trial, trial_out, rec,
                                      cov_failed)
                continue
            shutil.copyfile(trial_out, out)
            note = ('back surface was estimated'
                    if method.invents_geometry else _multi_note(multi))
            _attach(trial, method, tried, analysis_objects, recs,
                    note=note, source='auto_escalated', reached=True)
            _attach_fallback_budget(trial, work_budget, damage_units,
                                    budget_skips)
            return trial

    if deferred_graft is not None:
        hdev, method, trial, trial_out, rec, cov_failed = deferred_graft
        ft = _methods_mod().get_method(FTETWILD_METHOD_NUM)
        ft_tried = any(t.get('num') == FTETWILD_METHOD_NUM for t in tried)
        if ft is not None and not ft_tried and ft.available()[0]:
            _t0 = time.monotonic()
            ft_out = os.path.join(tmpdir, 'method_7%s' % ext)
            ft_trial = attempt_fn(src, ft_out, tmpdir, {**ctx, **ft.kwargs},
                                  multi)
            ft_eval = eval_fn(ft_trial, ft_out, multi, in_objs, ft)
            tried.append(_record(ft, ft_eval, 'accepted'
                                 if ft_eval['watertight'] else 'rejected',
                                 elapsed_ms=(time.monotonic() - _t0) * 1000))
            if ft_eval['watertight']:
                fdev = ft_eval.get('hausdorff_rel')
                # One shared tie-break (order.ftetwild_supersedes_graft): a
                # present fTetWild deviation no longer beats a None Graft
                # deviation (the ladder keeps Graft when either is None).
                if ftetwild_supersedes_graft(fdev, hdev, cov_failed):
                    shutil.copyfile(ft_out, out)
                    _attach(ft_trial, ft, tried, analysis_objects, recs,
                            note='lower healthy deviation than Graft',
                            source='auto_escalated', reached=True)
                    _attach_fallback_budget(ft_trial, work_budget,
                                            damage_units, budget_skips)
                    return ft_trial
        if cov_failed:
            # A Graft that failed the input-coverage gate no longer reproduces
            # the input surface: never adopt it as a last resort. Keep the
            # stage-1 result and state the rejection plainly.
            from sutura_engine.graft import GRAFT_MIN_INPUT_COVERAGE
            tg = trial.get('graft') if isinstance(trial, dict) else None
            cov = tg.get('input_coverage') if isinstance(tg, dict) else None
            reason = ('input coverage %s < %.2f, kept stage-1 output'
                      % ('n/a' if cov is None else ('%.4g' % cov),
                         GRAFT_MIN_INPUT_COVERAGE))
            # Surface the rejected Graft on the kept stage-1 report so both the
            # JSON and the --human output state the rejection plainly instead
            # of silently dropping it.
            surf = dict(tg) if isinstance(tg, dict) else {}
            surf['ran'] = True
            surf['adopted'] = False
            surf['reject_reason'] = 'coverage'
            surf['reason'] = reason
            result['graft'] = surf
            for record in tried:
                if record.get('num') == method.num:
                    record['outcome'] = 'rejected'
                    record['reason'] = reason
            _attach(result, base_method, tried, analysis_objects, recs,
                    note='Graft rejected: %s' % reason,
                    source='auto_baseline', reached=False)
            _attach_fallback_budget(result, work_budget, damage_units,
                                    budget_skips)
            return result
        shutil.copyfile(trial_out, out)
        _attach(trial, method, tried, analysis_objects, recs,
                note='back surface was estimated',
                source='auto_escalated', reached=True)
        _attach_fallback_budget(trial, work_budget, damage_units,
                                budget_skips)
        return trial

    if watchdog_reached:
        note = ('fallback watchdog exceeded (%.0fs); the ladder was stopped '
                'explicitly' % float(auto_watchdog_s))
    elif attempts >= auto_max_attempts:
        note = ('fallback attempts exhausted (%d); no further method was tried'
                % int(auto_max_attempts))
    elif budget_skips:
        note = ('fallback work budget skipped %d candidate(s); stage-1 result '
                'kept' % len(budget_skips))
    else:
        note = _multi_note(multi)
    _attach(result, base_method, tried, analysis_objects, recs,
            note=note, source='auto_baseline', reached=False)
    _attach_fallback_budget(result, work_budget, damage_units, budget_skips)
    if watchdog_reached:
        result['method_used']['fallback_watchdog'] = True
        result['method_used']['fallback_watchdog_s'] = float(auto_watchdog_s)
    elif attempts >= auto_max_attempts:
        result['method_used']['fallback_attempts_exhausted'] = True
        result['method_used']['fallback_attempts'] = int(attempts)
    return result


def _explicit(src: str, out: str, tmpdir: str, ctx: dict, multi: bool, ext: str, methods: list) -> dict:
    attempt_fn = _resolve_fn('_attempt', _attempt)
    eval_fn = _resolve_fn('_evaluate', _evaluate)

    in_objs = _safe_load(src) if os.path.exists(src) else []
    analysis_objects = []
    recs = []
    tried = []
    candidates = []
    for num in methods:
        method = _methods_mod().get_method(num)
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
        _t0 = time.monotonic()
        trial_out = os.path.join(tmpdir, 'method_%d%s' % (method.num, ext))
        trial = attempt_fn(src, trial_out, tmpdir, {**ctx, **method.kwargs},
                           multi)
        evaluation = eval_fn(trial, trial_out, multi, in_objs, method)
        tried.append(_record(method, evaluation, 'accepted'
                             if evaluation['watertight'] else 'rejected',
                             elapsed_ms=(time.monotonic() - _t0) * 1000))
        candidates.append((evaluation, method, trial, trial_out))
        if evaluation['watertight']:
            shutil.copyfile(trial_out, out)
            note = ('back surface was estimated'
                    if method.invents_geometry else _multi_note(multi))
            _attach(trial, method, tried, analysis_objects, recs,
                    note=note, source='tagged', reached=True)
            return trial

    candidates = [c for c in candidates if not c[0].get('guard_rejected')]
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

    result = {'input': src, 'output': out,
              'error': 'no requested method could run'}
    _attach(result, _methods_mod().get_method(methods[0]) if methods else None, tried,
            analysis_objects, recs, source='tagged', reached=False)
    return result
