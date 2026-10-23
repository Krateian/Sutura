# Mesh type-aware repair

[← Back to README](../../README.md) · [Türkçe](tr/mesh-classifier.md)


Sutura heuristically guesses whether an input mesh is **mechanical** (cube,
gear, CAD part) or **organic** (sculpt, scanned model) from pure geometry —
adjacent-face dihedral angles, computed in numpy. It is *not* an ML model and
is deliberately conservative: it only acts on high-confidence cases and
reports `unknown` otherwise, in which case the historical default Stage 1
parameters are used unchanged.

The confidence is a **signed-margin score**: three dihedral bands are computed
from adjacent-face angles — `near90` (`[60,120]°`, sharp edges), `flat` (`<1°`,
true planar surfaces) and `gentle` (`[1,15)°`, slight curvature) — and each is
mapped through a smooth sigmoid. The bands are combined as **mechanical =
max(near90 signal, flat signal)** (either is enough) and **organic = min(low
near90, low flat, high gentle)** (all three must hold), so the decision is a
soft margin rather than a single hard threshold — there is no sharp jump at
the `[55,60]` near90 boundary. The `flat`/`gentle` split is what keeps smooth
high-poly organic meshes from reading mechanical: their dihedrals are ~2–5°,
i.e. `gentle`, not `flat`. An `unknown` result still carries a non-zero
proximity value (which class the mesh leans toward, and how close) instead of
a flat 0, so even the fallback is informative.

The detected type is shown in the GUI defect-panel header (e.g. `Detected:
mechanical (0.92)`) and in the `--human` report as a `Type:` line; the JSON
report carries `detected_type` and `detected_confidence`. A calibration
harness (`scripts/calibrate_classifier.py`) measures precision/recall and
confidence separation against a labeled synthetic set
(`tests/make_classifier_set.py`), so the thresholds stay checkable and
reversible.

When classified, the type tunes two Stage 1 thresholds:

| Type | `mincomponentsize` (debris cutoff) | `maxholesize` (hole fill) | Effect |
|---|---|---|---|
| mechanical | 8 | 300 | preserve small sharp details, avoid oversized hole patches |
| organic | 12 | 1000 | drop scan debris more aggressively, close large open regions |
| unknown | 8 | 1000 | historical defaults (unchanged) |

> These per-type values are **experimental starting points**, not calibrated
> on real repair data — a conservative, reversible choice. Only the two
> thresholds above shift; they can be tuned in `repair.py` as more samples are
> collected.

A classified mesh only gets these tuned thresholds when its confidence clears
a **per-class gate** (mechanical ≥ 0.75, organic ≥ 0.70). Below the gate the
type is still reported (`detected_type`) but the conservative default
thresholds (`mincomponentsize=8`, `maxholesize=1000`) are used instead — the
report and the GUI defect panel show this as `tuning_applied: false` /
"default thresholds". (The organic gate used to sit lower because a classifier
bug appeared to cap organic confidence at ~0.62; with that bug fixed the
confidence is unbounded, so both gates sit just above the worst correct
prediction on the calibration set.)

### Known limitation of the classifier

Curved-but-mechanical parts (e.g. a cylinder, shaft, or filleted geometry) are
**not** classified by the classic engine — they fall into the `unknown` bucket
and keep the default parameters. This is a deliberate trade-off of classic: it
only fires on clearly flat/sharp mechanical or clearly smooth organic meshes,
and prefers to do nothing over applying a wrong parameter set. The **default
(experimental) engine now handles these shapes**: its curvature
developability signal (the area fraction of the surface whose normals lie on
a common great circle, the geometric signature of a cylinder/fillet/pipe)
plus the trained head classifies cylinders, tubes and fillets as mechanical,
including damaged variants. A genuinely doubly-curved mechanical part (a bent
pipe) is still missed — it is geometrically closer to a torus than to a
cylinder.

### Classifier engines (`--classifier-engine`)

The **experimental** engine (`sutura/mesh_classifier_v2.py`) is the **default**:
on the labeled synthetic set and the 40-mesh real-world corpus it is measurably
better than the classic heuristic: on the 36 labeled real samples it scores
**29/36** vs classic's **16/36** (mechanical recall **16/20** vs **2/20** — the
classic scanner bias is stark on the expanded corpus; organic 13/16 vs 14/16),
and on the 71-mesh labeled set (35 synthetic + 36 real) LOO-CV is 0.845 vs
classic 0.648. The original
**classic** engine (`mesh_classifier`) remains available — select it with
`--classifier-engine classic` or the `SUTURA_CLASSIFIER_ENGINE=classic` env var
(valid for repair, validate and `--dry-run` alike). Both engines report the
engine actually used as `classifier_engine` (JSON) and a `Classifier:` line
(`--human`).

