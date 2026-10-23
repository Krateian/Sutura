# Troubleshooting

[← Back to README](../../README.md) · [Türkçe](tr/troubleshooting.md)


* **`python3.11` not found (optional fTetWild only).** A normal install does
  not need Python 3.11. If you pass `SUTURA_WITH_FTETWILD=1`, `install.sh`
  downloads a standalone CPython 3.11 build into
  `~/.local/share/sutura/python311` (disable with
  `SUTURA_NO_PYTHON_DOWNLOAD=1`) or uses the system package:
  * Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
  * Fedora: `sudo dnf install python3.11`
* **`sutura_geom` source build fails.** The Manifold kernel is compiled from
  C++: install `cmake` and a C++20 compiler (g++/clang++), and Rust >= 1.89.
  Release installs normally use the prebuilt wheel and need none of these.
* **PySide6 install fails.** The GUI needs `PySide6-Essentials`, which is
  installed into the `venv` from PyPI. The version follows the interpreter:
  `6.12.0` on Python 3.12+, `6.11.2` on Python 3.11. On distros where `pip install
  PySide6-Essentials` fails (missing build tooling or a blocked PyPI), install
  the system Qt Python bindings instead and point the entry point at them:
  * Debian/Ubuntu: `sudo apt install python3-pyside6`
  * Fedora: `sudo dnf install python3-pyside6`
  * Arch: `sudo pacman -S pyside6` (in the official `extra` repository)
* **The GUI has no KDE file dialog.** The native dialog needs `plasma-integration`
  and a system Qt version that matches PySide6's. If the rubber-band selection
  is missing, Qt falls back to its embedded dialog — Ctrl/Shift+click still
  work for multi-selection.
* **PyMeshLab lingers on an upgraded install.** Since the native stage-1
  engine, PyMeshLab is neither installed nor imported at runtime (it survives
  only as an optional test-time parity oracle in `requirements-dev.txt`). An
  install upgraded from an older version may still carry it as an orphan
  package; remove it with `pip uninstall pymeshlab` (or
  `conda remove -n sutura-env pymeshlab`). Sutura does not use it.
* **Headless GUI "hangs" right after launch.** The first run (no config at
  `~/.config/sutura/config.json`) opens a **modal** update/usage-history
  dialog and blocks until answered. Since the version after 0.4.1 the dialog
  is skipped on Qt's windowless platforms (`QT_QPA_PLATFORM=offscreen` or
  `minimal`) and nothing is written, so it is still asked on the first
  on-screen start. On any other display without a user (e.g. a forwarded X
  session in a script), pre-seed the config, e.g.
  `{"check_for_updates": false, "history_enabled": true}`. The update
  prompt is skipped for AppImage builds; the history prompt is always asked
  on first run.
* **macOS: `grep: empty (sub)expression` on every `git push`.** The pre-push
  security hook's secret pattern used an empty alternation branch that BSD
  grep rejects, so the secret scan silently never ran. Fixed in
  `scripts/pre-push-security-check.sh` (optional group) — reinstall the hook
  with `./install.sh` / `./install-macos.sh`.
* **Standalone .app: stage 2 reports skipped / low confidence.** Stage 2 runs
  in the bundled `sutura_geom` extension. If it is missing from the bundle,
  Stage 2 is reported as skipped and confidence drops ~25 points; reinstall
  with `install-macos.sh`.
* **Antipodal winding number regression / opting out.** Sutura uses the
  accelerated antipodal robust generalized winding number by default across
  Graft, Tamponade, Dressing, and SDF generation. If an unusual mesh exhibits
  unwanted behaviour or you need exact parity with the classic winding number
  oracle, set `SUTURA_ANTIPODAL=0` in the environment to opt out and restore the
  classic winding path globally (the `antipodal=False` Python argument only
  reaches Graft's `shell_wrap`, whereas `dressing.py` has no switch).

If a repair fails, or a method looks unavailable, three commands help diagnose
and recover the installation:

* **`sutura --self-check`** verifies the installation by exercising the repair
  methods on a tiny built-in mesh and prints a summary line
  (`Methods: N/18 OK (X failed, Y skipped)`) in a couple of seconds. A method
  that cannot be loaded or that fails fails the check; an optional addon that is
  simply not installed is reported as skipped, not failed. The command exits
  non-zero if any method fails. The GUI runs the same availability check when it
  starts and shows a dismissible banner if a method is unavailable.
* **`sutura --diagnostics out.zip`** writes a single ZIP archive with the system
  and Python runtime information, the state of the `sutura_geom` extension, the
  availability of all nineteen methods, the user settings, and the most recent
  repair record. Personal paths and user names are scrubbed from the archive,
  and no file names are included, so it can be attached to a bug report.
* **`sutura --self-repair`** checks the installed application files against the
  release manifest for the installed version and repairs a missing or modified
  one. It always asks before replacing anything, backs up the files it changes
  and rolls back if verification fails, and it never touches user data
  (history, learning, settings). AppImage and macOS `.app` installs are
  read-only, so they print a pointer to the releases page instead of repairing
  in place. `sutura --self-repair --yes` skips the confirmation prompt.
