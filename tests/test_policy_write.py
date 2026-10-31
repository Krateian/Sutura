#!/usr/bin/env python3
"""Panel-owned user-policy writer tests.

Usage: <venv>/bin/python tests/test_policy_write.py
"""
import os
import stat
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

_TMP = tempfile.mkdtemp(prefix='sutura-policy-write-')
USER = os.path.join(_TMP, 'policy.toml')
os.environ['SUTURA_POLICY_USER'] = USER

import sutura_engine.policy as pol  # noqa: E402


def _write(text, mode=None):
    with open(USER, 'w', encoding='utf-8') as f:
        f.write(text)
    if mode is not None:
        os.chmod(USER, mode)


def _read():
    return _read_from(USER)


def _read_from(path):
    with open(path, encoding='utf-8') as f:
        return f.read()


def _fresh():
    for name in (USER, USER + '.bak'):
        if os.path.exists(name):
            os.unlink(name)


def test_default_order_write_and_hash():
    _fresh()
    default = open(pol.DEFAULT_POLICY_PATH, 'rb').read()
    order = ['local', 'graft', 'splint', 'coaptation', 'ftetwild']
    eff = pol.write_user_policy({'order': {'deep_tier_order': order}})
    assert eff['order']['deep_tier_order'] == order
    assert open(pol.DEFAULT_POLICY_PATH, 'rb').read() == default
    text = _read()
    assert 'Method control panel' in text
    assert 'deep_tier_order = ["local", "graft", "splint", "coaptation",' in text
    loaded = pol.load_policy()
    assert loaded['order']['deep_tier_order'] == order
    assert pol.policy_hash(loaded) == pol.policy_hash(eff)


def test_bak_and_preserve_other_user_keys():
    _fresh()
    _write('[ranking]\ntop_n = 4\n')
    first = ['local', 'graft', 'dressing', 'splint', 'coaptation', 'ftetwild']
    pol.write_user_policy({'order': {'deep_tier_order': first}})
    second = ['local', 'graft', 'splint', 'coaptation', 'ftetwild']
    pol.write_user_policy({'order': {'deep_tier_order': second}})
    assert os.path.exists(USER + '.bak')
    assert 'top_n = 4' in _read()
    assert pol.load_policy()['ranking']['top_n'] == 4
    assert pol.load_policy()['order']['deep_tier_order'] == second


def test_locked_existing_keys_are_preserved_verbatim():
    _fresh()
    _write('[si_escalation]\nvol_rel_tol = 0.5\n\n'
           '[gates.tuning]\nhausdorff_floor = 1\n\n[ranking]\ntop_n = 4\n')
    before = _read()
    pol.write_user_policy({'order': {'deep_tier_order': ['local', 'graft']}})
    text = _read()
    assert 'vol_rel_tol = 0.5' in text
    assert 'hausdorff_floor = 1' in text
    assert 'top_n = 4' in text
    assert before.split('[ranking]')[1] in text.split('[ranking]')[1]
    # They are still ignored by the effective policy.
    loaded = pol.load_policy()
    assert loaded['si_escalation']['vol_rel_tol'] == 0.005
    assert loaded['gates']['tuning']['hausdorff_floor'] == 200000


def test_gate_refused_and_file_untouched():
    _fresh()
    before = _read() if os.path.exists(USER) else None
    try:
        pol.write_user_policy({'si_escalation': {'vol_rel_tol': 0.5}})
    except ValueError as exc:
        assert 'locked gate' in str(exc)
    else:
        raise AssertionError('a locked section must be refused')
    assert (_read() if os.path.exists(USER) else None) == before


def test_gate_table_refused():
    for change in ({'gates': {'tuning': {'hausdorff_floor': 1}}},
                   {'tiers': {'flap': {'gates': {'debris_flatness': 1}}}},
                   {'auto': {'generative_max_hausdorff_rel': 0.5}}):
        try:
            pol.write_user_policy(change)
        except ValueError as exc:
            assert 'locked gate' in str(exc), (change, exc)
        else:
            raise AssertionError('locked gate accepted: %r' % (change,))


def test_unknown_and_invalid_order_refused():
    for change in ({'ranking': {'top_n': 4}},
                   {'order': {'deep_tier_order': []}},
                   {'order': {'deep_tier_order': ['local', 'nope']}},
                   {'order': {'deep_tier_order': ['local', 'local']}},
                   {'order': {'deep_tier_order': ['local', 'splint', 'graft']}},
                   {'order': {'deep_tier_order': ['ftetwild', 'graft']}}):
        try:
            pol.write_user_policy(change)
        except ValueError:
            pass
        else:
            raise AssertionError('invalid change accepted: %r' % (change,))