The experimental engine adds three things to the classic features:

1. **RANSAC plane segmentation** — robust planar patches covering ≥1% of the
   mesh area are detected with a RANSAC loop (face centroid + normal as the
   sample unit); the patch **count** and **area fraction** join the features.
2. **Curvature developability signal** (`developable_fraction`) — the
   area-weighted fraction of the surface whose normals lie on a common great
   circle, the geometric signature of a developable (zero-Gaussian-curvature)
   surface: cylinders, tubes, fillets and other ruled curved mechanical parts.
   Doubly-curved organic forms (spheres, blobs, torus) only ever have a thin
   band perpendicular to any single axis, so they score low. This is what a
   plane-only RANSAC cannot see — a cylinder has no planar patches.
3. **A small trained logistic-regression head** (pure numpy) over
    `[near90, flat, gentle, plane_count, plane_area, developable_fraction]`,
    trained on the 35-mesh synthetic set plus the 36 labeled meshes of the
    40-mesh real-world corpus
    (`tests/real-world-samples/`, the mechanical-but-organic scans labeled
    correctly). The weights are baked in;
    training + leave-one-out CV live in `scripts/train_classifier_v2.py`.

Caveats: the trained head is a weak signal on ~71 labeled meshes (LOO-CV 0.845
— expect variance), and a confident-but-wrong experimental prediction is not
re-checked against classic (only the near-boundary case is: when the head is a
coin-flip — `|p_mech − 0.5|·2 < 0.15` — and classic strongly agrees with the
barely-chosen class, the head keeps its class but reports classic's confidence
so the tuning gate sees the strong signal; see the Feature Status row). If the
experimental engine crashes or returns something invalid it silently falls back
to classic with a warning on stderr, so a broken v2 degrades to classic instead
of crashing a repair.

The v2 engine lives in `sutura/mesh_classifier_v2.py` as a full copy of the
classic engine plus the additions, so the classic module stays untouched.
`scripts/calibrate_classifier.py` (default engine = experimental) and
`scripts/compare_classifier_engines.py` compare both on the same sets;
`tests/test_mesh_classifier_v2.py` guards the invariants (stdlib+numpy only,
classic fallback when the head is disabled, synthetic set at 100%).

### Classifier methodology

The experimental engine's features are:

| Feature | Source | What it captures |
|---|---|---|
| `near90` | dihedral statistics (numpy) | sharp `[60,120]°` edges — mechanical |
| `flat` | dihedral statistics | true planar surfaces (`<1°`) |
| `gentle` | dihedral statistics | slight curvature `[1,15)°` — organic hint |
| `plane_count` | RANSAC plane segmentation | number of robust planar patches |
| `plane_area` | RANSAC plane segmentation | area fraction of those patches |
| `developable_fraction` | curvature Gauss-map signal | normals on a common great circle (cylinder/pipe/fillet) |

**Candidate evaluated — eigenvalue shape descriptors (NOT integrated).** We
tested the standard point-set eigenvalue descriptors of **Weinmann, Jutzi &
Mallet (2015)** — *"Feature relevance assessment for the semantic
interpretation of 3D point cloud data"*, ISPRS Annals of the Photogrammetry,
Remote Sensing and Spatial Information Sciences II-3/W5 (academic reference;
the formulas are public-domain-standard, no code was taken from any
implementation). These are computed from the eigenvalues
`λ1 ≥ λ2 ≥ λ3` of the 3×3 covariance matrix of the **vertex positions** (not
normals): `Linearity = (λ1−λ2)/λ1`, `Planarity = (λ2−λ3)/λ1`,
`Sphericity = λ3/λ1`, `Omnivariance = (λ1·λ2·λ3)^(1/3)`,
`Anisotropy = (λ1−λ3)/λ1`, `Eigentropy = −Σ(λi/s·ln(λi/s))`,
`Surface Variation = λ3/(λ1+λ2+λ3)`. The hypothesis was that long/thin
mechanical parts have high Linearity and round organic forms high Sphericity
— a signal orthogonal to the normal-based features. The descriptors were
implemented from the formulas in numpy and verified on canonical shapes
(sphere → Sphericity≈1; 100×1×1 box → Linearity≈1; flat plate → Planarity≈1).

