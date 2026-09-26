#!/usr/bin/env python3
"""Regression test suite for Sutura external engines module (sutura/engines.py).

Tests:
  - XDG_CONFIG_HOME honouring for engines directory
  - Loading valid engine TOML files into EngineConfig
  - Graceful rejection of malformed TOML and missing fields without crashing
  - Placeholder validation ({input}, {output}, {tmpdir}, unknown tokens)
  - Duplicate engine name detection and warning
  - Execution with fake python engine (success)
  - Missing executable handling (clean error, no crash)
  - Timeout handling (kill process group on timeout)
  - Missing or empty output handling (failure result, never an exception)
  - chain.toml loading and rejection of unknown stages/engines with fallback to default chain
  - Placement ordering in default chain
  - validate_output hook sanity checks

Usage:
  /opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python tests/test_engines.py
"""

import os
from pathlib import Path
import shutil
import stat
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, "sutura")
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

import engines  # noqa: E402


def _make_dummy_stl(path: Path | str, num_bytes: int = 100) -> None:
    """Create a dummy binary STL file of at least 84 bytes."""
    header = b"\x00" * 80
    count = (1).to_bytes(4, byteorder="little")
    triangle = b"\x00" * 50
    content = header + count + triangle
    if len(content) < num_bytes:
        content = content + b"\x00" * (num_bytes - len(content))
    with open(path, "wb") as f:
        f.write(content[:num_bytes])


def test_get_engines_dir_honours_xdg():
    with tempfile.TemporaryDirectory(prefix="sutura-test-xdg-") as tmp:
        custom_xdg = os.path.join(tmp, "custom_config")
        old_xdg = os.environ.get("XDG_CONFIG_HOME")
        try:
            os.environ["XDG_CONFIG_HOME"] = custom_xdg
            d = engines.get_engines_dir()
            assert d == Path(custom_xdg) / "sutura" / "engines", f"Got: {d}"

            os.environ["XDG_CONFIG_HOME"] = ""
            d_default = engines.get_engines_dir()
            assert d_default == Path.home() / ".config" / "sutura" / "engines", f"Got: {d_default}"
        finally:
            if old_xdg is not None:
                os.environ["XDG_CONFIG_HOME"] = old_xdg
            else:
                os.environ.pop("XDG_CONFIG_HOME", None)


def test_load_valid_engine_toml():
    with tempfile.TemporaryDirectory(prefix="sutura-test-load-") as tmp:
        p = Path(tmp) / "meshfix.toml"
        p.write_text(
            """
name = "meshfix"
command = ["meshfix", "{input}", "{output}", "-a", "{tmpdir}"]
input_format = "stl"
output_format = "obj"
timeout = 45.5
placement = "before_stage1"
enabled = true
cwd = "/tmp"

[env]
OMP_NUM_THREADS = "4"
MESHFIX_VERBOSE = "1"
""",
            encoding="utf-8",
        )
        cfg, err = engines.load_engine_file(p)
        assert err is None, f"Expected no error, got: {err}"
        assert cfg is not None
        assert cfg.name == "meshfix"
        assert cfg.command == ["meshfix", "{input}", "{output}", "-a", "{tmpdir}"]
        assert cfg.input_format == "stl"
        assert cfg.output_format == "obj"
        assert cfg.timeout == 45.5
        assert cfg.placement == "before_stage1"
        assert cfg.enabled is True
        assert cfg.cwd == "/tmp"
        assert cfg.env == {"OMP_NUM_THREADS": "4", "MESHFIX_VERBOSE": "1"}
        assert cfg.source_path == p