def test_malformed_existing_refused_without_overwrite():
    _fresh()
    _write('this is not toml [[[\n')
    before = _read()
    try:
        pol.write_user_policy({'order': {'deep_tier_order':
                                         ['local', 'graft']}})
    except ValueError:
        pass
    else:
        raise AssertionError('malformed existing file must be refused')
    assert _read() == before


def test_reset_removes_only_the_order_override():
    _fresh()
    _write('[order]\ndeep_tier_order = ["local", "graft"]\n\n'
           '[ranking]\ntop_n = 4\n')
    eff = pol.write_user_policy(reset=True)
    text = _read()
    assert '\n[order]' not in text and 'deep_tier_order' not in text
    assert 'top_n = 4' in text
    assert eff['order']['deep_tier_order'] == [
        'local', 'graft', 'dressing', 'splint', 'coaptation', 'ftetwild']
    # With only the panel-owned key, reset removes the file and keeps a .bak.
    _fresh()
    _write('[order]\ndeep_tier_order = ["local", "graft"]\n')
    pol.write_user_policy(reset=True)
    assert not os.path.exists(USER)
    assert os.path.exists(USER + '.bak')


def test_reset_recovers_an_unreadable_file():
    _fresh()
    _write('this is not toml [[[\n')
    old = _read()
    eff = pol.write_user_policy(reset=True)
    assert not os.path.exists(USER)
    assert _read_from(USER + '.bak') == old
    assert 'moved to policy.toml.bak' in (pol.LAST_WRITE_WARNING or '')
    assert eff['order']['deep_tier_order'] == [
        'local', 'graft', 'dressing', 'splint', 'coaptation', 'ftetwild']


def test_float_string_and_mixed_list_round_trip():
    _fresh()
    _write('[ranking]\npreference_boost = 1.5\n\n'
           '[intensity]\ndefault = "quick"\n\n'
           '[intensity.thorough]\ndense_target_ladder = [1.5, 3.0, "threshold"]\n')
    eff = pol.write_user_policy(
        {'order': {'deep_tier_order': ['local', 'graft', 'splint']}})
    loaded = pol.load_policy()
    assert loaded['ranking']['preference_boost'] == 1.5
    assert loaded['intensity']['default'] == 'quick'
    assert loaded['intensity']['thorough']['dense_target_ladder'] == [
        1.5, 3.0, 'threshold']
    assert pol.policy_hash(loaded) == pol.policy_hash(eff)


def test_nonfinite_and_unsupported_existing_are_refused_with_key():
    _fresh()
    _write('[ranking]\npreference_boost = inf\n')
    before = _read()
    try:
        pol.write_user_policy({'order': {'deep_tier_order': ['local']}})
    except ValueError as exc:
        assert 'preference_boost' in str(exc), exc
    else:
        raise AssertionError('non-finite float accepted')
    assert _read() == before

    _fresh()
    _write('[[foo]]\nx = 1\n')
    before = _read()
    try:
        pol.write_user_policy({'order': {'deep_tier_order': ['local']}})
    except ValueError as exc:
        assert 'foo' in str(exc), exc
    else:
        raise AssertionError('array-of-tables accepted')
    assert _read() == before


def test_existing_file_mode_is_preserved():
    _fresh()
    _write('[ranking]\ntop_n = 4\n', mode=0o640)
    pol.write_user_policy({'order': {'deep_tier_order': ['local']}})
    assert stat.S_IMODE(os.stat(USER).st_mode) == 0o640


if __name__ == '__main__':
    test_default_order_write_and_hash()
    test_bak_and_preserve_other_user_keys()
    test_locked_existing_keys_are_preserved_verbatim()
    test_gate_refused_and_file_untouched()
    test_gate_table_refused()
    test_unknown_and_invalid_order_refused()
    test_malformed_existing_refused_without_overwrite()
    test_reset_removes_only_the_order_override()
    test_reset_recovers_an_unreadable_file()
    test_float_string_and_mixed_list_round_trip()
    test_nonfinite_and_unsupported_existing_are_refused_with_key()
    test_existing_file_mode_is_preserved()
    print('policy write tests passed')
