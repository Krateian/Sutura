#!/usr/bin/env python3
"""Regression test for the print-impact report (pipeline/report.py).

Checks the per-defect decomposition of a finished repair report:
  1. build_print_report maps stage1 fields to severity-coded issues/fixed and
     derives ``orientation_corrected`` and the removed-shell count;
  2. it adds NO top-line verdict (the existing repair_score Status stays);
  3. the deviation gate reports ``not_assessed`` above DEVIATION_MAX_FACES;
  4. a tier's per-vertex deviation is reused instead of re-measured;
  5. aggregate_print_reports merges per-object reports worst-case;
  6. human_print_report renders the block;
  7. (measured path) the Rust closest_points deviation is roughly right on a
     known offset (skipped when sutura_geom is unavailable).
Usage: python3 tests/test_print_report.py
"""
import os
import sys

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.join(REPO, 'sutura'))

from sutura_engine.pipeline.report import (  # noqa: E402
    DEVIATION_MAX_FACES, SEVERITY_BREAKS, SEVERITY_COSMETIC, SEVERITY_MAY,
    aggregate_print_reports, build_print_report, human_print_report,
)


def _report(**s1):
    base = {
        'stage1': {'two_manifold': True, 'holes_remaining': 0,
                   'non_manifold_edges_remaining': 0,
                   'holes_closed': 2, 'non_manifold_edges_fixed': 1,
                   'faces_removed': 3, 'components': 1, 'components_before': 1,
                   'volume_change_percent': 0.0},
    }
    base['stage1'].update(s1)
    return base


def test_clean_report_has_no_issues():
    pr = build_print_report(_report(), in_arrays=None, out_arrays=None)
    assert pr is not None
    assert pr['issues'] == [], pr['issues']
    assert pr['advisories'] == [], pr['advisories']
    assert pr['not_assessed'] == ['deviation'], pr['not_assessed']
    fixed = {f['code']: f['count'] for f in pr['fixed']}
    assert fixed.get('holes_closed') == 2
    assert fixed.get('non_manifold_fixed') == 1
    assert fixed.get('faces_removed') == 3


def test_no_verdict_key():
    pr = build_print_report(_report(), in_arrays=None, out_arrays=None)
    assert 'verdict' not in pr
    assert 'printable' not in pr


def test_severities_and_codes():
    pr = build_print_report(_report(
        holes_remaining=4, non_manifold_edges_remaining=2,
        self_intersections_remaining=12, components=3, components_before=5,
        volume_change_percent=20.0), in_arrays=None, out_arrays=None)
    sev = {i['code']: i['severity'] for i in pr['issues']}
    assert sev['holes_remaining'] == SEVERITY_BREAKS
    assert sev['non_manifold_remaining'] == SEVERITY_BREAKS
    assert sev['self_intersections_remaining'] == SEVERITY_COSMETIC
    assert sev['multiple_shells'] == SEVERITY_MAY
    assert sev['volume_change'] == SEVERITY_BREAKS
    # breaks ranked before cosmetic
    assert pr['issues'][0]['severity'] == SEVERITY_BREAKS
    fixed = {f['code']: f['count'] for f in pr['fixed']}
    assert fixed.get('shells_removed') == 2


def test_volume_not_measurable():
    """C2: an open input has no measurable volume, so volume_change_percent is
    None. The print report lists 'volume' as not assessed and the human report
    states it plainly instead of printing 0.0 or a blank."""
    from sutura_engine.pipeline.report import human_report
    r = _report(volume_change_percent=None, volume_before=0.0,
                volume_after=0.0)
    pr = build_print_report(r, in_arrays=None, out_arrays=None)
    assert 'volume' in pr['not_assessed']
    text = human_report(r)
    assert 'Volume change          : n/a' in text
    assert 'input volume not measurable' in text


def test_orientation_corrected():
    pr = build_print_report(
        _report(volume_before=-10.0, volume_after=10.0),
        in_arrays=None, out_arrays=None)
    assert {'code': 'orientation_corrected', 'count': 1} in pr['fixed']
    pr2 = build_print_report(
        _report(volume_before=10.0, volume_after=10.0),
        in_arrays=None, out_arrays=None)
    assert all(f['code'] != 'orientation_corrected' for f in pr2['fixed'])