def test_load_bad_toml_and_missing_fields():
    with tempfile.TemporaryDirectory(prefix="sutura-test-bad-") as tmp:
        # 1. Corrupt syntax
        p_bad = Path(tmp) / "corrupt.toml"
        p_bad.write_text("name = [unclosed bracket", encoding="utf-8")
        cfg, err = engines.load_engine_file(p_bad)
        assert cfg is None
        assert "TOML parse error" in err

        # 2. Missing name
        p_noname = Path(tmp) / "noname.toml"
        p_noname.write_text('command = ["tool", "{input}", "{output}"]', encoding="utf-8")
        cfg, err = engines.load_engine_file(p_noname)
        assert cfg is None
        assert "Missing or empty 'name'" in err

        # 3. Missing command
        p_nocmd = Path(tmp) / "nocmd.toml"
        p_nocmd.write_text('name = "nocmd"', encoding="utf-8")
        cfg, err = engines.load_engine_file(p_nocmd)
        assert cfg is None
        assert "Field 'command'" in err

        # 4. Invalid placement
        p_badplace = Path(tmp) / "badplace.toml"
        p_badplace.write_text(
            'name = "test"\ncommand = ["t", "{input}", "{output}"]\nplacement = "invalid_placement"',
            encoding="utf-8",
        )
        cfg, err = engines.load_engine_file(p_badplace)
        assert cfg is None
        assert "Invalid 'placement'" in err

        # 5. Negative timeout
        p_negtimeout = Path(tmp) / "negtimeout.toml"
        p_negtimeout.write_text(
            'name = "test"\ncommand = ["t", "{input}", "{output}"]\ntimeout = -5',
            encoding="utf-8",
        )
        cfg, err = engines.load_engine_file(p_negtimeout)
        assert cfg is None
        assert "Invalid 'timeout'" in err


def test_placeholder_validation():
    with tempfile.TemporaryDirectory(prefix="sutura-test-placeholders-") as tmp:
        # Missing {input}
        p1 = Path(tmp) / "no_in.toml"
        p1.write_text('name = "p1"\ncommand = ["tool", "{output}"]', encoding="utf-8")
        cfg, err = engines.load_engine_file(p1)
        assert cfg is None
        assert "'{input}' placeholder" in err

        # Missing {output}
        p2 = Path(tmp) / "no_out.toml"
        p2.write_text('name = "p2"\ncommand = ["tool", "{input}"]', encoding="utf-8")
        cfg, err = engines.load_engine_file(p2)
        assert cfg is None
        assert "'{output}' placeholder" in err

        # Unknown placeholder
        p3 = Path(tmp) / "unknown.toml"
        p3.write_text(
            'name = "p3"\ncommand = ["tool", "{input}", "{output}", "{unknown_var}"]',
            encoding="utf-8",
        )
        cfg, err = engines.load_engine_file(p3)
        assert cfg is None
        assert "Unknown placeholder(s)" in err
        assert "unknown_var" in err

        # Allowed embedded placeholders e.g. --in={input}
        p4 = Path(tmp) / "embedded.toml"
        p4.write_text(
            'name = "p4"\ncommand = ["tool", "--in={input}", "--out={output}"]',
            encoding="utf-8",
        )
        cfg, err = engines.load_engine_file(p4)
        assert err is None
        assert cfg is not None


def test_duplicate_engine_name_skipped():
    with tempfile.TemporaryDirectory(prefix="sutura-test-dup-") as tmp:
        d = Path(tmp)
        (d / "engine_a.toml").write_text(
            'name = "shared_name"\ncommand = ["a", "{input}", "{output}"]',
            encoding="utf-8",
        )
        (d / "engine_b.toml").write_text(
            'name = "shared_name"\ncommand = ["b", "{input}", "{output}"]',
            encoding="utf-8",
        )

        loaded, warnings = engines.load_all_engines(config_dir=d)
        assert len(loaded) == 1
        assert "shared_name" in loaded
        assert len(warnings) == 1
        assert "Duplicate engine name 'shared_name'" in warnings[0]


def test_run_engine_success():
    with tempfile.TemporaryDirectory(prefix="sutura-test-run-") as tmp:
        # Create a mock engine script that copies input to output and writes to stdout/stderr
        engine_script = Path(tmp) / "fake_engine.py"
        engine_script.write_text(
            """#!/usr/bin/env python3
import sys, shutil
in_p, out_p = sys.argv[1], sys.argv[2]
sys.stdout.write("FakeEngine: processing mesh\\n")
sys.stderr.write("FakeEngine: non-fatal warning\\n")
shutil.copyfile(in_p, out_p)
sys.exit(0)
""",
            encoding="utf-8",
        )
        engine_script.chmod(engine_script.stat().st_mode | stat.S_IEXEC)

        in_stl = Path(tmp) / "input.stl"
        _make_dummy_stl(in_stl, 100)
        out_stl = Path(tmp) / "output.stl"

        cfg = engines.EngineConfig(
            name="fake_engine",
            command=[sys.executable, str(engine_script), "{input}", "{output}"],
            timeout=10.0,
        )

        res = engines.run_engine(cfg, in_stl, out_stl)
        assert res["success"] is True
        assert res["returncode"] == 0
        assert res["output_exists"] is True
        assert res["error"] is None
        assert "FakeEngine: processing mesh" in res["stdout_tail"]
        assert "FakeEngine: non-fatal warning" in res["stderr_tail"]
        assert out_stl.is_file() and out_stl.stat().st_size == 100


