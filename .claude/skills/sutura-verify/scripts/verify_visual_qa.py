#!/usr/bin/env python3
"""verify_visual_qa.py - Independent visual quality assurance for mesh repairs.

Outside-measurement verification tool: renders meshes across a 12-camera spherical
rig (CPU-only, headless) to detect defects that 3D scalar distances (Hausdorff /
volume) miss:
  * Topologic genus loss & plugged through-holes (MISSING_FOREGROUND).
  * Floating debris, disconnected shells & swelling (EXCESS_FOREGROUND).
  * Voxel stepping, staircasing & surface wrinkling (VOXEL_STEPPING).
  * Surface deformation & denting (SURFACE_DISTORTION).
  * Zoomed detail render (512 px) on defect/changed region for boundary/flagged cases.
  * Dual scoring on real broken models: unmasked (full) vs masked (healthy skin).

Dual use-cases:
  1. Bench / Oracle: 0-100 visual quality score per arm/engine.
  2. T2 Verification / Tuning: Three-way comparison (Input vs Baseline vs Candidate)
     with automated VISUAL-WORSE gating and contact sheet generation.

Zero runtime dependency: dev-only tool under .claude/skills/sutura-verify/.
Requires numpy and PySide6 (no scipy, no Pillow); the V2 X-ray channel uses
the mandatory `sutura_geom` extension (`raycast_points`) and degrades that
channel only when an older extension lacks it.
Never hardcodes private sample names or paths.

V2 additions (all V1 scores, flags and images are unchanged): PCA views with
tight framing, 1024 px silhouette IoU, X-ray ray parity/layers, deterministic
Z-slices, dihedral sharp-edge ratio, true 3D surface deviation and top-3
cluster zoom renders.  V2 flags are relative to the reference: an identical
pair must produce no flags.
"""
import argparse
import json
import math
import os
import struct
import sys
import time

import numpy as np

try:
    import sutura_geom
    HAS_SUTURA_GEOM = True
except ImportError:  # pragma: no cover - optional for the V2 metrics only
    sutura_geom = None
    HAS_SUTURA_GEOM = False

# Ensure offscreen Qt platform before any QtGui import
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

try:
    from PySide6.QtCore import QPoint, QRect, Qt
    from PySide6.QtGui import (QColor, QFont, QGuiApplication, QImage, QPainter,
                               QPen, QPolygon)
    HAS_PYSIDE6 = True
except ImportError:
    HAS_PYSIDE6 = False

_QAPP = None


def ensure_qapp():
    """Ensure a QGuiApplication instance exists on the main thread for offscreen text rendering."""
    global _QAPP
    if not HAS_PYSIDE6:
        return None
    if _QAPP is None:
        _QAPP = QGuiApplication.instance()
        if _QAPP is None:
            _QAPP = QGuiApplication(sys.argv[:1])
    return _QAPP


if HAS_PYSIDE6:
    ensure_qapp()


def np_gaussian_filter2d(img, sigma=1.5):
    """Separable 2D Gaussian filter matching scipy.ndimage.gaussian_filter with mode='reflect'."""
    radius = int(4.0 * float(sigma) + 0.5)
    x = np.arange(-radius, radius + 1)
    k = np.exp(-0.5 * (x / sigma) ** 2)
    k /= k.sum()

    H, W = img.shape
    # Axis 1 (columns) with symmetric padding (scipy reflect around center of edge pixel)
    p1 = np.pad(img, ((0, 0), (radius, radius)), mode='symmetric')
    c1 = sum(p1[:, i:i + W] * k[i] for i in range(len(k)))

    # Axis 0 (rows)
    p0 = np.pad(c1, ((radius, radius), (0, 0)), mode='symmetric')
    c0 = sum(p0[i:i + H, :] * k[i] for i in range(len(k)))
    return c0


def np_binary_dilation2d(mask, k):
    """Separable 2D binary dilation matching scipy.ndimage.binary_dilation with square structure."""
    rad = k // 2
    H, W = mask.shape
    p1 = np.pad(mask, ((0, 0), (rad, rad)), mode='constant', constant_values=False)
    d1 = np.zeros((H, W), dtype=bool)
    for i in range(k):
        d1 |= p1[:, i:i + W]
    p0 = np.pad(d1, ((rad, rad), (0, 0)), mode='constant', constant_values=False)
    d0 = np.zeros((H, W), dtype=bool)
    for i in range(k):
        d0 |= p0[i:i + H, :]
    return d0


PRIVATE_HINT = ("private samples: pass the path on the command line only; "
                "never add their name to a tracked file")


# --------------------------------------------------------------------------- #
# Mesh Loading & Defect Detection (Fast binary/ASCII STL parser, pure numpy)
# --------------------------------------------------------------------------- #
def read_stl_raw(path):
    """Return raw triangles as an (M, 3, 3) float32 array (no vertex weld)."""
    size = os.path.getsize(path)
    with open(path, 'rb') as fh:
        head = fh.read(84)
    if size >= 84:
        n = struct.unpack('<I', head[80:84])[0]
        if 84 + 50 * n == size:
            dt = np.dtype([('n', '<f4', 3), ('v', '<f4', (3, 3)), ('a', '<u2')])
            arr = np.fromfile(path, dtype=dt, count=n, offset=84)
            return np.ascontiguousarray(arr['v'].astype(np.float32))
    # ASCII fallback
    verts = []
    tris = []
    with open(path, 'r', errors='replace') as fh:
        for line in fh:
            line = line.strip()
            if line.startswith('vertex'):
                verts.append([float(x) for x in line.split()[1:4]])
            elif line.startswith('endfacet'):
                if len(verts) >= 3:
                    tris.append(verts[-3:])
    return np.asarray(tris, dtype=np.float32)


def weld_exact(tris_raw):
    """Reload-equivalent weld: cast to float32, merge exact coincident positions."""
    flat = np.asarray(tris_raw, dtype=np.float32).reshape(-1, 3)
    if len(flat) == 0:
        return np.zeros((0, 3), np.float64), np.zeros((0, 3), np.int64)
    verts, inv = np.unique(flat, axis=0, return_inverse=True)
    tris = inv.reshape(-1, 3).astype(np.int64)
    return verts.astype(np.float64), tris


def detect_defect_vertices(tris):
    """Identify boundary and non-manifold edge vertices from face indices."""
    if len(tris) == 0:
        return np.array([], dtype=np.int64)
    e = np.sort(np.concatenate([tris[:, [0, 1]], tris[:, [1, 2]], tris[:, [2, 0]]], axis=0), axis=1)
    uniq_e, counts = np.unique(e, axis=0, return_counts=True)
    b_edges = uniq_e[counts == 1]
    nm_edges = uniq_e[counts > 2]
    if len(b_edges) + len(nm_edges) == 0:
        return np.array([], dtype=np.int64)
    return np.unique(np.concatenate([b_edges, nm_edges])).astype(np.int64)


def load_mesh(path):
    """Load mesh and return (verts, tris, normals, defect_vidx). Supports STL/OBJ/3MF."""
    ext = os.path.splitext(path)[1].lower()
    if ext == '.stl':
        raw = read_stl_raw(path)
        verts, tris = weld_exact(raw)
    else:
        try:
            import trimesh
            m = trimesh.load(path, force='mesh')
            verts = np.asarray(m.vertices, dtype=np.float64)
            tris = np.asarray(m.faces, dtype=np.int64)
        except Exception as e:
            raise RuntimeError(f"Failed to load mesh {path} without trimesh: {e}")

    if len(tris) == 0:
        return verts, tris, np.zeros((0, 3), dtype=np.float64), np.array([], dtype=np.int64)

    # Face normals
    v0 = verts[tris[:, 0]]
    v1 = verts[tris[:, 1]]
    v2 = verts[tris[:, 2]]
    n = np.cross(v1 - v0, v2 - v0)
    norm = np.linalg.norm(n, axis=1, keepdims=True)
    norm[norm == 0] = 1.0
    normals = n / norm

    defect_vidx = detect_defect_vertices(tris)
    return verts, tris, normals, defect_vidx


# --------------------------------------------------------------------------- #
# Camera Rigs
# --------------------------------------------------------------------------- #
def get_canonical_cameras():
    """Three standard view angles: Isometric, Front, Top."""
    iso = np.array([
        [ 0.70710678,  0.0,       -0.70710678],
        [-0.40824829,  0.81649658, -0.40824829],
        [ 0.57735027,  0.57735027,  0.57735027]
    ], dtype=np.float64)
    front = np.array([
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0]
    ], dtype=np.float64)
    top = np.array([
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0,-1.0, 0.0]
    ], dtype=np.float64)
    return [("Isometric", iso), ("Front", front), ("Top", top)]


def get_dodecahedron_cameras():
    """12 cameras uniformly distributed across the sphere."""
    phi = (1.0 + math.sqrt(5.0)) / 2.0
    dirs = []
    for s1 in [-1, 1]:
        for s2 in [-1, 1]:
            dirs.append(np.array([0, s1, s2 * phi], dtype=np.float64))
            dirs.append(np.array([s1, s2 * phi, 0], dtype=np.float64))
            dirs.append(np.array([s2 * phi, 0, s1], dtype=np.float64))
    rots = []
    up_ref = np.array([0.0, 1.0, 0.0], dtype=np.float64)
    for i, d in enumerate(dirs):
        fwd = d / np.linalg.norm(d)
        up = up_ref - np.dot(up_ref, fwd) * fwd
        if np.linalg.norm(up) < 1e-4:
            up = np.array([1.0, 0.0, 0.0]) - np.dot(np.array([1.0, 0.0, 0.0]), fwd) * fwd
        up = up / np.linalg.norm(up)
        right = np.cross(up, fwd)
        right = right / np.linalg.norm(right)
        R = np.stack([right, up, fwd], axis=0)
        rots.append((f"Cam_{i:02d}", R))
    return rots


def compute_shared_frame(verts_list, w=256, h=256, pad=16):
    """Compute shared bounding sphere frame so all meshes align 1:1."""
    all_v = np.concatenate(verts_list, axis=0)
    lo = all_v.min(axis=0)
    hi = all_v.max(axis=0)
    center = (lo + hi) * 0.5
    extent = (hi - lo).max()
    scale = (min(w, h) - 2 * pad) / (extent if extent > 1e-6 else 1.0)
    return center, scale, extent


# --------------------------------------------------------------------------- #
# Headless CPU Rasterizer (Rust z-buffer, QPainter fallback)
# --------------------------------------------------------------------------- #
def _fast_raster():
    if os.environ.get("SUTURA_VQA_QT", "0") not in ("0", "", "false", "False"):
        return False
    return HAS_SUTURA_GEOM and hasattr(sutura_geom, "rasterize")


def _render_view_qt(px, py, tris, norm_colors, depth_vals, order, w, h,
                    need_depth=True, wireframe=False):
    """Legacy QPainter painter's-algorithm rasteriser (fallback / wireframe)."""
    img_norm = QImage(w, h, QImage.Format_RGB32)
    img_norm.fill(0)
    p_norm = QPainter(img_norm)
    if wireframe:
        pen = QPen(QColor(255, 255, 255, 70))
        pen.setWidthF(0.6)
        p_norm.setPen(pen)
    else:
        p_norm.setPen(Qt.NoPen)

    img_depth = None
    p_depth = None
    if need_depth:
        img_depth = QImage(w, h, QImage.Format_RGB32)
        img_depth.fill(0)
        p_depth = QPainter(img_depth)
        p_depth.setPen(Qt.NoPen)

    for idx in order:
        t = tris[idx]
        poly = QPolygon([
            QPoint(int(px[t[0]]), int(py[t[0]])),
            QPoint(int(px[t[1]]), int(py[t[1]])),
            QPoint(int(px[t[2]]), int(py[t[2]])),
        ])
        nc = norm_colors[idx]
        p_norm.setBrush(QColor(int(nc[0]), int(nc[1]), int(nc[2])))
        p_norm.drawPolygon(poly)

        if p_depth is not None:
            dc = depth_vals[idx]
            p_depth.setBrush(QColor(int(dc), int(dc), int(dc)))
            p_depth.drawPolygon(poly)

    p_norm.end()
    if p_depth is not None:
        p_depth.end()

    raw_norm = np.frombuffer(img_norm.constBits(), dtype=np.uint8).reshape((h, w, 4))
    rgb_norm = raw_norm[:, :, [2, 1, 0]].copy()

    if img_depth is not None:
        raw_depth = np.frombuffer(img_depth.constBits(), dtype=np.uint8).reshape((h, w, 4))
        val_depth = raw_depth[:, :, 0].copy().astype(np.float32) / 255.0
    else:
        val_depth = np.zeros((h, w), dtype=np.float32)

    silhouette = (rgb_norm[:, :, 0] > 0) | (rgb_norm[:, :, 1] > 0) | (rgb_norm[:, :, 2] > 0)
    return rgb_norm, val_depth, silhouette


