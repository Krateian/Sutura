# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Content-addressed disk cache for Sutura engine.

Caches expensive analysis and repair intermediate results:
  - Object analysis summaries and recommendations (object_analysis)
  - Repetition pattern detection results (repeat_repair)
  - SDF / winding grids (F2, compressed, size-capped LRU)
  - Per-method results within a session (so GUI Analyze -> Repair reuses result)

Features:
  - Cache location: $XDG_CACHE_HOME/sutura or ~/.cache/sutura
  - Key: SHA-256 of (input mesh bytes + engine version + settings digest)
  - Invalidation: automatic when engine version changes
  - Size-capped LRU eviction (default 500 MB)
  - Opt-out: --no-cache CLI flag, GUI Options checkbox, SUTURA_NO_CACHE=1
"""
import gzip
import hashlib
import json
import os
import shutil
import time
from typing import Any, Optional, Tuple

import numpy as np

DEFAULT_MAX_CACHE_BYTES = 500 * 1024 * 1024  # 500 MB
_CACHE_ENABLED: bool = True


def is_cache_enabled() -> bool:
    """Return True if caching is enabled."""
    if os.environ.get('SUTURA_NO_CACHE', '').strip() in ('1', 'true', 'yes'):
        return False
    return _CACHE_ENABLED


def set_cache_enabled(enabled: bool) -> None:
    """Enable or disable caching globally in this process."""
    global _CACHE_ENABLED
    _CACHE_ENABLED = bool(enabled)


def get_cache_dir() -> str:
    """Return the absolute path to the sutura cache root directory."""
    xdg = os.environ.get('XDG_CACHE_HOME')
    if xdg and os.path.isabs(xdg):
        base = os.path.join(xdg, 'sutura')
    else:
        base = os.path.expanduser('~/.cache/sutura')
    return base


def _ensure_dir(subdir: str) -> str:
    path = os.path.join(get_cache_dir(), subdir)
    os.makedirs(path, exist_ok=True)
    return path


def hash_file(path: str) -> str:
    """Compute SHA-256 hash of a file on disk."""
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def hash_arrays(verts: np.ndarray, tris: np.ndarray) -> str:
    """Compute SHA-256 hash of vertex and triangle numpy arrays."""
    h = hashlib.sha256()
    v_c = np.ascontiguousarray(verts, dtype=np.float32)
    t_c = np.ascontiguousarray(tris, dtype=np.int32)
    h.update(v_c.tobytes())
    h.update(t_c.tobytes())
    return h.hexdigest()


def hash_settings(settings: Any) -> str:
    """Compute SHA-256 hex digest of settings dictionary or primitive."""
    if settings is None:
        return 'none'
    s = json.dumps(settings, sort_keys=True, default=str)
    return hashlib.sha256(s.encode('utf-8')).hexdigest()[:16]


def get_cached_analysis(mesh_id: str, version: str) -> Optional[dict]:
    """Retrieve cached analysis dictionary if valid and version matches."""
    if not is_cache_enabled():
        return None
    d = os.path.join(get_cache_dir(), 'analysis')
    fpath = os.path.join(d, f'{mesh_id}.json.gz')
    if not os.path.isfile(fpath):
        return None
    try:
        with gzip.open(fpath, 'rt', encoding='utf-8') as f:
            data = json.load(f)
        if data.get('_cache_version') != version:
            try:
                os.remove(fpath)
            except OSError:
                pass
            return None
        os.utime(fpath, None)  # update access time
        return data.get('payload')
    except Exception:
        return None


def put_cached_analysis(mesh_id: str, version: str, payload: dict) -> None:
    """Store analysis dictionary in the cache."""
    if not is_cache_enabled():
        return
    try:
        d = _ensure_dir('analysis')
        fpath = os.path.join(d, f'{mesh_id}.json.gz')
        data = {
            '_cache_version': version,
            '_created_at': time.time(),
            'payload': payload,
        }
        tmp_path = fpath + f'.tmp.{os.getpid()}'
        with gzip.open(tmp_path, 'wt', encoding='utf-8') as f:
            json.dump(data, f)
        os.replace(tmp_path, fpath)
        prune_cache()
    except Exception:
        pass


def get_cached_repeat(mesh_id: str, version: str) -> Optional[dict]:
    """Retrieve cached repetition detection result if valid and version matches."""
    if not is_cache_enabled():
        return None
    d = os.path.join(get_cache_dir(), 'repeat')
    fpath = os.path.join(d, f'{mesh_id}.json.gz')
    if not os.path.isfile(fpath):
        return None
    try:
        with gzip.open(fpath, 'rt', encoding='utf-8') as f:
            data = json.load(f)
        if data.get('_cache_version') != version:
            try:
                os.remove(fpath)
            except OSError:
                pass
            return None
        os.utime(fpath, None)
        return data.get('payload')
    except Exception:
        return None


def put_cached_repeat(mesh_id: str, version: str, payload: dict) -> None:
    """Store repetition detection result in the cache."""
    if not is_cache_enabled():
        return
    try:
        d = _ensure_dir('repeat')
        fpath = os.path.join(d, f'{mesh_id}.json.gz')
        data = {
            '_cache_version': version,
            '_created_at': time.time(),
            'payload': payload,
        }
        tmp_path = fpath + f'.tmp.{os.getpid()}'
        with gzip.open(tmp_path, 'wt', encoding='utf-8') as f:
            json.dump(data, f)
        os.replace(tmp_path, fpath)
        prune_cache()
    except Exception:
        pass


def get_cached_method(mesh_id: str, method_num: int, settings_digest: str,
                      version: str) -> Optional[Tuple[np.ndarray, np.ndarray, dict]]:
    """Retrieve cached repair output (verts, tris, report) for a specific method."""
    if not is_cache_enabled():
        return None
    key = f'{mesh_id}_m{method_num}_{settings_digest}'
    d = os.path.join(get_cache_dir(), 'methods')
    fpath = os.path.join(d, f'{key}.npz')
    if not os.path.isfile(fpath):
        return None
    try:
        with np.load(fpath, allow_pickle=False) as npz:
            cached_version = str(npz['version'])
            if cached_version != version:
                try:
                    os.remove(fpath)
                except OSError:
                    pass
                return None
            verts = npz['verts']
            tris = npz['tris']
            report = json.loads(str(npz['report']))
            os.utime(fpath, None)
            return verts, tris, report
    except Exception:
        return None


def put_cached_method(mesh_id: str, method_num: int, settings_digest: str,
                      version: str, verts: np.ndarray, tris: np.ndarray,
                      report: dict) -> None:
    """Store method repair output in the cache."""
    if not is_cache_enabled():
        return
    try:
        key = f'{mesh_id}_m{method_num}_{settings_digest}'
        d = _ensure_dir('methods')
        fpath = os.path.join(d, f'{key}.npz')
        tmp_path = fpath + f'.tmp.{os.getpid()}.npz'
        np.savez_compressed(
            tmp_path,
            version=version,
            verts=np.ascontiguousarray(verts, dtype=np.float32),
            tris=np.ascontiguousarray(tris, dtype=np.int32),
            report=json.dumps(report, default=str),
        )
        os.replace(tmp_path, fpath)
        prune_cache()
    except Exception:
        pass


def get_cache_size() -> int:
    """Return total bytes occupied by cached files under get_cache_dir()."""
    root = get_cache_dir()
    if not os.path.isdir(root):
        return 0
    total = 0
    for dirpath, _, filenames in os.walk(root):
        for f in filenames:
            fp = os.path.join(dirpath, f)
            try:
                total += os.path.getsize(fp)
            except OSError:
                pass
    return total


def clear_cache() -> int:
    """Delete all files in the cache directory. Return bytes freed."""
    root = get_cache_dir()
    if not os.path.isdir(root):
        return 0
    size = get_cache_size()
    try:
        shutil.rmtree(root)
    except OSError:
        pass
    return size


def prune_cache(max_bytes: int = DEFAULT_MAX_CACHE_BYTES) -> int:
    """Prune cache if total size exceeds max_bytes, keeping most recently used."""
    root = get_cache_dir()
    if not os.path.isdir(root):
        return 0
    entries = []
    total = 0
    for dirpath, _, filenames in os.walk(root):
        for f in filenames:
            fp = os.path.join(dirpath, f)
            try:
                st = os.stat(fp)
                entries.append((st.st_mtime, st.st_size, fp))
                total += st.st_size
            except OSError:
                pass
    if total <= max_bytes:
        return 0
    # Sort oldest first
    entries.sort(key=lambda x: x[0])
    target = int(max_bytes * 0.85)
    freed = 0
    for _, sz, fp in entries:
        try:
            os.remove(fp)
            freed += sz
            total -= sz
            if total <= target:
                break
        except OSError:
            pass
    return freed


def format_bytes(num_bytes: int) -> str:
    """Format bytes as a human-readable string."""
    n = float(num_bytes)
    for unit in ['B', 'KB', 'MB', 'GB']:
        if abs(n) < 1024.0:
            return f"{n:.1f} {unit}" if unit != 'B' else f"{int(n)} B"
        n /= 1024.0
    return f"{n:.1f} TB"


_human_bytes = format_bytes