def test_deviation_gate_not_assessed():
    big = np.zeros((DEVIATION_MAX_FACES + 1, 3), dtype=np.int32)
    pr = build_print_report(
        _report(faces_before=DEVIATION_MAX_FACES + 1),
        in_arrays=(np.zeros((3, 3)), np.zeros((DEVIATION_MAX_FACES + 1, 3),
                                             dtype=np.int32)),
        out_arrays=(np.zeros((3, 3)), big))
    assert 'deviation' in pr['not_assessed']
    assert pr['geometry']['deviation_max_pct'] is None


def test_reuses_tier_deviation():
    r = _report()
    r['graft'] = {'per_vertex_deviation': np.array([10.0, 0.0, 5.0])}
    pr = build_print_report(
        r, in_arrays=(np.array([[0, 0, 0], [100, 0, 0]]), np.zeros((1, 3), np.int32)),
        out_arrays=None)
    # diag = 100 -> max 10% mean 5%
    assert pr['geometry']['deviation_source'] == 'graft'
    assert abs(pr['geometry']['deviation_max_pct'] - 10.0) < 1e-6
    assert abs(pr['geometry']['deviation_mean_pct'] - 5.0) < 1e-6
    assert 'deviation' not in pr['not_assessed']


def test_dressing_reuse():
    r = _report()
    r['dressing'] = {'hausdorff_rel_max': 0.03}
    pr = build_print_report(
        r, in_arrays=(np.array([[0, 0, 0], [100, 0, 0]]), np.zeros((1, 3), np.int32)),
        out_arrays=None)
    assert pr['geometry']['deviation_source'] == 'dressing'
    assert abs(pr['geometry']['deviation_max_pct'] - 3.0) < 1e-6


def test_aggregate():
    a = build_print_report(_report(holes_remaining=1), None, None)
    b = build_print_report(_report(volume_change_percent=-20.0), None, None)
    agg = aggregate_print_reports([a, b, None])
    codes = sorted(i['code'] for i in agg['issues'])
    assert codes == ['holes_remaining', 'volume_change']
    assert agg['geometry']['volume_change_pct'] == -20.0
    assert aggregate_print_reports([]) is None


def test_human_block():
    pr = build_print_report(_report(holes_remaining=1), None, None)
    text = human_print_report(pr)
    assert 'Print impact:' in text
    assert 'hole' in text
    assert human_print_report(None) is None


# Four equal-area triangles -> a flagged count of k is an area fraction k/4.
_THIN_ARR = (np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]] * 4, dtype=float),
             np.array([[0, 1, 2], [3, 4, 5], [6, 7, 8], [9, 10, 11]],
                      dtype=np.int32))


def _fake_thin_geom(ratio, thin_faces=1, omitted=False):
    import types
    mask = {'encoding': 'rle', 'length': 4, 'omitted': omitted,
            'runs': [] if omitted else [(0, thin_faces, 128)]}
    thin_wall = {'available': True, 'thin_ratio': ratio,
                 'thin_faces': thin_faces, 'min_norm': 0.002,
                 'median_norm': 0.004, 'threshold_norm': 0.005,
                 'estimated': True, 'min': 0.2, 'median': 0.4,
                 'threshold_mm': 0.5, 'units': 'mm_assumed'}
    return types.SimpleNamespace(
        diagnose=lambda v, t, detail='fast', masks=True:
        {'thin_wall': thin_wall, 'face_mask': mask})


def _with_geom(geom, fn):
    import sys
    saved = sys.modules.get('sutura_geom')
    sys.modules['sutura_geom'] = geom
    try:
        return fn()
    finally:
        if saved is not None:
            sys.modules['sutura_geom'] = saved
        else:
            sys.modules.pop('sutura_geom', None)


