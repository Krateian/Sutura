# Version history

[← Back to README](../../README.md) · [Türkçe](tr/version-history.md)


Only versions that added user-facing features are listed (bug-fix-only
versions are skipped). Full detail in [CHANGELOG.md](../../CHANGELOG.md).

- **v0.9.7 — 2026-10-26** — An experimental **last-resort solid engine**
  choice. The default stays **fTetWild**; an opt-in exact rebuild (**CDT**,
  constrained tetrahedralization) is available with `--solid-engine cdt`,
  under Options → Repair → **Solid engine**, or via the `solid_engine` settings
  key. CDT runs after Coaptation and before fTetWild and falls back to fTetWild
  if it cannot certify its result, fails or times out. Known limitation: it is
  slow on large meshes for now, so it is best on smaller parts. The OrcaSlicer
  plugin also gains OrcaSlicer 2.5 permission support.
- **v0.9.6 — 2026-10-26** — The last-resort **fTetWild** tier now ships by
  default and is far smaller (about 6 MB instead of ~1.1 GB, which came from a
  PyVista/VTK dependency the tier never used); `SUTURA_WITH_FTETWILD=0` still
  skips it. The AppImage bundles every module again, so `--self-check`,
  `--diagnostics` and self-repair work, and the build now checks itself. The
  installers add `sutura` to the PATH (and `uninstall.sh` removes exactly that
  line), and the Dolphin right-click repair works on Plasma 6 systems that only
  ship `qdbus6`. A Graft fallback that covers only a small part of the input
  surface is no longer adopted, and an unmeasurable volume reads `n/a`.
- **v0.9.5 — 2026-10-25** — **Diagnosis v2** adds a detailed, deterministic
  analysis report (orientation, parts, self-intersections, boundary loops,
  thin walls and more) without changing the repair itself. Repairs are now
  repeatable: the Flap, Tamponade, Coaptation, Graft and Splint limits and the
  Full Mend self-intersection excise count work instead of elapsed time, so the
  same file gives the same result on a fast or a busy machine (the optional
  fTetWild add-on stays outside this guarantee). The automatic fallback now
  skips methods the detected damage makes pointless, and Flap, Graft, Tamponade,
  coplanar contact and internal cavities are safer. The documentation moved
  into topic pages in English and Turkish.
