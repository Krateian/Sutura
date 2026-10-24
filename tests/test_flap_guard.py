#!/usr/bin/env python3
"""Regression tests for the Flap blow-up / self-intersection adoption guard.

Needs only numpy (the guard is a pure function; importing the tier module also
pulls the mandatory sutura_geom extension). Runnable directly:
    <repair-venv>/bin/python tests/test_flap_guard.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for path in (SUTURA, REPO):
    if path not in sys.path:
        sys.path.insert(0, path)

from sutura_engine.pipeline.tiers.flap import (  # noqa: E402
    _flap_blowup_reason,
)


def main():
    # The measured failure: 6,073 input faces / 8 loops / 93 loop vertices
    # became a 303,335-face, 151,675-vertex candidate. It must be rejected.
    reason = _flap_blowup_reason(6073, 3077, 303335, 151675, 93)
    assert reason is not None, reason
    assert 'face' in reason or 'vertex' in reason, reason

    # A small local refinement of a small mesh stays allowed.
    assert _flap_blowup_reason(12, 8, 30, 20, 4) is None
    assert _flap_blowup_reason(12, 8, 40, 24, 4) is None

    # A modest patch on a large input is not a blow-up even when it exceeds
    # the loop-slack term; the relative ceiling protects it.
    assert _flap_blowup_reason(90000, 45000, 95000, 47000, 100) is None

    # A face-only explosion is rejected even when the vertex count is sane.
    assert _flap_blowup_reason(1000, 500, 10000, 600, 40) is not None

    # A vertex-only explosion is rejected even when the face count is sane.
    assert _flap_blowup_reason(1000, 500, 1200, 5000, 40) is not None

    print('test_flap_guard: ok')


if __name__ == '__main__':
    main()
