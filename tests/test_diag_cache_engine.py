#!/usr/bin/env python3
"""Regression test: the analysis cache is keyed by classifier engine.

The analysis cache must not serve one engine's ``ObjectAnalysis`` to another:
``analyze_mesh`` receives ``engine``/``extra_features`` but the cache was
previously keyed on the mesh hash + app version only, so a classic run could
be returned for a later experimental run on the same mesh (and vice versa).

The test runs both engines on the same mesh with a throwaway cache
(``XDG_CACHE_HOME`` + ``SUTURA_DATA_DIR`` point at a temp dir, so the user's
real cache/learning are never touched) and asserts each cached result equals
that engine's uncached result.

Usage: ~/.local/share/sutura/venv/bin/python tests/test_diag_cache_engine.py
"""
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA, REPO):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402


def _cylinder():
    """A mesh the engines read differently (classic unknown, v2 mechanical)."""
    try:
        import trimesh
    except Exception:
        return None
    cyl = trimesh.creation.cylinder(radius=1.0, height=3.0, sections=64)
    return (np.asarray(cyl.vertices, dtype=np.float32),
            np.asarray(cyl.faces, dtype=np.int32))


def _run(diag, v, t, engine, cache):
    a = diag.analyze_mesh(v, t, engine=engine, use_cache=cache)
    return (a.mesh_type, round(float(a.type_confidence), 4),
            a.classifier_engine)


def test_analysis_cache_is_engine_keyed():
    mesh = _cylinder()
    if mesh is None:
        print('skip: trimesh unavailable')
        return
    v, t = mesh

    saved = {k: os.environ.get(k) for k in ('XDG_CACHE_HOME', 'SUTURA_DATA_DIR',
                                            'SUTURA_NO_CACHE')}
    try:
        with tempfile.TemporaryDirectory() as td:
            os.environ['XDG_CACHE_HOME'] = os.path.join(td, 'cache')
            os.environ['SUTURA_DATA_DIR'] = os.path.join(td, 'data')
            os.environ.pop('SUTURA_NO_CACHE', None)

            from sutura_engine import diagnosis as diag

            u_c = _run(diag, v, t, 'classic', False)
            u_e = _run(diag, v, t, 'experimental', False)
            print('uncached classic     :', u_c)
            print('uncached experimental:', u_e)

            # classic first, then experimental on the same mesh, cache on:
            # the second result must be the experimental one, not classic's.
            c_c = _run(diag, v, t, 'classic', True)
            c_e = _run(diag, v, t, 'experimental', True)
            print('cached   classic     :', c_c)
            print('cached   experimental:', c_e)
            assert c_c[:2] == u_c[:2], (c_c, u_c)
            assert c_e[:2] == u_e[:2], (c_e, u_e)
            assert c_e[2] == 'experimental', c_e

        # fresh cache, reversed order: experimental then classic.
        with tempfile.TemporaryDirectory() as td:
            os.environ['XDG_CACHE_HOME'] = os.path.join(td, 'cache')
            from sutura_engine import diagnosis as diag
            r_e = _run(diag, v, t, 'experimental', True)
            r_c = _run(diag, v, t, 'classic', True)
            assert r_e[:2] == u_e[:2], (r_e, u_e)
            assert r_c[:2] == u_c[:2], (r_c, u_c)
            assert r_c[2] == 'classic', r_c
    finally:
        for k, val in saved.items():
            if val is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = val

    print('ok  test_analysis_cache_is_engine_keyed')


if __name__ == '__main__':
    test_analysis_cache_is_engine_keyed()
