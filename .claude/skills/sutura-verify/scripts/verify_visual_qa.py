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
Requires only numpy and PySide6 (no scipy, no Pillow).
Never hardcodes private sample names or paths.
"""
import argparse
import json
import math
import os
import struct
import sys
import time

import numpy as np

# Ensure offscreen Qt platform before any QtGui import
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')

try:
    from PySide6.QtCore import QPoint, QRect, Qt
    from PySide6.QtGui import QColor, QFont, QGuiApplication, QImage, QPainter, QPolygon
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
# Headless CPU Rasterizer (QPainter Format_RGB32)
# --------------------------------------------------------------------------- #
def render_view(verts, tris, normals, rot, center, scale, w=256, h=256, defect_vidx=None):
    """Render normal map, depth map, silhouette and 2D defect mask."""
    if not HAS_PYSIDE6:
        raise RuntimeError("PySide6 is required for headless CPU rasterization")

    if len(tris) == 0:
        return (np.zeros((h, w, 3), dtype=np.uint8),
                np.zeros((h, w), dtype=np.float32),
                np.zeros((h, w), dtype=bool),
                np.zeros((h, w), dtype=bool))

    # Transform vertices to view space
    v_cam = (verts - center) @ rot.T
    px = w * 0.5 + v_cam[:, 0] * scale
    py = h * 0.5 - v_cam[:, 1] * scale
    z = v_cam[:, 2]

    # Depth sort
    z_face = z[tris].mean(axis=1)
    order = np.argsort(z_face)

    # Normals to view space
    n_cam = normals @ rot.T
    norm_colors = np.clip((n_cam * 0.5 + 0.5) * 255.0, 0, 255).astype(np.uint8)

    # Normalize depth
    z_min = float(z_face.min())
    z_max = float(z_face.max())
    z_span = (z_max - z_min) if (z_max - z_min) > 1e-6 else 1.0
    depth_vals = np.clip((z_face - z_min) / z_span * 255.0, 0, 255).astype(np.uint8)

    img_norm = QImage(w, h, QImage.Format_RGB32)
    img_norm.fill(0)
    p_norm = QPainter(img_norm)
    p_norm.setPen(Qt.NoPen)

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

        dc = depth_vals[idx]
        p_depth.setBrush(QColor(int(dc), int(dc), int(dc)))
        p_depth.drawPolygon(poly)

    p_norm.end()
    p_depth.end()

    raw_norm = np.frombuffer(img_norm.constBits(), dtype=np.uint8).reshape((h, w, 4))
    rgb_norm = raw_norm[:, :, [2, 1, 0]].copy()

    raw_depth = np.frombuffer(img_depth.constBits(), dtype=np.uint8).reshape((h, w, 4))
    val_depth = raw_depth[:, :, 0].copy().astype(np.float32) / 255.0

    silhouette = (rgb_norm[:, :, 0] > 0) | (rgb_norm[:, :, 1] > 0) | (rgb_norm[:, :, 2] > 0)

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
def compute_ssim(im1, im2, mask=None):
    """Compute channel-wise SSIM with Gaussian filtering."""
    im1 = im1.astype(np.float64)
    im2 = im2.astype(np.float64)
    C1 = (0.01 * 255.0) ** 2
    C2 = (0.03 * 255.0) ** 2

    ssim_channels = []
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
        ssim_map = num / den

        if mask is not None and mask.sum() > 0:
            ssim_channels.append(float(ssim_map[mask].mean()))
        else:
            ssim_channels.append(float(ssim_map.mean()))
    return float(np.mean(ssim_channels))


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
        ssim = compute_ssim(norm_ref, norm_cand, mask=inter_mask)
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
            ssim_h = compute_ssim(norm_ref, norm_cand, mask=healthy_inter)
            dot_h = np.clip(np.sum(n_ref * n_cand, axis=2), -1.0, 1.0)
            ang_h = float((np.arccos(dot_h) * (180.0 / np.pi))[healthy_inter].mean())
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

    # Exact nearest neighbor distance against full reference in chunks (no ref subsampling)
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


def evaluate_visual_diff(in_path, base_path, cand_path, out_sheet=None, defect_center=None, w=256, h=256):
    """Three-way comparison: Input vs Baseline vs Candidate.

    Returns dict with metrics, separate flags, masked/unmasked scores, and contact sheet.
    """
    ref_path = in_path if (in_path and os.path.exists(in_path)) else base_path
    dodec = get_dodecahedron_cameras()

    base_eval, _ = evaluate_mesh_quality(ref_path, base_path, cameras=dodec, w=w, h=h)
    cand_eval, _ = evaluate_mesh_quality(ref_path, cand_path, cameras=dodec, w=w, h=h)

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

    if need_detail:
        v_ref, t_ref, n_ref, _ = load_mesh(ref_path)
        v_base, t_base, n_base, _ = load_mesh(base_path)
        v_cand, t_cand, n_cand, _ = load_mesh(cand_path)

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

    # Assemble Contact Sheet if requested
    sheet_saved = None
    if out_sheet:
        canonical = get_canonical_cameras()
        _, base_can_renders = evaluate_mesh_quality(ref_path, base_path, cameras=canonical, w=w, h=h)
        _, cand_can_renders = evaluate_mesh_quality(ref_path, cand_path, cameras=canonical, w=w, h=h)

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
        sheet_saved = assemble_contact_sheet(title, views_data, col_names, out_sheet, detail_data=detail_row)

    return {
        "is_visual_worse": is_worse,
        "flags": flags,
        "reason": reason_str,
        "delta_vqa": delta_vqa,
        "baseline": base_eval,
        "candidate": cand_eval,
        "contact_sheet": sheet_saved,
        "detail_sheet_512": detail_png_saved
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
                               w=args.res, h=args.res)

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

    return 1 if res["is_visual_worse"] else 0


if __name__ == "__main__":
    sys.exit(main())
