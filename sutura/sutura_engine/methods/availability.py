# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Availability checks for repair method tiers."""
import importlib.util
from typing import Optional, Tuple


def always_available() -> Tuple[bool, Optional[str]]:
    return True, None


def indirect_available() -> Tuple[bool, Optional[str]]:
    """The exact indirect-predicate tier needs the Rust extension + bridge."""
    try:
        if importlib.util.find_spec('sutura_geom') is None:
            return False, 'rust extension sutura_geom is not installed'
        if importlib.util.find_spec('indirect_bridge') is None:
            return False, 'indirect_bridge.py is not available'
        return True, None
    except Exception as e:
        return False, 'indirect autorefine unavailable: %s' % e


def ftetwild_available() -> Tuple[bool, Optional[str]]:
    """Check fTetWild availability."""
    try:
        from sutura_engine.methods.protocol import _repair_mod
        if _repair_mod().ftetwild_available():
            return True, None
        return False, 'fTetWild (pytetwild + pyvista) is not installed'
    except Exception as e:
        return False, 'fTetWild unavailable: %s' % e


def closing_available() -> Tuple[bool, Optional[str]]:
    """Registry methods 8/9 need the standalone closing tier."""
    try:
        if (importlib.util.find_spec('sutura_engine.methods.closing') is None and
                importlib.util.find_spec('closing') is None):
            return False, 'closing.py is not available'
        return True, None
    except Exception as e:
        return False, 'closing unavailable: %s' % e


def proxy_available() -> Tuple[bool, Optional[str]]:
    """Registry method 10 needs proxy_repair plus its deps."""
    try:
        if (importlib.util.find_spec('sutura_engine.methods.proxy') is None and
                importlib.util.find_spec('proxy_repair') is None):
            return False, 'proxy_repair is not available'
        for mod in ('trimesh', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:
        return False, 'proxy repair unavailable: %s' % e


def mirror_available() -> Tuple[bool, Optional[str]]:
    """Registry method 14 needs mirror_repair plus its numpy/scipy deps."""
    try:
        if (importlib.util.find_spec('sutura_engine.methods.closing') is None and
                importlib.util.find_spec('closing') is None):
            return False, 'closing.py is not available'
        if (importlib.util.find_spec('sutura_engine.mirror') is None and
                importlib.util.find_spec('mirror_repair') is None and
                importlib.util.find_spec('sutura.mirror_repair') is None):
            return False, 'sutura_engine.mirror is not available'
        for mod in ('numpy', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:
        return False, 'mirror completion unavailable: %s' % e


def wall_available() -> Tuple[bool, Optional[str]]:
    """Registry method 15 needs wall_thickness plus numpy/scipy."""
    try:
        if (importlib.util.find_spec('sutura_engine.wall') is None and
                importlib.util.find_spec('wall_thickness') is None and
                importlib.util.find_spec('sutura.wall_thickness') is None):
            return False, 'sutura_engine.wall is not available'
        for mod in ('numpy', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:
        return False, 'wall thickness unavailable: %s' % e


def repeat_available() -> Tuple[bool, Optional[str]]:
    """Registry methods 11/12 need repeat_repair + its deps."""
    try:
        if (importlib.util.find_spec('sutura_engine.methods.repeat') is None and
                importlib.util.find_spec('repeat_repair') is None):
            return False, 'repeat_repair is not available'
        for mod in ('trimesh', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:
        return False, 'repeat repair unavailable: %s' % e


def graft_available() -> Tuple[bool, Optional[str]]:
    """Registry method 13 needs the Rust morphology core (Cast)."""
    try:
        if (importlib.util.find_spec('sutura_engine.graft') is None and
                importlib.util.find_spec('shell_wrap') is None):
            return False, 'sutura_engine.graft is not available'
        spec = importlib.util.find_spec('sutura_geom')
        if spec is None:
            return False, 'rust extension sutura_geom is not installed'
        try:
            import sutura_geom
            if not hasattr(sutura_geom, 'morph_close'):
                return False, 'sutura_geom has no morph_close (rebuild needed)'
        except Exception as e:
            return False, 'sutura_geom import failed: %s' % e
        for mod in ('numpy', 'trimesh', 'scipy'):
            if importlib.util.find_spec(mod) is None:
                return False, '%s is not available' % mod
        return True, None
    except Exception as e:
        return False, 'graft (shell wrap) unavailable: %s' % e
