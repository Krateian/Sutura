#!/usr/bin/env python3
"""Sutura Triage Engine - intensity presets.

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

The presets are read-only built-ins. A future "named profile" is a mapping of
field overrides applied on top of a base preset through
``dataclasses.replace``; ``resolve_intensity(..., user_profiles=...)`` is the
(unused) hook for that feature. No profile code exists yet.
"""
import os
import json
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


@dataclass(frozen=True)
class IntensitySpec:
    """Resolved post-Stage-1 knobs for one intensity preset.

    All fields are immutable, so a preset can be shared safely; a user
    profile builds a new spec with ``dataclasses.replace``. The watertight +
    Hausdorff shape guard is invariant and deliberately not a field here.
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


def _apply_profile(spec, name, overrides):
    """Build a named spec from a base preset plus field overrides (the future
    user-profile path; not reachable from today's CLI/GUI)."""
    overrides = dict(overrides)
    base = overrides.pop('base', None)
    if base in PRESETS:
        spec = PRESETS[base]
    return dataclasses.replace(spec, name=name, **overrides)


def resolve_intensity(cli_value=None, environ=None, config_path=None,
                      user_profiles=None):
    """Resolve the intensity preset.

    Precedence: CLI ``--intensity`` > ``SUTURA_INTENSITY`` > config.json
    ``intensity`` > ``balanced``. An invalid CLI value is rejected by
    argparse; an invalid env/config value falls through to the next source
    (ambient configuration must never crash a repair).

    ``user_profiles`` is the unused hook for the next feature: a mapping
    ``name -> {field: value, ...}`` (optionally with a ``base`` key naming the
    built-in to start from, default ``balanced``).
    """
    if user_profiles and cli_value in user_profiles:
        return _apply_profile(PRESETS[DEFAULT_INTENSITY], cli_value,
                              user_profiles[cli_value])
    if cli_value in PRESETS:
        return PRESETS[cli_value]
    env = (os.environ if environ is None else environ).get(INTENSITY_ENV)
    if env in PRESETS:
        return PRESETS[env]
    path = INTENSITY_CONFIG if config_path is None else config_path
    try:
        with open(path) as f:
            val = json.load(f).get('intensity')
        if val in PRESETS:
            return PRESETS[val]
    except (OSError, ValueError, AttributeError):
        pass
    return PRESETS[DEFAULT_INTENSITY]
