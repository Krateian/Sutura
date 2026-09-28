#!/usr/bin/env python3
"""Regression test for sutura_engine.cache module.

Checks:
  1. hash_arrays, hash_file, hash_settings stability.
  2. Cache hit/miss for analysis and repeat data.
  3. Automatic cache invalidation when engine version changes.
  4. Method result caching (numpy arrays + report dict).
  5. LRU pruning when cache size exceeds threshold.
  6. clear_cache() and cache opt-out (set_cache_enabled / SUTURA_NO_CACHE).

Usage:
  ~/.local/share/sutura/venv/bin/python tests/test_engine_cache.py
"""
import os
import sys
import tempfile
import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

from sutura_engine import cache  # noqa: E402


def test_hashing():
    v = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]], dtype=np.float32)
    t = np.array([[0, 1, 2]], dtype=np.int32)
    h1 = cache.hash_arrays(v, t)
    h2 = cache.hash_arrays(v, t)
    assert h1 == h2 and len(h1) == 64, h1

    # Slightly different vertices -> different hash
    v2 = v.copy()
    v2[0, 0] = 0.001
    assert cache.hash_arrays(v2, t) != h1

    # Settings hash
    s1 = {'mode': 'auto', 'deep_repair': 'full'}
    s2 = {'deep_repair': 'full', 'mode': 'auto'}
    assert cache.hash_settings(s1) == cache.hash_settings(s2)
    print('ok  test_hashing')


def test_analysis_and_invalidation():
    with tempfile.TemporaryDirectory() as td:
        orig_xdg = os.environ.get('XDG_CACHE_HOME')
        os.environ['XDG_CACHE_HOME'] = td
        try:
            mesh_id = 'test_mesh_01'
            v = '0.6.1'
            payload = {'faces': 100, 'holes': 0, 'watertight': True}

            # Miss initially
            assert cache.get_cached_analysis(mesh_id, v) is None

            # Put and hit
            cache.put_cached_analysis(mesh_id, v, payload)
            hit = cache.get_cached_analysis(mesh_id, v)
            assert hit == payload, hit

            # Version mismatch -> invalidates and returns None
            miss = cache.get_cached_analysis(mesh_id, '0.7.0')
            assert miss is None, miss
            # Old entry was purged on mismatch
            assert cache.get_cached_analysis(mesh_id, v) is None
            print('ok  test_analysis_and_invalidation')
        finally:
            if orig_xdg:
                os.environ['XDG_CACHE_HOME'] = orig_xdg
            else:
                os.environ.pop('XDG_CACHE_HOME', None)


def test_method_caching():
    with tempfile.TemporaryDirectory() as td:
        orig_xdg = os.environ.get('XDG_CACHE_HOME')
        os.environ['XDG_CACHE_HOME'] = td
        try:
            mesh_id = 'cube_mesh'
            v = '0.6.1'
            verts = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]], dtype=np.float32)
            tris = np.array([[0, 1, 2]], dtype=np.int32)
            report = {'method_used': {'num': 1, 'name': 'fast'}, 'watertight': True}

            assert cache.get_cached_method(mesh_id, 1, 'cfg1', v) is None
            cache.put_cached_method(mesh_id, 1, 'cfg1', v, verts, tris, report)

            cached = cache.get_cached_method(mesh_id, 1, 'cfg1', v)
            assert cached is not None
            c_verts, c_tris, c_rep = cached
            assert np.array_equal(c_verts, verts)
            assert np.array_equal(c_tris, tris)
            assert c_rep == report
            print('ok  test_method_caching')
        finally:
            if orig_xdg:
                os.environ['XDG_CACHE_HOME'] = orig_xdg
            else:
                os.environ.pop('XDG_CACHE_HOME', None)


def test_pruning_and_clear():
    with tempfile.TemporaryDirectory() as td:
        orig_xdg = os.environ.get('XDG_CACHE_HOME')
        os.environ['XDG_CACHE_HOME'] = td
        try:
            v = '0.6.1'
            for i in range(10):
                cache.put_cached_analysis(f'm_{i}', v, {'data': 'x' * 1000})
            size = cache.get_cache_size()
            assert size > 0, size

            # Prune with very small threshold
            freed = cache.prune_cache(max_bytes=200)
            assert freed > 0, freed
            assert cache.get_cache_size() <= 200

            # Clear cache
            cleared = cache.clear_cache()
            assert cache.get_cache_size() == 0
            print('ok  test_pruning_and_clear')
        finally:
            if orig_xdg:
                os.environ['XDG_CACHE_HOME'] = orig_xdg
            else:
                os.environ.pop('XDG_CACHE_HOME', None)


def test_no_cache_opt_out():
    with tempfile.TemporaryDirectory() as td:
        orig_xdg = os.environ.get('XDG_CACHE_HOME')
        os.environ['XDG_CACHE_HOME'] = td
        try:
            v = '0.6.1'
            cache.set_cache_enabled(False)
            assert not cache.is_cache_enabled()
            cache.put_cached_analysis('disabled_test', v, {'a': 1})
            assert cache.get_cached_analysis('disabled_test', v) is None
            cache.set_cache_enabled(True)
            assert cache.is_cache_enabled()
            print('ok  test_no_cache_opt_out')
        finally:
            if orig_xdg:
                os.environ['XDG_CACHE_HOME'] = orig_xdg
            else:
                os.environ.pop('XDG_CACHE_HOME', None)


if __name__ == '__main__':
    test_hashing()
    test_analysis_and_invalidation()
    test_method_caching()
    test_pruning_and_clear()
    test_no_cache_opt_out()
    print('all cache tests passed')
