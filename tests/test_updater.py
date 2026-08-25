#!/usr/bin/env python3
"""Regression test for sutura/updater.py license-boundary logic.

Checks that crosses_license_boundary() stops auto-update only when an older
install would jump across the license boundary (LICENSE_BOUNDARY_VERSION):
       - 0.1.9 -> 0.2.0        = True  (the boundary crossing)
       - 0.1.9 -> 0.1.10       = False (v0.1.x patch, still auto-updatable)
       - 0.1.9 -> 0.2.0-beta.1 = True  (a v0.2 prerelease already carries
                                        the new license)
       - 0.2.0 -> 0.2.1        = False (already past the boundary)
  3. check_for_update()'s return contract: (status, tag, cfg) with
     statuses 'update' / 'license' / 'none' (AppImage short-circuits to
     'none' before any network call).
Usage: python3 tests/test_updater.py
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)


def test_crosses_boundary_stops_at_020():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.1.9', '0.2.0') is True


def test_crosses_boundary_allows_010_patch():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.1.9', '0.1.10') is False


def test_crosses_boundary_includes_v02_prerelease():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.1.9', 'v0.2.0-beta.1') is True


def test_crosses_boundary_false_past_boundary():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('0.2.0', '0.2.1') is False
    assert crosses_license_boundary('0.3.0', '0.3.1') is False


def test_crosses_boundary_malformed_is_false():
    from updater import crosses_license_boundary
    assert crosses_license_boundary('garbage', '0.2.0') is False
    assert crosses_license_boundary('0.1.9', 'not-a-version') is False


def test_check_for_update_return_contract():
    """check_for_update returns (status, tag, cfg) with the right status and
    tag per scenario; network and config are mocked so the test never touches
    the real GitHub API or the user's ~/.config/sutura."""
    import updater

    if os.environ.get('APPIMAGE'):
        status, tag, cfg = updater.check_for_update()
        assert status == 'none', status
        assert tag is None
        return

    fake_cfg = {
        'check_for_updates': True, 'last_check': None, 'last_known_version': None,
    }
    real_version = updater.VERSION
    orig = (updater.load_config, updater.save_config,
            updater.should_check, updater.fetch_latest_release)

    updater.load_config = lambda: dict(fake_cfg)
    updater.save_config = lambda cfg: None
    updater.should_check = lambda cfg: True

    def run_scenario(version, latest, expect_status, expect_tag):
        updater.VERSION = version
        updater.fetch_latest_release = lambda: latest
        status, tag, _cfg = updater.check_for_update(force=True)
        assert status == expect_status, (version, latest, status)
        assert tag == expect_tag, (version, latest, tag)

    try:
        run_scenario('0.1.9', '0.1.10', 'update', '0.1.10')
        run_scenario('0.1.9', '0.2.0', 'license', '0.2.0')
        run_scenario('0.1.9', 'v0.2.0-beta.1', 'license', 'v0.2.0-beta.1')
        run_scenario('0.1.9', '0.1.9', 'none', None)
        run_scenario('0.2.0', '0.2.1', 'update', '0.2.1')
    finally:
        updater.VERSION = real_version
        (updater.load_config, updater.save_config,
         updater.should_check, updater.fetch_latest_release) = orig


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith('test_') and callable(fn):
            fn()
            print('ok  %s' % name)
    print('updater tests passed')


if __name__ == '__main__':
    main()