"""One self-intersection definition: the diagnosis SI face bit (16) must equal
`sutura_geom.si_faces_exact` (the crate's authoritative `melt::scan_si`:
exact proper crossings).  Coplanar overlaps must stay in the diagnosis coplanar
channel, not the SI bit.  `self_intersecting_faces` (the BVH float classifier)
is reported for reference only; its difference from `si_faces_exact` is exactly
the set of faces whose diagnosis SI bit changes."""
import numpy as np
import sutura_geom

SI_BIT = 16


def diagnose_si_mask(v, t):
    d = sutura_geom.diagnose(v, t, detail='fast', masks=True)
    runs = (d.get('face_mask') or {}).get('runs') or []
    m = np.zeros(len(t), bool)
    for s, ln, val in runs:
        if int(val) & SI_BIT:
            m[int(s):int(s) + int(ln)] = True
    return m


def exact_si_mask(v, t):
    m, _ = sutura_geom.si_faces_exact(v, t)
    return np.asarray(m, bool)


def bvh_si_mask(v, t):
    m, _ = sutura_geom.self_intersecting_faces(v, t)
    return np.asarray(m, bool)


def fixture():
    v = np.array([
        [0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0],
        [0.5, 0.5, -1.0], [1.0, 0.5, 1.0], [0.5, 1.0, 1.0],
        [1.4, 0.3, 0.0], [1.6, 0.3, 0.0], [1.4, 0.5, 0.0],
    ], float)
    t = np.array([[0, 1, 2], [3, 4, 5], [6, 7, 8]], np.int32)
    return v, t


def test_fixture():
    v, t = fixture()
    ex = exact_si_mask(v, t)
    di = diagnose_si_mask(v, t)
    assert ex.tolist() == [True, True, False], ex
    assert np.array_equal(ex, di), (ex, di)


def test_real(paths):
    bad = []
    import sys
    sys.path.insert(0, '/Users/krateian/sutura-wt/lava-melt/sutura')
    from sutura_engine.pipeline.mesh_io import read_mesh
    for p in paths:
        v, t = read_mesh(p)
        v = np.asarray(v, np.float64)
        t = np.asarray(t, np.int32)
        ex = exact_si_mask(v, t)
        di = diagnose_si_mask(v, t)
        bv = bvh_si_mask(v, t)
        diff = int((ex != di).sum())
        drift = int((ex != bv).sum())
        print('%-32s si_exact=%-5d si_diag=%-5d diff=%-3d  bvh_drift=%d'
              % (p.split('/')[-1], ex.sum(), di.sum(), diff, drift))
        if diff:
            bad.append((p, diff))
    return bad


if __name__ == '__main__':
    test_fixture()
    print('fixture OK: diagnosis SI bit == si_faces_exact (scan_si)')
    import os
    cand = '/Users/krateian/sutura-bench/runs/v098-shape/candidate'
    picks = ['thingi10k_100827.stl', 'core_syn_40051_hole_porosity.stl',
             'broken_46790.stl', 'broken_38465.stl']
    paths = [cand + '/' + n for n in picks]
    if all(os.path.exists(p) for p in paths):
        bad = test_real(paths)
        assert not bad, 'mismatch: %s' % bad
        print('all real meshes agree: diagnosis SI bit == si_faces_exact')
    else:
        print('candidate corpus not present; skipped the real-mesh cross-check')
