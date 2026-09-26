#!/usr/bin/env python3
"""Sutura Triage Engine - intensity presets and user profiles.

A single source of truth for the "how hard should the repair try" knobs
*after* Stage 1: the deep-repair ladder, the fTetWild tier (on/off, input
size cap, wall-clock budget), the dense-boundary decimation ladder and the
Hausdorff sample count.

A preset NEVER changes Stage 1: the repair mode (``--mode`` / the
classifier) and the optional profile (``--profile``) keep exactly the
behaviour they have today, in every preset. ``balanced`` is the shipped
default and its values ARE the historical ``repair.py`` module constants, so
``--intensity balanced`` (also the default when nothing is given) reproduces
the pre-triage behaviour byte for byte.

The four built-in presets are read-only. A named **user profile** is a base
preset plus a sparse mapping of field overrides, stored in
``~/.config/sutura/profiles.json`` (schema-versioned, atomically written). A
profile is selected exactly like a preset by ``--intensity`` /
``SUTURA_INTENSITY`` / the ``intensity`` config key. A corrupt or unreadable
profiles file never crashes a repair: it is reported on stderr and the
built-in presets are used. The Hausdorff sample-count floor is enforced in
the resolver as well (a hand-edited value below it is clamped with a
warning), not only in the GUI. The watertight + Hausdorff shape guard is
invariant and is deliberately NOT a profile field.
"""
import os
import sys
import json
import tempfile
import dataclasses
from dataclasses import dataclass
from typing import Optional, Union

# A dense-decimation ladder entry is either a float (a multiple of the input
# face count) or the special string 'threshold'
# (``dense_ratio * max(input_faces, dense_min_faces)``).
LadderEntry = Union[float, str]

INTENSITIES = ('quick', 'balanced', 'thorough', 'extreme')
DEFAULT_INTENSITY = 'balanced'
INTENSITY_ENV = 'SUTURA_INTENSITY'
INTENSITY_CONFIG = os.path.expanduser('~/.config/sutura/config.json')
# User profiles live in their own file, NOT inside config.json: config.json is
# rewritten whole by updater.save_config, so a corrupt profiles blob would
# poison every config read; a separate, schema-versioned file isolates it.
PROFILES_CONFIG = os.path.expanduser('~/.config/sutura/profiles.json')
PROFILES_SCHEMA_VERSION = 1
# The shape guard must stay honest on dense boundaries: never below this.
HAUSDORFF_FLOOR = 200000
DEEP_REPAIR_MODES = ('off', 'local', 'full')

_INVALID = object()


@dataclass(frozen=True)
class IntensitySpec:
    """Resolved post-Stage-1 knobs for one intensity preset.

    All fields are immutable, so a preset can be shared safely; a user
    profile builds a new spec with ``dataclasses.replace``. The watertight +
    Hausdorff shape guard is invariant and deliberately not a field here.
    ``base``/``overrides`` are set on a resolved profile (``None``/``{}`` for
    a built-in preset) so the report can describe where it came from.
    """
    name: str
    # fTetWild fallback tier.
    ftetwild_enabled: bool = True
    ftetwild_max_faces: Optional[int] = 300000   # None = no input-size cap
    ftetwild_timeout: float = 180.0
    # Extreme only: when a wastefully dense boundary fails every decimation
    # rung, run one more fTetWild attempt with optimize=True before falling
    # back to the undecimated boundary. Balanced/Thorough keep the plain
    # undecimated fallback.
    ftetwild_optimize_retry_on_dense_fail: bool = False
    # Deep-repair ladder: 'off' / 'local' / 'full' (see repair.py).
    deep_repair: str = 'full'
    # Dense-boundary decimation ladder and its size budget.
    dense_target_ladder: tuple = (1.5, 3.0)
    dense_ratio: int = 4
    dense_min_faces: int = 20000
    # Hausdorff sample count. NEVER below 200000: the shape guard must stay
    # honest on dense boundaries.
    ftetwild_hausdorff_samples: int = 200000
    # Provenance (a resolved user profile only).
    base: Optional[str] = None
    overrides: dict = dataclasses.field(default_factory=dict, compare=False)


