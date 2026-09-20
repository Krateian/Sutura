# Per-object Stage 2 for multi-object 3MF — investigation report

Date: session post-v0.2.5 (repo state: Task 1 + Task 2 approved, uncommitted).
Author note: this document is the agreed Task 3 deliverable (REPORT ONLY — no
code was changed for anything described here). It contains two investigations:

- **Investigation A** (added on request): the layered/duplicated-vertex 3MF
  failure on macOS — Stage 1 **destroys** the mesh instead of leaving it open.
  This is more urgent than the Stage-2 skip and is investigated first.
- **Investigation B**: what it would take to run Stage 2 (manifold3d) per
  object for multi-object 3MF files while preserving object ID, transforms,
  metadata, build-plate position and slicer settings.

## Implementation status (added when the plan was implemented)

Both are now IMPLEMENTED (approved session follow-up, not report-only):

- **Investigation A fixed.** `stage1_chain` and `delete_fallback_chain` run a
  second `meshing_remove_duplicate_faces` right after
  `meshing_remove_duplicate_vertices` (the root cause in A.3). The layered
  test file now repairs to 12 faces / 0 holes per object on macOS and Linux,
  and `make_layered_multiobject_3mf.py --check` passes (3 objects: two layered
  + one clean closed cube; the generator honours `$SUTURA`).
- **Investigation B implemented (B.7 steps 1–6).** `repair.maybe_run_stage2`
  is the shared per-object stage-2 helper (same gate/OBJ round-trip as
  single-mesh files); `repair_3mf` runs it per object, the geometry cache
  stores the full report so byte-identical objects each get a per-object
  report incl. `stage2`; the aggregate carries `objects_watertight` /
  `objects_stage2_ok`; `classification._classify_objects` derives the file
  verdict from ALL objects; `--human` and the GUI render the per-object
  stage-2 verdicts; per-object confidence/health/risk are computed after
  stage 2 so they see the outcome. Regression-tested (`tests/test_stage2_3mf.py`).
- **B.7 step 7 (real Bambu/Orca samples) was NOT done** — no real multi-object
  sample files were available on the development machine. It remains an open
  verification item.

---

## Investigation A — layered/duplicated-vertex 3MF: Stage 1 destroys the mesh on macOS

### A.1 The bug

`tests/make_layered_multiobject_3mf.py` writes a 2-object 3MF that reproduces
a real Bambu Studio export failure mode: every vertex is duplicated ~15× as
separate vertex layers, each layer with its own triangle list. On this macOS
machine (Apple Silicon, arm64), repairing that file fails:

```
{"input": "...", "output": "..._fixed.3mf",
 "error": "repair failed: all faces are degenerate; nothing to repair",
 "category": "error", ...}
```

exit 1, no `_fixed` file. This is **not** caused by the current session's
changes: it reproduces identically with the repo HEAD before any edits, with
the installed v0.2.5 copy, and with the current working tree.

### A.2 Exact filter that zeroes the faces (per-step trace)

`repair_mesh_from_arrays` runs the `stage1_chain` on each object. Tracing
`vertex_number`/`face_number` after every filter for **object 1** (the broken
layered cube — open top, inverted face, non-manifold fold):

```
input                                    verts=150  faces=180
meshing_remove_duplicate_faces           verts=150  faces=180   (no-op: not yet duplicates)
meshing_remove_null_faces                verts=150  faces=180
meshing_remove_duplicate_vertices        verts=10   faces=180   <-- collapse: 15 layers -> 10 verts
meshing_repair_non_manifold_edges        verts=10   faces=8     <-- soup collapse
meshing_re_orient_faces_coherently       verts=10   faces=8
meshing_repair_non_manifold_vertices     verts=18   faces=8
meshing_close_holes                      verts=18   faces=10
meshing_remove_connected_component_by_face_number  verts=0  faces=0   <-- faces drop to 0 HERE
meshing_remove_unreferenced_vertices     verts=0    faces=0
meshing_re_orient_faces_coherently       -> EXC (mesh has no faces)
meshing_close_holes                      -> EXC (mesh has no faces)
```

**The point where `faces_number` drops to 0 is
`meshing_remove_connected_component_by_face_number`
(`mincomponentsize=8`)** — it deletes the last fragment. But the real damage
starts two filters earlier:

