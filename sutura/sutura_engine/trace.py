# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Sutura pipeline trace: structured debug recording of pipeline stage boundaries.

When enabled via ``--trace out.json`` on the CLI or ``SUTURA_TRACE=<path>`` in
the environment, every repair stage records a structured snapshot containing:
  - stage name and method invoked
  - input and output measures (faces, verts, holes, non-manifold edges/verts,
    components, volume when watertight)
  - sha256 of the output geometry arrays (normalized float32/int32 bytes)
  - gate decisions with reasons (adopted, skipped, fallback)
  - elapsed time for that stage

When disabled (the default), `is_active()` returns False and `record_stage()` /
`stage_scope()` perform ZERO extra work (no hashing, no topometry calls, overhead
< 0.01%).

Tracing is strictly an observer and never mutates geometry or alters pipeline
control flow. Tracing exceptions are caught and recorded, never raised.
"""
import hashlib
import json
import os
import sys
import time
from typing import Any, Dict, List, Optional, Tuple

import numpy as np


def hash_mesh_arrays(verts: Optional[np.ndarray], tris: Optional[np.ndarray]) -> str:
    """Compute a deterministic SHA-256 of mesh arrays without mutating them.

    Normalizes vertices to contiguous float32 with -0.0 -> +0.0, and triangles
    to contiguous int32.
    """
    if verts is None or tris is None or len(verts) == 0:
        return "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"  # sha256 of empty bytes

    # Copy vertices to avoid mutating pipeline arrays during normalization
    v_norm = np.array(verts, dtype=np.float32, copy=True, order='C')
    v_norm[v_norm == 0.0] = 0.0
    t_norm = np.ascontiguousarray(tris, dtype=np.int32)

    h = hashlib.sha256()
    h.update(v_norm.tobytes())
    h.update(t_norm.tobytes())
    return h.hexdigest()


def measure_mesh(verts: Optional[np.ndarray], tris: Optional[np.ndarray]) -> Optional[Dict[str, Any]]:
    """Measure topological and geometric properties of a mesh.

    Only called when tracing is active. Reuses topology module.
    """
    if verts is None or tris is None:
        return None

    try:
        v_arr = np.asarray(verts, dtype=np.float32)
        t_arr = np.asarray(tris, dtype=np.int32)
        n_verts = int(len(v_arr))
        n_faces = int(len(t_arr))
        arr_hash = hash_mesh_arrays(v_arr, t_arr)

        if n_faces == 0:
            return {
                "faces": 0,
                "verts": n_verts,
                "holes": 0,
                "nm_edges": 0,
                "nm_verts": 0,
                "components": 0,
                "volume": None,
                "sha256": arr_hash,
            }

        try:
            import topology
        except ImportError:
            from sutura import topology
        top = topology.mesh_topology(v_arr, t_arr)

        try:
            from sutura_engine.pipeline.utils import boundary_loop_stats
        except ImportError:
            from sutura.sutura_engine.pipeline.utils import boundary_loop_stats
        holes = int(boundary_loop_stats(v_arr, t_arr)[0])
        nm_edges = int(top.get('non_two_manifold_edges', 0) or 0)
        nm_verts = int(top.get('non_two_manifold_vertices', 0) or 0)
        components = int(top.get('connected_components_number', 0) or 0)

        volume = None
        # Only compute volume when mesh is watertight to avoid expensive divergence integrals on open meshes
        if holes == 0 and nm_edges == 0 and nm_verts == 0:
            try:
                geom = topology.mesh_geometric_measures(v_arr, t_arr)
                vol = geom.get('mesh_volume')
                if vol is not None:
                    volume = round(float(vol), 6)
            except Exception:
                volume = None

        return {
            "faces": n_faces,
            "verts": n_verts,
            "holes": holes,
            "nm_edges": nm_edges,
            "nm_verts": nm_verts,
            "components": components,
            "volume": volume,
            "sha256": arr_hash,
        }
    except Exception as e:
        return {
            "faces": len(tris) if tris is not None else 0,
            "verts": len(verts) if verts is not None else 0,
            "error": str(e),
            "sha256": hash_mesh_arrays(verts, tris),
        }


class StageScope:
    """Context manager for tracing an individual pipeline stage."""

    def __init__(self, tracer: 'PipelineTracer', stage: str, method: str,
                 v_in: Optional[np.ndarray], t_in: Optional[np.ndarray],
                 object_id: str = 'main', attempt: str = 'primary'):
        self.tracer = tracer
        self.stage = stage
        self.method = method
        self.v_in = v_in
        self.t_in = t_in
        self.object_id = object_id
        self.attempt = attempt
        self.v_out = None
        self.t_out = None
        self.gate = None
        self.t0 = 0.0

    def __enter__(self):
        if self.tracer.is_active:
            self.t0 = time.perf_counter()
        return self

    def set_output(self, v_out: Optional[np.ndarray], t_out: Optional[np.ndarray],
                   gate: Optional[Dict[str, Any]] = None):
        """Record the output arrays and gate decision."""
        if not self.tracer.is_active:
            return
        self.v_out = v_out
        self.t_out = t_out
        self.gate = gate

    def __exit__(self, exc_type, exc_val, exc_tb):
        if not self.tracer.is_active:
            return False
        elapsed = round(time.perf_counter() - self.t0, 4)
        gate = self.gate or {}
        if exc_type is not None:
            gate['exception'] = str(exc_val)
        # If output was never set (e.g. stage failed or returned in-place without update), use input as output
        v_out = self.v_out if self.v_out is not None else self.v_in
        t_out = self.t_out if self.t_out is not None else self.t_in
        self.tracer.record_stage(
            stage=self.stage,
            method=self.method,
            v_in=self.v_in,
            t_in=self.t_in,
            v_out=v_out,
            t_out=t_out,
            gate=gate,
            elapsed=elapsed,
            object_id=self.object_id,
            attempt=self.attempt,
        )
        return False  # Propagate any original exception


def _is_timing_key(k: str) -> bool:
    kl = str(k).lower()
    return (
        kl in ('time', 'seconds', 'elapsed', 'budget_s', 'time_budget', 'elapsed_seconds', 'time_s')
        or kl.endswith('_s')
        or kl.endswith('_time')
        or kl.endswith('_seconds')
        or kl.endswith('_ms')
    )


def sanitize_gate(gate: Any) -> Any:
    """Filter out non-deterministic execution times from gate dicts."""
    if isinstance(gate, dict):
        return {
            k: sanitize_gate(v)
            for k, v in gate.items()
            if not _is_timing_key(k)
        }
    if isinstance(gate, list):
        return [sanitize_gate(item) for item in gate]
    return gate


class PipelineTracer:
    """Collects structured trace records for each stage of the repair pipeline."""

    def __init__(self, trace_path: Optional[str] = None, input_file: Optional[str] = None,
                 command_args: Optional[Dict[str, Any]] = None):
        self.path = trace_path or os.environ.get('SUTURA_TRACE')
        self.is_active = bool(self.path)
        self.input_file = input_file
        self.command_args = command_args or {}
        self.records: List[Dict[str, Any]] = []
        self._step = 0
        self.current_object_id = 'main'
        self.current_attempt = 'primary'
        self.t_start = time.perf_counter() if self.is_active else 0.0

    def set_context(self, object_id: Optional[str] = None, attempt: Optional[str] = None):
        """Set active object_id and attempt tags for subsequent stage records."""
        if object_id is not None:
            self.current_object_id = object_id
        if attempt is not None:
            self.current_attempt = attempt

    def record_stage(self, stage: str, method: str,
                     v_in: Optional[np.ndarray], t_in: Optional[np.ndarray],
                     v_out: Optional[np.ndarray], t_out: Optional[np.ndarray],
                     gate: Optional[Dict[str, Any]] = None, elapsed: float = 0.0,
                     object_id: Optional[str] = None, attempt: Optional[str] = None,
                     in_measures: Optional[Dict[str, Any]] = None,
                     out_measures: Optional[Dict[str, Any]] = None):
        """Append one structured stage record to the trace."""
        if not self.is_active:
            return

        obj = object_id if object_id is not None else self.current_object_id
        att = attempt if attempt is not None else self.current_attempt

        try:
            # Reuse previous stage's output measures if available to save computation
            if in_measures is None:
                if self.records and self.records[-1].get('object_id') == obj and self.records[-1].get('attempt') == att:
                    in_measures = self.records[-1].get('output')
                else:
                    in_measures = measure_mesh(v_in, t_in)

            if out_measures is None:
                out_measures = measure_mesh(v_out, t_out)

            sha = out_measures.get('sha256') if out_measures else hash_mesh_arrays(v_out, t_out)

            rec = {
                "step": self._step,
                "object_id": obj,
                "attempt": att,
                "stage": stage,
                "method": method,
                "elapsed_seconds": round(elapsed, 4),
                "input": in_measures,
                "output": out_measures,
                "sha256": sha,
                "gate": sanitize_gate(gate) if gate else {},
            }
            self._step += 1
            self.records.append(rec)
        except Exception as e:
            # Tracing exception must never abort repair
            self.records.append({
                "step": self._step,
                "object_id": obj,
                "attempt": att,
                "stage": stage,
                "method": method,
                "elapsed_seconds": round(elapsed, 4),
                "trace_error": str(e),
                "sha256": hash_mesh_arrays(v_out, t_out),
                "gate": gate or {},
            })
            self._step += 1

    def stage_scope(self, stage: str, method: str,
                    v_in: Optional[np.ndarray], t_in: Optional[np.ndarray],
                    object_id: str = 'main', attempt: str = 'primary') -> StageScope:
        """Create a context manager for tracking an execution stage."""
        return StageScope(self, stage, method, v_in, t_in, object_id, attempt)

    def write_to_file(self, final_verts: Optional[np.ndarray] = None,
                      final_tris: Optional[np.ndarray] = None,
                      final_report: Optional[Dict[str, Any]] = None):
        """Serialize records to JSON."""
        if not self.is_active or not self.path:
            return

        try:
            total_time = round(time.perf_counter() - self.t_start, 4)
            final_summary = None
            if final_verts is not None and final_tris is not None:
                final_summary = measure_mesh(final_verts, final_tris)
            elif self.records:
                final_summary = self.records[-1].get('output')

            data = {
                "version": 1,
                "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "input_file": self.input_file,
                "total_elapsed_seconds": total_time,
                "command_args": self.command_args,
                "stages": self.records,
                "final_summary": final_summary,
            }

            target = self.path
            # If path contains %s, format with basename
            if '%s' in target and self.input_file:
                stem = os.path.splitext(os.path.basename(self.input_file))[0]
                target = target % stem

            os.makedirs(os.path.dirname(os.path.abspath(target)), exist_ok=True)
            with open(target, 'w') as f:
                json.dump(data, f, indent=2)
        except Exception as e:
            # File writing failure should not crash repair
            sys.stderr.write(f"[sutura trace] Warning: failed to write trace to {self.path}: {e}\n")


# Global active tracer instance
_CURRENT_TRACER: Optional[PipelineTracer] = None


def is_active() -> bool:
    """Return True if pipeline tracing is currently active."""
    return _CURRENT_TRACER is not None and _CURRENT_TRACER.is_active


def current_tracer() -> PipelineTracer:
    """Return the currently active tracer (or a no-op inactive tracer)."""
    global _CURRENT_TRACER
    if _CURRENT_TRACER is None:
        _CURRENT_TRACER = PipelineTracer()
    return _CURRENT_TRACER


def begin_trace(trace_path: Optional[str] = None, input_file: Optional[str] = None,
                command_args: Optional[Dict[str, Any]] = None) -> PipelineTracer:
    """Start a trace session."""
    global _CURRENT_TRACER
    _CURRENT_TRACER = PipelineTracer(trace_path, input_file, command_args)
    return _CURRENT_TRACER


def finish_trace(final_verts: Optional[np.ndarray] = None,
                 final_tris: Optional[np.ndarray] = None,
                 final_report: Optional[Dict[str, Any]] = None):
    """Finish the trace session and write results."""
    global _CURRENT_TRACER
    if _CURRENT_TRACER is not None and _CURRENT_TRACER.is_active:
        _CURRENT_TRACER.write_to_file(final_verts, final_tris, final_report)
    _CURRENT_TRACER = None


def set_context(object_id: Optional[str] = None, attempt: Optional[str] = None):
    """Set object_id and attempt tags for the current tracer."""
    if is_active():
        current_tracer().set_context(object_id=object_id, attempt=attempt)


def stage_scope(stage: str, method: str,
                v_in: Optional[np.ndarray], t_in: Optional[np.ndarray],
                object_id: Optional[str] = None, attempt: Optional[str] = None) -> StageScope:
    """Convenience helper to enter a stage scope on the current tracer."""
    return current_tracer().stage_scope(stage, method, v_in, t_in,
                                        object_id=object_id or current_tracer().current_object_id,
                                        attempt=attempt or current_tracer().current_attempt)


def record_stage(stage: str, method: str,
                 v_in: Optional[np.ndarray], t_in: Optional[np.ndarray],
                 v_out: Optional[np.ndarray], t_out: Optional[np.ndarray],
                 gate: Optional[Dict[str, Any]] = None, elapsed: float = 0.0,
                 object_id: Optional[str] = None, attempt: Optional[str] = None,
                 in_measures: Optional[Dict[str, Any]] = None,
                 out_measures: Optional[Dict[str, Any]] = None):
    """Convenience helper to record a stage on the current tracer."""
    if not is_active():
        return
    current_tracer().record_stage(
        stage=stage, method=method,
        v_in=v_in, t_in=t_in, v_out=v_out, t_out=t_out,
        gate=gate, elapsed=elapsed, object_id=object_id, attempt=attempt,
        in_measures=in_measures, out_measures=out_measures,
    )