# Built-in presets. Balanced is the historical default (its values equal the
# pre-triage repair.py constants); Quick skips the expensive tiers entirely;
# Thorough/Extreme add the 'threshold' decimation rung
# (dense_ratio * max(input, dense_min_faces)) that the 1.5x/3x rungs of a
# wastefully dense boundary (thingi10k_73444) can fail.
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
        dense_target_ladder=(1.5, 3.0, 'threshold'),
        ftetwild_hausdorff_samples=400000,
    ),
    'extreme': IntensitySpec(
        name='extreme',
        ftetwild_max_faces=None,
        ftetwild_timeout=1800.0,
        ftetwild_optimize_retry_on_dense_fail=True,
        dense_target_ladder=(1.5, 3.0, 'threshold'),
        ftetwild_hausdorff_samples=1000000,
    ),
}

# Fields a profile may override (identity and provenance are not editable).
_PROFILE_FIELDS = tuple(f.name for f in dataclasses.fields(IntensitySpec)
                        if f.name not in ('name', 'base', 'overrides'))


def _warn(msg):
    """Warn on stderr; ambient configuration must never crash a repair."""
    try:
        print('sutura: warning: %s' % msg, file=sys.stderr)
    except Exception:
        pass


# ------------------------------------------------------------ validation

def _coerce_ladder(value):
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


def _coerce_field(field, value):
    """Return a validated value for one IntensitySpec field, or _INVALID."""
    if field in ('ftetwild_enabled', 'ftetwild_optimize_retry_on_dense_fail'):
        return value if isinstance(value, bool) else _INVALID
    if field == 'ftetwild_max_faces':
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            return _INVALID
        return int(value) if int(value) > 0 else None
    if field == 'ftetwild_timeout':
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


def _normalize_entry(name, raw):
    """Normalise one stored profile to ``{'base':..., 'overrides':{...}}``.

    Accepts both the current shape (``base`` + ``overrides``) and the legacy
    flat hook shape (``base`` + bare field keys), so the earlier
    ``resolve_intensity(user_profiles=...)`` tests keep working. Returns None
    when the entry is not an object.
    """
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


def _build_spec(name, base, overrides, quiet=False):
    """Apply a profile's overrides on top of its base preset.

    Returns ``(spec, applied_overrides)`` where ``applied`` holds only the
    validated fields that were actually changed (the Hausdorff floor is
    enforced here with a warning). Invalid fields are dropped, never fatal.
    """
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


# ------------------------------------------------------------ name rules

def name_error(name, profiles, exclude=None):
    """Return an error code for a proposed profile name, or None when valid.

    Codes: ``empty`` (blank), ``reserved`` (shadows a built-in preset,
    case-insensitive) and ``duplicate`` (clashes with another profile,
    case-insensitive). ``exclude`` names the profile being renamed.
    """
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


def unique_profile_name(profiles, base):
    """A free profile name derived from ``base`` (``base``, ``base 2``...)."""
    base = (base or 'profile').strip() or 'profile'
    if name_error(base, profiles) is None:
        return base
    index = 2
    while True:
        candidate = '%s %d' % (base, index)
        if name_error(candidate, profiles) is None:
            return candidate
        index += 1


def rename_profile(profiles, old, new):
    """Rename in place; returns an error code or None on success."""
    error = name_error(new, profiles, exclude=old)
    if error is not None:
        return error
    profiles[new.strip()] = profiles.pop(old)
    return None


def duplicate_profile(profiles, src):
    """Copy a profile in place; returns the new name, or None if missing."""
    entry = profiles.get(src)
    if entry is None:
        return None
    new = unique_profile_name(profiles, '%s copy' % src)
    profiles[new] = {'base': entry.get('base', DEFAULT_INTENSITY),
                     'overrides': dict(entry.get('overrides', {}))}
    return new


# ------------------------------------------------------------ storage