def render_view(verts, tris, normals, rot, center, scale, w=256, h=256, defect_vidx=None,
                view_center=(0.0, 0.0), face_colors=None, need_depth=True,
                wireframe=False):
    """Render normal map, depth map, silhouette and 2D defect mask.

    ``view_center`` shifts the projected view-space origin (used by the V2
    tight per-view framing).  ``face_colors`` replaces the normal shading with
    one RGB colour per face and skips the depth pass.  The defaults reproduce
    the V1 output byte-for-byte.
    """
    if not HAS_PYSIDE6:
        raise RuntimeError("PySide6 is required for headless CPU rasterization")

    if len(tris) == 0:
        return (np.zeros((h, w, 3), dtype=np.uint8),
                np.zeros((h, w), dtype=np.float32),
                np.zeros((h, w), dtype=bool),
                np.zeros((h, w), dtype=bool))

    # Transform vertices to view space
    v_cam = (verts - center) @ rot.T
    px = w * 0.5 + (v_cam[:, 0] - float(view_center[0])) * scale
    py = h * 0.5 - (v_cam[:, 1] - float(view_center[1])) * scale
    z = v_cam[:, 2]

    # Depth sort
    z_face = z[tris].mean(axis=1)
    order = np.argsort(z_face)

    # Normals to view space
    n_cam = normals @ rot.T
    norm_colors = np.clip((n_cam * 0.5 + 0.5) * 255.0, 0, 255).astype(np.uint8)
    if face_colors is not None:
        norm_colors = np.ascontiguousarray(face_colors, dtype=np.uint8)

    # Normalize depth
    z_min = float(z_face.min())
    z_max = float(z_face.max())
    z_span = (z_max - z_min) if (z_max - z_min) > 1e-6 else 1.0
    depth_vals = np.clip((z_face - z_min) / z_span * 255.0, 0, 255).astype(np.uint8)

    if _fast_raster() and not wireframe:
        rgb, dep, sil = sutura_geom.rasterize(
            np.asarray(verts, dtype=np.float64),
            np.asarray(tris, dtype=np.int32),
            np.asarray(rot, dtype=np.float64),
            (float(center[0]), float(center[1]), float(center[2])),
            (float(view_center[0]), float(view_center[1])),
            float(scale), int(w), int(h),
            colors=norm_colors, depth_colors=depth_vals,
            silhouette_only=False, order=np.asarray(order, dtype=np.int32))
        rgb_norm = np.asarray(rgb)
        val_depth = np.asarray(dep, dtype=np.float32) / 255.0
        silhouette = np.asarray(sil, dtype=bool)
    else:
        rgb_norm, val_depth, silhouette = _render_view_qt(
            px, py, tris, norm_colors, depth_vals, order, w, h,
            need_depth=need_depth and face_colors is None, wireframe=wireframe)

    # 2D Defect Mask Projection (for masking out open holes / boundaries on broken input)
    defect_mask_2d = np.zeros((h, w), dtype=bool)
    if defect_vidx is not None and len(defect_vidx) > 0:
        dv_px = np.clip(np.round(px[defect_vidx]).astype(int), 0, w - 1)
        dv_py = np.clip(np.round(py[defect_vidx]).astype(int), 0, h - 1)
        defect_mask_2d[dv_py, dv_px] = True
        k = max(7, int(w * 0.04))
        if k % 2 == 0:
            k += 1
        defect_mask_2d = np_binary_dilation2d(defect_mask_2d, k)

    return rgb_norm, val_depth, silhouette, defect_mask_2d


# --------------------------------------------------------------------------- #
# Image Metrics: Asymmetric Silhouette, Normal SSIM, Angular Error, Depth MAE
# --------------------------------------------------------------------------- #
def _ssim_maps(im1, im2):
    """Per-channel SSIM maps (shared by the full and healthy-mask scores)."""
    im1 = im1.astype(np.float64)
    im2 = im2.astype(np.float64)
    C1 = (0.01 * 255.0) ** 2
    C2 = (0.03 * 255.0) ** 2
    maps = []
    for ch in range(im1.shape[2]):
        c1 = im1[:, :, ch]
        c2 = im2[:, :, ch]
        mu1 = np_gaussian_filter2d(c1, 1.5)
        mu2 = np_gaussian_filter2d(c2, 1.5)
        sigma1_sq = np_gaussian_filter2d(c1 * c1, 1.5) - (mu1 * mu1)
        sigma2_sq = np_gaussian_filter2d(c2 * c2, 1.5) - (mu2 * mu2)
        sigma12 = np_gaussian_filter2d(c1 * c2, 1.5) - (mu1 * mu2)
        num = (2.0 * mu1 * mu2 + C1) * (2.0 * sigma12 + C2)
        den = (mu1 * mu1 + mu2 * mu2 + C1) * (sigma1_sq + sigma2_sq + C2)
        maps.append(num / den)
    return maps


def _ssim_from_maps(maps, mask=None):
    vals = []
    for m in maps:
        if mask is not None and mask.sum() > 0:
            vals.append(float(m[mask].mean()))
        else:
            vals.append(float(m.mean()))
    return float(np.mean(vals))


def compute_ssim(im1, im2, mask=None):
    """Compute channel-wise SSIM with Gaussian filtering."""
    return _ssim_from_maps(_ssim_maps(im1, im2), mask)


def compare_views(norm_ref, depth_ref, sil_ref, norm_cand, depth_cand, sil_cand, defect_mask_ref=None):
    """Compare a camera view between reference and candidate (full and healthy-masked)."""
    ref_cnt = float(sil_ref.sum())
    cand_cnt = float(sil_cand.sum())
    inter_mask = np.logical_and(sil_ref, sil_cand)
    union_mask = np.logical_or(sil_ref, sil_cand)

    # Asymmetric Silhouette Ratios
    missing_cnt = float(np.logical_and(sil_ref, ~sil_cand).sum())
    p_missing = missing_cnt / max(1.0, ref_cnt)

    excess_cnt = float(np.logical_and(~sil_ref, sil_cand).sum())
    p_excess = excess_cnt / max(1.0, ref_cnt)

    iou = float(inter_mask.sum() / max(1.0, union_mask.sum()))

    # Non-linear silhouette fidelity: floating debris penalized with 5.0x
    sil_score = max(0.0, 1.0 - 2.5 * p_missing - 5.0 * p_excess)

    # Normal & Depth metrics on full intersection
    if inter_mask.sum() > 0:
        n_ref = (norm_ref.astype(np.float32) / 255.0) * 2.0 - 1.0
        n_cand = (norm_cand.astype(np.float32) / 255.0) * 2.0 - 1.0

        l_ref = np.linalg.norm(n_ref, axis=2, keepdims=True)
        l_ref[l_ref == 0] = 1.0
        n_ref /= l_ref

        l_cand = np.linalg.norm(n_cand, axis=2, keepdims=True)
        l_cand[l_cand == 0] = 1.0
        n_cand /= l_cand

        dot = np.clip(np.sum(n_ref * n_cand, axis=2), -1.0, 1.0)
        angle_deg = np.arccos(dot) * (180.0 / np.pi)
        mean_angle_err = float(angle_deg[inter_mask].mean())
        ssim_maps = _ssim_maps(norm_ref, norm_cand)
        ssim = _ssim_from_maps(ssim_maps, inter_mask)
        depth_mae = float(np.abs(depth_ref[inter_mask] - depth_cand[inter_mask]).mean())
    else:
        mean_angle_err = 180.0
        ssim = 0.0
        depth_mae = 1.0

    depth_term = max(0.0, min(1.0, 1.0 - 5.0 * depth_mae))
    view_score_full = 100.0 * (0.45 * sil_score + 0.35 * max(0.0, ssim) + 0.20 * depth_term)

    # Masked Healthy-Skin Comparison (ignoring input defect/hole boundaries)
    if defect_mask_ref is not None and defect_mask_ref.sum() > 0:
        healthy_inter = inter_mask & (~defect_mask_ref)
        if healthy_inter.sum() > 0:
            ssim_h = _ssim_from_maps(ssim_maps, healthy_inter)
            ang_h = float(angle_deg[healthy_inter].mean())
            depth_h = float(np.abs(depth_ref[healthy_inter] - depth_cand[healthy_inter]).mean())
            d_term_h = max(0.0, min(1.0, 1.0 - 5.0 * depth_h))

            # Healthy silhouette ratios
            ref_h_cnt = float((sil_ref & (~defect_mask_ref)).sum())
            p_mis_h = float(((sil_ref & (~defect_mask_ref)) & (~sil_cand)).sum()) / max(1.0, ref_h_cnt)
            p_exc_h = float(((~sil_ref) & (sil_cand & (~defect_mask_ref))).sum()) / max(1.0, ref_h_cnt)
            sil_score_h = max(0.0, 1.0 - 2.5 * p_mis_h - 5.0 * p_exc_h)

            view_score_healthy = 100.0 * (0.45 * sil_score_h + 0.35 * max(0.0, ssim_h) + 0.20 * d_term_h)
        else:
            view_score_healthy = view_score_full
    else:
        view_score_healthy = view_score_full

    return {
        "iou": iou,
        "p_missing": p_missing,
        "p_excess": p_excess,
        "sil_score": sil_score,
        "normal_ssim": ssim,
        "angle_err_deg": mean_angle_err,
        "depth_mae": depth_mae,
        "view_score": view_score_full,
        "view_score_healthy": view_score_healthy
    }


def create_diff_map(norm_ref, sil_ref, norm_cand, sil_cand):
    """Difference visualizer: Red=missing, Cyan=excess/debris, Green->Yellow->Magenta=angular error."""
    h, w, _ = norm_ref.shape
    diff = np.zeros((h, w, 3), dtype=np.uint8)

    ref_only = sil_ref & (~sil_cand)
    cand_only = (~sil_ref) & sil_cand
    inter = sil_ref & sil_cand

    diff[ref_only] = [230, 40, 40]   # Red for missing foreground
    diff[cand_only] = [30, 180, 240] # Cyan for excess foreground

    if inter.sum() > 0:
        n_ref = (norm_ref.astype(np.float32) / 255.0) * 2.0 - 1.0
        n_cand = (norm_cand.astype(np.float32) / 255.0) * 2.0 - 1.0
        l_ref = np.linalg.norm(n_ref, axis=2, keepdims=True)
        l_ref[l_ref == 0] = 1.0
        n_ref /= l_ref
        l_cand = np.linalg.norm(n_cand, axis=2, keepdims=True)
        l_cand[l_cand == 0] = 1.0
        n_cand /= l_cand

        dot = np.clip(np.sum(n_ref * n_cand, axis=2), -1.0, 1.0)
        ang = np.arccos(dot) * (180.0 / np.pi)

        t = np.clip(ang / 60.0, 0.0, 1.0)
        r = np.where(t < 0.5, 40 + (240 - 40) * (t / 0.5), 240 + (220 - 240) * ((t - 0.5) / 0.5))
        g = np.where(t < 0.5, 200 + (210 - 200) * (t / 0.5), 210 + (30 - 210) * ((t - 0.5) / 0.5))
        b = np.where(t < 0.5, 80 + (40 - 80) * (t / 0.5), 40 + (120 - 40) * ((t - 0.5) / 0.5))

        diff[inter, 0] = r[inter].astype(np.uint8)
        diff[inter, 1] = g[inter].astype(np.uint8)
        diff[inter, 2] = b[inter].astype(np.uint8)

    return diff


