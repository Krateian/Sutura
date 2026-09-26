"""Sutura external repair engines module.

Enables registering third-party repair engines installed by the user.
Configuration files are discovered from:
    ${XDG_CONFIG_HOME:-~/.config}/sutura/engines/*.toml

Sutura never downloads engines; user installs and manages binaries.
Watertightness check and Hausdorff distance guard in repair.py are ALWAYS
applied post-execution; engines cannot disable or bypass them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
import time
import tomllib
from typing import Any

BUILTIN_STAGES = ("stage1", "stage2", "deep_repair", "ftetwild")
BUILTIN_STAGES_SET = set(BUILTIN_STAGES)

ALLOWED_FORMATS = ("stl", "obj", "ply")
ALLOWED_PLACEMENTS = (
    "before_stage1",
    "after_stage1",
    "after_stage2",
    "replace_ftetwild",
    "final_fallback",
)
ALLOWED_PLACEHOLDERS = {"input", "output", "tmpdir"}


@dataclass
class EngineConfig:
    name: str
    command: list[str]
    input_format: str = "stl"
    output_format: str = "stl"
    timeout: float = 120.0
    placement: str = "after_stage1"
    enabled: bool = True
    env: dict[str, str] = field(default_factory=dict)
    cwd: str | None = None
    source_path: Path | None = None


@dataclass
class ChainConfig:
    stages: list[str]
    source_path: Path | None = None


@dataclass
class EngineResult:
    success: bool
    engine: str
    returncode: int | None
    duration_sec: float
    stdout_tail: str
    stderr_tail: str
    error: str | None
    output_exists: bool

    def to_dict(self) -> dict[str, Any]:
        return {
            "success": self.success,
            "engine": self.engine,
            "returncode": self.returncode,
            "duration_sec": self.duration_sec,
            "stdout_tail": self.stdout_tail,
            "stderr_tail": self.stderr_tail,
            "error": self.error,
            "output_exists": self.output_exists,
        }


def get_engines_dir() -> Path:
    """Return the base directory for engine configurations, honouring XDG_CONFIG_HOME."""
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg and xdg.strip():
        base = Path(xdg)
    else:
        base = Path.home() / ".config"
    return base / "sutura" / "engines"


def resolve_executable(cmd0: str) -> str | None:
    """Resolve command binary via absolute path, relative path, or shutil.which."""
    if not cmd0 or not isinstance(cmd0, str):
        return None
    # If path contains slashes (relative or absolute)
    if os.path.sep in cmd0 or (os.path.altsep and os.path.altsep in cmd0):
        expanded = os.path.expanduser(cmd0)
        p = Path(expanded)
        if p.is_file() and os.access(p, os.X_OK):
            return str(p.resolve())
        return None
    return shutil.which(cmd0)


def is_engine_available(engine: EngineConfig) -> bool:
    """Check if the engine executable is found on the system."""
    if not engine.command:
        return False
    return resolve_executable(engine.command[0]) is not None


def _tail(text: str | None, max_chars: int = 4000) -> str:
    """Capture the tail of a process output string."""
    if not text:
        return ""
    if len(text) <= max_chars:
        return text
    return text[-max_chars:]


def load_engine_file(path: Path | str) -> tuple[EngineConfig | None, str | None]:
    """Parse and validate an engine TOML configuration file.

    Returns:
        (EngineConfig, None) on success.
        (None, error_message) on failure (caller should log/skip).
    """
    p = Path(path)
    try:
        with open(p, "rb") as f:
            data = tomllib.load(f)
    except Exception as exc:
        return None, f"TOML parse error: {exc}"

    if not isinstance(data, dict):
        return None, "Root element of TOML must be a table/dictionary"

    # Validate name
    name = data.get("name")
    if not isinstance(name, str) or not name.strip():
        return None, "Missing or empty 'name' field"
    name = name.strip()

    # Validate command
    cmd = data.get("command")
    if not isinstance(cmd, list) or not cmd:
        return None, "Field 'command' must be a non-empty list of argument strings"
    for i, arg in enumerate(cmd):
        if not isinstance(arg, str):
            return None, f"Command argument at index {i} must be a string, got {type(arg).__name__}"

    # Validate placeholders in command
    cmd_text = " ".join(cmd)
    found_placeholders = set(re.findall(r"\{([^{}]+)\}", cmd_text))
    unknown_placeholders = found_placeholders - ALLOWED_PLACEHOLDERS
    if unknown_placeholders:
        return None, (
            f"Unknown placeholder(s) in command: {', '.join(sorted(unknown_placeholders))}. "
            f"Allowed placeholders: {', '.join(sorted(ALLOWED_PLACEHOLDERS))}"
        )
    if "input" not in found_placeholders:
        return None, "Command template must contain '{input}' placeholder"
    if "output" not in found_placeholders:
        return None, "Command template must contain '{output}' placeholder"

    # Validate formats
    in_fmt = data.get("input_format", "stl")
    if not isinstance(in_fmt, str) or in_fmt.lower() not in ALLOWED_FORMATS:
        return None, f"Invalid 'input_format': {in_fmt!r}. Allowed: {', '.join(ALLOWED_FORMATS)}"

    out_fmt = data.get("output_format", "stl")
    if not isinstance(out_fmt, str) or out_fmt.lower() not in ALLOWED_FORMATS:
        return None, f"Invalid 'output_format': {out_fmt!r}. Allowed: {', '.join(ALLOWED_FORMATS)}"

    # Validate timeout
    timeout = data.get("timeout", 120.0)
    if not isinstance(timeout, (int, float)) or timeout <= 0:
        return None, f"Invalid 'timeout': {timeout!r}. Must be a positive number of seconds"

    # Validate placement
    placement = data.get("placement", "after_stage1")
    if not isinstance(placement, str) or placement not in ALLOWED_PLACEMENTS:
        return None, f"Invalid 'placement': {placement!r}. Allowed: {', '.join(ALLOWED_PLACEMENTS)}"

    # Validate enabled
    enabled = data.get("enabled", True)
    if not isinstance(enabled, bool):
        return None, f"Field 'enabled' must be boolean, got {type(enabled).__name__}"

    # Validate env
    env_data = data.get("env", {})
    if not isinstance(env_data, dict):
        return None, f"Field 'env' must be a dictionary/table, got {type(env_data).__name__}"
    env: dict[str, str] = {}
    for k, v in env_data.items():
        if not isinstance(k, str) or not isinstance(v, str):
            return None, f"All env keys and values must be strings, got {k!r}={v!r}"
        env[k] = v

    # Validate cwd
    cwd = data.get("cwd")
    if cwd is not None and not isinstance(cwd, str):
        return None, f"Field 'cwd' must be a string or null, got {type(cwd).__name__}"

    cfg = EngineConfig(
        name=name,
        command=cmd,
        input_format=in_fmt.lower(),
        output_format=out_fmt.lower(),
        timeout=float(timeout),
        placement=placement,
        enabled=enabled,
        env=env,
        cwd=cwd,
        source_path=p,
    )
    return cfg, None


def load_all_engines(
    config_dir: Path | str | None = None,
) -> tuple[dict[str, EngineConfig], list[str]]:
    """Discover and load all engine configurations from the config dir.

    Returns:
        (engines_dict, warnings_list)
        Bad files are skipped with a warning without crashing Sutura.
        Duplicate engine names are skipped with a warning.
    """
    if config_dir is None:
        c_dir = get_engines_dir()
    else:
        c_dir = Path(config_dir)

    engines: dict[str, EngineConfig] = {}
    warnings: list[str] = []

    if not c_dir.is_dir():
        return engines, warnings

    for path in sorted(c_dir.glob("*.toml")):
        if path.name == "chain.toml":
            continue
        cfg, err = load_engine_file(path)
        if err:
            warnings.append(f"Failed to load engine from {path}: {err}")
            continue
        if cfg is None:
            continue
        if cfg.name in engines:
            prev_src = engines[cfg.name].source_path
            warnings.append(
                f"Duplicate engine name '{cfg.name}' in {path} "
                f"(already defined in {prev_src}); skipping."
            )
            continue
        engines[cfg.name] = cfg

    return engines, warnings


def load_chain_config(
    chain_file: Path | str,
    available_engines: dict[str, EngineConfig] | None = None,
) -> tuple[ChainConfig | None, list[str]]:
    """Load chain.toml configuration.

    If chain.toml names an unknown stage or engine, it is rejected as a whole
    with a warning, returning (None, [warning]).
    """
    p = Path(chain_file)
    if not p.is_file():
        return None, []

    try:
        with open(p, "rb") as f:
            data = tomllib.load(f)
    except Exception as exc:
        return None, [f"Failed to parse chain config from {p}: {exc}"]

    if not isinstance(data, dict):
        return None, [f"Invalid chain config in {p}: root must be a table"]

    raw_stages = data.get("stages") or data.get("chain")
    if not isinstance(raw_stages, list) or not raw_stages:
        return None, [f"Invalid chain config in {p}: 'stages' must be a non-empty list of stage names"]

    stages: list[str] = []
    for i, s in enumerate(raw_stages):
        if not isinstance(s, str) or not s.strip():
            return None, [f"Invalid stage name at index {i} in {p}: must be a non-empty string"]
        stages.append(s.strip())

    if available_engines is not None:
        known_stages = BUILTIN_STAGES_SET | set(available_engines.keys())
        for s in stages:
            if s not in known_stages:
                return None, [
                    f"chain.toml in {p} names unknown stage or engine '{s}'. "
                    f"Rejected chain; falling back to default chain."
                ]

    return ChainConfig(stages=stages, source_path=p), []


def build_default_chain(engines: dict[str, EngineConfig] | None = None) -> list[str]:
    """Construct the default execution chain by slotting enabled engines into built-in stages."""
    active = {k: v for k, v in (engines or {}).items() if v.enabled}

    def by_placement(p: str) -> list[str]:
        return sorted([name for name, eng in active.items() if eng.placement == p])

    chain: list[str] = []
    chain.extend(by_placement("before_stage1"))
    chain.append("stage1")
    chain.extend(by_placement("after_stage1"))
    chain.append("stage2")
    chain.extend(by_placement("after_stage2"))
    chain.append("deep_repair")

    replace_ftw = by_placement("replace_ftetwild")
    if replace_ftw:
        chain.extend(replace_ftw)
    else:
        chain.append("ftetwild")

    chain.extend(by_placement("final_fallback"))
    return chain


def resolve_chain(
    engines: dict[str, EngineConfig] | None = None,
    chain_file: Path | str | None = None,
    config_dir: Path | str | None = None,
) -> tuple[list[str], list[str]]:
    """Resolve the active execution chain.

    Returns:
        (chain_stages_list, warnings_list)
    """
    if engines is None:
        engines = {}

    warnings: list[str] = []

    if chain_file is None:
        c_dir = Path(config_dir) if config_dir else get_engines_dir()
        chain_path = c_dir / "chain.toml"
    else:
        chain_path = Path(chain_file)

    if chain_path.is_file():
        cfg, chain_warnings = load_chain_config(chain_path, available_engines=engines)
        warnings.extend(chain_warnings)
        if cfg is not None:
            return cfg.stages, warnings

    return build_default_chain(engines), warnings


def run_engine(
    engine: EngineConfig,
    in_path: str | Path,
    out_path: str | Path,
    tmp_dir: str | Path | None = None,
    extra_env: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Execute an external engine with timeout and process-group isolation.

    Runs command via subprocess with start_new_session=True, kills entire
    process group on timeout, never uses shell=True, and returns a result dict.
    Missing or empty output file is treated as a failure result, never an exception.
    """
    start_time = time.monotonic()
    in_str = str(in_path)
    out_str = str(out_path)

    if not engine.command:
        return {
            "success": False,
            "engine": engine.name,
            "returncode": None,
            "duration_sec": 0.0,
            "stdout_tail": "",
            "stderr_tail": "",
            "error": "Engine command template is empty",
            "output_exists": False,
        }

    # Resolve executable
    cmd0 = engine.command[0]
    resolved_bin = resolve_executable(cmd0)
    if resolved_bin is None:
        return {
            "success": False,
            "engine": engine.name,
            "returncode": None,
            "duration_sec": 0.0,
            "stdout_tail": "",
            "stderr_tail": "",
            "error": f"Engine executable '{cmd0}' not found",
            "output_exists": False,
        }

    # Manage tempdir if requested in command placeholders
    temp_dir_mgr = None
    if not tmp_dir and any("{tmpdir}" in arg for arg in engine.command):
        temp_dir_mgr = tempfile.TemporaryDirectory(prefix=f"sutura-engine-{engine.name}-")
        tmp_str = temp_dir_mgr.name
    elif tmp_dir:
        tmp_str = str(tmp_dir)
    else:
        tmp_str = ""

    # Build command with resolved binary and placeholder substitution
    resolved_cmd = [resolved_bin]
    for arg in engine.command[1:]:
        val = arg.replace("{input}", in_str)
        val = val.replace("{output}", out_str)
        val = val.replace("{tmpdir}", tmp_str)
        resolved_cmd.append(val)

    # Prepare environment
    env = dict(os.environ)
    if engine.env:
        env.update(engine.env)
    if extra_env:
        env.update(extra_env)

    # Check cwd
    cwd = engine.cwd
    if cwd:
        cwd = os.path.expanduser(cwd)
        if not os.path.isdir(cwd):
            if temp_dir_mgr:
                temp_dir_mgr.cleanup()
            return {
                "success": False,
                "engine": engine.name,
                "returncode": None,
                "duration_sec": 0.0,
                "stdout_tail": "",
                "stderr_tail": "",
                "error": f"Engine working directory does not exist: {cwd}",
                "output_exists": False,
            }

    try:
        proc = subprocess.Popen(
            resolved_cmd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=cwd,
            env=env,
            start_new_session=True,
            text=True,
        )
    except Exception as exc:
        if temp_dir_mgr:
            temp_dir_mgr.cleanup()
        return {
            "success": False,
            "engine": engine.name,
            "returncode": None,
            "duration_sec": time.monotonic() - start_time,
            "stdout_tail": "",
            "stderr_tail": "",
            "error": f"Failed to spawn process: {exc}",
            "output_exists": False,
        }

    try:
        stdout, stderr = proc.communicate(timeout=engine.timeout)
    except subprocess.TimeoutExpired:
        # Kill the entire process group
        try:
            pgid = os.getpgid(proc.pid)
            os.killpg(pgid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError, AttributeError):
            try:
                proc.kill()
            except Exception:
                pass
        try:
            stdout, stderr = proc.communicate(timeout=2.0)
        except Exception:
            stdout, stderr = "", ""

        if temp_dir_mgr:
            temp_dir_mgr.cleanup()

        duration = time.monotonic() - start_time
        return {
            "success": False,
            "engine": engine.name,
            "returncode": proc.returncode,
            "duration_sec": duration,
            "stdout_tail": _tail(stdout),
            "stderr_tail": _tail(stderr),
            "error": f"Engine '{engine.name}' timed out after {engine.timeout}s",
            "output_exists": False,
        }
    finally:
        if temp_dir_mgr:
            temp_dir_mgr.cleanup()

    duration = time.monotonic() - start_time
    stdout_t = _tail(stdout)
    stderr_t = _tail(stderr)

    out_p = Path(out_path)
    output_exists = out_p.is_file() and out_p.stat().st_size > 0

    if proc.returncode != 0:
        return {
            "success": False,
            "engine": engine.name,
            "returncode": proc.returncode,
            "duration_sec": duration,
            "stdout_tail": stdout_t,
            "stderr_tail": stderr_t,
            "error": f"Engine '{engine.name}' exited with return code {proc.returncode}",
            "output_exists": output_exists,
        }

    if not out_p.exists():
        return {
            "success": False,
            "engine": engine.name,
            "returncode": 0,
            "duration_sec": duration,
            "stdout_tail": stdout_t,
            "stderr_tail": stderr_t,
            "error": f"Engine '{engine.name}' did not create output file: {out_path}",
            "output_exists": False,
        }

    if out_p.stat().st_size == 0:
        return {
            "success": False,
            "engine": engine.name,
            "returncode": 0,
            "duration_sec": duration,
            "stdout_tail": stdout_t,
            "stderr_tail": stderr_t,
            "error": f"Engine '{engine.name}' produced an empty output file: {out_path}",
            "output_exists": False,
        }

    return {
        "success": True,
        "engine": engine.name,
        "returncode": 0,
        "duration_sec": duration,
        "stdout_tail": stdout_t,
        "stderr_tail": stderr_t,
        "error": None,
        "output_exists": True,
    }


def validate_output(
    engine: EngineConfig,
    in_path: str | Path,
    out_path: str | Path,
) -> tuple[bool, str | None]:
    """Validate external engine output sanity.

    Checks that output exists, is a regular file, is non-empty, and satisfies
    minimal format checks.

    NOTE: The watertightness check and Hausdorff distance guard live in repair.py
    and are ALWAYS applied to every engine result. Engines cannot disable or bypass
    them.
    """
    p = Path(out_path)
    if not p.is_file():
        return False, f"Output file does not exist: {out_path}"
    try:
        size = p.stat().st_size
    except OSError as exc:
        return False, f"Failed to stat output file: {exc}"

    if size == 0:
        return False, f"Output file is empty: {out_path}"

    fmt = engine.output_format.lower()
    if fmt == "stl" and size < 84:
        return False, f"Output STL file too small (< 84 bytes minimum header): {size} bytes"

    return True, None