- **v0.9.4 — 2026-10-23** — **Coaptation (#19)**, an on-by-default seam-weld
  union: it joins a folded or overlapping multi-body model along the seam where
  the parts touch and folds the closed parts into one clean solid, adopted only
  when it is strictly better than the repair result so far. Also fixes a flat
  fold seam being capped with self-crossing faces, a healthy multi-part model
  losing its parts, the union of parts pushing the surface off the input, and
  load-bearing zero-area faces that hold a surface closed being deleted; several
  results that used to depend on the file's triangle order are now the same
  every time, and self-update installs every module a release ships.
- **v0.9.3 — 2026-10-22** — **No PyMeshLab any more:** hole closing,
  self-intersection detection, cleanup, decimation, distance measurement and
  mesh reading/writing all run in Sutura's own Rust core and Python engine, so
  installs are smaller and simpler; on the 40-mesh corpus no mesh got worse.
  Balloon (#8) rebuilds natively without self-intersections, and installs from
  0.5.1–0.9.1 can self-update again.
- **v0.9.2 — 2026-10-21** — **Splint (#18)** as an opt-in exact solid-rebuild tier
  (off by default; evaluated on the 40-mesh corpus and adopted on none so far);
  `sutura --self-check`, `--diagnostics` and `--self-repair`; a print-impact
  report; a smaller runtime (trimesh/SciPy removed, Manifold and the native
  geometry in the mandatory Rust core); and 3MF repairs that preserve the
  project file.
- **v0.9.0 — 2026-10-11** — **Tamponade** (method #17, on-by-default interior hole plug
  with an inflate-from-inside **Stent** candidate and multi-stent partition for
  large loops; strict plug acceptance rules — no leak, no new holes/non-manifold
  edges on reload, no anti-parallel double wall over 20 %, must stay connected
  and add no part); **Flap** per-loop candidate selection (on by default, tries
  alternative fills only when the primary patch is rejected, charged to the Flap
  budget; `SUTURA_FLAP_SELECT=0` opts out); a memory/time guard on the Scaffold
  resample (#10); native exact self-intersection detection; and dependency
  reduction (Phase 1 drops `psutil` /
  `pyrobust-predicates`, Phase 2a/2b route Stage-1 cleaning to the Rust kernel).
- **v0.8.1 — 2026-10-03** — Repairs speed up via a native Rust topology kernel
  (vertex welding, duplicate-face and edge counting; bit-for-bit identical output,
  +4–29% end-to-end on the sample models) with a transparent pure-numpy fallback;
  an automatic post-union micro-sliver cleanup so Stage-2 solid rebuilds open as a
  single slicer part without touching genuine multi-part assemblies; and corrected
  non-manifold face indices in the defect report and the heatmap.
- **v0.8.0 — 2026-10-03** — **Flap** surface hole fill (covers a boundary loop by
  continuing the neighbouring surface instead of a flat lid; on by default,
  `--no-flap` / `SUTURA_FLAP=0` disables); **Dressing** (method #16, opt-in
  variable-viscosity level-set skin) with a non-watertight suggestion and
  `--dressing-force-adopt`; the `--si-mode` self-intersection policy (default
  *Report only*); a Graft input face cap + wall-clock budget; and a guarded
  Stage-1 self-intersection excise + refined re-cap in Full Mend.
- **v0.7.2 — 2026-10-02** — OrcaSlicer plugin v2 (dock panel, repair notifications,
  correct transforms, load-back); 2-3.7x faster symmetry detection.
- **v0.7.1 — 2026-10-01** — Graft now works in installed builds: Rust extension
  shipped via prebuilt wheels + bundled in AppImage/.dmg; macOS .dmg startup fix.
- **v0.7.0 — 2026-09-28** — Sutura Triage Engine: modular architecture
  (`sutura_engine` with Triage, Diagnosis, Chart, Stitch, X-Ray, Hull, and Cast);
  extensible Plugin API (`RepairMethodProtocol`); content-addressed Chart cache
  (`~/.cache/sutura/`); Method #13 **Graft** (morphology shell wrap with healthy
  fidelity fallback before fTetWild); complete 15-method registry with canonical
  display names; and auto-escalation runtime fallback budget.
- **v0.6.1 — 2026-09-28** — Two opt-in geometry methods: **Mirror Complete**
  (#14, completes a single-sided symmetric scan by mirroring the visible
  surface across its detected symmetry plane, with a native implicit fallback) and
  **Wall Thicken** (#15, per-vertex wall thickness from an SDF grid plus a
  morphological thicken-to-min, `--wall-min-thickness T`); and **learning
  triage** (a small, bounded local `(object type, method)` ranking bonus,
  toggled with `--no-learning-triage` / `learning_triage` and reset with
  `sutura clear-learning` or the GUI Options).
- **v0.6.0 — 2026-09-28** — Repair method registry: twelve stable methods
  (`--list-methods`), per-object analysis and recommendations (`--analyze`),
  explicit tagging (`--methods 2,3,5`) and GUI right-click **per-file method
  tagging**; new standalone tiers for **single-side scan closing** (#8/#9),
  **proxy-template repair** (#10) and **repeated-element repair** (#11/#12,
  auto detection + GUI source/target picker); a **reload-honest watertight
  verdict** (P-HONEST) plus the **P-WELD** final pass, so the seven real-world
  samples that used to reload non-manifold now reload strictly watertight with
  unchanged face counts; new runtime dependencies scipy and trimesh.
- **v0.5.0 — 2026-09-26** — Triage Engine: **intensity presets and named
  profiles** (`--intensity quick|balanced|thorough|extreme|<profile>`, GUI
  combo + profile editor), a **deep-repair ladder**
  (`--deep-repair {off,local,full}`) with fTetWild dense-output decimation,
  **external repair engines** (`~/.config/sutura/engines/*.toml`, `chain.toml`,
  every result through the same watertight + Hausdorff guard as fTetWild) and
  an **fTetWild install/remove manager** (`sutura ftetwild`, GUI *Engines*
  tab), a **tabbed Options window**, explicit `--ftetwild-optimize`, and CI
  Linux/macOS end-to-end installs. The fTetWild fallback tier is on by
  default when installed (opt-out `--no-fallback-ftetwild`); the exact Rust
  arrangement is ~2.8× faster again (thingi10k_1038441: 29.8 s → 10.6 s on an
  M2, identical output).
- **v0.4.0 — 2026-09-25** — experimental self-intersection tiers:
  autorefine with iterative snap-rounding and an fTetWild fallback tier
  (optional ~1.1 GB extra), both as CLI flags + GUI checkboxes; the exact
  Rust arrangement (`--experimental-indirect-autorefine`) now completes dense
  scans (interval-filtered predicates, crossings from original data);
  OrcaSlicer plugin repairs the selected model in-memory via `orca.host`
  (never-tagged 0.3.1 content included).
- **v0.3.0 — 2026-09-20** — curved-but-mechanical classifier signal
  (`developable_fraction`) + 40-mesh real-world corpus; 115-mesh
  strict-watertight benchmark + corpus release asset + Manifold
  cross-validation; GUI color-coded defect view + "what changed" repair log;
  opt-in edge-tiebreak classifier head and join-components options (CLI + GUI
  checkboxes); synthetic defect injection tool; 3MF zip-bomb guard.
- **v0.2.7 — 2026-09-19** — unit-mismatch detection; repair budgets; per-object
  Stage 2 for multi-object 3MF.
- **v0.2.6 — 2026-09-19** — Repair Health/Risk scoring; macOS Finder Quick
  Action.
- **v0.2.5 — 2026-09-19** — repair profiles (`--profile` / GUI dropdown);
  macOS native launch (Spotlight); real-world calibration corpus.
- **v0.2.3 — 2026-09-19** — bundle-aware path resolution for a standalone
  macOS `.app`; unsigned macOS `.dmg` packaging.
- **v0.2.1 — 2026-08-30** — anonymous usage history (opt-out); graphify
  knowledge-graph integration; contributing section.
- **v0.2.0 — 2026-08-26** — interactive 3D viewer (rotate/zoom + surface
  deviation); `.obj` support; auto-update license boundary; sponsor section.
- **v0.1.9 — 2026-08-25** — Repair Confidence Score; GUI Analyze button +
  analysis pane; mode suggestions.
- **v0.1.8 — 2026-08-24** — `validate` subcommand; `--dry-run`; before/after
  comparison with worst-defect zoom.
- **v0.1.7 — 2026-08-22** — repair modes (`--mode` + GUI picker); extreme
  extra passes; before/after mesh comparison (GUI).
- **v0.1.6 — 2026-08-20** — before/after geometry diff in reports; `--diff`.
- **v0.1.4 — 2026-08-19** — batch repair summary; defect detection
  (`sutura/defects.py`); mesh type-aware repair; Turkish README.
- **v0.1.3 — 2026-08-19** — opt-in update checker; self-update with backup
  and rollback.
