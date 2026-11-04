# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""System diagnostics collector and sanitization for Sutura.

Generates a zip archive containing:
  - system_info.json: platform, python, environment, and sutura_geom symbol audit
  - methods_availability.json: status and availability reason for every
    registered repair method
  - config_sanitized.json: configuration with home paths and usernames scrubbed
  - last_repair_history.json: latest repair metrics record from history.jsonl
  - crash_log.txt: scrubbed tail of crash.log (present only when it exists)
  - README.txt: description of diagnostic contents

Also owns the shared crash log: both the CLI and the GUI enable ``faulthandler``
and uncaught-exception hooks into a single bounded ``crash.log`` in the runtime
user-data directory (see ``crash_log_path``), so a hard crash leaves a trace
instead of dying silently.
"""
import getpass
import json
import os
import platform
import re
import sys
import zipfile
from typing import Any, Dict, List, Optional

CRASH_LOG_NAME = 'crash.log'
# Bound the crash log so it can never grow without limit; trim keeps the tail
# (the most recent crash is what matters).
CRASH_LOG_MAX_BYTES = 200 * 1024

# Keeps the faulthandler file object alive: faulthandler requires the file it
# writes to stay open, so it must outlive the call that enables it.
_CRASH_LOG_FH = None


def crash_log_path() -> str:
    """Absolute path of the crash log in the runtime user-data directory.

    Uses the shared user-data helper (``sutura_engine.paths``) so the log sits
    beside history.jsonl; falls back to the same default location when that
    module cannot be imported.
    """
    try:
        from sutura_engine import paths
        return paths.data_file(CRASH_LOG_NAME)
    except Exception:
        base = os.environ.get(
            'SUTURA_DATA_DIR', os.path.expanduser('~/.local/share/sutura/data'))
        return os.path.join(base, CRASH_LOG_NAME)


def _trim_crash_log(path: str, max_bytes: int = CRASH_LOG_MAX_BYTES) -> None:
    """Keep only the tail of the crash log; never raises.

    When the file exceeds ``max_bytes`` it is replaced by a short marker plus
    its last ``max_bytes``; the newest crash (last written) is preserved.
    """
    try:
        if not os.path.isfile(path):
            return
        size = os.path.getsize(path)
        if size <= max_bytes:
            return
        with open(path, 'rb') as f:
            f.seek(size - max_bytes)
            tail = f.read()
        # Start at a clean line/UTF-8 boundary: a raw byte cut can split a
        # multi-byte character and leave the file unreadable as UTF-8.
        nl = tail.find(b'\n')
        if 0 <= nl < 1024:
            tail = tail[nl + 1:]
        else:
            tail = tail.decode('utf-8', errors='replace').encode('utf-8')
        with open(path, 'wb') as f:
            f.write(b'... crash log trimmed to the last %d bytes ...\n'
                    % max_bytes)
            f.write(tail)
    except Exception:
        pass


def enable_crash_logging():
    """Enable ``faulthandler`` writing to a bounded ``crash.log``.

    Returns the log path, or None when faulthandler is unavailable. Never
    raises: a crash reporter must not break startup. The file object is kept
    open in a module global because faulthandler requires it.
    """
    global _CRASH_LOG_FH
    try:
        import faulthandler
    except Exception:
        return None
    path = crash_log_path()
    try:
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    except Exception:
        pass
    _trim_crash_log(path)
    try:
        # No startup banner: crash.log must stay empty on a normal run and only
        # gain content when faulthandler actually catches a fatal signal (or a
        # hook logs an uncaught exception).
        fh = open(path, 'ab')
        faulthandler.enable(file=fh, all_threads=True)
    except Exception:
        return None
    _CRASH_LOG_FH = fh
    return path


def crash_log_tail(max_bytes: int = CRASH_LOG_MAX_BYTES) -> Optional[str]:
    """Scrubbed tail of the crash log for the diagnostics archive, or None.

    Reuses ``sanitize_value`` so the same home/user scrubbing applied to the
    rest of the archive also covers tracebacks (which may embed input paths).
    """
    path = crash_log_path()
    if not os.path.isfile(path):
        return None
    try:
        with open(path, 'rb') as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - max_bytes))
            data = f.read()
        text = data.decode('utf-8', errors='replace')
    except Exception:
        return None
    if not text.strip():
        return None
    home, user = _get_home_and_user()
    return sanitize_value(text, home, user)


def _get_home_and_user():
    home = os.path.expanduser('~')
    try:
        user = getpass.getuser()
    except Exception:
        user = os.environ.get('USER') or os.environ.get('USERNAME') or ''
    return home, user


def sanitize_value(val: Any, home: str, user: str) -> Any:
    """Recursively scrub personal paths and usernames from diagnostic data.

    Matching is case-insensitive (macOS APFS/HFS+ is case-preserving, so a home
    path can differ in case from ``getpass.getuser()``), and external drive /
    mount roots (``/Volumes/<name>``, ``/media/<user>``, ``/run/media/<user>``)
    keep their directory name out of the archive too.
    """
    if isinstance(val, str):
        res = val
        if home:
            res = re.sub(re.escape(home), '~', res, flags=re.IGNORECASE)
        if user:
            res = re.sub(re.escape(user), '<user>', res, flags=re.IGNORECASE)
        res = re.sub(r'(/run/media/)[^/\s]+', r'\1<user>', res,
                     flags=re.IGNORECASE)
        res = re.sub(r'(/media/)[^/\s]+', r'\1<user>', res, flags=re.IGNORECASE)
        res = re.sub(r'(/Volumes/)[^/\s]+', r'\1<name>', res,
                     flags=re.IGNORECASE)
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
        from sutura_engine import health
        from sutura_engine.methods import all_methods
        method_map = {m.id: m for m in all_methods()}
        details = health.check_availability_detail()
        for mid, (status, reason) in details.items():
            m = method_map.get(mid)
            results.append({
                'num': getattr(m, 'num', None),
                'id': mid,
                'name': getattr(m, 'name', mid),
                'status': status.value,
                'available': (status == health.Status.OK),
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
    try:
        from sutura_engine import paths
        history_path = paths.data_file('history.jsonl')
    except Exception:
        history_dir = os.environ.get('SUTURA_DATA_DIR', os.path.expanduser('~/.local/share/sutura/data'))
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
    crash_tail = crash_log_tail()
    # Method count comes from the registry-backed availability list, never a
    # hard-coded literal (methods are added over time).
    method_count = sum(1 for m in methods_avail if 'num' in m)

    _files = [
        "  - system_info.json: OS, Python runtime, and sutura_geom extension symbols\n",
        "  - methods_availability.json: Availability audit across all %d methods\n"
        % method_count,
        "  - config_sanitized.json: Cleaned user settings\n",
        "  - last_repair_history.json: Technical metrics from the last repair run\n",
    ]
    if crash_tail is not None:
        _files.append(
            "  - crash_log.txt: Scrubbed tail of the crash log (crash traces)\n")
    readme_text = (
        "Sutura Diagnostics Archive\n"
        "==========================\n\n"
        "This archive contains diagnostic metadata for troubleshooting Sutura.\n"
        "All personal paths and usernames have been scrubbed.\n\n"
        "Files included:\n"
    ) + ''.join(_files)

    out_dir = os.path.dirname(os.path.abspath(out_path))
    if out_dir and not os.path.exists(out_dir):
        os.makedirs(out_dir, exist_ok=True)

    with zipfile.ZipFile(out_path, 'w', compression=zipfile.ZIP_DEFLATED) as zf:
        zf.writestr('README.txt', readme_text)
        zf.writestr('system_info.json', json.dumps(sys_info, indent=2))
        zf.writestr('methods_availability.json', json.dumps(methods_avail, indent=2))
        zf.writestr('config_sanitized.json', json.dumps(config_clean, indent=2))
        zf.writestr('last_repair_history.json', json.dumps(last_history, indent=2))
        if crash_tail is not None:
            zf.writestr('crash_log.txt', crash_tail)

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
