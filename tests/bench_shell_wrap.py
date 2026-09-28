#!/usr/bin/env python3
"""F4 benchmark: Graft (#13 shell wrap) vs fTetWild (#7) on real-world samples.

One mesh at a time, each in its own subprocess with a wall-clock cap (10 min for
Graft, 4 min for fTetWild).  fTetWild is run through ``ftetwild_bridge`` on the
original input (the full ``repair.py --methods 7`` pipeline needs pymeshlab,
which is absent on this machine; the tier comparison is Graft(original) vs
fTetWild(original) — noted in the report).

Driver writes ``/tmp/opencode/f3/bench.md`` and ``bench.json``.

    <venv>/bin/python tests/bench_shell_wrap.py [sample_dir] [--only NAME]
"""
import argparse
import glob
import json
import os
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "sutura"))
SAMPLES = os.path.join(ROOT, "tests", "real-world-samples")
OUT_DIR = "/tmp/opencode/f3"
GRAFT_CAP = 600.0
FTET_CAP = 240.0


def _load(path):
    import numpy as np
    import trimesh
    m = trimesh.load(path, force="mesh")
    return (np.asarray(m.vertices, np.float64),
            np.asarray(m.faces, np.int64))


def _reload_ok(v, t):
    from repair import reload_strict_holes_nm
    h, nm = reload_strict_holes_nm(v, t)
    return int(h), int(nm)


def _hausdorff(orig_v, orig_t, out_v, out_t):
    from shell_wrap import _one_sided_hausdorff, _diag
    import numpy as np
    diag = _diag(orig_v)
    a, _ = _one_sided_hausdorff(orig_v, orig_t, out_v, out_t, diag)
    b, _ = _one_sided_hausdorff(out_v, out_t, orig_v, orig_t, diag)
    vals = [x for x in (a, b) if x is not None]
    return (max(vals), diag) if vals else (None, diag)


# --------------------------------------------------------------------------- #
# worker modes
# --------------------------------------------------------------------------- #
def run_graft(mesh):
    import numpy as np
    from shell_wrap import shell_wrap
    v, f = _load(mesh)
    t0 = time.perf_counter()
    try:
        ov, ot, rep = shell_wrap(v, f)
    except Exception as e:  # noqa: BLE001
        return {"error": "%s: %s" % (type(e).__name__, e),
                "seconds": round(time.perf_counter() - t0, 2)}
    dt = time.perf_counter() - t0
    h, nm = _reload_ok(ov, ot)
    diag = float(np.linalg.norm(v.max(0) - v.min(0)))
    hh = rep.get("hausdorff_healthy")
    return {
        "seconds": round(dt, 2),
        "ok": bool(h == 0 and nm == 0),
        "holes": h, "non_manifold": nm,
        "faces": int(len(ot)),
        "hausdorff_rel": hh,
        "hausdorff_mm": (hh * diag if hh is not None else None),
        "r_used": rep.get("r_used"), "tries": rep.get("tries"),
        "voxel": rep.get("voxel"), "mode": rep.get("mode"),
        "projected_fraction": rep.get("projected_fraction"),
        "detail_max_mm": rep.get("detail_max_mm"),
        "detail_area_moved": rep.get("detail_area_moved"),
        "remeshed": rep.get("remeshed"),
        "si_before": rep.get("si_before"), "si_after": rep.get("si_after"),
        "warnings": [w.get("code") for w in rep.get("warnings", [])],
    }


def run_ftetwild(mesh, out_obj):
    import numpy as np
    from ftetwild_bridge import run_bridge
    v, f = _load(mesh)
    t0 = time.perf_counter()

    class _Timeout(Exception):
        pass

    def _alarm(_s, _f):
        raise _Timeout()

    old = signal.signal(signal.SIGALRM, _alarm)
    signal.alarm(int(FTET_CAP))
    try:
        rep = run_bridge(mesh, out_obj)
    except _Timeout:
        rep = {"error": "timeout"}
    except Exception as e:  # noqa: BLE001
        rep = {"error": "%s: %s" % (type(e).__name__, e)}
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, old)
    dt = time.perf_counter() - t0
    if not rep.get("ok"):
        return {"seconds": round(dt, 2), "ok": False,
                "error": rep.get("error", "not ok")}
    import trimesh
    m = trimesh.load(out_obj, force="mesh")
    ov = np.asarray(m.vertices, np.float64)
    ot = np.asarray(m.faces, np.int64)
    h, nm = _reload_ok(ov, ot)
    hh, diag = _hausdorff(v, f, ov, ot)
    return {"seconds": round(dt, 2), "ok": bool(h == 0 and nm == 0),
            "holes": h, "non_manifold": nm, "faces": int(len(ot)),
            "hausdorff_rel": hh,
            "hausdorff_mm": (hh * diag if hh is not None else None),
            "output_faces": rep.get("output_faces"),
            "tet_cells": rep.get("tet_cells")}