# --------------------------------------------------------------------------- #
# Zoomed Detail Render (512 px) on Defect / Changed Region
# --------------------------------------------------------------------------- #
def find_most_changed_region(v_ref, v_cand, custom_center=None):
    """Find focal center and radius of the region with maximum deviation."""
    extent = (v_ref.max(axis=0) - v_ref.min(axis=0)).max() if len(v_ref) > 0 else 1.0
    if custom_center is not None:
        return np.asarray(custom_center, dtype=np.float64), max(extent * 0.15, 2.0)

    if len(v_ref) == 0 or len(v_cand) == 0:
        c = (v_ref.min(axis=0) + v_ref.max(axis=0)) * 0.5 if len(v_ref) > 0 else np.zeros(3)
        return c, max(extent * 0.15, 2.0)

    # Subsample candidate side only (fixed stride, <= 1000 pts)
    stride = max(1, len(v_cand) // 1000)
    sample_cand = np.asarray(v_cand[::stride], dtype=np.float64)
    v_ref_arr = np.asarray(v_ref, dtype=np.float64)

    # Exact nearest-vertex distance against the full reference.  The KdTree
    # path is the same metric (nearest reference vertex), not a surface query,
    # so the V1 zoom centre/radius are unchanged; it replaces the O(N*M) chunk
    # scan that dominated the detail pass on 90k-face meshes.
    dists = None
    if HAS_SUTURA_GEOM and hasattr(sutura_geom, "KdTree"):
        try:
            tree = sutura_geom.KdTree(v_ref_arr)
            d, _ = tree.query(sample_cand, k=1)
            dists = np.asarray(d, dtype=np.float64)
        except Exception:  # noqa: BLE001
            dists = None
    if dists is None:
        cand_sq = np.sum(sample_cand ** 2, axis=1)
        min_sq = np.full(len(sample_cand), np.inf, dtype=np.float64)
        chunk_size = 8000
        for i in range(0, len(v_ref_arr), chunk_size):
            chunk = v_ref_arr[i:i + chunk_size]
            ref_sq = np.sum(chunk ** 2, axis=1)
            dot = sample_cand @ chunk.T
            d_sq = cand_sq[:, None] - 2.0 * dot + ref_sq[None, :]
            min_sq = np.minimum(min_sq, np.min(d_sq, axis=1))
        dists = np.sqrt(np.maximum(min_sq, 0.0))
    worst_idx = np.argmax(dists)
    max_dev = float(dists[worst_idx])
    worst_pt = sample_cand[worst_idx]

    focal_radius = max(extent * 0.15, max_dev * 2.5)
    return worst_pt, focal_radius


def render_zoomed_detail(v_ref, t_ref, n_ref, v_cand, t_cand, n_cand,
                         focal_center, focal_radius, res=512):
    """Render 512x512 close-up centered on the defect/changed region."""
    pad = 24
    scale = (res - 2 * pad) / (2.0 * focal_radius)

    # Isometric view directed at focal region
    iso = np.array([
        [ 0.70710678,  0.0,       -0.70710678],
        [-0.40824829,  0.81649658, -0.40824829],
        [ 0.57735027,  0.57735027,  0.57735027]
    ], dtype=np.float64)

    norm_ref, _, sil_ref, _ = render_view(v_ref, t_ref, n_ref, iso, focal_center, scale, w=res, h=res)
    norm_cand, _, sil_cand, _ = render_view(v_cand, t_cand, n_cand, iso, focal_center, scale, w=res, h=res)
    diff = create_diff_map(norm_ref, sil_ref, norm_cand, sil_cand)

    return norm_ref, norm_cand, diff


# --------------------------------------------------------------------------- #
# V2: PCA views, 1024 px silhouettes, X-ray parity, slices, sharp edges,
# 3D surface deviation and top-3 cluster zooms.
#
# These metrics are ADDITIONS: every V1 score, flag and image is unchanged.
# The thresholds are provisional until calibrated against the core oracle.
# --------------------------------------------------------------------------- #
V2_SILHOUETTE_RES = 1024
V2_RAY_GRID = 256
V2_SLICE_COUNT = 16
V2_SHARP_ANGLE_DEG = 35.0
V2_CLUSTER_TOP = 3
V2_CLUSTER_RES = 512
MISSING_COLOR = (230, 40, 200)
# frac(sqrt(2) - 1), frac(sqrt(3) - 1): a deterministic irrational sub-pixel
# offset, so the ray grid never lands on a mesh vertex or edge by construction.
V2_RAY_OFFSET = (0.41421356237309515, 0.7320508075688772)

V2_THRESHOLDS = {
    "silhouette_iou": 0.985,
    "xray_leak_px": 0,
    "xray_layer_delta": 0.05,
    "slice_area_rel": 0.02,
    "slice_loop_delta": 0,
    "sharp_edge_ratio": 0.90,
    "deviation_max_tol_factor": 1.5,
}


def compute_pca_axes(verts):
    """Return (eigenvalues descending, eigenvectors as columns)."""
    v = np.asarray(verts, dtype=np.float64)
    if len(v) < 3:
        return np.ones(3), np.eye(3)
    c = v.mean(axis=0)
    d = v - c
    cov = (d.T @ d) / float(len(d))
    vals, vecs = np.linalg.eigh(cov)
    order = np.argsort(vals)[::-1]
    vals = vals[order]
    vecs = vecs[:, order]
    for i in range(3):
        j = int(np.argmax(np.abs(vecs[:, i])))
        if vecs[j, i] < 0.0:
            vecs[:, i] = -vecs[:, i]
    return vals, vecs


def _view_rot(forward, up_ref):
    """Right-handed view basis with rows (right, up, forward)."""
    f = np.asarray(forward, dtype=np.float64)
    n = np.linalg.norm(f)
    if n < 1e-12:
        f = np.array([0.0, 0.0, 1.0])
    else:
        f = f / n
    up = np.asarray(up_ref, dtype=np.float64)
    up = up - np.dot(up, f) * f
    if np.linalg.norm(up) < 1e-8:
        alt = np.array([1.0, 0.0, 0.0])
        if abs(np.dot(alt, f)) > 0.9:
            alt = np.array([0.0, 1.0, 0.0])
        up = alt - np.dot(alt, f) * f
    up = up / np.linalg.norm(up)
    right = np.cross(up, f)
    right = right / np.linalg.norm(right)
    up = np.cross(f, right)
    return np.stack([right, up, f], axis=0)


def _projected_aspect(verts, rot):
    p = np.asarray(verts, dtype=np.float64) @ np.asarray(rot, dtype=np.float64).T
    du = float(np.ptp(p[:, 0]))
    dv = float(np.ptp(p[:, 1]))
    return max(du, dv) / max(min(du, dv), 1e-12)


def get_pca_cameras(evecs, verts=None, max_aspect=8.0, oblique_deg=30.0):
    """Four deterministic principal-axis views (broadside/profile/section/iso).

    When ``verts`` is supplied, a view whose projected bbox aspect ratio exceeds
    ``max_aspect`` (a flat plate seen edge-on) is replaced by a 30-degree
    oblique view tilted toward the principal axis that minimises the aspect.
    """
    emax = evecs[:, 0]
    emid = evecs[:, 1]
    emin = evecs[:, 2]
    specs = [
        ("PCA Broadside", emin, emax),
        ("PCA Profile", emid, emax),
        ("PCA Cross-Section", emax, emid),
        ("PCA Isometric", emin + emid + emax, emax),
    ]
    axes = [emax, emid, emin]
    out = []
    theta = np.deg2rad(oblique_deg)
    for name, f, up in specs:
        rot = _view_rot(f, up)
        if verts is not None and _projected_aspect(verts, rot) > max_aspect:
            best = None
            for g in axes:
                gg = np.asarray(g, dtype=np.float64)
                ff = np.asarray(f, dtype=np.float64)
                if abs(float(np.dot(gg, ff))) > 0.9:
                    continue
                f2 = np.cos(theta) * ff + np.sin(theta) * gg
                r2 = _view_rot(f2, up)
                a2 = _projected_aspect(verts, r2)
                if best is None or a2 < best[0]:
                    best = (a2, r2)
            if best is not None:
                out.append((name + " (oblique %g)" % oblique_deg, best[1]))
                continue
        out.append((name, rot))
    return out


def tight_view_frame(verts_list, rot, center, w, h, pad=16):
    """Equal-scale frame from the combined projected bounding box."""
    all_v = np.concatenate([np.asarray(v, dtype=np.float64) for v in verts_list], axis=0)
    p = (all_v - center) @ rot.T
    u0, u1 = float(p[:, 0].min()), float(p[:, 0].max())
    v0, v1 = float(p[:, 1].min()), float(p[:, 1].max())
    cu = (u0 + u1) * 0.5
    cv = (v0 + v1) * 0.5
    extent = max(u1 - u0, v1 - v0)
    scale = (min(w, h) - 2 * pad) / extent if extent > 1e-9 else 1.0
    return cu, cv, scale, extent


def raster_silhouette(verts, tris, rot, center, view_center, scale, w, h):
    """Rasterise the projected triangles into a boolean silhouette."""
    out = np.zeros((h, w), dtype=bool)
    if len(tris) == 0:
        return out
    if _fast_raster():
        _, _, sil = sutura_geom.rasterize(
            np.asarray(verts, dtype=np.float64),
            np.asarray(tris, dtype=np.int32),
            np.asarray(rot, dtype=np.float64),
            (float(center[0]), float(center[1]), float(center[2])),
            (float(view_center[0]), float(view_center[1])),
            float(scale), int(w), int(h), silhouette_only=True)
        return np.asarray(sil, dtype=bool)
    v_cam = (np.asarray(verts, dtype=np.float64) - center) @ rot.T
    px = np.rint(w * 0.5 + (v_cam[:, 0] - float(view_center[0])) * scale).astype(np.int64)
    py = np.rint(h * 0.5 - (v_cam[:, 1] - float(view_center[1])) * scale).astype(np.int64)
    img = QImage(w, h, QImage.Format_RGB32)
    img.fill(0)
    p = QPainter(img)
    p.setPen(Qt.NoPen)
    p.setBrush(QColor(255, 255, 255))
    for t in tris:
        p.drawPolygon(QPolygon([
            QPoint(int(px[t[0]]), int(py[t[0]])),
            QPoint(int(px[t[1]]), int(py[t[1]])),
            QPoint(int(px[t[2]]), int(py[t[2]])),
        ]))
    p.end()
    raw = np.frombuffer(img.constBits(), dtype=np.uint8).reshape((h, w, 4))
    out = raw[:, :, 0] > 0
    return out


def silhouette_iou_v2(v_ref, t_ref, v_cand, t_cand, rot, center):
    cu, cv, scale, _ = tight_view_frame([v_ref, v_cand], rot, center,
                                        V2_SILHOUETTE_RES, V2_SILHOUETTE_RES, pad=24)
    s_ref = raster_silhouette(v_ref, t_ref, rot, center, (cu, cv), scale,
                              V2_SILHOUETTE_RES, V2_SILHOUETTE_RES)
    s_cand = raster_silhouette(v_cand, t_cand, rot, center, (cu, cv), scale,
                               V2_SILHOUETTE_RES, V2_SILHOUETTE_RES)
    inter = int(np.count_nonzero(s_ref & s_cand))
    union = int(np.count_nonzero(s_ref | s_cand))
    iou = float(inter) / float(union) if union else 1.0
    return {
        "iou": iou,
        "ref_px": int(np.count_nonzero(s_ref)),
        "cand_px": int(np.count_nonzero(s_cand)),
        "missing_px": int(np.count_nonzero(s_ref & (~s_cand))),
        "excess_px": int(np.count_nonzero((~s_ref) & s_cand)),
    }


def _ray_grid_for_view(verts_list, rot, center, n=V2_RAY_GRID):
    """Fixed orthographic ray grid covering the combined tight frame."""
    all_v = np.concatenate([np.asarray(v, dtype=np.float64) for v in verts_list], axis=0)
    p = (all_v - center) @ rot.T
    x0, x1 = float(p[:, 0].min()), float(p[:, 0].max())
    y0, y1 = float(p[:, 1].min()), float(p[:, 1].max())
    z0, z1 = float(p[:, 2].min()), float(p[:, 2].max())
    extent = max(x1 - x0, y1 - y0)
    if extent <= 1e-9:
        return None
    step = extent / float(n - 1)
    cx = (x0 + x1) * 0.5
    cy = (y0 + y1) * 0.5
    half = (n - 1) * 0.5
    xs = cx + (np.arange(n, dtype=np.float64) - half + (V2_RAY_OFFSET[0] - 0.5)) * step
    ys = cy + (np.arange(n, dtype=np.float64) - half + (V2_RAY_OFFSET[1] - 0.5)) * step
    gx, gy = np.meshgrid(xs, ys)
    back = (z1 - z0) + 2.0 * max(0.01 * extent, step)
    origins = (center
               + gx.reshape(-1, 1) * rot[0]
               + gy.reshape(-1, 1) * rot[1]
               + (z1 + back) * rot[2])
    dirs = np.tile(-rot[2], (origins.shape[0], 1))
    t_max = back + (z1 - z0) + step
    return origins, dirs, t_max, (n, n), max(1e-12, (z1 - z0) * 1e-9)


def _xray_stats(crossings, suspect, shape):
    crossings = np.asarray(crossings, dtype=np.int64).reshape(shape)
    suspect = np.asarray(suspect, dtype=bool).reshape(shape)
    return {
        "crossings": crossings,
        "layers": crossings // 2,
        "leak": (crossings % 2) == 1,
        "covered": crossings > 0,
        "suspect_px": int(np.count_nonzero(suspect)),
    }


def _raycast_many(verts, tris, grids):
    """One BVH build for all PCA-view ray grids of a mesh."""
    if not HAS_SUTURA_GEOM or not hasattr(sutura_geom, "raycast_points"):
        raise RuntimeError("sutura_geom.raycast_points is required for X-ray parity")
    origins = np.concatenate([g[0] for g in grids], axis=0)
    dirs = np.concatenate([g[1] for g in grids], axis=0)
    t_max = max(g[2] for g in grids)
    merge_eps = max(g[4] for g in grids)
    _, crossings, _, suspect = sutura_geom.raycast_points(
        np.asarray(verts, dtype=np.float64),
        np.asarray(tris, dtype=np.int32),
        origins, dirs, max_t=t_max, merge_eps=merge_eps)
    out = []
    pos = 0
    for g in grids:
        n = int(np.prod(g[3]))
        out.append(_xray_stats(crossings[pos:pos + n], suspect[pos:pos + n], g[3]))
        pos += n
    return out


def _xray_counts(verts, tris, grid):
    return _raycast_many(verts, tris, [grid])[0]


def _xray_image(ref_counts, cand_counts=None):
    h, w = ref_counts["crossings"].shape
    img = np.zeros((h, w, 3), dtype=np.uint8)
    img[:] = (18, 20, 24)
    if cand_counts is None:
        cov = ref_counts["covered"]
        img[cov] = (112, 118, 128)
        # Show interior/overlap layers in the reference itself: two interpenetrating
        # closed bodies give 4 crossings (2 layers) on the overlap rays.
        img[ref_counts["layers"] >= 2] = (255, 140, 60)
        img[ref_counts["leak"]] = (178, 90, 235)
        return img
    cov = ref_counts["covered"] | cand_counts["covered"]
    img[cov] = (88, 94, 104)
    extra = cand_counts["layers"] > ref_counts["layers"]
    missing = cand_counts["layers"] < ref_counts["layers"]
    img[missing & cov] = (60, 190, 230)
    img[extra & cov] = (255, 140, 60)
    img[cand_counts["leak"]] = (178, 90, 235)
    return img


def _xray_compare_stats(ref, cand):
    union = ref["covered"] | cand["covered"]
    denom = max(1, int(np.count_nonzero(union)))
    layer_diff = np.abs(cand["layers"] - ref["layers"]).astype(np.float64)
    return {
        "leak_ref_px": int(np.count_nonzero(ref["leak"])),
        "leak_cand_px": int(np.count_nonzero(cand["leak"])),
        "extra_layer_px": int(np.count_nonzero((cand["layers"] > ref["layers"]) & union)),
        "missing_layer_px": int(np.count_nonzero((cand["layers"] < ref["layers"]) & union)),
        "mean_abs_layer_delta": float(layer_diff[union].mean()) if denom else 0.0,
        "covered_px": denom,
        "ref_max_layers": int(ref["layers"].max()) if ref["layers"].size else 0,
        "cand_max_layers": int(cand["layers"].max()) if cand["layers"].size else 0,
        "suspect_ref_px": ref["suspect_px"],
        "suspect_cand_px": cand["suspect_px"],
        "image_ref": _xray_image(ref),
        "image_delta": _xray_image(ref, cand),
    }


def xray_compare(v_ref, t_ref, v_cand, t_cand, rot, center):
    """Ray-parity / layer comparison for one PCA view."""
    grid = _ray_grid_for_view([v_ref, v_cand], rot, center)
    if grid is None:
        return None
    return _xray_compare_stats(_xray_counts(v_ref, t_ref, grid),
                               _xray_counts(v_cand, t_cand, grid))


def _slice_segments(verts, tris, z):
    """Oriented plane/triangle intersection segments at z.

    Segments are directed so a closed outer loop is counter-clockwise
    (outer boundary positive, holes negative) when the face winding is
    consistent; no shoelace orientation guessing or nesting test is needed.
    """
    if len(tris) == 0:
        return np.zeros((0, 2, 2), dtype=np.float64)
    tri_v = np.asarray(verts, dtype=np.float64)[tris]
    zt = tri_v[:, :, 2]
    zmin = zt.min(axis=1)
    zmax = zt.max(axis=1)
    sel = (zmin < z) & (zmax > z)
    if not np.any(sel):
        return np.zeros((0, 2, 2), dtype=np.float64)
    tri_v = tri_v[sel]
    zt = zt[sel]
    tri_idx = []
    pts = []
    for i, j in ((0, 1), (1, 2), (2, 0)):
        m = (zt[:, i] - z) * (zt[:, j] - z) < 0.0
        if not np.any(m):
            continue
        tt = (z - zt[m, i]) / (zt[m, j] - zt[m, i])
        p = tri_v[m, i, :2] + tt[:, None] * (tri_v[m, j, :2] - tri_v[m, i, :2])
        tri_idx.append(np.nonzero(m)[0])
        pts.append(p)
    if not tri_idx:
        return np.zeros((0, 2, 2), dtype=np.float64)
    idx = np.concatenate(tri_idx)
    pp = np.concatenate(pts, axis=0)
    order = np.argsort(idx, kind="stable")
    idx = idx[order]
    pp = pp[order]
    counts = np.bincount(idx)
    if not np.all(counts == 2):
        segs = []
        start = 0
        for c in counts:
            if c == 2:
                segs.append(pp[start:start + 2])
            start += c
        seg = np.asarray(segs, dtype=np.float64) if segs else np.zeros((0, 2, 2))
    else:
        seg = pp.reshape(-1, 2, 2)
    if len(seg) == 0:
        return seg
    tri_sel = tri_v[idx[::2]]
    n = np.cross(tri_sel[:, 1] - tri_sel[:, 0], tri_sel[:, 2] - tri_sel[:, 0])
    d = np.stack([-n[:, 1], n[:, 0]], axis=1)
    dot = np.sum((seg[:, 1] - seg[:, 0]) * d, axis=1)
    flip = dot < 0.0
    if np.any(flip):
        seg[flip] = seg[flip][:, ::-1]
    return seg


def _chain_loops(segments, tol):
    """Chain undirected 2D segments into closed loops / open chains."""
    if len(segments) == 0:
        return [], 0
    pts = np.concatenate([segments[:, 0], segments[:, 1]], axis=0)
    keys = np.rint(pts / tol).astype(np.int64)
    uniq, inv = np.unique(keys, axis=0, return_inverse=True)
    ids = inv.reshape(2, -1).T
    id_pt = np.zeros((len(uniq), 2), dtype=np.float64)
    nseg = len(segments)
    for i in range(nseg):
        id_pt[ids[i, 0]] = pts[i]
        id_pt[ids[i, 1]] = pts[i + nseg]
    adj = [[] for _ in range(len(uniq))]
    for si, (a, b) in enumerate(ids):
        adj[int(a)].append(si)
        adj[int(b)].append(si)
    visited = set()
    loops = []
    open_chains = 0
    for si in range(nseg):
        if si in visited:
            continue
        a = int(ids[si, 0])
        b = int(ids[si, 1])
        visited.add(si)
        poly = [id_pt[a], id_pt[b]]
        start = a
        cur = b
        closed = False
        while cur != start:
            nxts = [sj for sj in adj[cur] if sj not in visited]
            if not nxts:
                break
            sj = min(nxts)
            visited.add(sj)
            cur = int(ids[sj, 0]) if int(ids[sj, 1]) == cur else int(ids[sj, 1])
            poly.append(id_pt[cur])
        if cur == start and len(poly) >= 4:
            loops.append(np.asarray(poly[:-1], dtype=np.float64))
            closed = True
        if not closed:
            open_chains += 1
    return loops, open_chains


def _polygon_area(poly):
    x = poly[:, 0]
    y = poly[:, 1]
    return 0.5 * abs(float(np.dot(x, np.roll(y, -1)) - np.dot(y, np.roll(x, -1))))


def _interior_point(poly):
    """A point just inside a simple polygon, independent of its orientation."""
    n = len(poly)
    if n < 3:
        return poly.mean(axis=0)
    e = np.roll(poly, -1, axis=0) - poly
    length2 = np.sum(e * e, axis=1)
    i = int(np.argmax(length2))
    if length2[i] <= 1e-30:
        return poly.mean(axis=0)
    mid = (poly[i] + poly[(i + 1) % n]) * 0.5
    d = e[i] / np.sqrt(length2[i])
    nrm = np.array([-d[1], d[0]])
    span = max(float(np.ptp(poly[:, 0])), float(np.ptp(poly[:, 1])))
    eps = max(span * 1e-6, 1e-9)
    for sign in (1.0, -1.0):
        q = mid + sign * nrm * eps
        if _point_in_polygon(q[None, :], poly)[0]:
            return q
    return poly.mean(axis=0)


def _slice_area(loops):
    """Net closed-loop area with a depth (outer minus hole) nesting test.

    A bounding-box prefilter keeps the containment step near-linear on slices
    with hundreds of disjoint loops (noise/debris); only loops whose bbox
    contains the probe point are tested.
    """
    if not loops:
        return 0.0
    areas = np.array([_polygon_area(p) for p in loops], dtype=np.float64)
    pts = np.array([_interior_point(p) for p in loops], dtype=np.float64)
    bbox = np.array([[p[:, 0].min(), p[:, 1].min(),
                      p[:, 0].max(), p[:, 1].max()] for p in loops],
                    dtype=np.float64)
    total = 0.0
    for i in range(len(loops)):
        qx, qy = pts[i]
        cand = np.nonzero((qx >= bbox[:, 0]) & (qx <= bbox[:, 2])
                          & (qy >= bbox[:, 1]) & (qy <= bbox[:, 3]))[0]
        depth = 0
        for j in cand:
            if j != i and _point_in_polygon(pts[i:i + 1], loops[j])[0]:
                depth += 1
        total += areas[i] if depth % 2 == 0 else -areas[i]
    return abs(float(total))


def _point_in_polygon(points, poly):
    """Even-odd ray casting, vectorised over both probe points and vertices."""
    x = points[:, 0:1]
    y = points[:, 1:2]
    xp = poly[:, 0]
    yp = poly[:, 1]
    xq = np.roll(xp, -1)
    yq = np.roll(yp, -1)
    cond = (yp[None, :] > y) != (yq[None, :] > y)
    denom = yq - yp
    denom = np.where(np.abs(denom) < 1e-300, 1e-300, denom)
    xi = (xq - xp)[None, :] * (y - yp[None, :]) / denom[None, :] + xp[None, :]
    return (np.count_nonzero(cond & (x < xi), axis=1) % 2) == 1


def _point_segment_distance(points, segments):
    if len(points) == 0 or len(segments) == 0:
        return np.zeros(len(points), dtype=np.float64)
    a = segments[None, :, 0, :]
    b = segments[None, :, 1, :]
    ab = b - a
    den = np.sum(ab * ab, axis=-1)
    den[den < 1e-30] = 1e-30
    out = np.empty(len(points), dtype=np.float64)
    chunk = 128
    for s in range(0, len(points), chunk):
        p = points[s:s + chunk, None, :]
        ap = p - a
        t = np.clip(np.sum(ap * ab, axis=-1) / den, 0.0, 1.0)
        proj = a + t[..., None] * ab
        d = np.linalg.norm(p - proj, axis=-1).min(axis=1)
        out[s:s + chunk] = d
    return out


def _sample_segments(segments, per=4):
    """Sample each segment at ``per + 1`` equally spaced points (2D)."""
    t = np.linspace(0.0, 1.0, per + 1, dtype=np.float64)[None, :, None]
    pts = segments[:, None, 0, :] * (1.0 - t) + segments[:, None, 1, :] * t
    return pts.reshape(-1, 2)


def _segments_hausdorff(seg_a, seg_b):
    """Approximate 2D segment-set Hausdorff distance.

    Exact point-to-segment distance is O(S^2) and dominated the slice pass on
    dense scans.  The tree version samples every segment (4 sub-segments) and
    queries a `sutura_geom.KdTree` in both directions, which is O(S log S); the
    sampling error is below a quarter of one segment length.
    """
    if len(seg_a) == 0 or len(seg_b) == 0:
        return None
    if HAS_SUTURA_GEOM and hasattr(sutura_geom, "KdTree"):
        qa = _sample_segments(seg_a)
        qb = _sample_segments(seg_b)
        # The mandatory tree is 3D; slices live in z = 0.
        pa = np.column_stack([qa, np.zeros(len(qa))])
        pb = np.column_stack([qb, np.zeros(len(qb))])
        tree_b = sutura_geom.KdTree(pb)
        da = np.asarray(tree_b.query(pa, k=1)[0], dtype=np.float64).max()
        tree_a = sutura_geom.KdTree(pa)
        db = np.asarray(tree_a.query(pb, k=1)[0], dtype=np.float64).max()
        return float(max(da, db))
    # Dependency-free fallback: cap the query count so a dense slice cannot go
    # quadratic without bound.
    qa = np.concatenate([seg_a[:, 0], seg_a[:, 1],
                         (seg_a[:, 0] + seg_a[:, 1]) * 0.5], axis=0)
    qb = np.concatenate([seg_b[:, 0], seg_b[:, 1],
                         (seg_b[:, 0] + seg_b[:, 1]) * 0.5], axis=0)
    if len(qa) > 4000:
        qa = qa[::max(1, len(qa) // 4000)]
    if len(qb) > 4000:
        qb = qb[::max(1, len(qb) // 4000)]
    da = _point_segment_distance(qa, seg_b).max()
    db = _point_segment_distance(qb, seg_a).max()
    return float(max(da, db))


def _nudge_slice_height(z, sorted_zs, spacing):
    """Shift z by a deterministic irrational fraction until it misses every vertex."""
    if len(sorted_zs) == 0:
        return z
    for k in range(1, 8):
        pos = int(np.searchsorted(sorted_zs, z))
        near = []
        if pos > 0:
            near.append(abs(sorted_zs[pos - 1] - z))
        if pos < len(sorted_zs):
            near.append(abs(sorted_zs[pos] - z))
        if min(near) > 1e-9 * max(1.0, spacing):
            return z
        z += (V2_RAY_OFFSET[0] + k * 0.112702) * spacing
    return z


def slice_compare(v_ref, t_ref, v_cand, t_cand, n=V2_SLICE_COUNT):
    """Z-slice area / loop / 2D-Hausdorff comparison at n deterministic heights."""
    all_v = np.concatenate([v_ref, v_cand], axis=0)
    z0 = float(all_v[:, 2].min())
    z1 = float(all_v[:, 2].max())
    span = z1 - z0
    if span <= 1e-9:
        return {"slices": [], "mean_area_rel": 0.0, "max_area_rel": 0.0,
                "max_loop_delta": 0, "max_hausdorff": 0.0}
    spacing = span / n
    sorted_zs = np.unique(all_v[:, 2])
    diag = float(np.linalg.norm(all_v.max(axis=0) - all_v.min(axis=0)))
    tol = max(1e-9, diag * 1e-7)
    per = []
    for k in range(n):
        z = z0 + (k + 0.5) * spacing
        z = _nudge_slice_height(z, sorted_zs, spacing)
        sr = _slice_segments(v_ref, t_ref, z)
        sc = _slice_segments(v_cand, t_cand, z)
        lr, orr = _chain_loops(sr, tol)
        lc, orc = _chain_loops(sc, tol)
        ar = _slice_area(lr)
        ac = _slice_area(lc)
        valid = bool(orr == 0 and orc == 0 and len(lr) > 0 and len(lc) > 0)
        area_rel = abs(ac - ar) / max(ar, ac, 1e-9)
        area_rel_ref = abs(ac - ar) / ar if ar > 1e-9 else 0.0
        hd = _segments_hausdorff(sc, sr)
        per.append({
            "z": z,
            "valid": valid,
            "ref_segments": int(len(sr)),
            "cand_segments": int(len(sc)),
            "ref_loops": int(len(lr)),
            "cand_loops": int(len(lc)),
            "ref_open": int(orr),
            "cand_open": int(orc),
            "ref_area": ar,
            "cand_area": ac,
            "area_rel": float(area_rel),
            "area_rel_ref": float(area_rel_ref),
            "loop_delta": int(abs(len(lc) - len(lr))),
            "hausdorff": hd,
        })
    valid_slices = [s for s in per if s["valid"]]
    return {
        "z_min": z0,
        "z_max": z1,
        "slice_count": n,
        "valid_slices": len(valid_slices),
        "invalid_slices": n - len(valid_slices),
        "slices": per,
        "mean_area_rel": float(np.mean([s["area_rel"] for s in valid_slices]))
        if valid_slices else 0.0,
        "max_area_rel": float(np.max([s["area_rel"] for s in valid_slices]))
        if valid_slices else 0.0,
        "max_loop_delta": int(max(s["loop_delta"] for s in valid_slices))
        if valid_slices else 0,
        "max_hausdorff": float(max((s["hausdorff"] or 0.0) for s in per)),
    }


def sharp_edge_ratio(v_ref, t_ref, n_ref, v_cand, t_cand, n_cand,
                     angle_deg=V2_SHARP_ANGLE_DEG):
    """Total sharp-edge length on the candidate relative to the reference."""
    def _stats(v, t, normals):
        if len(t) == 0:
            return 0.0, 0, 0.0
        e = np.concatenate([t[:, [0, 1]], t[:, [1, 2]], t[:, [2, 0]]], axis=0)
        fid = np.concatenate([np.arange(len(t)), np.arange(len(t)), np.arange(len(t))])
        es = np.sort(e, axis=1)
        key = es[:, 0].astype(np.int64) * max(1, len(v)) + es[:, 1]
        order = np.argsort(key, kind="stable")
        ks = key[order]
        starts = np.r_[0, np.nonzero(np.diff(ks))[0] + 1]
        counts = np.diff(np.r_[starts, len(ks)])
        two = counts == 2
        st = starts[two]
        pair = np.stack([st, st + 1], axis=1).ravel()
        faces = fid[order[pair]].reshape(-1, 2)
        dot = np.clip(np.sum(normals[faces[:, 0]] * normals[faces[:, 1]], axis=1), -1.0, 1.0)
        angle = np.degrees(np.arccos(dot))
        sel = angle >= angle_deg
        e_sel = es[order[st[sel]]]
        length = np.linalg.norm(v[e_sel[:, 0]] - v[e_sel[:, 1]], axis=1)
        total = float(length.sum())
        return total, int(sel.sum()), float(np.percentile(angle, 95)) if len(angle) else 0.0

    l_ref, n_ref_sharp, p_ref = _stats(v_ref, t_ref, n_ref)
    l_cand, n_cand_sharp, p_cand = _stats(v_cand, t_cand, n_cand)
    ratio = (l_cand / l_ref) if l_ref > 1e-12 else None
    return {
        "angle_threshold_deg": angle_deg,
        "ref_sharp_length": l_ref,
        "cand_sharp_length": l_cand,
        "ref_sharp_edges": n_ref_sharp,
        "cand_sharp_edges": n_cand_sharp,
        "ratio": ratio,
        "ref_dihedral_p95_deg": p_ref,
        "cand_dihedral_p95_deg": p_cand,
    }


def _one_way_deviation(v_src, t_src, v_dst, t_dst, tol):
    """Point-to-surface distance from source vertices to the destination mesh."""
    method = None
    dist = None
    if HAS_SUTURA_GEOM and hasattr(sutura_geom, "closest_points"):
        try:
            _, dist, _ = sutura_geom.closest_points(
                np.asarray(v_dst, dtype=np.float64),
                np.asarray(t_dst, dtype=np.int32),
                np.asarray(v_src, dtype=np.float64))
            dist = np.asarray(dist, dtype=np.float64)
            method = "surface"
        except Exception:  # noqa: BLE001
            dist = None
    if dist is None and HAS_SUTURA_GEOM and hasattr(sutura_geom, "KdTree"):
        try:
            tree = sutura_geom.KdTree(np.asarray(v_dst, dtype=np.float64))
            d, _ = tree.query(np.asarray(v_src, dtype=np.float64), k=1)
            dist = np.asarray(d, dtype=np.float64)
            method = "vertex"
        except Exception:  # noqa: BLE001
            dist = None
    if dist is None:
        return None
    face_dev = dist[t_src].max(axis=1)
    tri = v_src[t_src]
    areas = 0.5 * np.linalg.norm(
        np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
    over = face_dev > tol
    total_area = float(areas.sum())
    stats = {
        "method": method,
        "max": float(dist.max()),
        "p95": float(np.percentile(dist, 95)),
        "p99": float(np.percentile(dist, 99)),
        "mean": float(dist.mean()),
        "over_tol_faces": int(np.count_nonzero(over)),
        "over_tol_area_rel": float(areas[over].sum() / total_area) if total_area > 0 else 0.0,
    }
    return stats, dist, face_dev


def surface_deviation(v_ref, t_ref, v_cand, t_cand, tol):
    """Two-sided point-to-surface deviation (candidate->ref and ref->candidate).

    The top-level max/p95/p99/mean and the V2_DEVIATION flag use both
    directions; per-direction stats and arrays are kept for the heatmaps.
    """
    cand = _one_way_deviation(v_cand, t_cand, v_ref, t_ref, tol)
    ref = _one_way_deviation(v_ref, t_ref, v_cand, t_cand, tol)
    if cand is None and ref is None:
        return {
            "method": None, "max": None, "p95": None, "p99": None,
            "mean": None, "over_tol_faces": 0, "over_tol_area_rel": 0.0,
            "candidate_to_reference": None, "reference_to_candidate": None,
            "vertex_distance": None, "face_distance": None,
            "ref_vertex_distance": None, "ref_face_distance": None,
        }
    parts = [p for p in (cand, ref) if p is not None]
    method = parts[0][0]["method"]
    combined = np.concatenate([p[1] for p in parts])
    cstats = cand[0] if cand is not None else None
    rstats = ref[0] if ref is not None else None
    return {
        "method": method,
        "max": float(max(p[0]["max"] for p in parts)),
        "p95": float(max(p[0]["p95"] for p in parts)),
        "p99": float(max(p[0]["p99"] for p in parts)),
        "mean": float(combined.mean()),
        "over_tol_faces": int(sum(p[0]["over_tol_faces"] for p in parts)),
        "over_tol_area_rel": float(max(p[0]["over_tol_area_rel"] for p in parts)),
        "candidate_to_reference": cstats,
        "reference_to_candidate": rstats,
        "vertex_distance": cand[1] if cand is not None else None,
        "face_distance": cand[2] if cand is not None else None,
        "ref_vertex_distance": ref[1] if ref is not None else None,
        "ref_face_distance": ref[2] if ref is not None else None,
    }


def normal_face_colors(normals, rot):
    """The same flat normal colours used by render_view's normal map."""
    n_cam = np.asarray(normals, dtype=np.float64) @ np.asarray(rot, dtype=np.float64).T
    return np.clip((n_cam * 0.5 + 0.5) * 255.0, 0, 255).astype(np.uint8)


def deviation_colors(dist, tol):
    """Blue (0) -> green (0.5 tol) -> yellow (tol) -> red (> tol)."""
    d = np.maximum(np.asarray(dist, dtype=np.float64), 0.0)
    t = d / max(tol, 1e-12)
    blue = np.array([43, 92, 143], dtype=np.float64)
    green = np.array([46, 204, 113], dtype=np.float64)
    yellow = np.array([250, 220, 60], dtype=np.float64)
    red = np.array([217, 56, 41], dtype=np.float64)
    out = np.zeros((len(d), 3), dtype=np.float64)
    m0 = t <= 0.5
    a = np.clip(t[m0] / 0.5, 0.0, 1.0)[:, None]
    out[m0] = blue[None, :] * (1.0 - a) + green[None, :] * a
    m1 = (t > 0.5) & (t <= 1.0)
    b = np.clip((t[m1] - 0.5) / 0.5, 0.0, 1.0)[:, None]
    out[m1] = green[None, :] * (1.0 - b) + yellow[None, :] * b
    out[t > 1.0] = red[None, :]
    return out.astype(np.uint8)


def deviation_clusters(v, t, face_dev, tol, top=V2_CLUSTER_TOP):
    """Connected over-tolerance face clusters, ranked by max deviation x area."""
    sel = np.nonzero(face_dev > tol)[0]
    if len(sel) == 0:
        return []
    n_sel = len(sel)
    e = np.concatenate([t[sel][:, [0, 1]], t[sel][:, [1, 2]], t[sel][:, [2, 0]]], axis=0)
    fid = np.concatenate([np.arange(n_sel), np.arange(n_sel), np.arange(n_sel)])
    es = np.sort(e, axis=1)
    key = es[:, 0].astype(np.int64) * max(1, len(v)) + es[:, 1]
    order = np.argsort(key, kind="stable")
    ks = key[order]
    starts = np.r_[0, np.nonzero(np.diff(ks))[0] + 1]
    counts = np.diff(np.r_[starts, len(ks)])
    parent = np.arange(n_sel)

    def _find(a):
        while parent[a] != a:
            parent[a] = parent[parent[a]]
            a = parent[a]
        return a

    def _union(a, b):
        ra, rb = _find(a), _find(b)
        if ra != rb:
            parent[rb] = ra

    group_idx = np.nonzero(counts >= 2)[0]
    for g in group_idx:
        members = fid[order[starts[g]:starts[g] + counts[g]]]
        base = int(members[0])
        for m in members[1:]:
            _union(base, int(m))
    roots = np.array([_find(i) for i in range(n_sel)])
    tri = v[t[sel]]
    areas = 0.5 * np.linalg.norm(np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0]), axis=1)
    cents = tri.mean(axis=1)
    clusters = []
    for r in np.unique(roots):
        members = np.nonzero(roots == r)[0]
        f_idx = sel[members]
        area = float(areas[members].sum())
        max_dev = float(face_dev[f_idx].max())
        w = areas[members]
        if w.sum() > 0:
            center = (cents[members] * w[:, None]).sum(axis=0) / w.sum()
        else:
            center = cents[members].mean(axis=0)
        vidx = np.unique(t[f_idx].ravel())
        radius = max(float(np.linalg.norm(v[vidx] - center, axis=1).max()), 1e-6)
        clusters.append({
            "faces": f_idx,
            "face_count": int(len(f_idx)),
            "area": area,
            "max_dev": max_dev,
            "mean_dev": float(face_dev[f_idx].mean()),
            "center": center,
            "radius": radius,
            "score": max_dev * max(area, 1e-12),
        })
    clusters.sort(key=lambda c: (c["score"], c["max_dev"]), reverse=True)
    return clusters[:top]


def _global_up(normal):
    z = np.array([0.0, 0.0, 1.0])
    if abs(float(np.dot(normal, z))) > 0.9:
        return np.array([0.0, 1.0, 0.0])
    return z


def render_cluster_zoom(v, t, face_colors, center, radius, normal, res=V2_CLUSTER_RES,
                        normals=None, missing_mask=None):
    """512 px local close-up, normal-aligned with a 15 degree elevation + wireframe."""
    n = np.asarray(normal, dtype=np.float64)
    n = n / max(np.linalg.norm(n), 1e-12)
    up = _global_up(n)
    f = n + 0.268 * up
    f = f / max(np.linalg.norm(f), 1e-12)
    rot = _view_rot(f, up)
    local_r = 1.8 * radius
    vm = np.linalg.norm(v - center, axis=1) <= local_r
    fm = vm[t].all(axis=1)
    if not np.any(fm):
        fm = np.ones(len(t), dtype=bool)
    lv = v
    lt = t[fm]
    if face_colors is not None:
        lc = face_colors[fm]
    elif normals is not None:
        lc = normal_face_colors(normals, rot)[fm].copy()
        if missing_mask is not None:
            lc[missing_mask[fm]] = np.asarray(MISSING_COLOR, dtype=np.uint8)
    else:
        lc = np.full((len(lt), 3), 200, dtype=np.uint8)
    local_vidx = np.unique(lt.ravel())
    cu, cv, scale, _ = tight_view_frame([lv[local_vidx]], rot, center, res, res, pad=24)
    rgb, _, sil, _ = render_view(lv, lt, np.zeros((len(lv), 3)), rot, center,
                                 scale, w=res, h=res, view_center=(cu, cv),
                                 face_colors=lc, need_depth=False, wireframe=True)
    return rgb, sil.sum()


def assemble_v2_sheet(title_lines, rows, out_path, tol=None):
    """Assemble the V2 contact sheet (PCA deviation, X-ray, cluster zooms)."""
    if not HAS_PYSIDE6:
        raise RuntimeError("PySide6 is required to generate the V2 contact sheet")
    ensure_qapp()
    cell_w = cell_h = 256
    pad = 12
    hdr_h = 78
    row_lbl_w = 118
    legend_h = 112
    n_cols = max(1, max(len(r["images"]) for r in rows))
    total_w = row_lbl_w + n_cols * cell_w + (n_cols + 1) * pad
    total_h = hdr_h + len(rows) * cell_h + (len(rows) + 1) * pad + legend_h
    canvas = QImage(total_w, total_h, QImage.Format_RGB32)
    canvas.fill(QColor(25, 27, 31))
    p = QPainter(canvas)
    p.setRenderHint(QPainter.TextAntialiasing)
    p.setPen(QColor(240, 240, 245))
    p.drawText(pad, 26, title_lines[0])
    if len(title_lines) > 1:
        p.setPen(QColor(160, 165, 175))
        p.drawText(pad, 48, title_lines[1])
    for r, row in enumerate(rows):
        ry = hdr_h + pad + r * (cell_h + pad)
        p.setPen(QColor(190, 195, 205))
        p.drawText(8, ry + cell_h // 2 + 4, row["name"])
        for c, img_arr in enumerate(row["images"]):
            cx = row_lbl_w + pad + c * (cell_w + pad)
            ih, iw, _ = img_arr.shape
            img_c = np.ascontiguousarray(img_arr, dtype=np.uint8)
            q_img = QImage(img_c.data, iw, ih, iw * 3, QImage.Format_RGB888)
            if (iw, ih) != (cell_w, cell_h):
                q_img = q_img.scaled(cell_w, cell_h, Qt.IgnoreAspectRatio, Qt.SmoothTransformation)
            p.drawImage(cx, ry, q_img)
            cap = row.get("captions", [])
            if c < len(cap) and cap[c]:
                p.fillRect(QRect(cx, ry + cell_h - 22, cell_w, 22), QColor(0, 0, 0, 180))
                p.setPen(QColor(235, 235, 235))
                p.drawText(cx + 6, ry + cell_h - 6, cap[c])
    # Legends: X-ray colour key and the two-sided deviation ramp in mm.
    ly = hdr_h + len(rows) * (cell_h + pad) + pad
    legend_font = QFont()
    legend_font.setPointSize(8)
    p.setFont(legend_font)
    p.setPen(QColor(220, 222, 228))
    p.drawText(12, ly + 14, "X-ray colour key (per view):")
    key = [
        ((112, 118, 128), "covered"),
        ((255, 140, 60), "extra / overlap layers"),
        ((60, 190, 230), "fewer layers"),
        ((178, 90, 235), "odd parity (leak)"),
    ]
    kx = 12
    for color, label in key:
        p.fillRect(QRect(kx, ly + 24, 12, 12), QColor(*color))
        p.setPen(QColor(210, 213, 220))
        p.drawText(kx + 16, ly + 34, label)
        kx += 20 + 7 * len(label)
    tol_v = float(tol) if tol else 0.05
    bx, by, bw, bh = 660, ly + 24, 280, 14
    p.setPen(QColor(220, 222, 228))
    p.drawText(bx, ly + 14, "Two-sided deviation (mm):")
    for i in range(bw):
        d = 1.5 * tol_v * i / max(1, bw - 1)
        c = deviation_colors(np.array([d]), tol_v)[0]
        p.fillRect(QRect(bx + i, by, 1, bh), QColor(int(c[0]), int(c[1]), int(c[2])))
    p.setPen(QColor(210, 213, 220))
    for frac, label in ((0.0, "0"), (1.0 / 3.0, "%.3g" % (0.5 * tol_v)),
                        (2.0 / 3.0, "%.3g (tol)" % tol_v),
                        (1.0, "%.3g" % (1.5 * tol_v))):
        tx = bx + int(frac * (bw - 1))
        p.drawText(tx - 6 if frac else tx, by + bh + 12, label)
    mx = bx + bw + 18
    p.fillRect(QRect(mx, by, 12, 12), QColor(*MISSING_COLOR))
    p.setPen(QColor(210, 213, 220))
    p.drawText(mx + 16, by + 10, "missing on reference (> tol)")
    p.end()
    os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
    canvas.save(out_path, "PNG")
    return out_path


def evaluate_v2(v_ref, t_ref, n_ref, v_cand, t_cand, n_cand,
                tol, out_sheet=None, cluster_dir=None):
    """Compute the V2 metrics and images for one reference/candidate pair."""
    t0 = time.perf_counter()
    all_v = np.concatenate([v_ref, v_cand], axis=0)
    center = (all_v.min(axis=0) + all_v.max(axis=0)) * 0.5
    diag = float(np.linalg.norm(all_v.max(axis=0) - all_v.min(axis=0)))
    _, evecs = compute_pca_axes(all_v)
    cameras = get_pca_cameras(evecs, all_v)
    timings = {}
    t_sil = t_xray = 0.0

    sil_views = []
    xray_views = []
    xray_rows_ref = []
    xray_rows_delta = []
    grids = [_ray_grid_for_view([v_ref, v_cand], rot, center) for _, rot in cameras]
    batched = None
    if all(g is not None for g in grids):
        ts = time.perf_counter()
        try:
            batched = (_raycast_many(v_ref, t_ref, grids),
                       _raycast_many(v_cand, t_cand, grids))
        except Exception:  # noqa: BLE001
            batched = None
        t_xray += time.perf_counter() - ts
    for i, (name, rot) in enumerate(cameras):
        ts = time.perf_counter()
        s = silhouette_iou_v2(v_ref, t_ref, v_cand, t_cand, rot, center)
        t_sil += time.perf_counter() - ts
        s["view"] = name
        sil_views.append(s)
        ts = time.perf_counter()
        if batched is not None:
            x = _xray_compare_stats(batched[0][i], batched[1][i])
        else:
            try:
                x = xray_compare(v_ref, t_ref, v_cand, t_cand, rot, center)
            except Exception as ex:  # noqa: BLE001
                x = {"error": str(ex)}
        t_xray += time.perf_counter() - ts
        if x is not None:
            x["view"] = name
        xray_views.append(x)
        if x and "image_ref" in x:
            xray_rows_ref.append(x.pop("image_ref"))
            xray_rows_delta.append(x.pop("image_delta"))
    timings["silhouette_s"] = t_sil
    timings["xray_s"] = t_xray

    # Two-sided deviation: candidate->reference (ramp) and reference->candidate
    # (missing material, magenta on the reference views and cluster zooms).
    ref_imgs = []
    dev_imgs = []
    t_dev = time.perf_counter()
    dev = surface_deviation(v_ref, t_ref, v_cand, t_cand, tol)
    fcolors = (deviation_colors(dev["face_distance"], tol)
               if dev.get("face_distance") is not None else None)
    ref_face_dev = dev.get("ref_face_distance")
    ref_over = (ref_face_dev > tol) if ref_face_dev is not None else None
    timings["deviation_s"] = time.perf_counter() - t_dev
    t_views = time.perf_counter()
    for name, rot in cameras:
        cu, cv, scale, _ = tight_view_frame([v_ref, v_cand], rot, center, 384, 384, pad=16)
        ref_colors = normal_face_colors(n_ref, rot)
        if ref_over is not None and np.any(ref_over):
            ref_colors = ref_colors.copy()
            ref_colors[ref_over] = np.asarray(MISSING_COLOR, dtype=np.uint8)
        rgb_ref, _, _, _ = render_view(v_ref, t_ref, n_ref, rot, center, scale,
                                       w=384, h=384, view_center=(cu, cv),
                                       face_colors=ref_colors, need_depth=False)
        ref_imgs.append(rgb_ref)
        if fcolors is not None:
            rgb_dev, _, _, _ = render_view(v_cand, t_cand, n_cand, rot, center, scale,
                                           w=384, h=384, view_center=(cu, cv),
                                           face_colors=fcolors, need_depth=False)
        else:
            rgb_dev = np.zeros_like(rgb_ref)
        dev_imgs.append(rgb_dev)
    timings["pca_views_s"] = time.perf_counter() - t_views

    clusters = []
    cluster_imgs = []
    t_clu = time.perf_counter()
    if dev.get("face_distance") is not None:
        clusters.extend(deviation_clusters(v_cand, t_cand, dev["face_distance"], tol))
        for c in clusters:
            c["side"] = "candidate"
    if ref_face_dev is not None:
        ref_clusters = deviation_clusters(v_ref, t_ref, ref_face_dev, tol)
        for c in ref_clusters:
            c["side"] = "reference"
        clusters.extend(ref_clusters)
    clusters.sort(key=lambda c: (c["score"], c["max_dev"]), reverse=True)
    clusters = clusters[:V2_CLUSTER_TOP]
    for i, cl in enumerate(clusters):
        if cl["side"] == "reference":
            src_v, src_t, src_n = v_ref, t_ref, n_ref
        else:
            src_v, src_t, src_n = v_cand, t_cand, n_cand
        normals_c = src_n[cl["faces"]]
        nmean = normals_c.mean(axis=0) if len(normals_c) else np.array([0.0, 0.0, 1.0])
        nm = np.linalg.norm(nmean)
        nmean = nmean / nm if nm >= 1e-9 else np.array([0.0, 0.0, 1.0])
        rgb, _ = render_cluster_zoom(
            src_v, src_t, fcolors if cl["side"] == "candidate" else None,
            cl["center"], cl["radius"], nmean,
            normals=src_n if cl["side"] == "reference" else None,
            missing_mask=ref_over if cl["side"] == "reference" else None)
        cluster_imgs.append(rgb)
        if cluster_dir:
            os.makedirs(cluster_dir, exist_ok=True)
            save_rgb_png(rgb, os.path.join(cluster_dir, "cluster_%d_%s.png" % (
                i + 1, cl["side"])))
    timings["clusters_s"] = time.perf_counter() - t_clu

    t_sharp = time.perf_counter()
    sharp = sharp_edge_ratio(v_ref, t_ref, n_ref, v_cand, t_cand, n_cand)
    timings["sharp_s"] = time.perf_counter() - t_sharp
    t_slice = time.perf_counter()
    slices = slice_compare(v_ref, t_ref, v_cand, t_cand)
    timings["slices_s"] = time.perf_counter() - t_slice

    ious = [s["iou"] for s in sil_views]
    xr_ok = [x for x in xray_views if x and "error" not in x]
    leak_cand = int(sum(x["leak_cand_px"] for x in xr_ok))
    leak_ref = int(sum(x["leak_ref_px"] for x in xr_ok))
    leak_delta = leak_cand - leak_ref
    extra = int(sum(x["extra_layer_px"] for x in xr_ok))
    missing = int(sum(x["missing_layer_px"] for x in xr_ok))
    layer_delta = float(np.mean([x["mean_abs_layer_delta"] for x in xr_ok])) if xr_ok else 0.0
    max_layers_ref = int(max((x["ref_max_layers"] for x in xr_ok), default=0))
    max_layers_cand = int(max((x["cand_max_layers"] for x in xr_ok), default=0))

    flags = []
    reasons = []
    if min(ious) < V2_THRESHOLDS["silhouette_iou"]:
        flags.append("V2_SILHOUETTE")
        reasons.append("silhouette IoU %.4f < %.3f" % (
            min(ious), V2_THRESHOLDS["silhouette_iou"]))
    if xr_ok and leak_delta > V2_THRESHOLDS["xray_leak_px"]:
        flags.append("V2_XRAY_LEAK")
        reasons.append("X-ray leak worse: %d odd-crossing rays (ref %d, +%d)" % (
            leak_cand, leak_ref, leak_delta))
    if xr_ok and layer_delta > V2_THRESHOLDS["xray_layer_delta"]:
        flags.append("V2_XRAY_LAYERS")
        reasons.append("X-ray layer delta %.3f > %.3f" % (
            layer_delta, V2_THRESHOLDS["xray_layer_delta"]))
    if slices["max_area_rel"] > V2_THRESHOLDS["slice_area_rel"]:
        flags.append("V2_SLICE_AREA")
        reasons.append("slice area delta %.2f%% > %.1f%%" % (
            100.0 * slices["max_area_rel"], 100.0 * V2_THRESHOLDS["slice_area_rel"]))
    if slices["max_loop_delta"] > V2_THRESHOLDS["slice_loop_delta"]:
        flags.append("V2_SLICE_LOOPS")
        reasons.append("slice loop delta %d" % slices["max_loop_delta"])
    if sharp["ratio"] is not None and sharp["ratio"] < V2_THRESHOLDS["sharp_edge_ratio"]:
        flags.append("V2_SHARP_EDGES")
        reasons.append("sharp-edge ratio %.3f < %.2f" % (
            sharp["ratio"], V2_THRESHOLDS["sharp_edge_ratio"]))
    if dev.get("max") is not None and dev["max"] > (
            V2_THRESHOLDS["deviation_max_tol_factor"] * tol):
        flags.append("V2_DEVIATION")
        reasons.append("max deviation %.4f mm > %.1f x tol" % (
            dev["max"], V2_THRESHOLDS["deviation_max_tol_factor"]))

    timings["sheet_s"] = 0.0
    runtime_s = time.perf_counter() - t0
    result = {
        "runtime_s": runtime_s,
        "timings": timings,
        "bbox_diagonal": diag,
        "tolerance": tol,
        "pca_eigenvalues": [float(x) for x in compute_pca_axes(all_v)[0]],
        "silhouette": {
            "views": sil_views,
            "mean_iou": float(np.mean(ious)) if ious else 1.0,
            "min_iou": float(np.min(ious)) if ious else 1.0,
        },
        "xray": {
            "views": xray_views,
            "leak_ref_px": leak_ref,
            "leak_cand_px": leak_cand,
            "leak_delta_px": leak_delta,
            "extra_layer_px": extra,
            "missing_layer_px": missing,
            "mean_layer_delta": layer_delta,
            "max_layers_ref": max_layers_ref,
            "max_layers_cand": max_layers_cand,
            "ran": bool(xr_ok),
        },
        "slices": slices,
        "sharp_edges": sharp,
        "deviation": {k: v for k, v in dev.items()
                      if k not in ("vertex_distance", "face_distance",
                                   "ref_vertex_distance", "ref_face_distance")},
        "clusters": [{**{k: (c[k].tolist() if isinstance(c[k], np.ndarray) else c[k])
                         for k in ("face_count", "area", "max_dev", "mean_dev",
                                   "center", "radius", "score")},
                      "side": c["side"]}
                     for c in clusters],
        "flags": flags,
        "reason": "; ".join(reasons) if reasons else "V2 metrics within provisional thresholds",
        "thresholds_provisional": V2_THRESHOLDS,
    }

    if out_sheet:
        rows = [
            {"name": "PCA / reference", "images": ref_imgs,
             "captions": [c[0] for c in cameras]},
            {"name": "PCA / candidate dev", "images": dev_imgs,
             "captions": ["%s dev" % c[0].split()[-1] for c in cameras]},
            {"name": "X-ray ref", "images": xray_rows_ref,
             "captions": ["%s parity" % c[0].split()[-1] for c in cameras]},
            {"name": "X-ray cand", "images": xray_rows_delta,
             "captions": ["%s layers" % c[0].split()[-1] for c in cameras]},
            {"name": "Top clusters", "images": cluster_imgs,
             "captions": ["%s %d (%.2f mm)" % (c["side"], i + 1, c["max_dev"])
                          for i, c in enumerate(clusters)]},
        ]
        mesh_tag = os.path.splitext(os.path.basename(out_sheet))[0]
        title = [
            "Visual QA v2: %s | flags: %s" % (
                mesh_tag, ", ".join(flags) if flags else "NONE"),
            "silhouette min %.4f | xray leak %d / layers %.3f | slice area %.2f%% | "
            "sharp %.3f | two-sided dev max %.4f mm | %.1f s" % (
                min(ious) if ious else 1.0, leak_cand, layer_delta,
                100.0 * slices["max_area_rel"],
                sharp["ratio"] if sharp["ratio"] is not None else float("nan"),
                dev["max"] if dev.get("max") is not None else float("nan"),
                runtime_s),
        ]
        t_sheet = time.perf_counter()
        result["contact_sheet"] = assemble_v2_sheet(title, rows, out_sheet, tol=tol)
        timings["sheet_s"] = time.perf_counter() - t_sheet
        result["runtime_s"] = time.perf_counter() - t0
        if cluster_dir:
            result["cluster_dir"] = cluster_dir
    return result


# --------------------------------------------------------------------------- #
# Contact Sheet Generator & Image Exporter (QPainter / QImage)
# --------------------------------------------------------------------------- #
def save_rgb_png(arr_rgb, path):
    """Save (H, W, 3) uint8 numpy array to PNG via QImage."""
    if not HAS_PYSIDE6:
        raise RuntimeError("PySide6 is required to save PNG images")
    h, w, _ = arr_rgb.shape
    arr_c = np.ascontiguousarray(arr_rgb, dtype=np.uint8)
    qimg = QImage(arr_c.data, w, h, w * 3, QImage.Format_RGB888)
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    return qimg.save(path, "PNG")


def assemble_contact_sheet(title_lines, views_data, col_names, out_path, detail_data=None):
    """Assemble a multi-view contact sheet image and write to PNG using QPainter."""
    if not HAS_PYSIDE6:
        raise RuntimeError("PySide6 is required to generate contact sheet")

    ensure_qapp()

    # Base canonical views (256x256)
    n_canonical = len(views_data)
    has_detail = detail_data is not None
    n_rows = n_canonical + (1 if has_detail else 0)
    n_cols = len(col_names)

    cell_w, cell_h = 256, 256
    pad = 12
    hdr_h = 70
    row_lbl_w = 90

    total_w = row_lbl_w + n_cols * cell_w + (n_cols + 1) * pad
    total_h = hdr_h + n_rows * cell_h + (n_rows + 1) * pad

    canvas = QImage(total_w, total_h, QImage.Format_RGB32)
    canvas.fill(QColor(25, 27, 31))

    p = QPainter(canvas)
    p.setRenderHint(QPainter.TextAntialiasing)

    p.setPen(QColor(240, 240, 245))
    p.drawText(pad, 26, title_lines[0])
    if len(title_lines) > 1:
        p.setPen(QColor(160, 165, 175))
        p.drawText(pad, 48, title_lines[1])

    p.setPen(QColor(200, 210, 225))
    for c, cname in enumerate(col_names):
        cx = row_lbl_w + pad + c * (cell_w + pad)
        p.drawText(cx + 10, hdr_h - 14, cname)

    # Canonical rows
    for r, rdata in enumerate(views_data):
        ry = hdr_h + pad + r * (cell_h + pad)
        p.setPen(QColor(180, 185, 195))
        p.drawText(10, ry + cell_h // 2 + 4, rdata["view_name"])

        for c, img_arr in enumerate(rdata["images"]):
            cx = row_lbl_w + pad + c * (cell_w + pad)
            ih, iw, _ = img_arr.shape
            img_c = np.ascontiguousarray(img_arr, dtype=np.uint8)
            q_img = QImage(img_c.data, iw, ih, iw * 3, QImage.Format_RGB888)
            p.drawImage(cx, ry, q_img)

            if "captions" in rdata and c < len(rdata["captions"]):
                cap = rdata["captions"][c]
                if cap:
                    p.fillRect(QRect(cx, ry + cell_h - 22, cell_w, 22), QColor(0, 0, 0, 180))
                    p.setPen(QColor(230, 230, 230))
                    p.drawText(cx + 6, ry + cell_h - 6, cap)

    # Zoomed detail row (rescaled for grid, full 512px saved separately)
    if has_detail:
        ry = hdr_h + pad + n_canonical * (cell_h + pad)
        p.setPen(QColor(255, 215, 0))
        p.drawText(10, ry + cell_h // 2 + 4, detail_data["view_name"])
        for c, img_arr in enumerate(detail_data["images"]):
            cx = row_lbl_w + pad + c * (cell_w + pad)
            ih, iw, _ = img_arr.shape
            img_c = np.ascontiguousarray(img_arr, dtype=np.uint8)
            q_detail = QImage(img_c.data, iw, ih, iw * 3, QImage.Format_RGB888)
            if (iw, ih) != (cell_w, cell_h):
                q_detail = q_detail.scaled(cell_w, cell_h, Qt.IgnoreAspectRatio, Qt.SmoothTransformation)
            p.drawImage(cx, ry, q_detail)
            if "captions" in detail_data and c < len(detail_data["captions"]):
                cap = detail_data["captions"][c]
                if cap:
                    p.fillRect(QRect(cx, ry + cell_h - 22, cell_w, 22), QColor(0, 0, 0, 180))
                    p.setPen(QColor(255, 225, 100))
                    p.drawText(cx + 6, ry + cell_h - 6, cap)

    p.end()
    os.makedirs(os.path.dirname(os.path.abspath(out_path)), exist_ok=True)
    canvas.save(out_path, "PNG")
    return out_path


# --------------------------------------------------------------------------- #
# Core Evaluation API
# --------------------------------------------------------------------------- #
def evaluate_mesh_quality(ref_path, cand_path, cameras=None, w=256, h=256):
    """Evaluate candidate mesh against reference mesh across a camera rig."""
    if cameras is None:
        cameras = get_dodecahedron_cameras()
    v_ref, t_ref, n_ref, def_ref = load_mesh(ref_path)
    v_cand, t_cand, n_cand, _ = load_mesh(cand_path)
    return _evaluate_quality_arrays(v_ref, t_ref, n_ref, def_ref,
                                    v_cand, t_cand, n_cand, cameras, w, h)


def _evaluate_quality_arrays(v_ref, t_ref, n_ref, def_ref,
                             v_cand, t_cand, n_cand, cameras, w=256, h=256):
    """V1 comparison for already-loaded arrays (shared by the three-way API)."""
    center, scale, extent = compute_shared_frame([v_ref, v_cand], w=w, h=h, pad=16)

    view_metrics = []
    view_renders = []

    for name, rot in cameras:
        t0 = time.perf_counter()
        norm_ref, depth_ref, sil_ref, def_mask_2d = render_view(
            v_ref, t_ref, n_ref, rot, center, scale, w=w, h=h, defect_vidx=def_ref)
        norm_cand, depth_cand, sil_cand, _ = render_view(
            v_cand, t_cand, n_cand, rot, center, scale, w=w, h=h)
        dt = time.perf_counter() - t0

        m = compare_views(norm_ref, depth_ref, sil_ref, norm_cand, depth_cand, sil_cand,
                          defect_mask_ref=def_mask_2d)
        m["time_ms"] = dt * 1000.0
        m["camera"] = name
        view_metrics.append(m)

        diff = create_diff_map(norm_ref, sil_ref, norm_cand, sil_cand)
        view_renders.append({
            "name": name,
            "norm_ref": norm_ref,
            "norm_cand": norm_cand,
            "diff": diff,
            "metrics": m
        })

    scores_full = [m["view_score"] for m in view_metrics]
    mean_s = float(np.mean(scores_full))
    min_s = float(np.min(scores_full))
    # 50% mean, 50% worst view penalty
    final_vqa_full = round(0.50 * mean_s + 0.50 * min_s, 1)

    scores_h = [m["view_score_healthy"] for m in view_metrics]
    mean_sh = float(np.mean(scores_h))
    min_sh = float(np.min(scores_h))
    final_vqa_healthy = round(0.50 * mean_sh + 0.50 * min_sh, 1)

    agg = {
        "vqa_score": final_vqa_full,
        "vqa_score_healthy": final_vqa_healthy,
        "mean_view_score": round(mean_s, 1),
        "min_view_score": round(min_s, 1),
        "mean_iou": float(np.mean([m["iou"] for m in view_metrics])),
        "mean_p_excess": float(np.mean([m["p_excess"] for m in view_metrics])),
        "max_p_excess": float(np.max([m["p_excess"] for m in view_metrics])),
        "mean_p_missing": float(np.mean([m["p_missing"] for m in view_metrics])),
        "max_p_missing": float(np.max([m["p_missing"] for m in view_metrics])),
        "mean_ssim": float(np.mean([m["normal_ssim"] for m in view_metrics])),
        "min_ssim": float(np.min([m["normal_ssim"] for m in view_metrics])),
        "mean_angle_err_deg": float(np.mean([m["angle_err_deg"] for m in view_metrics])),
        "mean_depth_mae": float(np.mean([m["depth_mae"] for m in view_metrics])),
        "total_render_ms": float(np.sum([m["time_ms"] for m in view_metrics]))
    }
    return agg, view_renders


def evaluate_visual_diff(in_path, base_path, cand_path, out_sheet=None, defect_center=None,
                         w=256, h=256, tolerance=None, enable_v2=True):
    """Three-way comparison: Input vs Baseline vs Candidate.

    Returns dict with metrics, separate flags, masked/unmasked scores, and contact sheet.
    ``v2`` adds the PCA/silhouette/X-ray/slice/sharp-edge/deviation metrics and
    images; the V1 fields are unchanged.
    """
    ref_path = in_path if (in_path and os.path.exists(in_path)) else base_path
    dodec = get_dodecahedron_cameras()

    t_stage = time.perf_counter()
    v_ref, t_ref, n_ref, def_ref = load_mesh(ref_path)
    same_base = os.path.abspath(base_path) == os.path.abspath(ref_path)
    if not same_base:
        try:
            same_base = os.path.samefile(base_path, ref_path)
        except OSError:
            same_base = False
    if same_base:
        v_base, t_base, n_base = v_ref, t_ref, n_ref
    else:
        v_base, t_base, n_base, _ = load_mesh(base_path)
    v_cand, t_cand, n_cand, _ = load_mesh(cand_path)
    base_eval, _ = _evaluate_quality_arrays(v_ref, t_ref, n_ref, def_ref,
                                            v_base, t_base, n_base, dodec, w, h)
    cand_eval, _ = _evaluate_quality_arrays(v_ref, t_ref, n_ref, def_ref,
                                            v_cand, t_cand, n_cand, dodec, w, h)
    timings = {"v1_dodeca_s": time.perf_counter() - t_stage}

    delta_vqa = cand_eval["vqa_score"] - base_eval["vqa_score"]
    delta_ssim = cand_eval["mean_ssim"] - base_eval["mean_ssim"]
    delta_excess = cand_eval["max_p_excess"] - base_eval["max_p_excess"]
    delta_missing = cand_eval["max_p_missing"] - base_eval["max_p_missing"]

    # Separate flags
    flags = []
    reasons = []

    # 1. Excess Foreground (Floating debris / unwanted swelling)
    if cand_eval["max_p_excess"] >= 0.04 or delta_excess >= 0.04:
        flags.append("EXCESS_FOREGROUND")
        reasons.append(f"EXCESS_FOREGROUND: +{cand_eval['max_p_excess']*100:.1f}% excess pixels (vs {base_eval['max_p_excess']*100:.1f}%)")

    # 2. Missing Foreground (Closed through-hole / lost feature)
    if cand_eval["max_p_missing"] >= 0.04 or delta_missing >= 0.04:
        flags.append("MISSING_FOREGROUND")
        reasons.append(f"MISSING_FOREGROUND: -{cand_eval['max_p_missing']*100:.1f}% missing pixels (vs {base_eval['max_p_missing']*100:.1f}%)")

    # 3. Voxel Stepping / Wrinkling
    if delta_ssim <= -0.06 or cand_eval["mean_ssim"] <= 0.88:
        flags.append("VOXEL_STEPPING")
        reasons.append(f"VOXEL_STEPPING: normal SSIM dropped to {cand_eval['mean_ssim']:.3f} (vs {base_eval['mean_ssim']:.3f})")

    # 4. Surface Distortion
    if cand_eval["min_ssim"] < base_eval["min_ssim"] - 0.12:
        flags.append("SURFACE_DISTORTION")
        reasons.append(f"SURFACE_DISTORTION: worst-angle SSIM collapsed to {cand_eval['min_ssim']:.3f} (vs {base_eval['min_ssim']:.3f})")

    # 5. Score drop threshold
    if delta_vqa <= -5.0:
        if "SCORE_DROP" not in flags:
            flags.append("SCORE_DROP")
        reasons.append(f"VQA score fell: {cand_eval['vqa_score']} vs {base_eval['vqa_score']} (Δ={delta_vqa:+.1f})")

    is_worse = len(flags) > 0
    reason_str = "; ".join(reasons) if reasons else "visual quality preserved"

    # Zoomed 512px Detail Render for boundary/flagged cases
    need_detail = is_worse or cand_eval["vqa_score"] < 80.0 or delta_vqa <= -3.0 or out_sheet is not None
    detail_row = None
    detail_png_saved = None

    t_stage = time.perf_counter()
    if need_detail:
        f_center, f_radius = find_most_changed_region(v_base, v_cand, custom_center=defect_center)

        # 512px detail renders
        det_ref_b, det_base, det_diff_b = render_zoomed_detail(
            v_ref, t_ref, n_ref, v_base, t_base, n_base, f_center, f_radius, res=512)
        det_ref_c, det_cand, det_diff_c = render_zoomed_detail(
            v_ref, t_ref, n_ref, v_cand, t_cand, n_cand, f_center, f_radius, res=512)

        if out_sheet:
            detail_png_saved = out_sheet.replace(".png", "_detail_zoom_512.png")
            # Save full 512px candidate detail diff
            save_rgb_png(det_diff_c, detail_png_saved)

            detail_row = {
                "view_name": "Detail 512px",
                "images": [det_ref_c, det_base, det_diff_b, det_cand, det_diff_c],
                "captions": [
                    "Zoom Ref",
                    "Zoom Base",
                    "Zoom Diff Base",
                    "Zoom Cand",
                    "Zoom Diff Cand"
                ]
            }

    timings["v1_detail_s"] = time.perf_counter() - t_stage
    # Assemble Contact Sheet if requested
    sheet_saved = None
    if out_sheet:
        t_stage = time.perf_counter()
        canonical = get_canonical_cameras()
        _, base_can_renders = _evaluate_quality_arrays(
            v_ref, t_ref, n_ref, def_ref, v_base, t_base, n_base, canonical, w, h)
        _, cand_can_renders = _evaluate_quality_arrays(
            v_ref, t_ref, n_ref, def_ref, v_cand, t_cand, n_cand, canonical, w, h)
        timings["v1_canonical_s"] = time.perf_counter() - t_stage

        views_data = []
        for i, (cam_name, _) in enumerate(canonical):
            rb = base_can_renders[i]
            rc = cand_can_renders[i]
            views_data.append({
                "view_name": cam_name,
                "images": [rb["norm_ref"], rb["norm_cand"], rb["diff"], rc["norm_cand"], rc["diff"]],
                "captions": [
                    "Input / Ref",
                    f"Baseline (IoU:{rb['metrics']['iou']:.2f})",
                    f"Diff Base (SSIM:{rb['metrics']['normal_ssim']:.2f})",
                    f"Candidate (IoU:{rc['metrics']['iou']:.2f})",
                    f"Diff Cand (SSIM:{rc['metrics']['normal_ssim']:.2f})"
                ]
            })
        col_names = [
            "Input / Reference",
            "Baseline Output",
            "Diff (Ref vs Base)",
            "Candidate Output",
            "Diff (Ref vs Cand)"
        ]
        mesh_tag = os.path.splitext(os.path.basename(cand_path))[0]
        title = [
            f"Visual QA: {mesh_tag} | Flags: {', '.join(flags) if flags else 'NONE'}",
            f"Baseline: {base_eval['vqa_score']}/100 (healthy: {base_eval['vqa_score_healthy']}) | "
            f"Candidate: {cand_eval['vqa_score']}/100 (healthy: {cand_eval['vqa_score_healthy']}) | {reason_str}"
        ]
        t_stage = time.perf_counter()
        sheet_saved = assemble_contact_sheet(title, views_data, col_names, out_sheet, detail_data=detail_row)
        timings["v1_sheet_s"] = time.perf_counter() - t_stage

    v2 = None
    if enable_v2:
        v2_sheet = None
        cluster_dir = None
        if out_sheet:
            stem = out_sheet[:-4] if out_sheet.lower().endswith('.png') else out_sheet
            v2_sheet = stem + "_v2.png"
            cluster_dir = stem + "_v2_clusters"
        try:
            if tolerance is not None:
                tol = float(tolerance)
            else:
                all_v = np.concatenate([v_ref, v_cand], axis=0)
                diag = float(np.linalg.norm(all_v.max(axis=0) - all_v.min(axis=0)))
                tol = min(0.05, max(0.0125, diag / 10000.0))
            v2 = evaluate_v2(v_ref, t_ref, n_ref, v_cand, t_cand, n_cand,
                             tol, out_sheet=v2_sheet, cluster_dir=cluster_dir)
        except Exception as ex:  # noqa: BLE001 - V2 must never break the V1 report
            v2 = {"error": str(ex)}

    return {
        "is_visual_worse": is_worse,
        "flags": flags,
        "reason": reason_str,
        "delta_vqa": delta_vqa,
        "baseline": base_eval,
        "candidate": cand_eval,
        "contact_sheet": sheet_saved,
        "detail_sheet_512": detail_png_saved,
        "v2": v2,
        "timings": timings
    }


# --------------------------------------------------------------------------- #
# CLI Entry Point
# --------------------------------------------------------------------------- #
def main():
    ap = argparse.ArgumentParser(description="Independent visual QA for mesh repair")
    ap.add_argument("baseline", help="Baseline mesh STL/OBJ")
    ap.add_argument("candidate", help="Candidate mesh STL/OBJ")
    ap.add_argument("--input", default=None, help="Optional original/clean input mesh")
    ap.add_argument("--contact-sheet", default=None, help="Save contact sheet PNG")
    ap.add_argument("--defect-center", default=None, help="Optional focal center X,Y,Z")
    ap.add_argument("--res", type=int, default=256, help="Render resolution (default 256)")
    ap.add_argument("--json", action="store_true", help="Print structured JSON")
    ap.add_argument("--tolerance", type=float, default=None,
                    help="Deviation tolerance in mesh units (default clamp(diag/10000, 0.0125, 0.05))")
    ap.add_argument("--no-v2", dest="v2", action="store_false",
                    help="Disable the V2 PCA/X-ray/slice/deviation metrics")
    args = ap.parse_args()

    if not HAS_PYSIDE6:
        print("error: PySide6 is required for verify_visual_qa.py", file=sys.stderr)
        return 2

    if not os.path.exists(args.baseline) or not os.path.exists(args.candidate):
        print("error: baseline or candidate mesh file does not exist", file=sys.stderr)
        return 2

    center = None
    if args.defect_center:
        try:
            center = [float(x) for x in args.defect_center.split(",")]
        except Exception:
            pass

    res = evaluate_visual_diff(args.input, args.baseline, args.candidate,
                               out_sheet=args.contact_sheet, defect_center=center,
                               w=args.res, h=args.res, tolerance=args.tolerance,
                               enable_v2=args.v2)

    if args.json:
        print(json.dumps(res, indent=2))
    else:
        b = res["baseline"]
        c = res["candidate"]
        print(f"Baseline  VQA: {b['vqa_score']:5.1f} (healthy: {b['vqa_score_healthy']:5.1f}) | SSIM: {b['mean_ssim']:.3f} | AngErr: {b['mean_angle_err_deg']:.1f}° | DepthMAE: {b['mean_depth_mae']:.4f}")
        print(f"Candidate VQA: {c['vqa_score']:5.1f} (healthy: {c['vqa_score_healthy']:5.1f}) | SSIM: {c['mean_ssim']:.3f} | AngErr: {c['mean_angle_err_deg']:.1f}° | DepthMAE: {c['mean_depth_mae']:.4f}")
        print(f"Flags: {', '.join(res['flags']) if res['flags'] else 'NONE'}")
        print(f"ΔVQA: {res['delta_vqa']:+.1f} | Status: {'VISUAL-WORSE' if res['is_visual_worse'] else 'PASS'} ({res['reason']})")
        if res.get("contact_sheet"):
            print(f"Contact sheet: {res['contact_sheet']}")
        if res.get("detail_sheet_512"):
            print(f"Detail sheet (512px): {res['detail_sheet_512']}")
        v2 = res.get("v2")
        if v2 and "error" in v2:
            print(f"V2: ERROR ({v2['error']})")
        elif v2:
            print("V2 silhouette: mean %.4f min %.4f | X-ray: leak %d (ref %d) layers %.3f "
                  "extra %d | slices: area %.2f%% loops %d | sharp %.3f | dev %.4f mm | %.2f s" % (
                      v2["silhouette"]["mean_iou"], v2["silhouette"]["min_iou"],
                      v2["xray"]["leak_cand_px"], v2["xray"]["leak_ref_px"],
                      v2["xray"]["mean_layer_delta"], v2["xray"]["extra_layer_px"],
                      100.0 * v2["slices"]["max_area_rel"],
                      v2["slices"]["max_loop_delta"],
                      v2["sharp_edges"]["ratio"] if v2["sharp_edges"]["ratio"] is not None
                      else float("nan"),
                      v2["deviation"]["max"] if v2["deviation"]["max"] is not None
                      else float("nan"),
                      v2["runtime_s"]))
            print(f"V2 flags: {', '.join(v2['flags']) if v2['flags'] else 'NONE'} ({v2['reason']})")
            if v2.get("contact_sheet"):
                print(f"V2 contact sheet: {v2['contact_sheet']}")
            if v2.get("cluster_dir"):
                print(f"V2 cluster zooms: {v2['cluster_dir']}")

    return 1 if res["is_visual_worse"] else 0


if __name__ == "__main__":
    sys.exit(main())
