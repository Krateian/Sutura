#!/usr/bin/env python3
"""F4 benchmark: Graft (#13 shell wrap) vs fTetWild (#7) on the real-world
samples whose default repair is NOT already reload-watertight.

Only meshes that need a tier are judged (a clean mesh is not a repair test).
One mesh at a time, each stage in its own subprocess with a wall-clock cap:
Graft 600 s, fTetWild (`repair.py --methods 7`) 600 s.

Driver writes ``/tmp/opencode/f3/bench.md`` and ``bench.json``.

    <venv>/bin/python tests/bench_shell_wrap.py [sample_dir] [--only NAME]
"""
import argparse
import glob
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SUTURA = os.path.join(ROOT, "sutura")
sys.path.insert(0, SUTURA)
SAMPLES = os.path.join(ROOT, "tests", "real-world-samples")
OUT_DIR = "/tmp/opencode/f3"
GRAFT_CAP = 600.0
FTET_CAP = 600.0


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


def _hausdorff_healthy(v0, t0, out_v, out_t):
    """One-sided max on the healthy region (excluding the damaged band)."""
    import numpy as np
    from repair import _damaged_region, _referenced_only
    from shell_wrap import (_behind_on_healthy, _diag, _median_edge_length,
                            _one_sided_hausdorff)
    diag = _diag(v0)
    mask, _ = _damaged_region(t0)
    hf = ~mask if mask is not None else np.ones(len(t0), dtype=bool)
    if not hf.any():
        hf = np.ones(len(t0), dtype=bool)
    hv, ht = _referenced_only(v0, t0[hf])
    r = max(2.0 * _median_edge_length(v0, t0), 0.005 * diag)
    rng = np.random.default_rng(7)
    ahead, _ = _one_sided_hausdorff(hv, ht, out_v, out_t, diag, rng=rng)
    behind = _behind_on_healthy(v0, t0, mask, out_v, out_t, diag, r, rng)
    vals = [x for x in (ahead, behind) if x is not None]
    return (max(vals), diag) if vals else (None, diag)


# --------------------------------------------------------------------------- #
# worker modes
# --------------------------------------------------------------------------- #
def run_graft(mesh):
    import numpy as np
    import pymeshlab
    from shell_wrap import shell_wrap
    v, f = _load(mesh)
    t0 = time.perf_counter()
    try:
        ov, ot, rep = shell_wrap(v, f, ml=pymeshlab)
    except Exception as e:  # noqa: BLE001
        return {"error": "%s: %s" % (type(e).__name__, e),
                "seconds": round(time.perf_counter() - t0, 2)}
    dt = time.perf_counter() - t0
    h, nm = _reload_ok(ov, ot)
    hh, diag = _hausdorff_healthy(v, f, ov, ot)
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
        "fidelity_ok": rep.get("fidelity_ok"),
        "remeshed": rep.get("remeshed"),
        "si_before": rep.get("si_before"), "si_after": rep.get("si_after"),
        "warnings": [w.get("code") for w in rep.get("warnings", [])],
    }


def run_ftetwild(mesh, out_obj):
    """Run the real method #7 through `repair.py --methods 7`."""
    import numpy as np
    import trimesh
    v, f = _load(mesh)
    from shell_wrap import _diag
    cmd = [sys.executable, os.path.join(SUTURA, "repair.py"), mesh,
           "--methods", "7", "--no-history", "-o", out_obj]
    t0 = time.perf_counter()
    try:
        r = subprocess.run(cmd, capture_output=True, text=True,
                           timeout=FTET_CAP, cwd=ROOT)
    except subprocess.TimeoutExpired:
        return {"seconds": round(time.perf_counter() - t0, 2), "ok": False,
                "error": "timeout"}
    dt = time.perf_counter() - t0
    rep = None
    for ln in reversed(r.stdout.strip().splitlines()):
        ln = ln.strip()
        if ln.startswith("{"):
            try:
                rep = json.loads(ln)
            except Exception:  # noqa: BLE001
                rep = None
            break
    if rep is None or not os.path.exists(out_obj):
        return {"seconds": round(dt, 2), "ok": False,
                "error": (rep or {}).get("error", "no output")}
    m = trimesh.load(out_obj, force="mesh")
    ov = np.asarray(m.vertices, np.float64)
    ot = np.asarray(m.faces, np.int64)
    h, nm = _reload_ok(ov, ot)
    hh, diag = _hausdorff_healthy(v, f, ov, ot)
    ft = rep.get("experimental_ftetwild") or {}
    return {
        "seconds": round(dt, 2), "ok": bool(h == 0 and nm == 0),
        "holes": h, "non_manifold": nm, "faces": int(len(ot)),
        "category": rep.get("category"),
        "husdorff_report": rep.get("hausdorff_rel"),
        "hausdorff_rel": hh,
        "hausdorff_mm": (hh * diag if hh is not None else None),
        "ftetwild_ran": ft.get("ran"),
        "ftetwild_time": ft.get("wall_time"),
        "adopted": ft.get("adopted"),
    }


