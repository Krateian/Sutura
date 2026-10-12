# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""System diagnostics collector and sanitization for Sutura.

Generates a zip archive containing:
  - system_info.json: platform, python, environment, and sutura_geom symbol audit
  - methods_availability.json: status and availability reason for all 17 methods
  - config_sanitized.json: configuration with home paths and usernames scrubbed
  - last_repair_history.json: latest repair metrics record from history.jsonl
  - README.txt: description of diagnostic contents
"""
import getpass
import json
import os
import platform
import sys
import zipfile
from typing import Any, Dict, List, Optional


def _get_home_and_user():
    home = os.path.expanduser('~')
    try:
        user = getpass.getuser()
    except Exception:
        user = os.environ.get('USER') or os.environ.get('USERNAME') or ''
    return home, user


def sanitize_value(val: Any, home: str, user: str) -> Any:
    """Recursively scrub personal paths and usernames from diagnostic data."""
    if isinstance(val, str):
        res = val
        if home and home in res:
            res = res.replace(home, '~')
        if user and user in res:
            res = res.replace(user, '<user>')
        return res
    if isinstance(val, dict):
        return {sanitize_value(k, home, user): sanitize_value(v, home, user) for k, v in val.items()}
    if isinstance(val, list):
        return [sanitize_value(item, home, user) for item in val]
    return val


def collect_system_info() -> Dict[str, Any]:
    home, user = _get_home_and_user()
    
    # sutura version
    try:
        from sutura.repair import VERSION
    except ImportError:
        try:
            from repair import VERSION
        except ImportError:
            VERSION = 'unknown'

    # environment type
    env_type = 'system'
    if os.environ.get('CONDA_PREFIX'):
        env_type = 'conda'
    elif os.environ.get('VIRTUAL_ENV'):
        env_type = 'venv'

    # sutura_geom extension audit
    geom_info: Dict[str, Any] = {
        'installed': False,
        'path': None,
        'version': None,
        'symbols': [],
        'error': None,
    }
    try:
        import sutura_geom
        geom_info['installed'] = True
        geom_file = getattr(sutura_geom, '__file__', None)
        if geom_file:
            geom_info['path'] = sanitize_value(geom_file, home, user)
        geom_info['version'] = getattr(sutura_geom, '__version__', None)
        geom_info['symbols'] = sorted([
            attr for attr in dir(sutura_geom) if not attr.startswith('_')
        ])
    except Exception as e:
        geom_info['error'] = str(e)

    info = {
        'sutura_version': VERSION,
        'platform': platform.platform(),
        'system': platform.system(),
        'release': platform.release(),
        'machine': platform.machine(),
        'python_version': platform.python_version(),
        'python_executable': sanitize_value(sys.executable, home, user),
        'environment_type': env_type,
        'sutura_geom': geom_info,
    }
    return info


def collect_methods_availability() -> List[Dict[str, Any]]:
    results = []
    try:
        from sutura_engine.methods import all_methods
        for m in all_methods():
            ok, reason = m.available()
            results.append({
                'num': m.num,
                'id': m.id,
                'name': m.name,
                'display_name': getattr(m, 'display_name', m.name),
                'family': getattr(m, 'family', 'unknown'),
                'available': bool(ok),
                'reason': reason,
            })
    except Exception as e:
        results.append({'error': 'failed to query method availability: %s' % e})
    return results


def collect_sanitized_config() -> Dict[str, Any]:
    home, user = _get_home_and_user()
    config_dir = os.path.expanduser('~/.config/sutura')
    config_path = os.path.join(config_dir, 'config.json')
    if not os.path.isfile(config_path):
        return {'status': 'not_found'}
    try:
        with open(config_path, 'r', encoding='utf-8') as f:
            raw = json.load(f)
        return sanitize_value(raw, home, user)
    except Exception as e:
        return {'status': 'error', 'error': str(e)}


def collect_last_repair_history() -> Dict[str, Any]:
    history_dir = os.environ.get('SUTURA_DIR', os.path.expanduser('~/.local/share/sutura'))
    history_path = os.path.join(history_dir, 'history.jsonl')
    if not os.path.isfile(history_path):
        return {'status': 'not_found'}
    try:
        last_line = None
        with open(history_path, 'r', encoding='utf-8', errors='replace') as f:
            for line in f:
                line = line.strip()
                if line:
                    last_line = line
        if not last_line:
            return {'status': 'empty'}
        return json.loads(last_line)
    except Exception as e:
        return {'status': 'error', 'error': str(e)}


def create_diagnostics_zip(out_path: str) -> Dict[str, Any]:
    """Gather diagnostic info and package into a ZIP file."""
    sys_info = collect_system_info()
    methods_avail = collect_methods_availability()
    config_clean = collect_sanitized_config()
    last_history = collect_last_repair_history()

    readme_text = (
        "Sutura Diagnostics Archive\n"
        "==========================\n\n"
        "This archive contains diagnostic metadata for troubleshooting Sutura.\n"
        "All personal paths and usernames have been scrubbed.\n\n"
        "Files included:\n"
        "  - system_info.json: OS, Python runtime, and sutura_geom extension symbols\n"
        "  - methods_availability.json: Availability audit across methods 1-17\n"
        "  - config_sanitized.json: Cleaned user settings\n"
        "  - last_repair_history.json: Technical metrics from the last repair run\n"
    )

    out_dir = os.path.dirname(os.path.abspath(out_path))
    if out_dir and not os.path.exists(out_dir):
        os.makedirs(out_dir, exist_ok=True)

    with zipfile.ZipFile(out_path, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
        zf.writestr('README.txt', readme_text)
        zf.writestr('system_info.json', json.dumps(sys_info, indent=2))
        zf.writestr('methods_availability.json', json.dumps(methods_avail, indent=2))
        zf.writestr('config_sanitized.json', json.dumps(config_clean, indent=2))
        zf.writestr('last_repair_history.json', json.dumps(last_history, indent=2))

    size_bytes = os.path.getsize(out_path)
    return {
        'ok': True,
        'path': out_path,
        'size_bytes': size_bytes,
        'version': sys_info.get('sutura_version'),
        'symbols_count': len(sys_info.get('sutura_geom', {}).get('symbols', [])),
    }


def run_diagnostics_cli(out_path: str) -> int:
    """CLI runner for sutura --diagnostics out.zip."""
    try:
        res = create_diagnostics_zip(out_path)
        print("Diagnostics written to %s (%.1f KB)" % (out_path, res['size_bytes'] / 1024.0))
        return 0
    except Exception as e:
        print("error: failed to write diagnostics to %s: %s" % (out_path, e), file=sys.stderr)
        return 1


if __name__ == '__main__':
    target = sys.argv[1] if len(sys.argv) > 1 else 'sutura-diagnostics.zip'
    sys.exit(run_diagnostics_cli(target))
