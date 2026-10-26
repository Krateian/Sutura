# Robustness and known limitations

[← Back to README](../../README.md) · [Türkçe](tr/limitations.md)

## Robustness

Malformed or hostile inputs are rejected with a clear error and a non-zero
exit code, never a crash or a silently wrong result:

| Input | Behaviour |
|---|---|
| Truncated / cut-off binary STL | rejected: "Unable to open file ... Malformed file" |
| Header claims more triangles than the file holds | rejected: "Malformed file" |
| NaN/Infinity vertex coordinates | rejected: "input mesh contains NaN or infinite coordinates" |
| Empty mesh (0 triangles) | rejected: "input mesh is empty (no triangles)" |
| Fully degenerate mesh (only zero-area faces) | rejected: "all faces are degenerate; nothing to repair" |
| Wrong extension (OBJ content in `.stl`, or the reverse) | rejected: "Unable to open file" |

Any of these returns exit code 1, so scripts can reliably detect failure.

## Known limitations

* **macOS right-click needs a Quick Action, not a Dolphin-style menu.**
  macOS has no ServiceMenu equivalent; instead the installer adds a Finder
  **Quick Action** ("Sutura — Repair") in `~/Library/Services/`, available
  under right-click → Quick Actions for selected STL/3MF files. It calls the
  bundled `sutura-cli` inside a PyInstaller `Sutura.app`, so it only repairs
  when such an app is present (in `/Applications` or `~/Applications`) — the
  dev-install wrapper app alone has no bundled CLI. A downloaded
  (quarantined) `Sutura.app` must be opened once via right-click → Open
  before the Quick Action can run it (Gatekeeper; the notification tells you
  to do this).
* **No macOS uninstall script.** There is no macOS equivalent of the Linux
  `uninstall.sh`; removing a macOS install is manual (see "Removing a macOS
  install" under the macOS install section).
* **Native KDE file dialog.** The GUI sets `QT_QPA_PLATFORMTHEME=kde` and
  points `QT_PLUGIN_PATH` at `/usr/lib/qt6/plugins` so QFileDialog uses the
  native KDE dialog (rubber-band rectangle selection included). This works
  only when the system Qt version matches the bundled PySide6 Qt — the GUI
  checks the versions (via `qmake`) and only mixes in the system plugins on a
  match. When they differ (e.g. system Qt 6.11.2 vs bundled 6.11.1), the
  system platform plugins cannot load into the bundled Qt, so the GUI keeps
  Qt on its own bundled plugins and falls back to the embedded dialog —
  rectangle selection may be unavailable, but Ctrl/Shift+click always works.
* **Self-intersections within one connected shell.** Manifold rebuilds the
  mesh as a solid, which resolves interior/overlapping geometry, but the
  rebuild can slightly reshape features in pathological cases. Always check
  the result in a slicer.
* **Large hole patches.** The stage-1 closer fills holes with flat triangulated patches; for
  very large holes the fill is a simple patch, not a smart reconstruction.
  It closes the mesh, but the patch quality is average and may need smoothing.
* **Tiny disconnected debris.** Components with fewer than 8 faces are
  removed. A small legitimate part that is not connected to the main body
  will be removed too.
* **Dense coplanar contacts are analysed with a bounded exact pass.** For a
  mechanical part with very many coincident faces on one contact patch, the
  exact coplanar shell analysis is capped at 512 triangles / 1,024 constraint
  segments per contact cluster, 1,024 triangles per plane and 8,192 processed
  faces in total. When a cap is reached, the exact pass is skipped, the input
  is kept unchanged and the normal Stage-1 repair continues; the report shows
  `budget_exceeded` under `tamponade_shell_prepass` and
  `outer_shell_skipped: shell_budget_exceeded`. This prevents a very dense
  contact mesh from stalling the repair, but such a mesh may finish as a
  `warning` instead of a watertight result.
* **Inverted whole models.** If the repaired volume comes out negative, the
  whole mesh is flipped; a model that was consistently wound "inside out"
  will be corrected automatically.
* **Manifold kernel.** It rejects any input with open edges; if stage 1
  cannot close a hole, stage 2 is skipped and the stage 1 result is used as-is
  (the report says so).
* **Experimental solid engine (CDT).** The opt-in exact `--solid-engine cdt`
  rebuild is slow for now: on many models, even ones with a few thousand faces,
  it does not finish within its time limit, and Sutura then falls back to fTetWild
  automatically. Use it on small or simple models, or when the original surface
  must be kept exactly. When
  CDT's result fails its checks (watertight, one closed outward-facing solid) or
  runs out of time, the ladder falls back to fTetWild (which then runs with its
  own time limit, so the worst case takes about twice as long), so a result is
  still produced; the report records which engine ran under `solid_engine`.
* **STL reload and pinched vertices.** STL stores no vertex sharing; a mesh
  that is two-manifold in memory can reload with pinched vertices where two
  distinct float32 positions weld into one. P-WELD separates the common
  float32-coincidence case and the report stays honest
  (`reload_watertight=false`, `reload_non_manifold_vertices`) when it cannot.
  For pathological cases, inspect the saved file in your slicer.
* **Intentional internal cavities and debris.** Stage 2 keeps a genuine internal
  void (a CAD pocket, or a hole meant for a magnet, weight or insert) instead of
  filling it, as long as that void is the only thing the union would change and
  preserving it does not add self-intersections and the void is clearly smaller
  than the body (its bounding-box diagonal is below 0.5 of the body's). A cavity
  shell that crosses the body or another shell is rejected and the ordinary
  union result is used instead; an oversized inner shell that is effectively a
  parallel copy of the body is not treated as a pocket and is filled the same
  way. A void in a model where Stage 2 also had to remove debris is still
  filled for now, because the current fix cannot both drop the debris and keep
  the void. The report says `cavity_preserved` when the void was kept;
  `cavity_si_whole` / `cavity_si_union` / `cavity_rejected_si` describe the
  self-intersection comparison and `cavity_rejected_bbox` / `cavity_bbox_ratio`
  the size check.
* **Layered/duplicated-vertex 3MF exports.** Some slicers (Bambu Studio
  included) write 3MFs whose objects repeat every vertex position ~15x as
  separate vertex entries, and whose surfaces are folded (several faces
  coincident on one edge). The Stage 1 chain can turn such meshes into valid 2-manifolds:
  the Stage 1 chain dedups faces again after the vertex dedup (vertex dedup
  is what *creates* the duplicate faces on a layered mesh), then repairs
  non-manifold vertices before hole closing and finishes with a close pass,
  so the test exports now repair fully closed — 0 holes remaining (reported
  as a real boundary-loop count, not half the boundary edges). Closed
  layered objects also get a per-object stage 2 watertight rebuild, so they
  are reported watertight rather than `stage2_skipped`. Example from
  development: a 2-object Bambu export that previously reported 13 and 26
  micro-holes (or was destroyed to 0 faces on some platforms before the
  duplicate-faces fix) now reports 0 and 0, both stage-2 confirmed.
* **All objects and project data are preserved.** 3MFs are repaired object by
  object and only the repaired `<mesh>` XML is written back; every other
  archive entry (thumbnail, slicer/plate/wipe-tower settings, OPC relationships
  and Production sub-models) is kept, with object/build IDs, transforms, units
  and namespaces. No object is lost, even when several meshes share one
  `.model` entry. The per-object result (including each object's stage-2 verdict
  and the `triangle_attributes` keep/drop counts) is reported in the CLI output
  and the GUI.
