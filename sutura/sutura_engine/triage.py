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
import re
import shutil
import sys
import tempfile
import time
from typing import Any, Callable, Dict, List, Optional, Tuple, Union
import zipfile

import classification

def _repair_mod():
    for name in ('repair', '__main__'):
        mod = sys.modules.get(name)
        if mod is not None and hasattr(mod, 'repair_mesh_from_arrays'):
            return mod
    import repair as _repair
    return _repair

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

AUTO_MAX_ATTEMPTS = 3
AUTO_FALLBACK_BUDGET_FACTOR = 2.0
AUTO_FALLBACK_MIN_S = 30.0
AUTO_FALLBACK_MAX_S = 120.0
AUTO_INVENT_MIN_SCORE = 0.8
GENERATIVE_MAX_HAUSDORFF_REL = 0.01

# --- Intensity Profiles -----------------------------------------------------

LadderEntry = Union[float, str]

INTENSITIES = ('quick', 'balanced', 'thorough', 'extreme')
DEFAULT_INTENSITY = 'balanced'
INTENSITY_ENV = 'SUTURA_INTENSITY'
INTENSITY_CONFIG = os.path.expanduser('~/.config/sutura/config.json')
PROFILES_CONFIG = os.path.expanduser('~/.config/sutura/profiles.json')
PROFILES_SCHEMA_VERSION = 1
HAUSDORFF_FLOOR = 200000
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


PRESETS = {
    'quick': IntensitySpec(
        name='quick',
        ftetwild_enabled=False,
        ftetwild_max_faces=None,
        ftetwild_timeout=180.0,
        deep_repair='off',
        dense_target_ladder=(),
    ),
    'balanced': IntensitySpec(name='balanced'),
    'thorough': IntensitySpec(
        name='thorough',
        ftetwild_timeout=600.0,
        # Graft's own tier budget mirrors fTetWild's per-preset budget so a
        # hard mesh fails over to fTetWild instead of running unbounded.
        graft_timeout=900.0,
        dense_target_ladder=(1.5, 3.0, 'threshold'),
        ftetwild_hausdorff_samples=400000,
    ),
    'extreme': IntensitySpec(
        name='extreme',
        ftetwild_max_faces=None,
        ftetwild_timeout=1800.0,
        graft_max_faces=None,
        graft_timeout=1800.0,
        ftetwild_optimize_retry_on_dense_fail=True,
        dense_target_ladder=(1.5, 3.0, 'threshold'),
        ftetwild_hausdorff_samples=1000000,
    ),
}

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
    return _repair_mod().load_meshes(path)


def _safe_load(path: str) -> list:
    try:
        return _load_objects(path)
    except Exception:
        return []


def one_sided_hausdorff(in_v, in_t, out_v, out_t, samples=None):
    """One-sided input -> output Hausdorff, relative to the input bbox diagonal."""
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


def _evaluate(result, path, multi, in_objs, method):
    """Evaluate one attempt against strict-watertight + shape guard."""
    rec = {'watertight': False, 'holes': None, 'non_manifold': None,
           'hausdorff_rel': None, 'geom_change_pct': None, 'reason': None}
    if not isinstance(result, dict) or 'error' in result:
        rec['reason'] = (result or {}).get('error', 'no result')
        return rec
    category, _issues, _key = classification.classify(result)
    needs_objs = bool(method is not None and method.invents_geometry and in_objs
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
        if hd is not None and hd > gen_max_hd:
            rec['watertight'] = False
            rec['reason'] = ('shape changed (one-sided Hausdorff %.3f > %.3f)'
                             % (hd, gen_max_hd))
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
    except Exception as e:
        if isinstance(e, getattr(repair, 'ExtremeRemovedAllError', ())):
            raise
        return {'input': src, 'output': out, 'error': 'repair failed: %s' % e}


def _baseline_num(kwargs):
    mode = kwargs.get('deep_repair')
    if mode == 'local':
        return 2
    if mode == 'off':
        return 1
    return 3


def _record(method, evaluation, outcome, template=None, elapsed_ms=None):
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
        # Learning-triage inputs (anonymous: template + method slug only).
        'template': template,
        'elapsed_ms': int(elapsed_ms) if elapsed_ms is not None else None,
    }


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