def test_run_engine_missing_binary():
    with tempfile.TemporaryDirectory(prefix="sutura-test-nobin-") as tmp:
        in_stl = Path(tmp) / "input.stl"
        _make_dummy_stl(in_stl, 100)
        out_stl = Path(tmp) / "output.stl"

        cfg = engines.EngineConfig(
            name="missing_tool",
            command=["/nonexistent/path/to/binary_12345", "{input}", "{output}"],
            timeout=5.0,
        )

        res = engines.run_engine(cfg, in_stl, out_stl)
        assert res["success"] is False
        assert res["returncode"] is None
        assert res["output_exists"] is False
        assert "not found" in res["error"]


def test_run_engine_timeout():
    with tempfile.TemporaryDirectory(prefix="sutura-test-timeout-") as tmp:
        # Script sleeps longer than engine timeout
        engine_script = Path(tmp) / "sleep_engine.py"
        engine_script.write_text(
            """#!/usr/bin/env python3
import time, sys
time.sleep(5)
sys.exit(0)
""",
            encoding="utf-8",
        )
        in_stl = Path(tmp) / "input.stl"
        _make_dummy_stl(in_stl, 100)
        out_stl = Path(tmp) / "output.stl"

        cfg = engines.EngineConfig(
            name="slow_engine",
            command=[sys.executable, str(engine_script), "{input}", "{output}"],
            timeout=0.4,
        )

        res = engines.run_engine(cfg, in_stl, out_stl)
        assert res["success"] is False
        assert res["output_exists"] is False
        assert "timed out after 0.4s" in res["error"]


def test_run_engine_missing_or_empty_output():
    with tempfile.TemporaryDirectory(prefix="sutura-test-empty-") as tmp:
        # Script exits 0 but does not create output
        script_no_out = Path(tmp) / "no_output.py"
        script_no_out.write_text(
            """#!/usr/bin/env python3
import sys
sys.exit(0)
""",
            encoding="utf-8",
        )
        in_stl = Path(tmp) / "input.stl"
        _make_dummy_stl(in_stl, 100)
        out_stl = Path(tmp) / "output.stl"

        cfg1 = engines.EngineConfig(
            name="no_output_engine",
            command=[sys.executable, str(script_no_out), "{input}", "{output}"],
            timeout=5.0,
        )
        res1 = engines.run_engine(cfg1, in_stl, out_stl)
        assert res1["success"] is False
        assert res1["output_exists"] is False
        assert "did not create output file" in res1["error"]

        # Script exits 0 and creates 0-byte output
        script_empty_out = Path(tmp) / "empty_output.py"
        script_empty_out.write_text(
            """#!/usr/bin/env python3
import sys
with open(sys.argv[2], "wb") as f:
    pass
sys.exit(0)
""",
            encoding="utf-8",
        )
        cfg2 = engines.EngineConfig(
            name="empty_output_engine",
            command=[sys.executable, str(script_empty_out), "{input}", "{output}"],
            timeout=5.0,
        )
        res2 = engines.run_engine(cfg2, in_stl, out_stl)
        assert res2["success"] is False
        assert res2["output_exists"] is False
        assert "empty output file" in res2["error"]