# --------------------------------------------------------------------------- #
# driver
# --------------------------------------------------------------------------- #
def _run_sub(module_flags, mesh, cap, out_obj=None):
    cmd = [sys.executable, os.path.abspath(__file__)] + module_flags + [mesh]
    if out_obj:
        cmd.append(out_obj)
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=cap)
    except subprocess.TimeoutExpired:
        return {"error": "timeout", "seconds": cap}
    line = ""
    for ln in reversed(r.stdout.strip().splitlines()):
        if ln.startswith("{"):
            line = ln
            break
    if not line:
        return {"error": "no output", "stderr": r.stderr[-300:]}
    try:
        return json.loads(line)
    except Exception:  # noqa: BLE001
        return {"error": "bad json", "raw": line[:200]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sample_dir", nargs="?", default=SAMPLES)
    ap.add_argument("--only", default=None)
    ap.add_argument("--graft", action="store_true")
    ap.add_argument("--ftetwild", action="store_true")
    ap.add_argument("mesh_pos", nargs="?")
    ap.add_argument("out_obj", nargs="?")
    args = ap.parse_args()

    # worker modes
    if args.graft:
        mesh = args.mesh_pos or args.sample_dir
        print(json.dumps(run_graft(mesh)))
        return 0
    if args.ftetwild:
        mesh = args.mesh_pos or args.sample_dir
        print(json.dumps(run_ftetwild(mesh, args.out_obj)))
        return 0

    os.makedirs(OUT_DIR, exist_ok=True)
    meshes = sorted(glob.glob(os.path.join(args.sample_dir, "*.stl")))
    if args.only:
        meshes = [m for m in meshes if args.only in os.path.basename(m)]
    rows = []
    for i, mesh in enumerate(meshes, 1):
        name = os.path.basename(mesh)
        print(f"[{i}/{len(meshes)}] {name}", flush=True)
        graft = _run_sub(["--graft"], mesh, GRAFT_CAP)
        print(f"    graft: ok={graft.get('ok')} {graft.get('seconds')}s "
              f"faces={graft.get('faces')}", flush=True)
        out_obj = os.path.join("/tmp/opencode/f3", name + ".ftetwild.obj")
        ftet = _run_sub(["--ftetwild"], mesh, FTET_CAP + 30, out_obj=out_obj)
        print(f"    ftet : ok={ftet.get('ok')} {ftet.get('seconds')}s "
              f"faces={ftet.get('faces')} {ftet.get('error', '')}", flush=True)
        rows.append({"mesh": name, "graft": graft, "ftetwild": ftet})
        _write(rows)
    _write(rows)
    print(f"wrote {OUT_DIR}/bench.md ({len(rows)} meshes)")
    return 0


def _fmt(v, nd=3):
    if v is None:
        return "-"
    if isinstance(v, float):
        return f"{v:.{nd}f}"
    return str(v)


def _write(rows):
    g_ok = sum(1 for r in rows if r["graft"].get("ok"))
    f_ok = sum(1 for r in rows if r["ftetwild"].get("ok"))
    lines = [
        "# F4 benchmark — Graft (#13) vs fTetWild (#7)",
        "",
        "Graft: `shell_wrap(original)` (standalone, no registry).  fTetWild:",
        "`ftetwild_bridge.run_bridge(original)` (the full",
        "`repair.py --methods 7` pipeline needs pymeshlab, absent on this",
        "machine, so the tier itself is compared on the original input).",
        "One mesh at a time; caps: Graft 600 s, fTetWild 240 s.",
        "",
        f"Meshes: {len(rows)}  |  Graft reload-watertight: {g_ok}/{len(rows)}"
        f"  |  fTetWild reload-watertight: {f_ok}/{len(rows)}",
        "",
        "| mesh | Graft ok | Graft s | Graft faces | Graft Hausdorff (rel) | "
        "Graft detail mm | fTetW ok | fTetW s | fTetW faces | fTetW Hausdorff (rel) |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for r in rows:
        g = r["graft"]
        ft = r["ftetwild"]
        lines.append(
            f"| {r['mesh']} | {g.get('ok')} | {_fmt(g.get('seconds'), 2)} | "
            f"{_fmt(g.get('faces'))} | {_fmt(g.get('hausdorff_rel'), 4)} | "
            f"{_fmt(g.get('detail_max_mm'))} | {ft.get('ok')} | "
            f"{_fmt(ft.get('seconds'), 2)} | {_fmt(ft.get('faces'))} | "
            f"{_fmt(ft.get('hausdorff_rel'), 4)} |")
    # summary
    g_times = [r["graft"].get("seconds") for r in rows
               if r["graft"].get("seconds") is not None]
    f_times = [r["ftetwild"].get("seconds") for r in rows
               if r["ftetwild"].get("seconds") is not None]
    if g_times:
        lines += ["", f"Graft total/median time: {sum(g_times):.1f} s / "
                  f"{sorted(g_times)[len(g_times)//2]:.1f} s"]
    if f_times:
        lines += [f"fTetWild total/median time: {sum(f_times):.1f} s / "
                  f"{sorted(f_times)[len(f_times)//2]:.1f} s"]
    path = os.path.join(OUT_DIR, "bench.md")
    with open(path, "w") as fh:
        fh.write("\n".join(lines) + "\n")
    with open(os.path.join(OUT_DIR, "bench.json"), "w") as fh:
        json.dump(rows, fh, indent=2)


if __name__ == "__main__":
    sys.exit(main())
