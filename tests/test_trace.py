# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Tests for pipeline debug trace and stage diff tool."""
import json
import os
import subprocess
import sys
import tempfile
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
for _p in (os.path.join(REPO, 'sutura'), REPO):
    if _p not in sys.path:
        sys.path.insert(0, _p)

from sutura_engine import trace  # noqa: E402
from scripts.trace_diff import diff_traces, _load_trace  # noqa: E402


def test_trace_inactive_zero_overhead():
    """Verify that when tracing is inactive, record_stage performs zero extra work."""
    tracer = trace.PipelineTracer(trace_path=None)
    assert not tracer.is_active

    # Benchmark 100,000 record_stage calls
    n_iters = 100000
    t0 = time.perf_counter()
    for _ in range(n_iters):
        if trace.is_active():
            trace.record_stage("stage1_chain", "dummy_method", None, None, None, None)
    elapsed = time.perf_counter() - t0

    per_call_ns = (elapsed / n_iters) * 1e9
    total_trace_overhead_per_repair_ms = 15 * (per_call_ns / 1e6)
    # Even on a fast 50 ms repair, 15 inactive checks must be < 1% overhead
    pct_overhead_on_fast_repair = (total_trace_overhead_per_repair_ms / 50.0) * 100.0

    assert elapsed < 0.05, f"Inactive tracer overhead too high: {elapsed * 1000:.2f}ms for 100k calls"
    assert pct_overhead_on_fast_repair < 1.0, f"Overhead {pct_overhead_on_fast_repair:.4f}% exceeds 1% limit"


def test_trace_schema_and_content(tmp_path):
    """Verify trace JSON structure, stage sequence, and measure fields."""
    sample = "tests/real-world-samples/thingi10k_224108.stl"
    trace_path = str(tmp_path / "trace_224108.json")
    out_stl = str(tmp_path / "out_224108.stl")

    cmd = [
        sys.executable,
        "sutura/repair.py",
        sample,
        "-o",
        out_stl,
        "--trace",
        trace_path,
    ]
    env = os.environ.copy()
    env["PYTHONPATH"] = "sutura:sutura/sutura_engine:" + env.get("PYTHONPATH", "")
    res = subprocess.run(cmd, env=env, capture_output=True, text=True)
    assert res.returncode == 0, f"Repair failed: {res.stderr}"

    assert os.path.exists(trace_path), "Trace file was not created"
    with open(trace_path, "r") as f:
        data = json.load(f)

    assert data.get("version") == 1
    assert "total_elapsed_seconds" in data
    assert "stages" in data
    assert "final_summary" in data

    stages = data["stages"]
    assert len(stages) >= 10

    expected_stages = [
        "load",
        "tamponade_prepass",
        "flap_prepass",
        "stage1_chain",
        "stage1_delete_fallback",
        "si_excise",
        "closing_ladder",
        "tamponade_post",
        "graft",
        "dressing",
        "splint",
        "coaptation",
        "ftetwild",
        "pinch_split",
        "stage2",
        "p_weld",
        "validate",
    ]
    recorded_stage_names = [s["stage"] for s in stages]
    assert recorded_stage_names == expected_stages

    # Verify each stage record structure
    for idx, s in enumerate(stages):
        assert s["step"] == idx
        assert s["object_id"] == "main"
        assert s["attempt"] == "primary"
        assert "stage" in s
        assert "method" in s
        assert "sha256" in s
        assert len(s["sha256"]) == 64
        assert "gate" in s
        assert isinstance(s["gate"], dict)
        if s["output"]:
            assert "faces" in s["output"]
            assert "verts" in s["output"]
            assert "sha256" in s["output"]

    # Final summary should match final stage output
    assert data["final_summary"]["sha256"] == stages[-1]["sha256"]


