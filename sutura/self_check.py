# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura --self-check: deterministic health and method verification.

Evaluates all 17 registered repair methods:
  - Prerequisite and Rust symbol checks via sutura_engine.health
  - Deterministic fixture execution for fast methods (<5s total budget)
  - Availability-only checks for heavy iterative methods (Poisson, proxy) and interactive addons
  - Formatted human output: 'Methods: N/17 OK (X failed, Y skipped)'
"""
import os
import sys
import tempfile
import time
from typing import Any, Dict, List, Optional, Tuple

import numpy as np

# Methods that cannot fit the <5s budget (iterative solvers: Screened Poisson,
# proxy template resampling, subprocess tetrahedralization) or require manual input.
# These are verified via health.check_availability_detail() (imports, symbols, and configs).
AVAILABILITY_ONLY_METHODS = {
    'balloon',          # Method 8: Poisson reconstruction takes ~3s
    'scaffold',         # Method 10: Proxy template resampling takes ~2s
    'mirror_complete',  # Method 14: Symmetrized Poisson reconstruction takes ~3s
    'ftetwild',         # Method 7: Subprocess tetrahedralization
    'transplant_plus',  # Method 12: Interactive manual transplant
}


def get_self_check_fixture() -> Tuple[np.ndarray, np.ndarray]:
    """Generate a tiny 6-vertex, 6-triangle deterministic open shell (octahedron with hole)."""
    verts = np.array([
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, -1.0],
    ], dtype=np.float32)
    # 6 triangles of an octahedron (missing two adjacent faces forming a boundary loop)
    tris = np.array([
        [0, 1, 2],
        [0, 2, 3],
        [0, 3, 4],
        [5, 2, 1],
        [5, 3, 2],
        [5, 4, 3],
    ], dtype=np.int32)
    return verts, tris


def run_self_check(run_fixtures: bool = True) -> Dict[str, Any]:
    """Execute the self-check suite and return results dict."""
    try:
        from sutura_engine import health
        from sutura_engine.methods import all_methods
    except ImportError:
        try:
            import health
            from methods import all_methods
        except ImportError:
            return {
                'total': 0, 'ok': 0, 'failed': 1, 'skipped': 0,
                'error': 'sutura_engine.health is unavailable',
                'methods': {},
            }

    methods_list = all_methods()
    avail_details = health.check_availability_detail()
    method_results = {}

    verts, tris = get_self_check_fixture()

    t_start = time.time()
    with tempfile.TemporaryDirectory() as tmpdir:
        for m in methods_list:
            mid = m.id
            avail_status, avail_reason = avail_details.get(mid, (health.Status.OK, None))

            # If availability check already skipped or failed, don't run fixture
            if avail_status == health.Status.SKIP:
                method_results[mid] = {
                    'num': m.num,
                    'name': m.name,
                    'status': health.Status.SKIP.value,
                    'reason': avail_reason,
                    'elapsed_s': 0.0,
                    'mode': 'skipped',
                }
                continue

            if avail_status == health.Status.FAIL:
                method_results[mid] = {
                    'num': m.num,
                    'name': m.name,
                    'status': health.Status.FAIL.value,
                    'reason': avail_reason,
                    'elapsed_s': 0.0,
                    'mode': 'availability',
                }
                continue

            # Method is available. If it cannot fit <5s budget or fixtures are disabled:
            if not run_fixtures or mid in AVAILABILITY_ONLY_METHODS:
                method_results[mid] = {
                    'num': m.num,
                    'name': m.name,
                    'status': health.Status.OK.value,
                    'reason': None,
                    'elapsed_s': 0.0,
                    'mode': 'availability_only',
                }
                continue

            # Fast method: run on deterministic fixture
            t0 = time.time()
            try:
                m.run(verts, tris, tmpdir)
                dt = time.time() - t0
                method_results[mid] = {
                    'num': m.num,
                    'name': m.name,
                    'status': health.Status.OK.value,
                    'reason': None,
                    'elapsed_s': dt,
                    'mode': 'fixture',
                }
            except Exception as e:
                dt = time.time() - t0
                method_results[mid] = {
                    'num': m.num,
                    'name': m.name,
                    'status': health.Status.FAIL.value,
                    'reason': 'fixture execution failed: %s' % e,
                    'elapsed_s': dt,
                    'mode': 'fixture',
                }

    total_time = time.time() - t_start
    ok_count = sum(1 for v in method_results.values() if v['status'] == health.Status.OK.value)
    fail_count = sum(1 for v in method_results.values() if v['status'] == health.Status.FAIL.value)
    skip_count = sum(1 for v in method_results.values() if v['status'] == health.Status.SKIP.value)

    return {
        'total': len(method_results),
        'ok': ok_count,
        'failed': fail_count,
        'skipped': skip_count,
        'total_time_s': total_time,
        'methods': method_results,
    }


def run_self_check_cli() -> int:
    """CLI runner for sutura --self-check."""
    print("Sutura Self-Check:")
    report = run_self_check(run_fixtures=True)

    try:
        from sutura_engine import health
        human_summary = health.human_line(report)
    except Exception:
        human_summary = f"Methods: {report['ok']}/{report['total']} OK"

    for mid, res in sorted(report.get('methods', {}).items(), key=lambda x: x[1].get('num', 0)):
        st = res['status']
        num = int(res.get('num', 0))
        name = str(res.get('name') or mid)
        mode = res['mode']
        dt = res['elapsed_s']
        reason = res.get('reason')

        if st == 'ok':
            if mode == 'fixture':
                detail = f"({dt:.3f}s)"
            else:
                detail = "(available)"
            print(f"  [OK]   #{num:2d} {name:18s} {detail}")
        elif st == 'skip':
            print(f"  [SKIP] #{num:2d} {name:18s} ({reason or 'optional addon not installed'})")
        else:
            print(f"  [FAIL] #{num:2d} {name:18s} ({reason or 'failed'})")

    print(f"\n{human_summary} in {report.get('total_time_s', 0):.2f}s")
    return 1 if report['failed'] > 0 else 0


if __name__ == '__main__':
    sys.exit(run_self_check_cli())
