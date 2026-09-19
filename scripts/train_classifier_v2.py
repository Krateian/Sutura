#!/usr/bin/env python3
"""Train + evaluate the v2 classifier head (RANSAC + logistic regression).

Builds a labeled feature matrix from:
  * the 35-mesh synthetic set (tests/make_classifier_set.py, labels as-is)
  * the 40-mesh real-world corpus (tests/real-world-samples/, mechanical
    scans correctly labeled mechanical -- the documented known-limitation
    files; unlabeled/unknown meshes are EXCLUDED from training but shown in
    the comparison table)

Features per mesh: [near90, flat, gentle, plane_count, plane_area,
developable_fraction] where plane_* come from mesh_classifier_v2's RANSAC
plane segmentation and developable_fraction from its curvature signal.

Reports:
  1. classic engine confusion on the labeled set (baseline)
  2. v2 classic-with-RANSAC-metrics decision (head not baked) confusion
  3. logistic head fitted with numpy gradient descent (no scipy), evaluated
     with LEAVE-ONE-OUT cross-validation -- the honest small-data estimate;
     the overfitting risk is stated explicitly because n is tiny (~40)
  4. final weights fit on the full labeled set, printed as the constants to
     bake into mesh_classifier_v2.py

Usage (needs the venv: numpy + trimesh + pymeshlab):
    /opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python \
        scripts/train_classifier_v2.py

No files are written. Read-only measurement tool.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
TESTS = os.path.join(REPO, 'tests')
for p in (SUTURA, TESTS):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402

import pymeshlab as ml  # noqa: E402
import mesh_classifier as classic  # noqa: E402
import mesh_classifier_v2 as v2  # noqa: E402
from make_classifier_set import iter_meshes  # noqa: E402

SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')

# Real-world truth labels. The artec mechanical scans are the documented
# known-limitation files (classic reads them organic); here they carry their
# TRUE label. Unknown-classified meshes are excluded from training.
REAL_LABELS = {
    'artec_spanner.stl': 'mechanical',
    'artec_pipe-bend.stl': 'mechanical',
    'artec_metal-nut.stl': 'mechanical',
    'artec_copper-key.stl': 'mechanical',
    'artec_plastic-bolt.stl': 'mechanical',
    'artec_crocodile-statue.stl': 'organic',
    'artec_bovine-heart.stl': 'organic',
    'artec_lobster-hd.stl': 'organic',
    'thingi10k_100077.stl': 'mechanical',
    'thingi10k_1038439.stl': 'mechanical',
    'thingi10k_100173.stl': 'organic',
    'thingi10k_1038441.stl': 'organic',
    # Phase 2 additions (Thingi10K, CC-BY/CC0 -- see
    # tests/real-world-samples/ATTRIBUTION.md). A deliberately hard mix:
    # curved/free-form mechanical parts and free-form organics.
    'thingi10k_55772.stl': 'mechanical',    # Spiral Panpipes (curved)
    'thingi10k_71691.stl': 'mechanical',    # Pirate Hook (curved)
    'thingi10k_235725.stl': 'mechanical',   # Threadless Ball Screw
    'thingi10k_81221.stl': 'mechanical',    # Nautilus Gears (curved shells)
    'thingi10k_228302.stl': 'mechanical',   # Geared Coffee Sleeve
    'thingi10k_248395.stl': 'mechanical',   # HingeBox
    'thingi10k_42844.stl': 'mechanical',    # Ring Adapter
    'thingi10k_60916.stl': 'mechanical',    # Gear O'Clock
    'thingi10k_59226.stl': 'mechanical',    # Exploded Planetary Gear Set
    'thingi10k_70561.stl': 'mechanical',    # Tiny Planetary Gears (CC0)
    'thingi10k_57854.stl': 'mechanical',    # Panasonic Bracket
    'thingi10k_475828.stl': 'mechanical',   # Fennec Fox Head Drawer Handle
    'thingi10k_145065.stl': 'mechanical',   # Eiffel Tower (engineered lattice)
    'thingi10k_260537.stl': 'organic',      # Great White Skull
    'thingi10k_313444.stl': 'organic',      # Waving Cat
    'thingi10k_39507.stl': 'organic',       # Roal The Bratty Dragon
    'thingi10k_55280.stl': 'organic',       # Dragon (Artec scan)
    'thingi10k_40886.stl': 'organic',       # Fist Sculpture
    'thingi10k_100281.stl': 'organic',      # Holed Christmas Ornament
    'thingi10k_63785.stl': 'organic',       # Mouse skull (micro-CT)
    'thingi10k_136634.stl': 'organic',      # Decorative Cat Bowls
    'thingi10k_1356633.stl': 'organic',     # Gowanus Monster
    'thingi10k_331105.stl': 'organic',      # Angel Candle Holder
    'thingi10k_46012.stl': 'organic',       # Earth Shot
}
TRAIN_EXCLUDED = ('thingi10k_100045.stl', 'thingi10k_100827.stl',
                  'thingi10k_224108.stl', 'thingi10k_502009.stl')

FEATURES = ('near90', 'flat', 'gentle', 'plane_count', 'plane_area',
            'developable_fraction')


def load_stl(path):
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), dtype=np.float32),
            np.asarray(m.face_matrix(), dtype=np.int32))


def features_of(verts, tris, engine):
    r = engine.classify_mesh(verts, tris)
    m = r['metrics']
    return np.array([m['near90'], m['flat'], m['gentle'],
                     m['plane_count'], m['plane_area'],
                     m['developable_fraction']], dtype=np.float64)


def build_dataset():
    rows = []
    for mesh in iter_meshes():
        rows.append((mesh['name'], mesh['label'], mesh['verts'], mesh['tris']))
    for name in sorted(REAL_LABELS):
        v, t = load_stl(os.path.join(SAMPLES, name))
        rows.append((name, REAL_LABELS[name], v, t))
    return rows


def confusion(rows, engine, feature_names=None):
    """Rows: list of (name, label, verts, tris). Returns (table, summary)."""
    n_mech = sum(1 for r in rows if r[1] == 'mechanical')
    n_org = sum(1 for r in rows if r[1] == 'organic')
    correct = 0
    mech_correct = mech_total = 0
    org_correct = org_total = 0
    table = []
    for name, label, verts, tris in rows:
        pred = engine.classify_mesh(verts, tris)['type']
        table.append((name, label, pred))
        if pred == label:
            correct += 1
        if label == 'mechanical':
            mech_total += 1
            mech_correct += pred == 'mechanical'
        else:
            org_total += 1
            org_correct += pred == 'organic'
    return table, {
        'n': len(rows), 'correct': correct,
        'accuracy': correct / len(rows),
        'mech': '%d/%d' % (mech_correct, mech_total),
        'org': '%d/%d' % (org_correct, org_total),
    }


def print_table(title, table, summary, marks=None):
    print()
    print('=== %s ===' % title)
    print('  accuracy=%.3f (%d/%d)  mechanical=%s  organic=%s' % (
        summary['accuracy'], summary['correct'], summary['n'],
        summary['mech'], summary['org']))
    for name, label, pred in table:
        mark = ''
        if marks is not None and name in marks:
            mark = '  [KNOWN MECH-SCAN]'
        flag = '' if pred == label else '   <--'
        print('  %-28s truth=%-10s pred=%-10s%s%s' % (
            name, label, pred, flag, mark))


# ------------------------------------------------------------- logistic head

def _standardize(X, mean=None, std=None):
    if mean is None:
        mean = X.mean(axis=0)
    if std is None:
        std = X.std(axis=0)
    std[std == 0] = 1.0
    return (X - mean) / std, mean, std


def fit_logistic(X, y, l2=1e-3, lr=0.5, iters=2000, rng_seed=1):
    """Binary logistic regression (y in {0,1}), numpy gradient descent.
    Returns (weights, mean, std) so prediction reuses the training scaler."""
    rng = np.random.default_rng(rng_seed)
    n, d = X.shape
    Xs, mean, std = _standardize(X)
    Xb = np.hstack([np.ones((n, 1)), Xs])
    w = rng.normal(0, 0.01, d + 1)
    for _ in range(iters):
        z = Xb @ w
        p = 1.0 / (1.0 + np.exp(-np.clip(z, -50, 50)))
        grad = Xb.T @ (p - y) / n + l2 * w
        grad[0] -= l2 * w[0]  # no reg on bias
        w = w - lr * grad
    return w, mean, std


def predict_logistic(X, w, mean, std):
    Xs = (X - mean) / std
    Xb = np.hstack([np.ones((X.shape[0], 1)), Xs])
    return 1.0 / (1.0 + np.exp(-np.clip(Xb @ w, -50, 50)))


def leave_one_out(X, y, floors):
    """LOO-CV of the head decision (mech p>=floor, org p<=1-floor)."""
    n = len(y)
    loo = []
    for i in range(n):
        mask = np.ones(n, dtype=bool)
        mask[i] = False
        w, mean, std = fit_logistic(X[mask], y[mask])
        p = predict_logistic(X[i:i + 1], w, mean, std)[0]
        pred = 'mechanical' if p >= floors[0] else (
            'organic' if p <= 1 - floors[0] else 'unknown')
        loo.append(pred)
    return loo


def main():
    no_head = '--no-head' in sys.argv
    if no_head:
        saved = (v2.V2_WEIGHTS, v2.V2_MEAN, v2.V2_STD)
        v2.V2_WEIGHTS = v2.V2_MEAN = v2.V2_STD = None
    try:
        _run(no_head)
    finally:
        if no_head:
            v2.V2_WEIGHTS, v2.V2_MEAN, v2.V2_STD = saved
    return 0


def _run(no_head):
    rows = build_dataset()
    labels = np.array([1 if r[1] == 'mechanical' else 0 for r in rows])
    names = [r[0] for r in rows]
    mech_scans = set(n for n, l in REAL_LABELS.items() if l == 'mechanical')

    # classic baseline on the full labeled set
    t1, s1 = confusion(rows, classic)
    print_table('classic (baseline) on labeled set (n=%d)' % len(rows),
                t1, s1, mech_scans)

    # v2 current state (head baked if V2_WEIGHTS is set, classic otherwise).
    # With no baked head this must equal classic exactly (regression check).
    t2, s2 = confusion(rows, v2)
    print_table('v2 (head %s) on labeled set (n=%d)' % (
        'disabled' if no_head else 'baked', len(rows)), t2, s2, mech_scans)

    # feature matrix
    X = np.array([features_of(v, t, v2) for _, _, v, t in rows])
    Xs, mean, std = _standardize(X)

    # LOO-CV of the trained head over several floors
    print()
    print('=== logistic head LOO-CV (numpy, no scipy) ===')
    best = None
    for floor in (0.5, 0.55, 0.6, 0.65, 0.7):
        loo = leave_one_out(X, labels, (floor,))
        n_correct = sum(p == l for p, l in zip(loo, [r[1] for r in rows]))
        n_mech = sum(p == 'mechanical' and l == 'mechanical' for p, l in
                     zip(loo, [r[1] for r in rows]))
        n_org = sum(p == 'organic' and l == 'organic' for p, l in
                    zip(loo, [r[1] for r in rows]))
        n_mech_t = sum(l == 'mechanical' for l in [r[1] for r in rows])
        n_org_t = sum(l == 'organic' for l in [r[1] for r in rows])
        acc = n_correct / len(rows)
        print('  floor=%.2f  acc=%.3f (%d/%d)  mech=%d/%d  org=%d/%d' % (
            floor, acc, n_correct, len(rows), n_mech, n_mech_t, n_org, n_org_t))
        if best is None or acc > best[0]:
            best = (acc, floor, n_mech, n_mech_t, n_org, n_org_t)
    print('  -> best LOO floor: %.2f (acc=%.3f)' % (best[1], best[0]))
    print('  WARNING: n=%d total (28 synthetic + %d labeled real-world); '
          'LOO on this size is optimistic -- expect variance. The head is a '
          'weak signal, NOT a proof of generalization.' % (len(rows),
          len(REAL_LABELS)))

    # final model on ALL data, print baked constants
    w, mean, std = fit_logistic(X, labels)
    print()
    print('=== final model on full labeled set (to bake) ===')
    print('feature order: %s' % ', '.join(FEATURES))
    print('V2_WEIGHTS = [%s]' % ', '.join('%.4f' % x for x in w))
    print('V2_MEAN = [%s]' % ', '.join('%.4f' % x for x in mean))
    print('V2_STD  = [%s]' % ', '.join('%.4f' % x for x in std))
    print()
    print('Final-model predictions on the labeled set (overfitted check):')
    p = predict_logistic(X, w, mean, std)
    n_c = 0
    for (name, label, _, _), prob in zip(rows, p):
        pred = 'mechanical' if prob >= best[1] else ('organic' if prob <= 1 - best[1] else 'unknown')
        flag = '' if pred == label else '   <--'
        n_c += pred == label
        print('  %-28s truth=%-10s p_mech=%.3f pred=%-10s%s' % (
            name, label, prob, pred, flag))
    print('final-model accuracy on training set: %.3f (%d/%d)' % (
        n_c / len(rows), n_c, len(rows)))
    return 0


if __name__ == '__main__':
    sys.exit(main())