**Result — the descriptors do NOT help on the labeled set, so they were NOT
integrated.** Leave-one-out CV on the 71 labeled meshes (35 synthetic + 36
real), floor 0.50:

| Feature set | LOO-CV accuracy | mechanical | organic |
|---|---|---|---|
| base 6 (current) | **0.845** (60/71) | 32/39 | 28/32 |
| + `linearity`, `sphericity` | 0.831 (59/71) | 33/39 | 26/32 |
| + all 7 descriptors | 0.817 (58/71) | 32/39 | 26/32 |

The conclusion is robust across decision floors 0.50–0.65. Mechanical recall
gains at most 1 while organic recall loses 2 — the global eigenvalue shape is
largely redundant with the existing features on this corpus (the head already
sees the curvature/planarity structure), so adding the descriptors creates new
errors without a net gain. Per the "only integrate when it genuinely helps"
rule, they stay out; the classifier code is unchanged by this evaluation.

### Local roughness / geometrical / statistical features as a tie-breaker (NOT integrated)

Follow-up evaluation (FAZ 8): the source repositories were re-read in full and
the remaining feature categories were reimplemented **from scratch** from their
academic references (GPL-3.0 code is never copied — only the public formulas):
**roughness** (Gaussian-curvature roughness, Wang et al. 2012; Difference of
Normals, Ioannou et al. 2012; local density, Rabbani et al. 2006 — local
intensity entropy is N/A because STL has no vertex colour), **geometrical**
(local density, farthest distance, maximum height, height std dev, Blomley /
Jutzi / Weinmann 2016) and **statistical shape distribution** (point-to-centroid
distance, pairwise point distance, sqrt triangle area — D1/D2/D3). The
`timzhang642/3D-Machine-Learning` repo was checked again: it is a paper/course
**link list, no code**, so it adds no reference implementation.

**Methodology — tie-breaker, not feature fusion.** Unlike FAZ 6 (which blended
new features into the head's vector and LOO-CV'd the whole corpus), FAZ 8
identified the **boundary/disagreement subset** first — meshes where the head's
confidence is low (< 0.5) or the classic and experimental engines predict
different real classes — and measured the new features **only as a secondary
signal on that subset** (leave-one-out logistic over the new local-feature
aggregates). This is the intended "extra hint when the engines are unsure"
role, not a change to the default decision.

| Boundary subset | n | Baseline (head) | Tie-breaker (new features, LOO) |
|---|---|---|---|
| low confidence OR any disagreement | 27 | 0.778 (21/27) | 0.741 (20/27) |
| low confidence OR real-class disagreement (refined) | 14 | 0.643 (9/14) | 0.429 (6/14) |

Per-feature correlation with the truth label on the refined subset tops out at
|r| ≈ 0.36 (`gc_std`); almost every feature sits at |r| ≤ 0.2. The tie-breaker
**does not improve** the boundary-subset accuracy (0.778 → 0.741 and 0.643 →
0.429) — with ~14–27 samples and 18 features a LOO logistic overfits, and no
single local feature carries enough separation signal to disambiguate the
genuinely-ambiguous cases. **Decision: NOT integrated** — no
`--experimental-tiebreaker-features` flag and no GUI toggle. The default
classifier is unchanged; this is a documented negative result with the same
honesty bar as FAZ 6.

### MeshCNN edge features as fusion and tie-breaker (NOT integrated)

Follow-up evaluation (FAZ 9): the **MeshCNN** 5-D edge-invariant feature
(Hanoeka et al. 2019, MIT-licensed; reimplemented here cleanly from the
formula, no code copied) was implemented from scratch in pure numpy. It is
conceptually different from the rejected Weinmann covariance descriptors: not
a point-cloud neighbourhood covariance, but a **per-edge** feature defined by
mesh connectivity. For every interior edge (shared by exactly two triangles):
`dihedral` = the angle between the two adjacent face planes
(`π − arccos(n₁·n₂)`), `symmetric_opposite_angles` (2 values) = the apex angle
at the vertex opposite the edge in each triangle (sorted), and
`symmetric_ratios` (2 values) = the apex-height-to-edge-length ratio in each
triangle (sorted). The 5 per-edge values were aggregated to **10 global
statistics** (mean + std per dimension). Verified on canonical shapes: a cube
(a mix of π/2 folds and coplanar diagonals) gives dihedral mean ≈ 2.09, a
smooth sphere ≈ 2.97 (near π).

