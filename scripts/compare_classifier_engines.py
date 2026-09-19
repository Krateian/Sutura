#!/usr/bin/env python3
"""Compare the classic vs experimental (v2) classifier on the real-world corpus.

Runs ``mesh_classifier`` (classic) and ``mesh_classifier_v2`` (experimental,
RANSAC + trained head) over every STL in ``tests/real-world-samples/`` and
prints a before/after confusion table. The artec mechanical scans (spanner,
pipe-bend, metal-nut, copper-key, plastic-bolt) are the DOCUMENTED known
limitation: classic reads them as organic with high confidence; the v2 head
was trained to correct exactly those (truth label = mechanical). Meshes the
corpus README lists as ``unknown``/unclassified are shown for reference but
excluded from the accuracy counts.

This is the comparison gate for the v2 experiment: experimental must raise
mechanical recall on the scans with NO organic regression.

Usage (needs the venv: numpy + pymeshlab):
    /opt/homebrew/Caskroom/miniforge/base/envs/sutura-env/bin/python \
        scripts/compare_classifier_engines.py

Read-only; writes nothing.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
for p in (SUTURA,):
    if p not in sys.path:
        sys.path.insert(0, p)

import numpy as np  # noqa: E402
import pymeshlab as ml  # noqa: E402
import mesh_classifier as classic  # noqa: E402
import mesh_classifier_v2 as experimental  # noqa: E402

SAMPLES = os.path.join(REPO, 'tests', 'real-world-samples')

# truth labels (None = unknown/unclassified, excluded from accuracy counts)
LABELS = {
    'artec_spanner.stl': 'mechanical',
    'artec_pipe-bend.stl': 'mechanical',
    'artec_metal-nut.stl': 'mechanical',
    'artec_copper-key.stl': 'mechanical',
    'artec_plastic-bolt.stl': 'mechanical',
    'artec_crocodile-statue.stl': 'organic',
    'artec_bovine-heart.stl': 'organic',
    'artec_lobster-hd.stl': 'organic',
    'thingi10k_100045.stl': None,
    'thingi10k_100077.stl': 'mechanical',
    'thingi10k_100173.stl': 'organic',
    'thingi10k_100827.stl': None,
    'thingi10k_1038439.stl': 'mechanical',
    'thingi10k_1038441.stl': 'organic',
    'thingi10k_224108.stl': None,
    'thingi10k_502009.stl': None,
}


def load_stl(path):
    ms = ml.MeshSet()
    ms.load_new_mesh(path)
    m = ms.current_mesh()
    return (np.asarray(m.vertex_matrix(), dtype=np.float32),
            np.asarray(m.face_matrix(), dtype=np.int32))


def main():
    files = sorted(f for f in os.listdir(SAMPLES) if f.lower().endswith('.stl'))
    rows = []
    for name in files:
        v, t = load_stl(os.path.join(SAMPLES, name))
        pc = classic.classify_mesh(v, t)
        pe = experimental.classify_mesh(v, t)
        rows.append((name, LABELS.get(name), pc, pe))

    known = [r for r in rows if r[1] is not None]
    total = len(known)
    mech_t = sum(1 for r in known if r[1] == 'mechanical')
    org_t = total - mech_t
    c_correct = sum(1 for r in known if r[2]['type'] == r[1])
    e_correct = sum(1 for r in known if r[3]['type'] == r[1])
    c_mech = sum(1 for r in known if r[1] == 'mechanical' and r[2]['type'] == 'mechanical')
    e_mech = sum(1 for r in known if r[1] == 'mechanical' and r[3]['type'] == 'mechanical')
    c_org = sum(1 for r in known if r[1] == 'organic' and r[2]['type'] == 'organic')
    e_org = sum(1 for r in known if r[1] == 'organic' and r[3]['type'] == 'organic')

    print('=== real-world corpus: classic vs experimental (n=%d labeled) ===' % total)
    print('  classic      : acc=%d/%d  mech=%d/%d  org=%d/%d' % (
        c_correct, total, c_mech, mech_t, c_org, org_t))
    print('  experimental : acc=%d/%d  mech=%d/%d  org=%d/%d' % (
        e_correct, total, e_mech, mech_t, e_org, org_t))
    print('  mech-scan known-limitation files: %d of the %d labelled mechanical' % (
        sum(1 for r in known if r[1] == 'mechanical' and r[0].startswith('artec_')), mech_t))
    print()
    print('%-30s %-10s %-12s %-12s %-9s %-9s' % (
        'file', 'truth', 'classic', 'experimental', 'classic', 'exp'))
    for name, label, pc, pe in rows:
        tag = label or 'unlabelled'
        c_mark = '' if label is None else ('OK' if pc['type'] == label else 'MISS')
        e_mark = '' if label is None else ('OK' if pe['type'] == label else 'MISS')
        star = ' *' if label == 'mechanical' and name.startswith('artec_') else ''
        print('%-30s %-10s %-12s %-12s %-9s %-9s%s' % (
            name, tag, pc['type'], pe['type'], c_mark, e_mark, star))
    print()
    print('* = artec mechanical scan (classic known-limitation target)')
    return 0


if __name__ == '__main__':
    sys.exit(main())