def _replace_3mf_meshes(in_3mf, out_3mf, mesh_list, tmpdir=None):
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
            obj_analysis = _analysis_mod().analyze_mesh(in_v, in_t)

        recs = rank_fn(obj_analysis)
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
            try:
                trial_rep = method.run(in_v, in_t, tmpdir, ctx)
                trial_v = trial_rep.pop('_verts')
                trial_t = trial_rep.pop('_tris')
            except Exception as e:
                tried.append({'num': method.num, 'id': method.id, 'name': method.name,
                              'outcome': 'rejected', 'watertight': False, 'holes': None,
                              'non_manifold': None, 'hausdorff_rel': None,
                              'geom_change_pct': None, 'reason': str(e),
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
             and len(_repair_mod().parse_3mf_meshes(src)) > 1)
    if methods is None:
        return _auto(src, out, tmpdir, ctx, multi, ext, bool(auto_escalation))
    return _explicit(src, out, tmpdir, ctx, multi, ext, list(methods))


def _auto_escalation_allowed(ctx: dict) -> bool:
    return (ctx.get('deep_repair') in (None, 'full')
            and ctx.get('ftetwild') is not False)


def _graft_fidelity(result):
    """``(fidelity_ok, healthy_deviation)`` from a Graft trial report, or
    ``(None, None)`` for a non-Graft / older report."""
    if not isinstance(result, dict):
        return None, None
    g = result.get('graft')
    if not isinstance(g, dict):
        return None, None
    return g.get('fidelity_ok'), g.get('hausdorff_healthy')


def _auto(src: str, out: str, tmpdir: str, ctx: dict, multi: bool, ext: str,
          allow_escalation: bool = True) -> dict:
    attempt_fn = _resolve_fn('_attempt', _attempt)
    eval_fn = _resolve_fn('_evaluate', _evaluate)
    rank_fn = _resolve_fn('rank_methods', _methods_mod().rank_methods)

    base_method = _methods_mod().get_method(_baseline_num(ctx))
    baseline_start = time.monotonic()
    result = attempt_fn(src, out, tmpdir, ctx, multi)
    baseline_s = time.monotonic() - baseline_start
    evaluation = eval_fn(result, out, multi, None, base_method)
    tried = [_record(base_method, evaluation, 'accepted'
                     if evaluation['watertight'] else 'rejected',
                     elapsed_ms=baseline_s * 1000)]
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

    auto_fallback_max_s = getattr(sys.modules.get('methods'), 'AUTO_FALLBACK_MAX_S', AUTO_FALLBACK_MAX_S)
    auto_fallback_min_s = getattr(sys.modules.get('methods'), 'AUTO_FALLBACK_MIN_S', AUTO_FALLBACK_MIN_S)
    auto_fallback_budget_factor = getattr(sys.modules.get('methods'), 'AUTO_FALLBACK_BUDGET_FACTOR', AUTO_FALLBACK_BUDGET_FACTOR)
    auto_max_attempts = getattr(sys.modules.get('methods'), 'AUTO_MAX_ATTEMPTS', AUTO_MAX_ATTEMPTS)
    auto_invent_min_score = getattr(sys.modules.get('methods'), 'AUTO_INVENT_MIN_SCORE', AUTO_INVENT_MIN_SCORE)

    if baseline_s > auto_fallback_max_s:
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
    combined = _analysis_mod().combine(analysis_objects) if analysis_objects \
        else _analysis_mod().ObjectAnalysis()
    recs = rank_fn(combined)
    # Graft (#13) is the primary last-resort tier and must be attempted before
    # fTetWild (#7); the templates already prefer it, this guarantees the order
    # even without a template match.
    _nums = [r.num for r in recs]
    if 13 in _nums and 7 in _nums and _nums.index(13) > _nums.index(7):
        recs.insert(_nums.index(7), recs.pop(_nums.index(13)))
    attempts = 0
    fallback_start = time.monotonic()
    fallback_budget = min(max(auto_fallback_min_s,
                              auto_fallback_budget_factor * baseline_s),
                          auto_fallback_max_s)
    budget_reached = False
    deferred_graft = None  # (healthy_dev, method, trial, trial_out, rec)
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
        if time.monotonic() - fallback_start >= fallback_budget:
            budget_reached = True
            break
        attempts += 1
        _t0 = time.monotonic()
        trial_out = os.path.join(tmpdir, 'method_%d%s' % (method.num, ext))
        trial = attempt_fn(src, trial_out, tmpdir, {**ctx, **method.kwargs},
                           multi)
        evaluation = eval_fn(trial, trial_out, multi, in_objs, method)
        tried.append(_record(method, evaluation, 'accepted'
                             if evaluation['watertight'] else 'rejected',
                             template=rec.template,
                             elapsed_ms=(time.monotonic() - _t0) * 1000))
        if evaluation['watertight']:
            fav, hdev = _graft_fidelity(trial)
            if method.num == 13 and fav is False:
                # Watertight but the healthy surface moved beyond tolerance:
                # keep it as a candidate and let fTetWild be tried; the lower
                # healthy deviation wins.
                if (deferred_graft is None
                        or (hdev is not None
                            and (deferred_graft[0] is None
                                 or hdev < deferred_graft[0]))):
                    deferred_graft = (hdev, method, trial, trial_out, rec)
                continue
            shutil.copyfile(trial_out, out)
            note = ('back surface was estimated'
                    if method.invents_geometry else _multi_note(multi))
            _attach(trial, method, tried, analysis_objects, recs,
                    note=note, source='auto_escalated', reached=True)
            return trial

    if deferred_graft is not None:
        hdev, method, trial, trial_out, rec = deferred_graft
        ft = _methods_mod().get_method(7)
        ft_tried = any(t.get('num') == 7 for t in tried)
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
                if fdev is not None and (hdev is None or fdev < hdev):
                    shutil.copyfile(ft_out, out)
                    _attach(ft_trial, ft, tried, analysis_objects, recs,
                            note='lower healthy deviation than Graft',
                            source='auto_escalated', reached=True)
                    return ft_trial
        shutil.copyfile(trial_out, out)
        _attach(trial, method, tried, analysis_objects, recs,
                note='back surface was estimated',
                source='auto_escalated', reached=True)
        return trial

    note = ('fallback budget reached' if budget_reached else _multi_note(multi))
    _attach(result, base_method, tried, analysis_objects, recs,
            note=note, source='auto_baseline', reached=False)
    if budget_reached:
        result['method_used']['budget_reached'] = True
        result['method_used']['fallback_budget_s'] = round(fallback_budget, 2)
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