1. `meshing_remove_duplicate_vertices` collapses 150 → 10 vertices (the 15
   layers are position-identical) and **keeps all 180 faces**. The faces are
   remapped to the 10 surviving vertices (verified: max face index 9, no
   dangling references), so the 180 faces become **12 unique faces × 15
   copies**.
2. `meshing_repair_non_manifold_edges` then sees every edge shared by ~15
   overlapping faces → the mesh is a non-manifold soup → VCG deletes most
   faces, leaving only 8.
3. `meshing_close_holes` adds 2 → 10 faces, but the remaining fragment is
   fragmented into components each `< mincomponentsize=8`, so the debris
   cutoff removes everything → 0 faces → `"all faces are degenerate"`.

### A.3 Root cause (verified experimentally, NOT fixed)

The Stage 1 chain removes duplicate **faces before** duplicate **vertices**
(`stage1_chain` in `sutura/repair.py`). For a layered mesh, vertex dedup is
what *creates* the duplicate faces (10 verts + 180 faces = 12×15 copies), and
by then the duplicate-faces pass has already run. A second
`meshing_remove_duplicate_faces` **immediately after**
`meshing_remove_duplicate_vertices` collapses 180 → 12 and the rest of the
chain then repairs the broken cube correctly:

```
meshing_remove_duplicate_vertices   verts=10  faces=180
meshing_remove_duplicate_faces      verts=10  faces=12    <-- extra pass
meshing_repair_non_manifold_edges   verts=10  faces=10    (fold faces removed cleanly)
meshing_repair_non_manifold_vertices verts=10 faces=10
meshing_close_holes                 verts=10  faces=12    (top face closed)
... final: 12 faces, two-manifold, 0 holes
```

So the fix is a one-line chain-order change (a second duplicate-faces pass
after vertex dedup) — **deliberately not implemented here**, per the report-
only constraint. This also matches how the healthy layered cube already
behaves: for object 2 (closed cube) the same trace ends with 12 faces and
0 holes even without the extra pass, because the healthy cube's 12 unique
faces survive `meshing_repair_non_manifold_edges` intact.

### A.4 Build-config comparison (conda-forge vs PyPI)

Both macOS environments were compared side-by-side on this machine:

| Build | pymeshlab | numpy | Python | Result on the layered file |
|---|---|---|---|---|
| conda-forge (`py311h2cd5cf3_10`) | 2025.7.post1 (MeshLab 2025.07d) | 2.4.6 | 3.11 arm64 | identical trace, 0 faces |
| PyPI wheel (`cp311-cp311-macosx_11_0_arm64`) | 2025.7.post1 (MeshLab 2025.07d) | 2.4.6 | 3.11 arm64 | identical trace, 0 faces |

Both produce the **exact same** per-filter trace (A.2). The
"conda-forge vs PyPI build difference" hypothesis is therefore **refuted on
macOS arm64** — same pymeshlab version, same MeshLab 2025.07d, same numpy,
same architecture, same behaviour. Any remaining difference would have to be
**Linux x86_64 vs macOS arm64** in the VCG binary itself (build flags / SIMD /
hashtable behaviour in the non-manifold or duplicate-vertex stages), which
cannot be confirmed from this machine (no Linux environment available here).

**Linux evidence from the repo:** `.github/workflows/ci.yml` has a "Layered
multi-object 3MF regression" step that runs the same repair on `ubuntu-latest`
and asserts `objects == 2` plus a valid output zip, **without** `|| true`
(i.e. a failed repair fails the CI step). If Linux behaved like macOS, that
step would be red. The repo's documented claim ("a layered/duplicated-vertex
3MF repairs fully closed") is therefore consistent with Linux repairing the
mesh — but the CI assertion is weak (it checks the zip parses and the object
count, not `two_manifold`/`holes_remaining`). Re-verifying on Linux CI is a
recommended follow-up; it cannot be done from this macOS machine.

### A.5 Implication for Stage 2 (why this matters)

