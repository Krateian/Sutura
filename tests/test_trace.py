# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Tests for pipeline debug trace and stage diff tool."""
import json
import os
import subprocess
import sys
import tempfile
import time

import pytest

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
        "outer_shell",
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
        "save",
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

    # The save stage measures the written file; final_summary follows it, not
    # the last internal candidate.
    assert stages[-1]["stage"] == "save"
    assert stages[-1]["gate"]["status"] == "written"
    assert data["final_summary"]["sha256"] == stages[-1]["sha256"]
    assert data["final_summary"] == stages[-1]["output"]


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


def test_trace_final_summary_is_the_written_output(tmp_path, monkeypatch):
    """Regression for the 39635 class of bug: a rejected candidate recorded
    after the output was written must not become ``final_summary``.

    The repair registry is replaced with a fake that writes a known mesh to
    the trial output and then records a larger rejected candidate, mirroring a
    baseline repair followed by a fallback attempt. The save stage must
    describe the written file, not the last internal record.
    """
    import numpy as np
    import repair
    from sutura_engine import trace as trace_mod

    verts = np.array([
        [0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0],
        [0, 0, 1], [1, 0, 1], [1, 1, 1], [0, 1, 1]], dtype=np.float64)
    tris = np.array([
        [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
        [0, 1, 5], [0, 5, 4], [1, 2, 6], [1, 6, 5],
        [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7]], dtype=np.int32)

    def fake_repair(src, out, tmpdir, **kwargs):
        repair.save_mesh(out, verts, tris)
        rejected_v = np.vstack([verts, verts])
        rejected_t = np.vstack([tris, tris + len(verts)])
        trace_mod.record_stage(
            "stage1_chain", "fake.rejected_candidate",
            rejected_v, rejected_t, rejected_v, rejected_t,
            gate={"status": "rejected"})
        return {"input": src, "output": out}

    monkeypatch.setattr(repair.method_registry, "repair_with_methods",
                        fake_repair)
    src = str(tmp_path / "in.stl")
    out = str(tmp_path / "out.stl")
    trace_path = str(tmp_path / "trace.json")
    repair.save_mesh(src, verts, tris)

    result, _category = repair.process_file(
        src, False, out=out, trace_path=trace_path, no_history=True,
        fidelity_enabled=False)
    assert "error" not in result

    with open(trace_path, "r") as f:
        data = json.load(f)
    stages = data["stages"]
    assert stages[-1]["stage"] == "save"
    assert stages[-1]["gate"]["status"] == "written"
    assert stages[-1]["output"]["faces"] == len(tris)
    assert data["final_summary"]["faces"] == len(tris)
    # The rejected candidate exists in the trace but is not the final summary.
    assert any(s["stage"] == "stage1_chain"
               and s["output"]["faces"] == 2 * len(tris) for s in stages)


def test_trace_outer_shell_follows_tamponade(tmp_path):
    """The outer-shell pre-step is part of Tamponade and gets its own stage.

    Default: adopted on thingi10k_55772 (4,408 -> 3,938 faces). With
    ``--no-tamponade`` the pre-step is disabled, so its input and output are
    the same mesh; only the opt-out path changes.
    """
    sample = "tests/real-world-samples/thingi10k_55772.stl"
    env = os.environ.copy()
    env["PYTHONPATH"] = "sutura:sutura/sutura_engine:" + env.get("PYTHONPATH", "")

    def _run(extra, name):
        out = str(tmp_path / name)
        trace_path = str(tmp_path / (name + ".json"))
        cmd = [sys.executable, "sutura/repair.py", sample, "-o", out,
               "--trace", trace_path] + extra
        res = subprocess.run(cmd, env=env, capture_output=True, text=True)
        assert res.returncode == 0, f"Repair failed: {res.stderr}"
        with open(trace_path, "r") as f:
            data = json.load(f)
        stage = next(s for s in data["stages"] if s["stage"] == "outer_shell")
        return out, stage

    out_on, shell_on = _run([], "on.stl")
    out_off, shell_off = _run(["--no-tamponade"], "off.stl")

    assert shell_on["gate"]["status"] == "adopted"
    assert shell_on["input"]["faces"] == 4408
    assert shell_on["output"]["faces"] == 3938

    assert shell_off["gate"]["status"] == "disabled"
    assert shell_off["gate"]["reason"] == "tamponade_disabled"
    assert shell_off["input"]["faces"] == shell_off["output"]["faces"] == 4408

    with open(out_on, "rb") as f_on, open(out_off, "rb") as f_off:
        assert f_on.read() != f_off.read()


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


def _stage(i, name, sha):
    return {
        "step": i, "object_id": "main", "attempt": "primary",
        "stage": name, "method": "m", "sha256": sha, "gate": {},
        "output": {"faces": 10, "verts": 10, "holes": 0, "nm_edges": 0,
                   "nm_verts": 0, "components": 1},
        "elapsed_seconds": 0.0,
    }


def _write_trace(path, stages):
    data = {"version": 1, "total_elapsed_seconds": 0.0, "stages": stages,
            "final_summary": {"sha256": stages[-1]["sha256"]}}
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f)
    return str(path)


STAGE_NAMES = [
    "load", "tamponade_prepass", "flap_prepass", "stage1_chain",
    "stage1_delete_fallback", "si_excise", "closing_ladder",
    "tamponade_post", "graft", "dressing", "splint", "coaptation",
    "ftetwild", "pinch_split", "stage2", "p_weld", "validate",
]


def test_trace_diff_identifies_ftetwild_divergence(tmp_path):
    """A synthetic pair that differs only at step 12 (stage 'ftetwild')."""
    a = [_stage(i, n, "a" * 64) for i, n in enumerate(STAGE_NAMES)]
    b = [_stage(i, n, "a" * 64) for i, n in enumerate(STAGE_NAMES)]
    b[12]["sha256"] = "b" * 64
    pa = _write_trace(tmp_path / "a.json", a)
    pb = _write_trace(tmp_path / "b.json", b)
    res = subprocess.run(
        [sys.executable, "scripts/trace_diff.py", pa, pb],
        capture_output=True, text=True)
    assert res.returncode == 1
    assert "FIRST DIVERGING STAGE: Step 12 ['ftetwild']" in res.stdout
    assert "Verdict: Runs first diverged at Step 12 ['ftetwild']." in res.stdout


def test_trace_diff_identical_pair(tmp_path):
    """The same trace twice must report identical and exit 0."""
    a = [_stage(i, n, "a" * 64) for i, n in enumerate(STAGE_NAMES)]
    pa = _write_trace(tmp_path / "a.json", a)
    res = subprocess.run(
        [sys.executable, "scripts/trace_diff.py", pa, pa],
        capture_output=True, text=True)
    assert res.returncode == 0
    assert "executed identically" in res.stdout


if __name__ == '__main__':
    raise SystemExit(pytest.main([os.path.abspath(__file__), '-q']))