def load_profiles(path=None):
    """Load the user profiles, never raising.

    A missing file is an empty mapping. A corrupt/unreadable/unsupported file
    warns on stderr and is treated as empty, so a hand-edited profiles file
    can never crash a repair.
    """
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


def save_profiles(profiles, path=None):
    """Atomically write the profiles file (temp file + os.replace)."""
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


# ------------------------------------------------------------ resolution

def resolve_intensity_detail(cli_value=None, environ=None, config_path=None,
                             user_profiles=None, profiles_path=None):
    """Resolve the intensity and describe its provenance.

    Precedence: CLI ``--intensity`` > ``SUTURA_INTENSITY`` > config.json
    ``intensity`` > ``balanced``. At every level the value may name a preset
    or a user profile. An invalid CLI value is rejected by argparse; an
    invalid env/config value falls through to the next source (ambient
    configuration must never crash a repair). Returns ``(spec, meta)`` where
    ``meta`` is empty for a built-in and ``{'base':..., 'overrides':...}``
    for a profile.
    """
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
    """Resolve the effective IntensitySpec (CLI > env > config > balanced)."""
    return resolve_intensity_detail(cli_value, environ, config_path,
                                    user_profiles, profiles_path)[0]


def effective_spec(name, profiles=None, profiles_path=None):
    """The effective IntensitySpec for a preset or profile name, or None."""
    if name in PRESETS:
        return PRESETS[name]
    loaded = load_profiles(profiles_path) if profiles is None else profiles
    entry = loaded.get(name)
    if entry is None:
        return None
    normalized = _normalize_entry(name, entry)
    spec, _ = _build_spec(name, normalized['base'], normalized['overrides'])
    return spec


def list_intensities(profiles=None, profiles_path=None):
    """Presets first, then profiles, each with its effective spec.

    Returns a list of ``{'name', 'kind', 'base', 'spec'}`` dicts.
    """
    loaded = load_profiles(profiles_path) if profiles is None else profiles
    rows = [{'name': n, 'kind': 'preset', 'base': None, 'spec': PRESETS[n]}
            for n in INTENSITIES]
    for name, entry in loaded.items():
        normalized = _normalize_entry(name, entry)
        spec, _ = _build_spec(name, normalized['base'], normalized['overrides'])
        rows.append({'name': name, 'kind': 'profile',
                     'base': normalized['base'], 'spec': spec})
    return rows


def triage_report_fields(spec):
    """The report fields describing the selected intensity (profile-aware)."""
    if spec is None:
        return {'triage_intensity': DEFAULT_INTENSITY}
    out = {'triage_intensity': spec.name}
    if spec.base:
        out['triage_profile_base'] = spec.base
        out['triage_overrides'] = dict(spec.overrides)
    return out


def _format_row(row):
    spec = row['spec']
    cap = 'no-limit' if spec.ftetwild_max_faces is None \
        else str(spec.ftetwild_max_faces)
    ftetwild = ('on(cap=%s,timeout=%gs,optretry=%s)'
                % (cap, spec.ftetwild_timeout,
                   'yes' if spec.ftetwild_optimize_retry_on_dense_fail
                   else 'no')) if spec.ftetwild_enabled else 'off'
    ladder = ','.join(str(x) for x in spec.dense_target_ladder) or '-'
    base = ' [base=%s]' % spec.base if spec.base else ''
    return ('%-20s ftetwild=%s deep_repair=%s ladder=%s ratio=%d '
            'min_faces=%d hausdorff=%d%s'
            % (row['name'], ftetwild, spec.deep_repair, ladder,
               spec.dense_ratio, spec.dense_min_faces,
               spec.ftetwild_hausdorff_samples, base))


def format_intensities(profiles=None, profiles_path=None):
    """Human-readable listing of the presets and user profiles."""
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


def profile_overrides(base_spec, values):
    """Overrides of ``values`` (field -> value) relative to ``base_spec``.

    Only fields whose value actually differs are returned, so an edited
    profile stores a sparse override set.
    """
    out = {}
    for field, new in values.items():
        if field not in _PROFILE_FIELDS:
            continue
        if new != getattr(base_spec, field):
            out[field] = new
    return out
