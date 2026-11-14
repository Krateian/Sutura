#!/usr/bin/env python3
"""fTetWild fallback bridge: tetrahedralize a broken surface and extract the
boundary as a watertight, self-intersection-free triangle mesh.

Guaranteed-correct fallback tier (FAZ17) for meshes where the stage-1 chain
(and --experimental-autorefine) still leave residual self-intersections.
fTetWild (MPL-2.0, via the pytetwild wrapper) tetrahedralizes within an
epsilon-envelope using floating-point construction, then the boundary surface
of the tet mesh is extracted here as the repaired output.

Runs under the main Sutura virtualenv / conda environment, or a legacy
python3.11 fTetWild virtualenv. Reads an input surface mesh (STL/OBJ), calls
pytetwild.tetrahedralize(), keeps the triangle faces incident to exactly ONE
tetrahedron (the boundary), orients them outward/consistently, writes an OBJ
for the caller and prints a JSON report.

Only pytetwild is required. pytetwild imports its optional PyVista accessor at
import time; that accessor's ``import pyvista`` is stubbed out here so the extra
carries no VTK dependency (the API used below, raw numpy arrays, never touches
PyVista). When pytetwild is missing the bridge reports an explicit skip, never
a crash.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

import numpy as np

# pytetwild.tetrahedralize keyword arguments used unless the caller passes
# its own. optimize=False skips fTetWild's mesh-quality optimisation, which
# only improves the (discarded) interior tets: measured on macOS
# (2026-09-26), thingi10k_46012 took 6 s instead of 34 s, with a one-sided
# output-to-input Hausdorff distance of 0.09 % of the bounding-box diagonal.
# The unoptimised boundary can carry non-manifold edges; the Manifold
# post-process in repair.py removes them.
DEFAULT_PARAMS = {'optimize': False}


def tetrahedralize_params(params=None):
    """DEFAULT_PARAMS overridden by ``params`` (a dict or None)."""
    kw = dict(DEFAULT_PARAMS)
    kw.update(params or {})
    return kw


def write_obj(path, verts, tris):
    with open(path, 'w') as f:
        f.write('# ftetwild bridge output\n')
        for v in verts:
            f.write('v %.9g %.9g %.9g\n' % (v[0], v[1], v[2]))
        for t in tris:
            f.write('f %d %d %d\n' % (t[0] + 1, t[1] + 1, t[2] + 1))


def extract_boundary(verts, tets):
    """Boundary triangles of a tet mesh: faces incident to exactly one tet,
    wound outward (normal pointing away from the tet's 4th vertex)."""
    verts = np.asarray(verts, dtype=np.float64)
    tets = np.asarray(tets, dtype=np.int64)
    keyed = {}
    for ti, tet in enumerate(tets):
        a, b, c, d = tet
        for v0, v1, v2, opp in ((a, b, c, d), (a, b, d, c),
                                (a, c, d, b), (b, c, d, a)):
            key = tuple(sorted((int(v0), int(v1), int(v2))))
            keyed.setdefault(key, []).append((v0, v1, v2, opp))
    out = []
    for key, recs in keyed.items():
        if len(recs) != 1:
            continue
        v0, v1, v2, opp = recs[0]
        n = np.cross(verts[v1] - verts[v0], verts[v2] - verts[v0])
        if np.dot(n, verts[opp] - verts[v0]) > 0:
            v1, v2 = v2, v1
        out.append((int(v0), int(v1), int(v2)))
    return out


def _import_pytetwild():
    """Import pytetwild, neutralising its optional PyVista accessor.

    pytetwild's ``__init__`` unconditionally imports ``pytetwild._accessor``,
    which does ``import pyvista as pv`` at module top; PyVista is declared
    optional but that import is never guarded, so a plain ``import pytetwild``
    drags in VTK (~520 MB) even though the API used here works on raw numpy
    arrays and never touches PyVista. Seed ``sys.modules`` with an empty stub
    *only* after the real import fails specifically because ``pyvista`` is
    missing; any other ``ImportError`` is re-raised unchanged.
    """
    try:
        import pytetwild  # noqa: F401
        return pytetwild
    except ModuleNotFoundError as e:
        if (e.name or '') != 'pyvista':
            raise
        import types
        sys.modules['pytetwild._accessor'] = types.ModuleType(
            'pytetwild._accessor')
        import pytetwild  # noqa: F401
        return pytetwild


def _find_binary():
    try:
        from sutura.ftetwild_manager import find_ftetwild_binary
        return find_ftetwild_binary()
    except Exception:
        _this = os.path.dirname(os.path.abspath(__file__))
        _repo = os.path.dirname(_this)
        if _repo not in sys.path:
            sys.path.insert(0, _repo)
        try:
            from sutura.ftetwild_manager import find_ftetwild_binary
            return find_ftetwild_binary()
        except Exception:
            return None


def run_binary(bin_path, src, dst, params=None, timeout=180):
    """Run the deterministic standalone fTetWild CLI binary via subprocess.

    Enforces --max-threads 1 for bitwise determinism and reads the boundary
    surface directly from the generated __sf.obj. Kills the entire process
    group on timeout or error.
    """
    from sutura_engine.pipeline.mesh_io import read_mesh, read_obj

    t0 = time.perf_counter()
    report = {'engine': 'binary', 'binary': bin_path}
    kw = tetrahedralize_params(params)
    report['params'] = kw

    try:
        in_v, in_t = read_mesh(src)
        report['input_vertices'] = int(len(in_v))
        report['input_faces'] = int(len(in_t))
    except Exception as e:
        report['error'] = f'failed reading input mesh: {e}'
        report['time'] = round(time.perf_counter() - t0, 2)
        return report

    with tempfile.TemporaryDirectory(prefix='sutura_ftw_') as tmpdir:
        msh_out = os.path.join(tmpdir, 'out.msh')
        sf_obj = os.path.join(tmpdir, 'out.msh__sf.obj')

        cmd = [
            bin_path,
            '-i', os.path.abspath(src),
            '-o', msh_out,
            '--max-threads', '1',
            '-q',
            '--no-color',
        ]

        if kw.get('optimize') is False:
            cmd.extend(['--max-its', '0'])
        elif 'max_its' in kw:
            cmd.extend(['--max-its', str(kw['max_its'])])

        if kw.get('simplify') is False or kw.get('skip_simplify') is True:
            cmd.append('--skip-simplify')

        if 'stop_energy' in kw:
            cmd.extend(['--stop-energy', str(kw['stop_energy'])])

        if 'ideal_edge_length' in kw:
            cmd.extend(['-e', str(kw['ideal_edge_length'])])
        elif 'edge_length_abs' in kw and kw['edge_length_abs'] is not None:
            cmd.extend(['-e', str(kw['edge_length_abs'])])
        elif 'lr' in kw:
            cmd.extend(['-l', str(kw['lr'])])
        elif 'edge_length_fac' in kw and kw['edge_length_fac'] is not None:
            cmd.extend(['-l', str(kw['edge_length_fac'])])

        if 'epsilon' in kw:
            cmd.extend(['--eps', str(kw['epsilon'])])

        proc_timeout = kw.get('timeout', timeout)

        try:
            proc = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                cwd=tmpdir,
                start_new_session=True,
            )
            stdout, stderr = proc.communicate(timeout=proc_timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            except (ProcessLookupError, OSError):
                pass
            try:
                proc.wait(timeout=2.0)
            except Exception:
                pass
            report['error'] = 'timeout'
            report['time'] = round(time.perf_counter() - t0, 2)
            return report
        except Exception as e:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            except Exception:
                pass
            report['error'] = f'process launch error: {e}'
            report['time'] = round(time.perf_counter() - t0, 2)
            return report

        report['time'] = round(time.perf_counter() - t0, 2)
        report['returncode'] = proc.returncode

        if proc.returncode != 0:
            err_tail = (stderr.decode('utf-8', errors='replace') if stderr else '').strip()
            report['error'] = f'fTetWild failed (rc={proc.returncode}): {err_tail[-300:]}'
            return report

        if not os.path.isfile(sf_obj):
            report['error'] = 'fTetWild completed but produced no surface mesh (__sf.obj)'
            return report

        try:
            out_v, out_t = read_obj(sf_obj)
            if len(out_t) == 0:
                report['error'] = 'fTetWild produced empty surface mesh'
                return report
            shutil.copyfile(sf_obj, dst)
            report['output_vertices'] = int(len(out_v))
            report['output_faces'] = int(len(out_t))
            report['ok'] = True
        except Exception as e:
            report['error'] = f'failed reading/copying surface mesh: {e}'
            return report

        return report


def run_pytetwild(src, dst, params=None):
    """Tetrahedralize the surface mesh at src and write the boundary surface
    to dst via pytetwild fallback. Returns the report dict; ok is True on success.
    Never raises for missing dependencies (reports an explicit skip instead)."""
    report = {}
    try:
        pytetwild = _import_pytetwild()
    except ImportError as e:
        report['error'] = ('fTetWild fallback skipped: pytetwild not '
                           'available in this environment (%s); it is '
                           'installed by default with Sutura and can be added '
                           'or repaired with `sutura ftetwild install` (see '
                           'requirements-ftetwild.txt)' % e)
        return report

    from sutura_engine.pipeline.mesh_io import read_mesh

    t0 = time.perf_counter()
    try:
        _in_v, _in_t = read_mesh(src)
        verts = np.asarray(_in_v, dtype=np.float64)
        tris = np.asarray(_in_t, dtype=np.int32)
        report['input_vertices'] = int(len(verts))
        report['input_faces'] = int(len(tris))
        kw = tetrahedralize_params(params)
        report['params'] = kw
        # pytetwild writes a debug mesh (__tracked_surface.stl, ~1 MB) into the
        # CURRENT WORKING DIRECTORY; run it from the output's directory so no
        # artifact is left where the caller happened to be (repo root, a
        # file-manager right-click folder, ...).
        _prev_cwd = os.getcwd()
        _workdir = os.path.dirname(os.path.abspath(dst))
        try:
            if _workdir:
                os.chdir(_workdir)
            tmesh_v, tmesh_c = pytetwild.tetrahedralize(
                verts, tris, quiet=True, **kw)
        finally:
            os.chdir(_prev_cwd)
        report['time'] = round(time.perf_counter() - t0, 2)
        report['tet_vertices'] = int(len(tmesh_v))
        report['tet_cells'] = int(len(tmesh_c))
        boundary = extract_boundary(tmesh_v, tmesh_c)
        report['output_vertices'] = int(len(tmesh_v))
        report['output_faces'] = int(len(boundary))
        write_obj(dst, tmesh_v, boundary)
        report['ok'] = True
    except Exception as e:  # noqa: BLE001 - a bad mesh must never crash a repair
        report['error'] = '%s: %s' % (type(e).__name__, e)
        report['time'] = round(time.perf_counter() - t0, 2)
    return report


def run_bridge(src, dst, params=None):
    """Tetrahedralize the surface mesh at src and write the boundary surface
    to dst. Uses deterministic standalone fTetWild binary when available,
    falling back to pytetwild. Returns the report dict; ok is True on success.
    Never raises for missing dependencies (reports an explicit skip instead)."""
    bin_path = _find_binary()
    if bin_path:
        return run_binary(bin_path, src, dst, params)
    return run_pytetwild(src, dst, params)


def main():
    src, dst = sys.argv[1], sys.argv[2]
    params = json.loads(sys.argv[3]) if len(sys.argv) > 3 else None
    report = run_bridge(src, dst, params)
    print(json.dumps(report))


if __name__ == '__main__':
    main()