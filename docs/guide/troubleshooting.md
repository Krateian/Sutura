# Troubleshooting

[← Back to README](../../README.md) · [Türkçe](tr/troubleshooting.md)


* **macOS says the app is from an unidentified developer.** Expected: the bundle
  is ad-hoc signed, not notarized. Open it once with **right-click → Open**, or
  clear the quarantine attribute:
  `xattr -dr com.apple.quarantine ~/Applications/Sutura.app`.
* **`--check-update` or `--repair-install` says there is nothing to download.**
  Those two need a published build for your exact platform. Only four exist:
  Linux x86_64, Linux aarch64, macOS arm64 and macOS Intel. On anything else
  they refuse rather than guessing, and point at the releases page.
* **An update did not start afterwards.** It should have put the previous version
  back by itself and told you where from. If it says the rollback also failed,
  your previous version is still in `~/.local/share/sutura.backup/`; restore it
  with `sutura --rollback`, or copy it over `~/.local/share/sutura` keeping
  `data/`.
* **`sutura --rollback` went back too far, or not far enough.** It restores the
  **newest** backup only, and only two are kept — the third-newest is deleted on
  each new update. There is no command to pick an older one.
* **The update prompt did not appear.** It only asks when there is a terminal. A
  run without one declines by design and prints
  `(no terminal to ask on; pass --yes to confirm)`. Add `--yes` in scripts.
* **The GUI has no KDE file dialog.** The native dialog needs
  `plasma-integration` installed in your desktop. Without it the toolkit falls
  back to its own dialog — Ctrl/Shift+click still work for multi-selection.
* **A hard crash left no message, but there is a crash log.** When Sutura (CLI or
  GUI) crashes, it writes a `crash.log` in Sutura's user-data folder, and the GUI
  also shows a short dialog pointing at Diagnostics. The log keeps only its most
  recent part (~200 KB). Location by OS: on Linux and macOS it is
  `~/.local/share/sutura/data/crash.log`; when the `SUTURA_DATA_DIR` environment
  variable is set, it is `$SUTURA_DATA_DIR/crash.log` instead. No Windows build
  exists. The file never leaves your machine on its own — nothing is uploaded;
  `sutura --diagnostics out.zip` bundles a scrubbed copy that you can attach to a
  bug report yourself.
* **`--whats-new` says no notes are saved.** The notes are written when an
  install succeeds, so a fresh install has none until the first update or repair.
  The command points at the releases page instead.

If a repair fails, or a method looks unavailable, three commands help diagnose
and recover the installation:

* **`sutura --self-check`** verifies the installation by exercising the repair
  methods on a tiny built-in mesh and prints a summary line
  (`Methods: N/19 OK (X failed, Y skipped)`) in a couple of seconds. A method
  that cannot be loaded or that fails fails the check; an optional addon that is
  simply not installed is reported as skipped, not failed. The command exits
  non-zero if any method fails. The GUI runs the same availability check when it
  starts and shows a dismissible banner if a method is unavailable.
* **`sutura --diagnostics out.zip`** writes a single ZIP archive with the system
  information, the state of the geometry core, the availability of all nineteen
  methods, the user settings, the most recent repair record, and a scrubbed copy
  of the crash log (`crash_log.txt`, present only when a crash log exists).
  Personal paths and user names are scrubbed from the archive, and no file names
  are included, so it can be attached to a bug report.
* **`sutura --repair-install`** re-downloads the release package for the version
  you are **running** (never the newest one, so it cannot update or downgrade by
  accident), verifies its `sha256`, and swaps it in. It backs up the current
  install first and rolls back if the new tree does not start. It never touches
  user data (history, learning, settings).

  It **asks before replacing anything, and the answer defaults to no** — press
  Enter and nothing happens. `sutura --repair-install --yes` answers for you,
  and a run with no terminal declines rather than assuming consent.
  `--self-repair` is the same command under Python's older name. The GUI's
  **Repair installation** button does the same thing.

  If you want the opposite — going back to the previous version instead of
  reinstalling this one — that is `sutura --rollback`.