# --------------------------------------------------------------------------- #
# driver
# --------------------------------------------------------------------------- #
def _run_sub(flags, mesh, cap, out_obj=None):
    cmd = [sys.executable, os.path.abspath(__file__)] + flags + [mesh]
    if out_obj:
        cmd.append(out_obj)
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=cap,
                           cwd=ROOT)
    except subprocess.TimeoutExpired:
        return {"error": "timeout", "seconds": cap}
    for ln in reversed(r.stdout.strip().splitlines()):
        if ln.startswith("{"):
            try:
                return json.loads(ln)
            except Exception:  # noqa: BLE001
                return {"error": "bad json", "raw": ln[:200]}
    return {"error": "no output", "stderr": r.stderr[-300:]}


def _needs_repair(mesh):
    """True when the default repair is NOT already reload-watertight."""
    v, f = _load(mesh)
    h, nm = _reload_ok(v, f)
    return (h + nm) > 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sample_dir", nargs="?", default=SAMPLES)
    ap.add_argument("--only", default=None)
    ap.add_argument("--graft", action="store_true")
    ap.add_argument("--ftetwild", action="store_true")
    ap.add_argument("mesh_pos", nargs="?")
    args = ap.parse_args()

    if args.graft:
        print(json.dumps(run_graft(args.sample_dir)))
        return 0
    if args.ftetwild:
        print(json.dumps(run_ftetwild(args.sample_dir, args.mesh_pos)))
        return 0

    os.makedirs(OUT_DIR, exist_ok=True)
    meshes = sorted(glob.glob(os.path.join(args.sample_dir, "*.stl")))
    if args.only:
        meshes = [m for m in meshes if args.only in os.path.basename(m)]
    rows = []
    for i, mesh in enumerate(meshes, 1):
        name = os.path.basename(mesh)
        if not _needs_repair(mesh):
            print(f"[{i}/{len(meshes)}] {name}: default repair already "
                  f"reload-watertight -> skipped", flush=True)
            continue
        print(f"[{i}/{len(meshes)}] {name}", flush=True)
        graft = _run_sub(["--graft"], mesh, GRAFT_CAP)
        print(f"    graft: ok={graft.get('ok')} {graft.get('seconds')}s "
              f"faces={graft.get('faces')} mode={graft.get('mode')}", flush=True)
        out_obj = os.path.join(OUT_DIR, name + ".ftetwild.stl")
        ftet = _run_sub(["--ftetwild"], mesh, FTET_CAP + 30, out_obj=out_obj)
        print(f"    ftet : ok={ftet.get('ok')} {ftet.get('seconds')}s "
              f"faces={ftet.get('faces')} {ftet.get('error', '')}", flush=True)
        rows.append({"mesh": name, "graft": graft, "ftetwild": ftet})
        _write(rows)
    _write(rows)
    print(f"wrote {OUT_DIR}/bench.md ({len(rows)} damaged meshes)")
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
        "Only meshes whose default repair is NOT already reload-watertight are",
        "included.  Graft: `shell_wrap(original, ml=pymeshlab)`.  fTetWild: the",
        "real `repair.py --methods 7` pipeline.  One mesh at a time; 600 s cap",
        "per stage.  Hausdorff = one-sided max on the healthy region, relative",
        "to the bbox diagonal.",
        "",
        f"Meshes: {len(rows)}  |  Graft reload-watertight: {g_ok}/{len(rows)}"
        f"  |  fTetWild reload-watertight: {f_ok}/{len(rows)}",
        "",
        "| mesh | Graft ok | Graft s | Graft faces | Graft mode | Graft Hausdorff (rel) | "
        "Graft detail mm | fTetW ok | fTetW s | fTetW faces | fTetW Hausdorff (rel) |",
        "|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for r in rows:
        g = r["graft"]
        ft = r["ftetwild"]
        lines.append(
            f"| {r['mesh']} | {g.get('ok')} | {_fmt(g.get('seconds'), 2)} | "
            f"{_fmt(g.get('faces'))} | {g.get('mode')} | "
            f"{_fmt(g.get('hausdorff_rel'), 5)} | {_fmt(g.get('detail_max_mm'))} | "
            f"{ft.get('ok')} | {_fmt(ft.get('seconds'), 2)} | "
            f"{_fmt(ft.get('faces'))} | {_fmt(ft.get('hausdorff_rel'), 5)} |")
    g_times = [r["graft"].get("seconds") for r in rows
               if isinstance(r["graft"].get("seconds"), (int, float))]
    f_times = [r["ftetwild"].get("seconds") for r in rows
               if isinstance(r["ftetwild"].get("seconds"), (int, float))]
    if g_times:
        lines += ["", f"Graft total/median time: {sum(g_times):.1f} s / "
                  f"{sorted(g_times)[len(g_times)//2]:.1f} s"]
    if f_times:
        lines += [f"fTetWild total/median time: {sum(f_times):.1f} s / "
                  f"{sorted(f_times)[len(f_times)//2]:.1f} s"]
    with open(os.path.join(OUT_DIR, "bench.md"), "w") as fh:
        fh.write("\n".join(lines) + "\n")
    with open(os.path.join(OUT_DIR, "bench.json"), "w") as fh:
        json.dump(rows, fh, indent=2)


if __name__ == "__main__":
    sys.exit(main())
