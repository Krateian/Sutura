#!/usr/bin/env python3
"""Damage-channel fallback pruning (triage._prune_avoid_nums / opt-out).

The pruning reads only the direct diagnosis-v2 channels:
  * ``duplicate`` > 0                     -> skip #4  Join
  * ``interior`` > 0 or an interior region -> skip #11 Transplant and
                                              #15 Wall Thicken
It is ON by default and opts out with SUTURA_TRIAGE_PRUNE=0.

Usage: <venv>/bin/python tests/test_triage_prune.py
"""
import os
import sys
from types import SimpleNamespace

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import sutura_engine.triage as triage  # noqa: E402


def _analysis(**channels):
    return SimpleNamespace(diagnosis_v2={'channels': channels, 'regions': []})


def _ch(count):
    return {'count': count}


def test_no_diagnosis_prunes_nothing():
    assert triage._prune_avoid_nums(SimpleNamespace(diagnosis_v2=None)) == set()
    assert triage._prune_avoid_nums(SimpleNamespace()) == set()


def test_duplicate_channel_skips_join():
    assert triage._prune_avoid_nums(_analysis(duplicate=_ch(3))) == {4}
    assert triage._prune_avoid_nums(_analysis(duplicate=_ch(0))) == set()


def test_interior_channel_skips_transplant_and_wall_thicken():
    assert triage._prune_avoid_nums(_analysis(interior=_ch(12))) == {11, 15}


def test_interior_region_skips_transplant_and_wall_thicken():
    a = SimpleNamespace(diagnosis_v2={
        'channels': {'interior': _ch(0)},
        'regions': [{'channel': 'interior', 'kind': 'interior_surface'}]})
    assert triage._prune_avoid_nums(a) == {11, 15}


def test_both_channels_union():
    assert triage._prune_avoid_nums(
        _analysis(duplicate=_ch(2), interior=_ch(5))) == {4, 11, 15}


def test_prune_opt_out_env():
    assert triage._triage_prune_enabled({}) is True
    assert triage._triage_prune_enabled({'SUTURA_TRIAGE_PRUNE': '1'}) is True
    for off in ('0', 'false', 'no', 'off', ' OFF '):
        assert triage._triage_prune_enabled({'SUTURA_TRIAGE_PRUNE': off}) is False


if __name__ == '__main__':
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok', name)