def test_chain_toml_valid():
    with tempfile.TemporaryDirectory(prefix="sutura-test-chain-") as tmp:
        d = Path(tmp)
        # Create an engine to be recognized in chain
        my_engine = engines.EngineConfig(
            name="custom_fixer",
            command=["fixer", "{input}", "{output}"],
        )
        available = {"custom_fixer": my_engine}

        chain_file = d / "chain.toml"
        chain_file.write_text(
            """
stages = [
    "stage1",
    "custom_fixer",
    "stage2",
    "ftetwild"
]
""",
            encoding="utf-8",
        )

        cfg, warnings = engines.load_chain_config(chain_file, available_engines=available)
        assert len(warnings) == 0
        assert cfg is not None
        assert cfg.stages == ["stage1", "custom_fixer", "stage2", "ftetwild"]

        chain, resolve_warnings = engines.resolve_chain(engines=available, chain_file=chain_file)
        assert len(resolve_warnings) == 0
        assert chain == ["stage1", "custom_fixer", "stage2", "ftetwild"]


def test_chain_toml_unknown_stage_rejected():
    with tempfile.TemporaryDirectory(prefix="sutura-test-chain-rej-") as tmp:
        d = Path(tmp)
        chain_file = d / "chain.toml"
        chain_file.write_text(
            """
stages = [
    "stage1",
    "nonexistent_stage_or_engine",
    "stage2"
]
""",
            encoding="utf-8",
        )

        cfg, warnings = engines.load_chain_config(chain_file, available_engines={})
        assert cfg is None
        assert len(warnings) == 1
        assert "names unknown stage or engine 'nonexistent_stage_or_engine'" in warnings[0]
        assert "falling back to default chain" in warnings[0]

        # resolve_chain must fall back to default chain with warning
        chain, resolve_warnings = engines.resolve_chain(engines={}, chain_file=chain_file)
        assert len(resolve_warnings) == 1
        assert chain == ["stage1", "stage2", "deep_repair", "ftetwild"]


def test_build_default_chain_placement():
    eng_before = engines.EngineConfig(name="eng_pre", command=["p", "{input}", "{output}"], placement="before_stage1")
    eng_after1 = engines.EngineConfig(name="eng_mid", command=["p", "{input}", "{output}"], placement="after_stage1")
    eng_after2 = engines.EngineConfig(name="eng_post2", command=["p", "{input}", "{output}"], placement="after_stage2")
    eng_replace_ftw = engines.EngineConfig(name="eng_tet", command=["p", "{input}", "{output}"], placement="replace_ftetwild")
    eng_final = engines.EngineConfig(name="eng_last", command=["p", "{input}", "{output}"], placement="final_fallback")
    eng_disabled = engines.EngineConfig(name="eng_dis", command=["p", "{input}", "{output}"], placement="before_stage1", enabled=False)

    engs = {
        "eng_pre": eng_before,
        "eng_mid": eng_after1,
        "eng_post2": eng_after2,
        "eng_tet": eng_replace_ftw,
        "eng_last": eng_final,
        "eng_dis": eng_disabled,
    }

    chain = engines.build_default_chain(engs)
    expected = [
        "eng_pre",
        "stage1",
        "eng_mid",
        "stage2",
        "eng_post2",
        "deep_repair",
        "eng_tet",
        "eng_last",
    ]
    assert chain == expected, f"Expected {expected}, got {chain}"


def test_validate_output_hook():
    with tempfile.TemporaryDirectory(prefix="sutura-test-val-") as tmp:
        cfg = engines.EngineConfig(name="dummy", command=["d", "{input}", "{output}"], output_format="stl")
        in_p = Path(tmp) / "in.stl"
        _make_dummy_stl(in_p, 100)

        # Missing file
        ok, msg = engines.validate_output(cfg, in_p, Path(tmp) / "no_such_file.stl")
        assert not ok
        assert "does not exist" in msg

        # Empty file
        empty_p = Path(tmp) / "empty.stl"
        empty_p.touch()
        ok, msg = engines.validate_output(cfg, in_p, empty_p)
        assert not ok
        assert "empty" in msg

        # STL too small (< 84 bytes)
        small_p = Path(tmp) / "small.stl"
        _make_dummy_stl(small_p, 50)
        ok, msg = engines.validate_output(cfg, in_p, small_p)
        assert not ok
        assert "too small" in msg

        # Valid STL
        valid_p = Path(tmp) / "valid.stl"
        _make_dummy_stl(valid_p, 120)
        ok, msg = engines.validate_output(cfg, in_p, valid_p)
        assert ok
        assert msg is None


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok  %s" % name)
    print("All engine tests passed")
