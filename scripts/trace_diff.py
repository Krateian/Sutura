#!/usr/bin/env python3
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""trace_diff.py - Compares two Sutura pipeline execution traces.

Finds and reports the FIRST stage where two runs diverge in:
  - Output array geometry (SHA-256 mismatch)
  - Gate decisions (adopted vs skipped, differing reasons)
  - Output measures (faces, vertices, holes, non-manifold edges/vertices, components)
  - Pipeline stage flow (a stage present in one run but missing in the other)

Usage:
    python3 scripts/trace_diff.py <trace_a.json> <trace_b.json> [--full] [--quiet]

Exit codes:
    0: Traces are completely identical across all stages.
    1: Divergence detected between the two traces.
    2: File not found, invalid JSON, or missing required fields.
"""
import argparse
import json
import os
import sys
from typing import Any, Dict, List, Optional, Tuple


def _load_trace(path: str) -> Dict[str, Any]:
    if not os.path.exists(path):
        sys.stderr.write(f"Error: Trace file not found: {path}\n")
        sys.exit(2)
    try:
        with open(path, 'r') as f:
            data = json.load(f)
        if not isinstance(data, dict) or 'stages' not in data:
            sys.stderr.write(f"Error: Invalid trace JSON format in {path} (missing 'stages')\n")
            sys.exit(2)
        return data
    except Exception as e:
        sys.stderr.write(f"Error: Failed to read {path}: {e}\n")
        sys.exit(2)


def _fmt_val(val: Any) -> str:
    if val is None:
        return "None"
    if isinstance(val, float):
        return f"{val:.4f}" if abs(val) < 1000 else f"{val:.1f}"
    if isinstance(val, int):
        return f"{val:,}"
    return str(val)


def _is_timing_key(k: str) -> bool:
    kl = str(k).lower()
    return (
        kl in ('time', 'seconds', 'elapsed', 'budget_s', 'time_budget', 'elapsed_seconds', 'time_s')
        or kl.endswith('_s')
        or kl.endswith('_time')
        or kl.endswith('_seconds')
        or kl.endswith('_ms')
    )


def _clean_gate(gate: Any) -> Any:
    if isinstance(gate, dict):
        return {
            k: _clean_gate(v)
            for k, v in gate.items()
            if not _is_timing_key(k)
        }
    if isinstance(gate, list):
        return [_clean_gate(item) for item in gate]
    return gate


def _format_measures_diff(m_a: Optional[Dict[str, Any]], m_b: Optional[Dict[str, Any]]) -> str:
    if not m_a or not m_b:
        return "  Measures unavailable\n"
    keys = ["faces", "verts", "holes", "nm_edges", "nm_verts", "components", "volume"]
    labels = {
        "faces": "Faces",
        "verts": "Vertices",
        "holes": "Holes",
        "nm_edges": "NM Edges",
        "nm_verts": "NM Vertices",
        "components": "Components",
        "volume": "Volume",
    }
    lines = ["  Metric                Trace A         Trace B         Delta",
             "  -------------------------------------------------------------"]
    for k in keys:
        va = m_a.get(k)
        vb = m_b.get(k)
        delta = "(=)"
        if va != vb:
            if isinstance(va, (int, float)) and isinstance(vb, (int, float)):
                diff = vb - va
                delta = f"{diff:+,}" if isinstance(diff, int) else f"{diff:+.2f}"
            else:
                delta = "(!)"
        lbl = labels.get(k, k).ljust(20)
        sa = _fmt_val(va).ljust(15)
        sb = _fmt_val(vb).ljust(15)
        lines.append(f"  {lbl}  {sa} {sb} {delta}")
    return "\n".join(lines)


def diff_traces(trace_a: Dict[str, Any], trace_b: Dict[str, Any],
                name_a: str, name_b: str, full: bool = False,
                quiet: bool = False) -> int:
    stages_a = trace_a.get('stages', [])
    stages_b = trace_b.get('stages', [])

    n_a = len(stages_a)
    n_b = len(stages_b)
    max_stages = max(n_a, n_b)

    divergence_idx = None
    divergence_type = None
    divergence_detail = {}

    for i in range(max_stages):
        if i >= n_a:
            divergence_idx = i
            divergence_type = "stage_missing_in_a"
            divergence_detail = {"b_stage": stages_b[i]}
            break
        if i >= n_b:
            divergence_idx = i
            divergence_type = "stage_missing_in_b"
            divergence_detail = {"a_stage": stages_a[i]}
            break

        sa = stages_a[i]
        sb = stages_b[i]

        stage_id_a = (sa.get('object_id', 'main'), sa.get('attempt', 'primary'), sa.get('stage'))
        stage_id_b = (sb.get('object_id', 'main'), sb.get('attempt', 'primary'), sb.get('stage'))

        if stage_id_a != stage_id_b:
            divergence_idx = i
            divergence_type = "stage_flow_divergence"
            divergence_detail = {"stage_a": sa, "stage_b": sb}
            break

        # Check sha256 of output geometry
        hash_a = sa.get('sha256')
        hash_b = sb.get('sha256')
        if hash_a != hash_b:
            divergence_idx = i
            divergence_type = "geometry_sha256_mismatch"
            divergence_detail = {"stage_a": sa, "stage_b": sb}
            break

        # Check gate decision
        gate_a = _clean_gate(sa.get('gate', {}))
        gate_b = _clean_gate(sb.get('gate', {}))
        if gate_a != gate_b:
            divergence_idx = i
            divergence_type = "gate_decision_diff"
            divergence_detail = {"stage_a": sa, "stage_b": sb}
            break

        # Check output measures
        out_a = sa.get('output', {}) or {}
        out_b = sb.get('output', {}) or {}
        for k in ["faces", "verts", "holes", "nm_edges", "nm_verts", "components"]:
            if out_a.get(k) != out_b.get(k):
                divergence_idx = i
                divergence_type = "output_measures_diff"
                divergence_detail = {"stage_a": sa, "stage_b": sb}
                break
        if divergence_idx is not None:
            break

    if divergence_idx is None:
        if not quiet:
            print("=" * 80)
            print("SUTURA REPAIR TRACE DIFF: IDENTICAL")
            print(f"Trace A: {name_a} ({n_a} stages)")
            print(f"Trace B: {name_b} ({n_b} stages)")
            print("=" * 80)
            print(f"All {n_a} stages executed identically with matching array SHA-256 and gate decisions.")
        return 0

    if quiet:
        return 1

    print("=" * 80)
    print("SUTURA REPAIR TRACE DIFF: DIVERGENCE DETECTED")
    print(f"Trace A: {name_a} (stages: {n_a}, elapsed: {_fmt_val(trace_a.get('total_elapsed_seconds'))}s)")
    print(f"Trace B: {name_b} (stages: {n_b}, elapsed: {_fmt_val(trace_b.get('total_elapsed_seconds'))}s)")
    print("=" * 80)

    # Print preceding matching stages if full or brief summary
    for i in range(divergence_idx):
        sa = stages_a[i]
        stg = sa.get('stage')
        sha = (sa.get('sha256') or '')[:8]
        if full or i >= max(0, divergence_idx - 3):
            print(f"  [✓] Step {i}: '{stg}' -> MATCH (sha256: {sha}...)")

    print(f"  [!] Step {divergence_idx}: DIVERGENCE DETECTED!")
    print("-" * 80)

    if divergence_type in ("geometry_sha256_mismatch", "gate_decision_diff", "output_measures_diff"):
        sa = divergence_detail['stage_a']
        sb = divergence_detail['stage_b']
        stage_name = sa.get('stage')
        method = sa.get('method')
        print(f"FIRST DIVERGING STAGE: Step {divergence_idx} ['{stage_name}'] (method: {method})")
        print("-" * 80)
        print("Array SHA256:")
        print(f"  Trace A: {sa.get('sha256')}")
        print(f"  Trace B: {sb.get('sha256')}")
        print()
        print("Measures Comparison (Output of this stage):")
        print(_format_measures_diff(sa.get('output'), sb.get('output')))
        print()
        print("Gate Decisions:")
        print(f"  Trace A: {sa.get('gate')}")
        print(f"  Trace B: {sb.get('gate')}")
        print()
        print("Execution Time:")
        print(f"  Trace A: {_fmt_val(sa.get('elapsed_seconds'))}s | Trace B: {_fmt_val(sb.get('elapsed_seconds'))}s")
    elif divergence_type == "stage_flow_divergence":
        sa = divergence_detail['stage_a']
        sb = divergence_detail['stage_b']
        print(f"STAGE FLOW DIVERGENCE AT STEP {divergence_idx}:")
        print(f"  Trace A executed: '{sa.get('stage')}' (method: {sa.get('method')})")
        print(f"  Trace B executed: '{sb.get('stage')}' (method: {sb.get('method')})")
    elif divergence_type == "stage_missing_in_a":
        sb = divergence_detail['b_stage']
        print(f"STAGE FLOW DIVERGENCE AT STEP {divergence_idx}:")
        print(f"  Trace A finished at step {n_a}")
        print(f"  Trace B executed extra stage: '{sb.get('stage')}' (method: {sb.get('method')})")
    elif divergence_type == "stage_missing_in_b":
        sa = divergence_detail['a_stage']
        print(f"STAGE FLOW DIVERGENCE AT STEP {divergence_idx}:")
        print(f"  Trace A executed extra stage: '{sa.get('stage')}' (method: {sa.get('method')})")
        print(f"  Trace B finished at step {n_b}")

    print("-" * 80)
    # Check final summary
    fin_a = trace_a.get('final_summary') or {}
    fin_b = trace_b.get('final_summary') or {}
    sha_a = fin_a.get('sha256')
    sha_b = fin_b.get('sha256')
    match_final = "YES (same final output despite divergence)" if sha_a == sha_b else "NO"
    print(f"Downstream Cascade Impact:")
    print(f"  Subsequent stages remaining: A has {n_a - 1 - divergence_idx}, B has {n_b - 1 - divergence_idx}")
    print(f"  Final output byte-identical: {match_final}")
    print("=" * 80)
    print(f"Verdict: Runs first diverged at Step {divergence_idx} ['{stages_a[divergence_idx].get('stage') if divergence_idx < n_a else stages_b[divergence_idx].get('stage')}'].")
    return 1


def main():
    parser = argparse.ArgumentParser(description="Compare two Sutura repair pipeline traces.")
    parser.add_argument("trace_a", help="Path to first trace JSON file")
    parser.add_argument("trace_b", help="Path to second trace JSON file")
    parser.add_argument("--full", action="store_true", help="Print all matching steps before divergence")
    parser.add_argument("--quiet", action="store_true", help="Silent output; exit code only (0=same, 1=diff)")

    args = parser.parse_args()

    trace_a = _load_trace(args.trace_a)
    trace_b = _load_trace(args.trace_b)

    ret = diff_traces(trace_a, trace_b, args.trace_a, args.trace_b,
                      full=args.full, quiet=args.quiet)
    sys.exit(ret)


if __name__ == "__main__":
    main()