The documented premise of the per-object Stage 2 skip is that layered/folded
Bambu-style objects *remain open after Stage 1* (the `TODO(stage2)` comment in
`repair_3mf`). Investigation A shows that on macOS the reality is worse: for
the **broken** layered cube, Stage 1 does not leave the object open — it
**destroys it outright** (0 faces, `"all faces are degenerate"`). Per-object
Stage 2 (Investigation B) is therefore **moot on macOS until A is fixed**:
there is no closed mesh to rebuild, because there is no mesh at all. A is the
more urgent bug and should be fixed (chain-order change, A.3) and verified on
Linux CI before or alongside any per-object Stage 2 work.

---

## Investigation B — per-object Stage 2 for multi-object 3MF

### B.1 Current architecture (facts from the code)

- `parse_3mf_meshes` (`sutura/repair.py`) returns `{model_name: [(verts,
  tris, span)]}` per `<mesh>` block, keyed by `.model` archive entry.
- `repair_3mf` repairs each object in memory via
  `repair_mesh_from_arrays` and splices the rebuilt `<mesh>` block back into
  the original XML with `build_mesh_block`; every other byte of the archive
  is preserved (each non-mesh entry and every character outside the
  `<mesh>...</mesh>` span is untouched).
- **Stage 2 already runs for single-mesh files** (incl. single-mesh 3MF):
  `repair_file` → watertight check → `write_obj` → `run_stage2` (venv311
  subprocess, or in-process on macOS) → `read_obj` → `save_mesh`.
- `repair_3mf` calls `run_stage2` **nowhere**; the `TODO(stage2)` comment
  documents the skip. Per-object reports already carry `repair_health`,
  `repair_risk`, `repair_confidence`, `defects` and `_fp` (fingerprint).

### B.2 Feasibility: MEDIUM

The plumbing is already proven in `repair_file` (the OBJ round-trip, the
bridge subprocess, the watertight gate). Adding the same block to
`repair_3mf`'s per-object loop is a small, mechanical change. The honest
caveat is worth: because of Investigation A, the motivating files (layered
Bambu exports) do not even reach a closed mesh on macOS, so per-object Stage 2
would be skipped for them until A is fixed. On Linux, where the layered file
repairs closed per the CI claim, per-object Stage 2 would finally let those
objects claim `watertight` instead of `stage2_skipped`.

### B.3 What is already preserved (verified, no work needed)

- **Object IDs** — `<object id="...">` lives outside the `<mesh>` block.
- **Build-plate position** — `<build><item transform="...">` is in the main
  model file, untouched.
- **Component transforms / instances** — `<component transform="...">` is
  untouched; a single rebuilt object mesh is automatically inherited by every
  component/item that references it (including mirrored/rotated instances),
  because Stage 2 works in the object's local coordinate space.
- **Metadata and slicer settings** — any XML outside the spliced `<mesh>`
  span and any non-model zip entries (Bambu/Orca metadata, `Metadata/`
  folders, textures, print settings) are preserved byte-for-byte.
- **Per-object scoring/reporting** — `object_reports[i]` already carries the
  per-object stage-1 fields, `repair_risk`/`repair_health`, and defects.

### B.4 Technical blockers / gaps (ordered by severity)

1. **A is a prerequisite on macOS** — per-object Stage 2 needs a closed mesh;
   on macOS the broken layered object is destroyed by Stage 1 (Investigation
   A). Fix A first (or at least land B gated so a 0-face object is an error,
   never a silent skip).
2. **Parser robustness (pre-existing, unrelated to Stage 2 but worth noting):
   the `<vertex>` regex requires `x`, then `y`, then `z`, no other
   attributes.** A reordered or extended vertex element is silently dropped →
   object loss. This undermines the "is the object closed?" check that gates
   Stage 2. Fixing the parser (or at least failing loudly) is advisable before
   relying on per-object watertight detection.
3. **Precision**: `build_mesh_block` writes `%.7g`; manifold3d output
   round-trips through float32 (`trimesh.load` / `read_obj`). Fine for mm
   scale, a note for high-tolerance parts.
4. **Per-vertex attributes** are already dropped by the mesh rebuild (Stage 1
   and Stage 2 both rebuild pure verts/tris); per-vertex color/texcoord in a
   3MF mesh block would be lost. Same limitation as today, worth stating.