def test_trace_on_vs_off_byte_identical(tmp_path):
    """Verify strict byte-gate parity: output with trace ON vs OFF must be 100% byte identical."""
    models = [
        "rust/sutura-geom/tests/fixtures/case1_open_cup.stl",
        "rust/sutura-geom/tests/fixtures/case4_cracked_sphere.stl",
        "rust/sutura-geom/tests/fixtures/plate_a_dent.stl",
        "tests/real-world-samples/thingi10k_224108.stl",
    ]

    env = os.environ.copy()
    env["PYTHONPATH"] = "sutura:sutura/sutura_engine:" + env.get("PYTHONPATH", "")

    for model in models:
        out_off = str(tmp_path / f"off_{os.path.basename(model)}")
        out_on = str(tmp_path / f"on_{os.path.basename(model)}")
        trace_file = str(tmp_path / f"trace_{os.path.basename(model)}.json")

        # Run without trace
        cmd_off = [sys.executable, "sutura/repair.py", model, "-o", out_off]
        res_off = subprocess.run(cmd_off, env=env, capture_output=True, text=True)
        assert res_off.returncode == 0, f"Repair failed (off): {res_off.stderr}"

        # Run with trace
        cmd_on = [sys.executable, "sutura/repair.py", model, "-o", out_on, "--trace", trace_file]
        res_on = subprocess.run(cmd_on, env=env, capture_output=True, text=True)
        assert res_on.returncode == 0, f"Repair failed (on): {res_on.stderr}"

        # Compare output bytes
        with open(out_off, "rb") as f1, open(out_on, "rb") as f2:
            bytes_off = f1.read()
            bytes_on = f2.read()

        assert bytes_off == bytes_on, f"Byte divergence with trace ON for {model}!"


def test_trace_diff_tool_identical(tmp_path):
    """Verify that comparing identical traces reports IDENTICAL and exits 0."""
    trace_path = str(tmp_path / "self_diff.json")
    out_stl = str(tmp_path / "self_diff.stl")
    model = "rust/sutura-geom/tests/fixtures/case1_open_cup.stl"

    env = os.environ.copy()
    env["PYTHONPATH"] = "sutura:sutura/sutura_engine:" + env.get("PYTHONPATH", "")
    cmd = [sys.executable, "sutura/repair.py", model, "-o", out_stl, "--trace", trace_path]
    subprocess.run(cmd, env=env, check=True)

    t_data = _load_trace(trace_path)
    exit_code = diff_traces(t_data, t_data, trace_path, trace_path, quiet=True)
    assert exit_code == 0


def test_trace_diff_identifies_ftetwild_divergence(tmp_path):
    """Run thingi10k_100827 twice and verify trace_diff identifies ftetwild as the first divergence."""
    model = "tests/real-world-samples/thingi10k_100827.stl"
    t1_path = str(tmp_path / "100827_run1.json")
    t2_path = str(tmp_path / "100827_run2.json")
    out1 = str(tmp_path / "out1.stl")
    out2 = str(tmp_path / "out2.stl")

    env = os.environ.copy()
    env["PYTHONPATH"] = "sutura:sutura/sutura_engine:" + env.get("PYTHONPATH", "")

    cmd1 = [sys.executable, "sutura/repair.py", model, "-o", out1, "--trace", t1_path]
    subprocess.run(cmd1, env=env, check=True)

    cmd2 = [sys.executable, "sutura/repair.py", model, "-o", out2, "--trace", t2_path]
    subprocess.run(cmd2, env=env, check=True)

    t1_data = _load_trace(t1_path)
    t2_data = _load_trace(t2_path)

    # Run trace_diff via CLI to check terminal report
    diff_cmd = [sys.executable, "scripts/trace_diff.py", t1_path, t2_path]
    res = subprocess.run(diff_cmd, capture_output=True, text=True)

    assert res.returncode == 1
    assert "FIRST DIVERGING STAGE: Step 12 ['ftetwild']" in res.stdout
    assert "Verdict: Runs first diverged at Step 12 ['ftetwild']." in res.stdout
