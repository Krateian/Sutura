# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Repair policy: declarative, versioned decision rules (step 1: the order).

The data-like repair rules live in ``default_policy.toml`` next to this module
and ship with the package. This loader reads that file (later steps will layer
the user ``~/.config/sutura/policy.toml`` on top) and exposes:

- ``SCHEMA_VERSION``  the policy schema version;
- ``CONTENT_HASH``    a short hash of the effective merged policy;
- ``ORDER``           the deep-repair tier order data.

Stdlib only (``tomllib`` on Python 3.11+, which every supported path uses:
``install.sh`` rejects < 3.11, the macOS installer pins 3.11, the AppImage
bundles 3.14, CI tests 3.11/3.14 and the Orca plugin requires >= 3.12). No
numpy / third-party import, so ``pipeline.order`` stays a lightweight leaf.
"""
import hashlib
import json
import math
import os
import shutil
import tempfile
import tomllib

DEFAULT_POLICY_PATH = os.path.join(os.path.dirname(__file__), 'default_policy.toml')

_REQUIRED = ('policy', 'order', 'auto', 'si_escalation', 'ranking', 'templates',
             'intensity', 'modes', 'stage1_profiles', 'stage1_types',
             'stage1_tuning')
_ORDER_KEYS = ('deep_tier_order', 'final_tier_precedence', 'graft', 'ftetwild',
               'deep_tier_method_num')
_AUTO_NUM_KEYS = ('max_attempts', 'fallback_watchdog_s', 'fallback_budget_factor',
                  'fallback_min_s', 'fallback_max_s', 'invent_min_score',
                  'generative_max_hausdorff_rel')
_SI_NUM_KEYS = ('method_num', 'vol_rel_tol', 'dist_tol_min_mm',
                'dist_tol_max_mm', 'dist_tol_div')
_TEMPLATE_NUM_KEYS = ('open_area_scan', 'open_area_relief', 'large_loop_ratio',
                      'many_loops', 'si_moderate', 'si_heavy')
_TEMPLATE_INT_KEYS = ('many_loops', 'si_moderate', 'si_heavy')
_TEMPLATE_IDS = ('mechanical', 'organic', 'single_side_scan', 'relief',
                 'repeated_pattern', 'dense_scan_heavy_si')
_INTENSITY_FIELDS = ('ftetwild_enabled', 'ftetwild_max_faces', 'ftetwild_timeout',
                     'graft_max_faces', 'graft_timeout',
                     'ftetwild_optimize_retry_on_dense_fail', 'deep_repair',
                     'dense_target_ladder', 'dense_ratio', 'dense_min_faces',
                     'ftetwild_hausdorff_samples')
_INTENSITY_CAP = ('ftetwild_max_faces', 'graft_max_faces')
_INTENSITY_BOOL = ('ftetwild_enabled', 'ftetwild_optimize_retry_on_dense_fail')
_INTENSITY_INT = ('dense_ratio', 'dense_min_faces', 'ftetwild_hausdorff_samples')
# Single keys locked as non-overridable, in addition to the gate tables.
_EXTRA_GATE_KEYS = ('auto.generative_max_hausdorff_rel',)
# Whole sections locked as non-overridable: [si_escalation] holds the candidate
# method and the never-worse volume/distance tolerances (the auto-SI verdict).
_EXTRA_GATE_SECTIONS = ('si_escalation',)


def _validate(data):
    for section in _REQUIRED:
        if section not in data:
            raise ValueError('policy: missing section [%s]' % section)
    if not isinstance(data['policy'].get('schema_version'), int):
        raise ValueError('policy: [policy].schema_version must be an integer')
    order = data['order']
    for key in _ORDER_KEYS:
        if key not in order:
            raise ValueError('policy: missing [order].%s' % key)
    method_num = order['deep_tier_method_num']
    for tier in order['deep_tier_order']:
        if tier not in method_num:
            raise ValueError('policy: no method number for tier %r' % tier)
    auto = data['auto']
    for key in _AUTO_NUM_KEYS:
        value = auto.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError('policy: [auto].%s must be a number' % key)
    si = data['si_escalation']
    for key in _SI_NUM_KEYS:
        value = si.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError('policy: [si_escalation].%s must be a number' % key)
    if not isinstance(si['method_num'], int):
        raise ValueError('policy: [si_escalation].method_num must be an integer')
    _validate_tiers(data.get('tiers') or {})
    rank = data['ranking']
    for key in ('preference_boost', 'top_n', 'learning_max_bonus', 'learning_prior'):
        value = rank.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError('policy: [ranking].%s must be a number' % key)
    if not isinstance(rank['top_n'], int):
        raise ValueError('policy: [ranking].top_n must be an integer')
    tpl = data['templates']
    for key in _TEMPLATE_NUM_KEYS:
        value = tpl.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError('policy: [templates].%s must be a number' % key)
    for key in _TEMPLATE_INT_KEYS:
        if not isinstance(tpl.get(key), int):
            raise ValueError('policy: [templates].%s must be an integer' % key)
    pref = tpl.get('preferred') or {}
    for tid in _TEMPLATE_IDS:
        seq = pref.get(tid)
        if not isinstance(seq, list) or not seq or any(
                isinstance(x, bool) or not isinstance(x, int) for x in seq):
            raise ValueError(
                'policy: [templates.preferred].%s must be a list of ints' % tid)
    adoption = (data.get('gates') or {}).get('adoption')
    if adoption is not None:
        for key in ('face_ratio', 'vertex_ratio', 'min_slack', 'loop_face_slack',
                    'loop_vertex_slack', 'ftetwild_abs_floor'):
            value = adoption.get(key)
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise ValueError(
                    'policy: [gates.adoption].%s must be a number' % key)
        for key in ('min_slack', 'loop_face_slack', 'loop_vertex_slack',
                    'ftetwild_abs_floor'):
            if not isinstance(adoption[key], int):
                raise ValueError(
                    'policy: [gates.adoption].%s must be an integer' % key)
    tuning = (data.get('gates') or {}).get('tuning')
    if tuning is not None:
        for key in ('hausdorff_floor', 'decimate_hd_margin',
                    'ftetwild_max_hausdorff_rel'):
            value = tuning.get(key)
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise ValueError('policy: [gates.tuning].%s must be a number' % key)
        if not isinstance(tuning['hausdorff_floor'], int):
            raise ValueError('policy: [gates.tuning].hausdorff_floor must be an integer')
    sec = data['intensity']
    presets = {k: v for k, v in sec.items() if k != 'default'}
    dflt = sec.get('default')
    if not isinstance(dflt, str) or dflt not in presets:
        raise ValueError('policy: [intensity].default must name a preset')
    for name, spec in presets.items():
        if not isinstance(spec, dict):
            raise ValueError('policy: [intensity.%s] must be a table' % name)
        for key, value in spec.items():
            if key not in _INTENSITY_FIELDS:
                raise ValueError(
                    'policy: [intensity.%s].%s is not a known field' % (name, key))
            if key in _INTENSITY_CAP:
                if value == 'none':
                    continue
                if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
                    raise ValueError('policy: [intensity.%s].%s must be a positive '
                                     'integer or "none"' % (name, key))
            elif key in _INTENSITY_BOOL:
                if not isinstance(value, bool):
                    raise ValueError('policy: [intensity.%s].%s must be a boolean'
                                     % (name, key))
            elif key in _INTENSITY_INT:
                if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                    raise ValueError('policy: [intensity.%s].%s must be a '
                                     'non-negative integer' % (name, key))
            elif key in ('ftetwild_timeout', 'graft_timeout'):
                if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
                    raise ValueError('policy: [intensity.%s].%s must be positive'
                                     % (name, key))
            elif key == 'deep_repair':
                if value not in ('off', 'local', 'full'):
                    raise ValueError(
                        'policy: [intensity.%s].deep_repair must be off/local/full' % name)
            elif key == 'dense_target_ladder':
                ok = isinstance(value, list) and all(
                    x == 'threshold' or (not isinstance(x, bool)
                                         and isinstance(x, (int, float)) and x > 0)
                    for x in value)
                if not ok:
                    raise ValueError('policy: [intensity.%s].dense_target_ladder must '
                                     'be positive numbers and/or "threshold"' % name)
    for section in ('modes', 'stage1_profiles', 'stage1_types'):
        _validate_threshold_table(section, data[section])
    tun = data['stage1_tuning']
    for key in ('mech_gate', 'org_gate'):
        value = tun.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError('policy: [stage1_tuning].%s must be a number' % key)
    return data


def _validate_threshold_table(name, table):
    """Validate a ``{name: {mincomponentsize, maxholesize}}`` table."""
    if not isinstance(table, dict) or not table:
        raise ValueError('policy: [%s] must be a non-empty table' % name)
    for key, spec in table.items():
        if not isinstance(spec, dict):
            raise ValueError('policy: [%s.%s] must be a table' % (name, key))
        for field in ('mincomponentsize', 'maxholesize'):
            value = spec.get(field)
            if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                raise ValueError('policy: [%s.%s].%s must be a non-negative integer'
                                 % (name, key, field))
        extra = set(spec) - {'mincomponentsize', 'maxholesize'}
        if extra:
            raise ValueError('policy: [%s.%s] unknown keys %r'
                             % (name, key, sorted(extra)))


def _validate_tiers(tiers):
    """Validate the tier sections present (each sub-step adds one)."""
    for name, sec in tiers.items():
        for key, value in (sec.get('gates') or {}).items():
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise ValueError(
                    'policy: [tiers.%s.gates].%s must be a number' % (name, key))
        if 'default_enabled' in sec and not isinstance(sec['default_enabled'], bool):
            raise ValueError(
                'policy: [tiers.%s].default_enabled must be a boolean' % name)
        if 'work_by_intensity' in sec:
            wbi = sec['work_by_intensity']
            if not isinstance(wbi, dict) or not wbi or any(
                    isinstance(v, bool) or not isinstance(v, int) for v in wbi.values()):
                raise ValueError(
                    'policy: [tiers.%s].work_by_intensity must map names to ints' % name)
        for key in ('default_work', 'res', 'seam_ulp_k'):
            if key in sec and (isinstance(sec[key], bool)
                               or not isinstance(sec[key], int) or sec[key] < 0):
                raise ValueError(
                    'policy: [tiers.%s].%s must be a non-negative integer' % (name, key))
    flap = tiers.get('flap')
    if flap is not None:
        dw = flap.get('default_work')
        if isinstance(dw, bool) or not isinstance(dw, int):
            raise ValueError('policy: [tiers.flap].default_work must be an integer')
    tamp = tiers.get('tamponade')
    if tamp is not None:
        lmf = tamp.get('leak_max_faces')
        if isinstance(lmf, bool) or not isinstance(lmf, int):
            raise ValueError('policy: [tiers.tamponade].leak_max_faces must be an integer')
        gates = tamp.get('gates') or {}
        for key in ('shape_depth_cells', 'shape_side_samples'):
            value = gates.get(key)
            if isinstance(value, bool) or not isinstance(value, int):
                raise ValueError('policy: [tiers.tamponade.gates].%s must be an integer' % key)
    dress = tiers.get('dressing')
    if dress is not None:
        if not isinstance(dress.get('default_enabled'), bool):
            raise ValueError(
                'policy: [tiers.dressing].default_enabled must be a boolean')
        slack = (dress.get('gates') or {}).get('max_parts_slack')
        if isinstance(slack, bool) or not isinstance(slack, int):
            raise ValueError(
                'policy: [tiers.dressing.gates].max_parts_slack must be an integer')


USER_POLICY_PATH = os.path.expanduser('~/.config/sutura/policy.toml')
USER_POLICY_ENV = 'SUTURA_POLICY_USER'   # test hook for the user-file path


def _read_toml(path):
    with open(path, 'rb') as f:
        return tomllib.load(f)


def _gate_paths_for(data):
    paths = set('gates.%s' % name for name in (data.get('gates') or {}))
    paths.update('tiers.%s.gates' % name for name in (data.get('tiers') or {}))
    paths.update(_EXTRA_GATE_KEYS)
    paths.update(_EXTRA_GATE_SECTIONS)
    return paths


def _is_gate(dotted, gates):
    # A user key is locked when it *is* a gate or sits under one. A parent of a
    # gate table is not itself locked, so we recurse and reject only the gate
    # subtree (recorded with its precise dotted key).
    for gate in gates:
        if dotted == gate or dotted.startswith(gate + '.'):
            return True
    return False


def _merge_known(base, override, prefix, ignored, gates):
    """Deep-merge ``override`` onto ``base``, dropping locked-gate and unknown
    keys (both recorded in ``ignored``). Only keys present in ``base`` survive."""
    out = dict(base)
    for key, value in override.items():
        dotted = '.'.join(prefix + (key,))
        if _is_gate(dotted, gates) or key not in base:
            ignored.append(dotted)
            continue
        if isinstance(base[key], dict):
            if not isinstance(value, dict):
                ignored.append(dotted)
            else:
                out[key] = _merge_known(base[key], value, prefix + (key,), ignored, gates)
        else:
            out[key] = value
    return out


def _warn(msg):
    try:
        print('sutura: %s' % msg, file=sys.stderr)
    except Exception:
        pass


def load_policy(path=None):
    """Load the packaged policy and (once) merge the user override.

    Called once at import: every constant is bound then, so there is no
    mid-run re-read. Each CLI run is a fresh process, so a GUI "Apply" takes
    effect on the next repair. An explicit ``path`` loads only that file (used
    by tests/tooling; no user merge). The user file is ``$SUTURA_POLICY_USER``
    (test hook) or ``~/.config/sutura/policy.toml``; it sits between the
    packaged default and the existing env/CLI layers, which are unchanged.
    """
    global USER_OVERRIDE, POLICY_ERROR, IGNORED_KEYS
    USER_OVERRIDE = False
    POLICY_ERROR = None
    IGNORED_KEYS = []
    base = _validate(_read_toml(path or os.environ.get('SUTURA_POLICY')
                                or DEFAULT_POLICY_PATH))
    if path is not None:
        return base
    user_path = os.environ.get(USER_POLICY_ENV) or USER_POLICY_PATH
    if not os.path.exists(user_path):
        return base
    USER_OVERRIDE = True
    try:
        user = _read_toml(user_path)
    except Exception as exc:  # malformed file: fall back to the default
        POLICY_ERROR = '%s: %s' % (type(exc).__name__, exc)
        _warn('policy: user override unreadable (%s); using the packaged default'
              % POLICY_ERROR)
        return base
    if not isinstance(user, dict):
        POLICY_ERROR = 'top level is not a table'
        _warn('policy: user override invalid (%s); using the packaged default'
              % POLICY_ERROR)
        return base
    ignored = []
    merged = _merge_known(base, user, (), ignored, _gate_paths_for(base))
    IGNORED_KEYS = ignored
    if ignored:
        _warn('policy: ignored %d override key(s): %s'
              % (len(ignored), ', '.join(ignored)))
    try:
        merged = _validate(merged)
    except ValueError as exc:
        POLICY_ERROR = str(exc)
        _warn('policy: user override invalid (%s); using the packaged default'
              % POLICY_ERROR)
        return base
    return merged


def policy_hash(data):
    """Short content hash of a policy dict (deterministic, order-insensitive)."""
    blob = json.dumps(data, sort_keys=True, separators=(',', ':')).encode('utf-8')
    return hashlib.sha256(blob).hexdigest()[:16]


USER_OVERRIDE = False
POLICY_ERROR = None
IGNORED_KEYS = []
LAST_WRITE_WARNING = None

_POLICY = load_policy()
SCHEMA_VERSION = int(_POLICY['policy']['schema_version'])
CONTENT_HASH = policy_hash(_POLICY)
ORDER = _POLICY['order']
AUTO = _POLICY['auto']
SI_ESCALATION = _POLICY['si_escalation']
TIERS = _POLICY.get('tiers') or {}
RANKING = _POLICY['ranking']
TEMPLATES_CFG = _POLICY['templates']
GATES = _POLICY.get('gates') or {}
INTENSITY = _POLICY['intensity']
MODES = _POLICY['modes']
STAGE1_PROFILES = _POLICY['stage1_profiles']
STAGE1_TYPES = _POLICY['stage1_types']
STAGE1_TUNING = _POLICY['stage1_tuning']


def gate_paths():
    """Dotted paths of the non-overridable gates: ``gates.*`` and
    ``tiers.*.gates`` tables, the ``si_escalation`` section, and the locked
    single keys."""
    paths = set('gates.%s' % name for name in GATES)
    paths.update('tiers.%s.gates' % name for name in TIERS)
    paths.update(_EXTRA_GATE_KEYS)
    paths.update(_EXTRA_GATE_SECTIONS)
    return frozenset(paths)


_PANEL_OWNED_KEYS = (('order', 'deep_tier_order'),)
_PANEL_HEADER = (
    '# Managed by the Sutura Method control panel (Options -> Methods).\n'
    '# Panel-owned sections: [order] / [methods]. Comments in these sections\n'
    '# may be regenerated. The packaged default policy is never modified.\n'
    '\n'
)


def _flatten_policy(data, prefix=()):
    for key, value in data.items():
        dotted = prefix + (key,)
        if isinstance(value, dict):
            yield from _flatten_policy(value, dotted)
        else:
            yield dotted, value


def _pverr(message, key=None, kind=None):
    exc = ValueError(message)
    exc.policy_key = key
    exc.policy_kind = kind
    return exc


def _toml_key(key):
    text = str(key)
    if text and all(c.isalnum() or c in '_-' for c in text):
        return text
    return json.dumps(text, ensure_ascii=False)


def _toml_value(value, key=''):
    if isinstance(value, bool):
        return 'true' if value else 'false'
    if isinstance(value, float):
        if not math.isfinite(value):
            raise _pverr('policy: cannot serialize non-finite float at %r'
                         % key, key=key, kind='unsupported')
        return repr(value)
    if isinstance(value, int):
        return repr(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, list):
        out = []
        for i, item in enumerate(value):
            if isinstance(item, dict):
                raise _pverr('policy: cannot serialize %r of type dict'
                             % key, key=key, kind='unsupported')
            out.append(_toml_value(item, '%s[%d]' % (key, i)))
        return '[' + ', '.join(out) + ']'
    raise _pverr('policy: cannot serialize %r of type %s'
                 % (key, type(value).__name__), key=key, kind='unsupported')


def _toml_dump(data):
    lines = []

    def emit(table, prefix):
        scalars = [(k, v) for k, v in table.items()
                   if not isinstance(v, dict)]
        tables = [(k, v) for k, v in table.items()
                  if isinstance(v, dict)]
        if prefix:
            lines.append('')
            lines.append('[%s]' % '.'.join(prefix))
        for key, value in scalars:
            dotted = '.'.join(prefix + (key,))
            lines.append('%s = %s' % (_toml_key(key),
                                      _toml_value(value, dotted)))
        for key, value in tables:
            emit(value, prefix + (key,))

    emit(data, ())
    return '\n'.join(lines).rstrip() + '\n'


def write_user_policy(changes=None, path=None, reset=False):
    """Write panel-owned keys to the user policy, atomically.

    ``changes`` is a nested dict; v1 allows only
    ``order.deep_tier_order``, and it must be a subsequence of the packaged
    canonical order (Auto is enable/disable, not reorder). Locked gates
    (``gate_paths()``) and unknown keys are refused; existing keys are kept
    verbatim, including ignored gate entries. ``reset=True`` removes only the
    panel-owned ``[order].deep_tier_order`` key. A ``.bak`` copy is kept and
    the existing file mode is preserved. Returns the validated effective policy.

    The module constants (``CONTENT_HASH``, ``USER_OVERRIDE``) are loaded once
    at import; callers that need the new state should use this return value.
    """
    global LAST_WRITE_WARNING
    LAST_WRITE_WARNING = None
    if not reset and (not isinstance(changes, dict) or not changes):
        raise ValueError('policy: changes must be a non-empty table')
    if reset and changes:
        raise ValueError('policy: reset and changes are mutually exclusive')

    base = _validate(_read_toml(DEFAULT_POLICY_PATH))
    user_path = path or os.environ.get(USER_POLICY_ENV) or USER_POLICY_PATH
    user_data = {}
    mode = None
    if os.path.exists(user_path):
        try:
            existing = _read_toml(user_path)
            mode = os.stat(user_path).st_mode & 0o777
            if not isinstance(existing, dict):
                raise ValueError('top level is not a table')
        except Exception as exc:
            if reset:
                os.replace(user_path, user_path + '.bak')
                LAST_WRITE_WARNING = ('unreadable user policy moved to '
                                      'policy.toml.bak and reset')
                return base
            raise ValueError('policy: user override unreadable: %s' % exc)
        user_data = existing

    if reset:
        order_table = user_data.get('order')
        if isinstance(order_table, dict):
            order_table.pop('deep_tier_order', None)
            if not order_table:
                user_data.pop('order', None)
    else:
        allowed = set(_PANEL_OWNED_KEYS)
        gates = set(gate_paths())
        for dotted, _value in _flatten_policy(changes):
            key = '.'.join(dotted)
            if any(key == gate or key.startswith(gate + '.') for gate in gates):
                raise _pverr('policy: %s is a locked gate' % key,
                              key=key, kind='locked')
            if dotted not in allowed:
                raise _pverr('policy: %s is not panel-owned' % key,
                              key=key, kind='unsupported')

        for dotted, value in _flatten_policy(changes):
            cursor = user_data
            for part in dotted[:-1]:
                child = cursor.get(part)
                if not isinstance(child, dict):
                    child = {}
                    cursor[part] = child
                cursor = child
            cursor[dotted[-1]] = value

        order = (user_data.get('order') or {}).get('deep_tier_order')
        known = list(base['order']['deep_tier_method_num'])
        if not isinstance(order, list) or not order:
            raise ValueError('policy: order.deep_tier_order must be a non-empty list')
        if any(not isinstance(tier, str) or tier not in known for tier in order):
            raise ValueError('policy: order.deep_tier_order has an unknown tier')
        if len(set(order)) != len(order):
            raise ValueError('policy: order.deep_tier_order has duplicate tiers')
        canonical = [t for t in base['order']['deep_tier_order'] if t in order]
        if order != canonical:
            raise _pverr('policy: order.deep_tier_order must keep the '
                         'canonical order; use Reset to default',
                         key='order.deep_tier_order', kind='canonical')

    merged = _merge_known(base, user_data, (), [], _gate_paths_for(base))
    _validate(merged)

    if reset and not user_data:
        if os.path.exists(user_path):
            shutil.copy2(user_path, user_path + '.bak')
            os.unlink(user_path)
        return merged

    directory = os.path.dirname(os.path.abspath(user_path))
    os.makedirs(directory, exist_ok=True)
    text = _PANEL_HEADER + _toml_dump(user_data)
    fd, tmp_path = tempfile.mkstemp(prefix='.policy.', suffix='.tmp',
                                    dir=directory)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        if mode is not None:
            os.chmod(tmp_path, mode)
        if os.path.exists(user_path):
            shutil.copy2(user_path, user_path + '.bak')
        os.replace(tmp_path, user_path)
        try:
            dfd = os.open(directory, os.O_RDONLY)
            try:
                os.fsync(dfd)
            finally:
                os.close(dfd)
        except OSError:
            pass
    finally:
        if os.path.exists(tmp_path):
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
    return merged

def report_block():
    """The report's ``policy`` block (no filesystem paths)."""
    return {
        'schema_version': SCHEMA_VERSION,
        'content_hash': CONTENT_HASH,
        'user_override': USER_OVERRIDE,
        'policy_error': POLICY_ERROR,
        'ignored_keys': list(IGNORED_KEYS),
    }


def human_line(block=None):
    """One ``--human`` line, only when a user override or an error is present."""
    block = block or report_block()
    if block.get('policy_error'):
        return 'policy: %s (packaged default used)' % block['policy_error']
    if block.get('user_override'):
        return ('policy: user override applied (schema %s, %d ignored key(s))'
                % (block.get('schema_version'), len(block.get('ignored_keys') or [])))
    return None