5. **Aggregate/classification semantics are object-0-centric**: `repair_3mf`
   sets `agg['stage1']` from `object_reports[0]`, and `classification.classify`
   evaluates the top-level `stage1` — i.e. object 0's outcome drives the whole
   file's category. Per-object Stage 2 needs a defined aggregate (e.g.
   `objects_watertight: n`/`objects_stage2_ok: n`) and a classification rule
   that reflects ALL objects, otherwise one confirmed object flips the file
   summary misleadingly.
6. **Subprocess cost**: `run_stage2` spawns the venv311 interpreter per call
   (per object). For 10+ objects this is sequential process overhead;
   acceptable, but a batched/bridging option is a future optimisation.

### B.5 Risks

- A slicer's per-object settings that reference geometry *size/count* could go
  stale after a Stage-2 rebuild. Low: settings key on object IDs, and the
  rebuild is topology-only; still worth a spot-check on real Bambu/Orca
  exports.
- manifold3d's boolean-union of multi-shell objects per object can change
  topology; the current `run_stage2` behaviour is reused unchanged, so risk is
  the same as the single-mesh path already in production.
- The geometry-cache in `repair_3mf` (byte-identical objects share one repair)
  must keep Stage 2 deterministic per geometry — it is (pure function of the
  mesh), but per-object `stage2` reports for cached duplicates need care (the
  cached object currently does not append a report).

### B.6 Estimated complexity

- Implementation: small — extract `maybe_run_stage2(report, verts, tris,
  tmpdir)` shared by `repair_file` and `repair_3mf`, call it per object,
  splice the rebuilt mesh, extend `agg` + classification. Roughly one focused
  session plus test updates.
- Verification: the real uncertainty is whether *any* real-world multi-object
  file has a closed object after Stage 1 (on macOS: not until A is fixed).
  Needs a small sample of genuine Bambu/Orca multi-object exports to confirm
  the feature fires in practice.
- Effort estimate: ~1–2 focused sessions (code + tests + sample validation).

### B.7 Step-by-step implementation plan

1. Fix Investigation A (second `meshing_remove_duplicate_faces` after
   `meshing_remove_duplicate_vertices`) and re-verify `make_layered_
   multiobject_3mf.py --check` on macOS and Linux CI.
2. Extract `maybe_run_stage2(report, verts, tris, tmpdir)` from `repair_file`
   (write OBJ → `run_stage2` → read OBJ → set `report['stage2']` →
   return rebuilt arrays), with the same "watertight gate + bridge available"
   condition.
3. In `repair_3mf`'s per-object loop, call it; write the rebuilt arrays with
   `build_mesh_block`; keep the geometry-cache (Stage 2 is deterministic per
   geometry) but ensure cached duplicates still surface a per-object `stage2`.
4. Extend the aggregate: `agg['objects_watertight']`, `agg['objects_stage2_ok']`
   (and keep `stage1` as object-0 for backward compatibility).
5. Update `classification.classify` for multi-object 3MF to consider all
   objects (not object-0's `stage1`), and `human_report`/GUI rendering.
6. Tests: extend `make_layered_multiobject_3mf.py` with a *closed* multi-object
   case so per-object Stage 2 actually runs; assert per-object `stage2.ok`;
   add a multi-object 3MF budget case; run the corpus.
7. Sample real Bambu/Orca multi-object exports and document whether per-object
   Stage 2 fires in practice (the verdict for the README Feature Status row).

---

## Appendix — raw evidence

- Trace scripts run with `sutura-env` (conda-forge) and a throwaway venv
  holding the PyPI wheel; both output the tables in A.2 verbatim.
- `pip download pymeshlab==2025.7.post1 --only-binary :all: --platform
  macosx_11_0_arm64 --python-version 311 --implementation cp --abi cp311`
  yields `pymeshlab-2025.7.post1-cp311-cp311-macosx_11_0_arm64.whl`.
- `conda list -n sutura-env`: `pymeshlab 2025.7.post1 py311h2cd5cf3_10
  conda-forge`, `numpy 2.4.6 py311hbd1492f_0 conda-forge`.
- CI reference: `.github/workflows/ci.yml` lines 62–70 (layered 3MF
  regression: asserts `objects == 2` and zip validity only).