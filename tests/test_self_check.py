# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Test suite for Sutura --self-check and health verification."""
import os
import sys
from unittest.mock import MagicMock, patch

os.environ['QT_QPA_PLATFORM'] = 'offscreen'
# Self-sufficient import path (no PYTHONPATH needed): REPO_DIR makes the
# dotted `sutura.*` package importable, REPO_DIR/sutura makes the bare `gui`
# and `sutura_engine` modules importable. Same bootstrap as test_dressing.py.
REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
for _path in (REPO_DIR, os.path.join(REPO_DIR, 'sutura')):
    if _path not in sys.path:
        sys.path.insert(0, _path)

from PySide6.QtWidgets import QApplication
_APP = QApplication.instance() or QApplication([])

import numpy as np

from sutura.self_check import (
    AVAILABILITY_ONLY_METHODS,
    get_self_check_fixture,
    run_self_check,
    run_self_check_cli,
)
from sutura.sutura_engine import health


def test_self_check_fixture_geometry_invariants():
    """Verify that the embedded fixture is a deterministic open shell with 6 vertices and 6 faces."""
    verts, tris = get_self_check_fixture()
    assert isinstance(verts, np.ndarray)
    assert isinstance(tris, np.ndarray)
    assert verts.shape == (6, 3)
    assert tris.shape == (6, 3)
    assert verts.dtype == np.float32
    assert tris.dtype == np.int32

    # Invariants: bounding box fits within [-1, 1] cube
    assert np.all(verts >= -1.0)
    assert np.all(verts <= 1.0)

    # Face indices are valid vertex references
    assert np.min(tris) >= 0
    assert np.max(tris) < 6


def test_availability_only_partition_set():
    """Ensure heavy iterative methods and interactive addons are partitioned."""
    expected = {'balloon', 'scaffold', 'mirror_complete', 'ftetwild', 'transplant_plus'}
    assert AVAILABILITY_ONLY_METHODS == expected


def test_self_check_runs_within_budget():
    """Verify self-check executes all 19 methods within the <5 s budget."""
    report = run_self_check(run_fixtures=True)
    assert report['total'] == 19
    assert report['failed'] == 0
    assert report['total_time_s'] < 5.0, f"Self-check exceeded budget: {report['total_time_s']:.2f}s"

    methods = report['methods']
    for mid, res in methods.items():
        if mid in AVAILABILITY_ONLY_METHODS:
            assert res['mode'] in ('availability_only', 'skipped')
        else:
            assert res['mode'] in ('fixture', 'skipped')
            if res['status'] == 'ok':
                assert res['elapsed_s'] < 3.0


def test_self_check_cli_exit_code_success():
    """Verify CLI returns exit code 0 when all checks pass."""
    exit_code = run_self_check_cli()
    assert exit_code == 0


def test_self_check_cli_exit_code_failure():
    """Verify CLI returns exit code 1 when any method fails."""
    fake_avail = {
        'quick_clean': (health.Status.FAIL, 'Mocked symbol failure'),
    }
    m = MagicMock()
    m.id = 'quick_clean'
    m.num = 1
    m.name = 'Quick Clean'
    with patch('sutura_engine.health.check_availability_detail', return_value=fake_avail):
        with patch('sutura_engine.methods.all_methods', return_value=[m]):
            report = run_self_check(run_fixtures=True)
            assert report['failed'] == 1
            code = run_self_check_cli()
            assert code == 1


def test_gui_health_banner_behavior():
    """Verify GUI startup banner shows only on Status.FAIL and hides on clean health."""
    from gui import MainWindow

    win = MainWindow()
    # On clean environment, banner should be hidden
    assert win.health_banner.isHidden()

    # Simulate availability failure
    fake_avail = {
        'quick_clean': (health.Status.FAIL, 'Simulated failure'),
        'join': (health.Status.OK, None),
    }
    with patch('sutura_engine.health.check_availability_detail', return_value=fake_avail):
        win._check_method_health()
        assert not win.health_banner.isHidden()
        assert 'Some repair capabilities failed availability checks' in win.health_banner_label.text()


