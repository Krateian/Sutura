# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura Engine — core mesh repair and geometry processing engine."""
import os
import sys
from typing import Any, Dict, Optional, Tuple, Union

_parent = os.path.dirname(os.path.dirname(os.path.realpath(__file__)))
if _parent not in sys.path:
    sys.path.insert(0, _parent)


def _get_version():
    try:
        cur = os.path.dirname(os.path.realpath(__file__))
        for cand in (os.path.join(cur, '..', 'repair.py'), os.path.join(cur, 'repair.py')):
            if os.path.isfile(cand):
                with open(cand, 'r', encoding='utf-8') as f:
                    for line in f:
                        if line.startswith('VERSION ='):
                            return line.split('=')[1].strip().strip('\'"')
    except Exception:
        pass
    return "0.7.2"


VERSION = _get_version()
__version__ = VERSION


class Report(dict):
    """Structured report returned by sutura_engine.repair()."""

    @property
    def ok(self) -> bool:
        return bool(self.get('ok', True) and not self.get('error'))

    @property
    def category(self) -> str:
        if 'category' in self:
            return self['category']
        try:
            import classification
            cat, _, _ = classification.classify(self)
            return cat
        except Exception:
            return 'unknown'

    @property
    def watertight(self) -> bool:
        return bool(self.get('method_reached_watertight', False))

    @property
    def method_used(self) -> Optional[dict]:
        return self.get('method_used')

    @property
    def verts(self):
        return self.get('_verts')

    @property
    def tris(self):
        return self.get('_tris')


def repair(path_or_arrays: Union[str, os.PathLike, Tuple[Any, Any]],
           out_path: Optional[Union[str, os.PathLike]] = None,
           methods: Optional[list] = None,
           intensity: Optional[str] = 'balanced',
           mode: str = 'auto',
           no_cache: bool = False,
           **kwargs) -> Report:
    """Public Sutura repair entry point.

    Args:
        path_or_arrays: File path (str/Path) or (verts, tris) numpy arrays tuple.
        out_path: Optional destination path. If None and path given, creates
                  a fixed file in a temp directory.
        methods: Optional list of method numbers to attempt (e.g. [1, 3]).
        intensity: Preset name ('quick', 'balanced', 'thorough', 'extreme') or user profile.
        mode: Repair mode ('auto', 'low', 'medium', 'aggressive', 'extreme').
        no_cache: If True, bypasses and disables cache for this call.
        **kwargs: Additional parameters forwarded to repair pipeline.

    Returns:
        Report dictionary subclass with structured outcomes.
    """
    import tempfile
    from sutura_engine import chart as cache
    from sutura_engine.triage import repair_with_methods, resolve_intensity

    cache_prev = cache.is_cache_enabled()
    if no_cache:
        cache.set_cache_enabled(False)
    try:
        spec = resolve_intensity(intensity) if intensity is not None else None
        triage_kwargs = dict(kwargs)
        if spec is not None:
            triage_kwargs['deep_repair'] = spec.deep_repair
            triage_kwargs['ftetwild'] = spec.ftetwild_enabled
        if mode is not None:
            triage_kwargs['mode'] = mode

        if isinstance(path_or_arrays, (str, os.PathLike)):
            src_path = str(path_or_arrays)
            with tempfile.TemporaryDirectory(prefix='sutura-engine-') as tmpdir:
                if out_path is None:
                    ext = os.path.splitext(src_path)[1] or '.stl'
                    target_out = os.path.join(tmpdir, f'repaired{ext}')
                else:
                    target_out = str(out_path)
                res = repair_with_methods(src_path, target_out, tmpdir, methods=methods, **triage_kwargs)
                return Report(res)
        elif isinstance(path_or_arrays, (tuple, list)) and len(path_or_arrays) == 2:
            verts, tris = path_or_arrays
            from sutura_engine.core import stl_write_binary
            with tempfile.TemporaryDirectory(prefix='sutura-engine-') as tmpdir:
                src_path = os.path.join(tmpdir, 'input.stl')
                stl_write_binary(src_path, verts, tris)
                target_out = str(out_path) if out_path is not None else os.path.join(tmpdir, 'output.stl')
                res = repair_with_methods(src_path, target_out, tmpdir, methods=methods, **triage_kwargs)
                if out_path is None and os.path.exists(target_out):
                    try:
                        from sutura_engine.core import load_meshes
                        meshes = load_meshes(target_out)
                        if meshes:
                            res['_verts'] = meshes[0][1]
                            res['_tris'] = meshes[0][2]
                    except Exception:
                        pass
                return Report(res)
        else:
            raise TypeError("path_or_arrays must be a file path or (verts, tris) tuple")
    finally:
        if no_cache:
            cache.set_cache_enabled(cache_prev)


__all__ = ['repair', 'Report', 'VERSION', '__version__']
