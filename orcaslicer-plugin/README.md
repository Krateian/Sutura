# Sutura × OrcaSlicer plugin

Repair OrcaSlicer models straight from a dockable panel beside the 3D view:
the objects on the plate are read through the `orca.host` API, exported to STL
in world coordinates, repaired with the [separately-installed Sutura
CLI](https://github.com/Krateian/Sutura), and the repaired copy is loaded back
into the slicer as a new object. The plugin does not bundle the repair engine
into OrcaSlicer's embedded Python; repair runs through the installed Sutura CLI,
which requires the mandatory native `sutura_geom` extension (no PyMeshLab). The
plugin itself declares `numpy`
as a plugin dependency (installed by OrcaSlicer's bundled uv) and falls back
to the numpy-free mesh accessors when numpy is unavailable.

## ⚠️ Version requirement — OrcaSlicer 2.5.0+ / nightly REQUIRED

The OrcaSlicer Python plugin system ships in **OrcaSlicer 2.5.0 and newer /
nightly builds**. The **2.4.x releases have no "Plugins" menu**, so the plugin
will not work there. It was verified end-to-end on a 2.5.0-dev nightly (macOS);
the first stable release with the plugin API is 2.5.0.

## ⚠️ EXPERIMENTAL — early stage

The plugin API usage is stub-tested against a mock host
(`tests/test_orca_plugin.py`), including loading with numpy blocked, the
transform/winding export, the multi-part union, the message protocol, the
lifecycle debounce and the notification de-duplication. It remains an
early-stage feature: treat it as a verified starting point, not a
guaranteed-working product, and report issues (see "Feedback" below).

## Platform: Linux primary, macOS bonus verification

- On **both** Linux and macOS the Sutura CLI is installed at the **same path,
  `~/.local/bin/sutura`** (`install.sh` on Linux, `install-macos.sh` on macOS;
  only the Python interpreter behind the wrapper differs).
- Windows is not supported.
- macOS-specific behaviour: the repaired file is loaded back with the native
  `open -b com.orcaslicer.OrcaSlicer <path>` (bundle-ID matching, skipped when
  more than one OrcaSlicer process is running); Linux uses
  `orca-slicer --single-instance <path>` (the binary name can be overridden
  with the `ORCA_BIN` environment variable).

**File name.** The single file is `sutura_repair_linux_x86_64.py`. That name
reflects the OrcaCloud/plugin naming convention, not a platform restriction —
the same file runs on both Linux and macOS.

## The dock panel

Running the plugin (Plugins dialog → Run) opens a dockable panel on the right
of the 3D view; with the **Open panel at startup** setting (default on) it
opens automatically too. OrcaSlicer starts on the Home tab and restores the
Plater/AUI layout after the plugin loads, so a pane created at load stays
hidden even after `show()`; the panel is therefore created lazily on the first
`ObjectAdded` / `ProjectOpened` / `NewProject` event, on the UI thread, once the
layout exists. The outcome is logged to stderr (`dock panel created on …`). The
panel lists every object on the plate and offers:

- a **Quick / Balanced / Thorough / Extreme** preset control (the Sutura
  `--intensity` presets; Balanced is the default);
- per-object **Analyze** (read-only: holes, non-manifold regions,
  self-intersections and the top ranked repair methods with confidence bars)
  and **Repair** actions, plus a "Repair selected (n)" footer button and a
  "Select broken" shortcut;
- live job phases (queued → exporting → repairing → loading → done / failed /
  cancelled) with elapsed time and a Cancel button;
- a **Show file** link for a finished job that opens the repaired-output
  folder in the platform file browser;
- collapsible settings for the preset, notifications and panel-at-startup.

The panel HTML is embedded in the plugin file (copied from
`panel/panel.html`) so the published single-file plugin stays
self-contained; `panel/preview.html` is a browser-only development preview.

## Proactive check and notifications

On `ObjectAdded`, `ObjectChanged` and `ProjectOpened` the plugin schedules a
debounced (1 s) rescan — the lifecycle hook itself only enqueues, never does
work inline. For any object whose `mesh_errors_count() > 0` (errors OrcaSlicer
repaired on import) or whose model-part volumes are non-manifold, it pushes
**one** warning notification per object per session:
"_<name>: <n> mesh error(s) repaired by Orca — check with Sutura_" with a
"Repair with Sutura" action that repairs the object with the current preset.
Notifications can be disabled with the **Notify about broken meshes** setting.

## Export semantics

- Every `is_model_part()` volume of an object is exported; parameter
  modifiers, negative volumes and support blockers are **skipped** (and
  counted in the panel's "mod" indicator).
- Each volume is transformed by `instance(0).matrix() @ volume.matrix()`, i.e.
  the object is written in **world coordinates**; the triangle winding is
  flipped when the combined transform is left-handed (mirrored parts keep
  outward-facing normals).
- A multi-part object becomes **one** STL containing all its model parts (the
  union surface Sutura is asked to repair).
- **Staging root.** The host exposes no `data_dir()` API, so the data dir is
  derived from the plugin's own install location: walk up from the plugin file
  to the `orca_plugins` component and take its parent (cloud plugins live
  under `<data_dir>/orca_plugins/_subscribed/<user>/`, local ones under
  `<data_dir>/orca_plugins/`). Jobs stage under
  `<data_dir>/orca_plugins/.sutura_work/<uuid>/`; if that tree cannot be
  located, repair/analyze report a clear error instead of writing outside the
  audit-allowed root. No path component the plugin creates contains `conf`,
  `config`, `secret` or `cert`, which the audit hook denies.
- The repaired file is written to the **persistent** output folder
  `<data_dir>/orca_plugins/.sutura_work/out/` as
  `<object-name>_sutura_<YYYYmmdd-HHMMSS>.stl`, with any source mesh extension
  (`.stl`/`.obj`/`.3mf`) stripped from the object name so a name such as
  `broken_cube.stl` yields `broken_cube_sutura_...stl`, not a double extension
  (a numeric suffix is added only if that name already exists). The repaired
  copy keeps the exported world coordinates, so OrcaSlicer places it on top of
  the original; the panel's done row says so: **Added as a new object — press
  A (Arrange) to separate it from the original; Ctrl/Cmd+Z removes it.** It is
kept after the job so OrcaSlicer can still read it while the asynchronous
load-back completes; the **Show file** link opens that fixed `out/` folder in
the platform file browser (`open` on macOS, `explorer` on Windows, `xdg-open` on
Linux) — a stable command, so it is declared once and never prompts. Only the
per-job input staging
  (`<uuid>/input.stl`) is deleted; output files and staging dirs older than
  seven days are pruned when the plugin loads.

## Repair

Repair always runs via the **subprocess CLI** (`~/.local/bin/sutura <stage>
--intensity <preset> -o <unique_out>`): the embedded interpreter ships only
`pip` (plus the declared `numpy`), so in-process repair is not possible. The
CLI is run with a 600 s timeout and can be cancelled from the panel; Analyze
runs `sutura <stage> --analyze` (read-only, no output file). The JSON report
on stdout is parsed and mapped onto the panel protocol. Each spawn runs on a
plugin-created worker thread, whose audit context is empty, so on OrcaSlicer 2.5
it is not audited and raises no prompt; a denial would be reported as a clear
panel message. The installed CLI
exposes all 18 repair methods; the experimental **Splint (#18)** exact solid
rebuild is opt-in and off by default, so the panel presets do not enable it.

## Permissions

`register_capabilities()` declares, up front, everything the plugin can name in
advance:

- **`fs_read`** for the Sutura CLI path — **only for paths that actually
  exist** (the resolved `SUTURA_CLI`, `~/.local/bin/sutura`, or a `sutura`
  found on `PATH`); requesting a non-existent path is rejected, so it is
  skipped.
- **`threading`** — the panel and the job workers run on plugin-created
  threads, so Threading is granted during load, before any thread starts.
- **`process`** for the two stable commands: the OrcaSlicer process-count probe
  (`pgrep -f OrcaSlicer`) and the fixed-folder reveal command (one per
  platform). The audit hook keys a process grant on the exact
  `str(argv_list)`, so each target is built the same way here and at the call
  site.

**What the user actually sees.** On a build whose `request_permissions()`
accepts the `threading`/`process` keywords, the plugin prompts **once at load**
and never again. The shipped **2.5.0-alpha** accepts **only `fs_read`**, so the
declaration above raises `TypeError` and the plugin falls back to an
`fs_read`-only call that preserves the read grant; on that build the user sees
a **file-read prompt at load** and then **one reactive "create a thread" prompt
on the first thread start**. Approving the thread prompt persists the grant
(`.install_state.json` then records the thread grant and no process grants);
after that, **Repair, Analyze and Show file run with no further prompts**. (The
same fallback keeps older nightlies working.)

CLI repair/analyze spawns are **not** declared: they execute inside
plugin-created worker threads, whose audit context is empty, so on OrcaSlicer
2.5 they are not audited and raise no prompt.

**Denials are named, not guessed.** When a permission is denied anyway, the
panel message names the operation that failed: a **red banner** only when the
repair or Analyze job itself could not start ("approve it to repair" /
"approve it to analyze"), and a **quiet notice** for a denied background status
refresh ("the panel cannot refresh background status") or broken-mesh scan
("the broken-mesh scan is disabled"). A denied secondary thread therefore never
reads as a repair failure.

**Honest scope.** Process declarations are not existence-checked, and the
per-run CLI commands carry unique staging paths, so they can never be a stable
declared target; they are covered by the unaudited-worker behaviour above
rather than by a declaration.

## Install (OrcaSlicer 2.5.0+ / nightly)

1. Install Sutura first: `./install.sh` (Linux) or `install-macos.sh` (macOS),
   so `~/.local/bin/sutura` exists.
2. Copy the plugin folder into OrcaSlicer's plugin dir:

   ```sh
   mkdir -p ~/.config/OrcaSlicer/orca_plugins/SuturaRepair
   cp sutura_repair_linux_x86_64.py ~/.config/OrcaSlicer/orca_plugins/SuturaRepair/
   ```

   (macOS: `~/Library/Application Support/OrcaSlicer/orca_plugins/`.)
3. Enable it in the OrcaSlicer Plugins dialog, then run it to open the panel.

The plugin is also published to the **OrcaCloud plugin listing** (its version
follows the Sutura release tag), so it can be installed from OrcaSlicer's
Plugins dialog without the manual copy.

## Configuration

- Panel settings (preset, notify about broken meshes, open panel at startup)
  are stored in the plugin's capability config.
- `sutura_cli` config key — explicit CLI path; empty (default) auto-detects
  `~/.local/bin/sutura`, then `sutura` on `PATH`.
- `SUTURA_CLI` env var — highest-precedence CLI override.
- `ORCA_BIN` env var — override the OrcaSlicer binary used for
  `--single-instance` (default `orca-slicer`).

## Known limitations

- The repaired surface is added as a **new object**; the original object is
  left untouched. Press A (Arrange) to separate it from the original, or
  Ctrl/Cmd+Z to remove it (OrcaSlicer records a "Load File" undo snapshot).
- Because the export is in world coordinates, OrcaSlicer may re-centre the
  imported object on reload (or drop it on some builds), which places it on
  top of the original. The exact placement after reload has not been verified
  on a real nightly (owner live test pending); the A (Arrange) shortcut is the
  intended way to separate the copy.
- The export is a per-object triangle soup of its model parts; Sutura repairs
  the union surface. Overlapping or intentionally separate parts are treated
  as one solid.
- OrcaSlicer 2.5.0+ / nightly only (see the version requirement above).

## Feedback

This is experimental. If you find a bug, a missing feature, or something that
does not work in your OrcaSlicer, please open an issue at
https://github.com/Krateian/Sutura/issues — your report helps make it better.