def test_thin_wall_advisory_trigger():
    """Both calibrated conditions must hold: thin_ratio >= THIN_WALL_MIN_RATIO
    and the flagged-face area fraction >= THIN_WALL_MIN_AREA_FRAC."""
    from sutura_engine.pipeline.report import (
        THIN_WALL_MIN_AREA_FRAC, THIN_WALL_MIN_RATIO, _thin_wall_advisory)
    assert THIN_WALL_MIN_RATIO == 0.02 and THIN_WALL_MIN_AREA_FRAC == 0.005

    # fires: ratio 0.05, one of four equal-area faces flagged (area 0.25)
    fires = _with_geom(_fake_thin_geom(0.05, thin_faces=1),
                       lambda: _thin_wall_advisory({'stage1': {}}, _THIN_ARR))
    assert fires['code'] == 'thin_wall'
    assert fires['thin_area_frac'] == 0.25
    assert fires['min_mm'] == 0.2

    # no fire: ratio below the floor
    assert _with_geom(_fake_thin_geom(0.01, thin_faces=1),
                      lambda: _thin_wall_advisory({'stage1': {}}, _THIN_ARR)) is None
    # no fire: thin area below the floor (no flagged faces)
    assert _with_geom(_fake_thin_geom(0.05, thin_faces=0),
                      lambda: _thin_wall_advisory({'stage1': {}}, _THIN_ARR)) is None
    # no fire: face mask omitted (cannot measure the area)
    assert _with_geom(_fake_thin_geom(0.05, omitted=True),
                      lambda: _thin_wall_advisory({'stage1': {}}, _THIN_ARR)) is None
    # unit warning keeps the advisory but suppresses the absolute mm label
    warned = _with_geom(_fake_thin_geom(0.05, thin_faces=1),
                        lambda: _thin_wall_advisory({'stage1': {},
                                                     'unit_warning': True},
                                                    _THIN_ARR))
    assert warned['code'] == 'thin_wall' and 'min_mm' not in warned


def test_thin_wall_advisory_empty_faces():
    """An empty face list yields no advisory."""
    from sutura_engine.pipeline.report import _thin_wall_advisory
    geom = _fake_thin_geom(0.05, thin_faces=1)
    assert _with_geom(geom, lambda: _thin_wall_advisory(
        {'stage1': {}}, (np.zeros((4, 3)), np.zeros((0, 3), np.int32)))) is None


def test_advisory_human_and_aggregate():
    pr = build_print_report(_report(), None, None)
    pr['advisories'] = [{'code': 'thin_wall', 'message_key': 'pr_thin_wall',
                         'thin_ratio': 0.05, 'regions': 120, 'min_norm': 0.002,
                         'median_norm': 0.004, 'threshold_norm': 0.005,
                         'estimated': True, 'heuristic': True, 'min_mm': 0.2,
                         'median_mm': 0.4, 'threshold_mm': 0.5,
                         'units': 'mm_assumed'}]
    text = human_print_report(pr)
    assert 'Advisory:' in text
    assert 'thin walls' in text
    assert '0.20 mm' in text
    assert '[estimated]' in text
    agg = aggregate_print_reports([pr])
    assert agg['advisories'][0]['code'] == 'thin_wall'
    assert aggregate_print_reports(
        [build_print_report(_report(), None, None)])['advisories'] == []


def test_measured_deviation_matches_offset():
    try:
        import sutura_geom  # noqa: F401
    except Exception:  # noqa: BLE001
        print('  (skip measured deviation: sutura_geom unavailable)')
        return
    tri = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0], [0, 0, 1]], dtype=np.int32)
    faces = np.array([[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]],
                     dtype=np.int32)
    # output shifted 0.1 along +x; diagonal of the unit cube is sqrt(3)
    empty = {'stage1': {'two_manifold': True, 'holes_remaining': 0,
                        'non_manifold_edges_remaining': 0, 'components': 1,
                        'components_before': 1}}
    out = (tri.astype(float) + np.array([0.1, 0, 0]), faces)
    pr = build_print_report(empty, in_arrays=(tri.astype(float), faces),
                            out_arrays=out)
    diag = float(np.linalg.norm(np.array([1.0, 1.0, 1.0])))
    assert pr['geometry']['deviation_source'] == 'measured'
    expect = 0.1 / diag * 100
    assert abs(pr['geometry']['deviation_max_pct'] - expect) < 0.5


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith('test_')]
    for t in tests:
        t()
        print('ok -', t.__name__)
    print('\n%d passed' % len(tests))


if __name__ == '__main__':
    main()