**Test A — feature fusion (FAZ 6 style).** LOO-CV on the 71 labeled meshes:

| Feature vector | LOO-CV accuracy | mechanical | organic |
|---|---|---|---|
| base 6 (current) | **0.845** (60/71) | 32/39 | 28/32 |
| base 6 + 10 edge features | 0.831 (59/71) | 33/39 | 26/32 |

Fusion **worsens** accuracy. **Test B — tie-breaker (FAZ 8 style)**, same
boundary/disagreement subset definition:

| Boundary subset | n | Baseline (head) | Tie-breaker (edge features) |
|---|---|---|---|
| low confidence OR any disagreement | 27 | 0.778 (21/27) | 0.741 (20/27, LOO logistic) |
| low confidence OR real-class disagreement (refined) | 14 | 0.643 (9/14) | 0.857 (12/14, LOO) — but 3-fold×20 repeats: all-10 = 0.733, best-3 = 0.902 |

Per-feature correlations with the truth on the union subset reach |r| ≈ 0.44
(`opp_angle_min_mean`); the refined-subset numbers look promising but n = 14
is far too small to be **clear** evidence, the "best-3 features" estimate
carries selection bias, and the larger (n = 27) union subset shows **no**
improvement. Test A is a clean negative and Test B is not a robust win, so the
same bar as FAZ 6/FAZ 8 applies: **NOT integrated** — no
`--experimental-edge-features` flag and no GUI toggle. The default classifier
is unchanged; the exact numbers above are the honest record.

### Retrospective single-feature scan (FAZ 10)

Per the maintainer's standing rule (always report per-feature correlations on
rejected experiments), the three rejected families (Weinmann FAZ 6, local
roughness/geometrical/statistical FAZ 8, MeshCNN edge FAZ 9) were re-scanned
on the **full 71-mesh labeled set** (not just the boundary subsets). The top
signals are **real** — above the n=71 noise bar of |r| ≈ 0.4, and sign-stable
across 30 bootstrap subsamples (std ≈ 0.04):

| Feature | r (full 71-set) | family |
|---|---|---|
| `oppmax_std` (MeshCNN ratio-max std) | +0.718 | FAZ 9 |
| `don_mean` (Difference of Normals) | +0.673 | FAZ 8 |
| `dihed_mean` (MeshCNN dihedral mean) | −0.633 | FAZ 9 |
| `rmin_mean` (MeshCNN ratio-min mean) | +0.563 | FAZ 9 |
| `gc_mean` (Gaussian-curvature roughness) | +0.480 | FAZ 8 |

But as **single** features they add almost nothing to the head: standalone
LOO-CV gain is ≤ +0.007 mean accuracy (5-fold×20; deterministic LOO 0.845 →
0.859 = one mesh) and a final-model fit flips **zero** verdicts. These strong
correlates are largely **redundant** with the existing dihedral-based features
(`near90`/`flat`/`gentle` + `developable_fraction`). **None cleared the
very-high standalone-integration bar** — flagged as "interesting sub-pieces,
revisit only via a proper feature-selection framework or with new data", not
integrated.

### Opt-in edge-tiebreak head (`--experimental-edge-tiebreak`, NOT the default)

Per the maintainer's "pick the interesting sub-pieces with tweezers" decision,
the five strongest FAZ10 scan signals (`oppmax_std`, `don_mean`, `dihed_mean`,
`rmin_mean`, `gc_mean`) are NOT added to the default vector (LOO-CV showed they
flip only ~1 mesh and overlap the dihedral features — no default-path risk
taken). Instead they are available as an **opt-in 11-feature head** through
`--experimental-edge-tiebreak` (CLI) or the *Edge-tiebreak classifier*
checkbox in the GUI's **Options** window (batch-wide). The 11-feature head was
retrained on the same 71 labeled meshes (LOO-CV 0.845, 5-fold×20 mean 0.852 vs
base 0.850). This is experimental and not the default; the gain is small but
the underlying signals are statistically real.
