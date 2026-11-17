# Install and requirements

[← Back to README](../../README.md) · [Türkçe](tr/install.md)

## Requirements

Sutura is a native program. It does **not** need Python, a virtualenv, conda,
Homebrew, Rust or a C++ compiler — on any supported platform.

Supported platforms, which are exactly the ones the Python version covered, so
nothing is lost in the move:

* **Linux x86_64**
* **Linux aarch64**
* **macOS on Apple Silicon** (arm64)
* **macOS on Intel** (x86_64)

Anything else has no published package. On such a machine `--check-update` and
`--repair-install` cannot fetch anything, and the GUI says so rather than
failing silently.

* Linux needs a typical desktop for the GUI (X11 or Wayland, plus OpenGL) and
  the usual `tar`. KDE Plasma is optional: the installer adds the Dolphin
  service menu when it can, and the CLI and GUI work without it.
* macOS needs 12 or later. The app is not notarized, so the first open shows
  the *unidentified developer* warning — see below.

## Install

### From a downloaded package

Download the package for your machine from the
[releases page](https://github.com/Krateian/Sutura/releases/latest) and put the
installer next to it:

| Platform | Package |
|---|---|
| Linux x86_64 | `Sutura-rust-v<version>-x86_64.tar.gz` |
| Linux aarch64 | `Sutura-rust-v<version>-aarch64.tar.gz` |
| macOS arm64 | `Sutura-rust-v<version>-arm64.dmg` |
| macOS Intel | `Sutura-rust-v<version>-x86_64.dmg` |

Linux:

```sh
bash install-rust.sh --from Sutura-rust-v<version>-x86_64.tar.gz
```

macOS:

```sh
bash install-macos-rust.sh --from Sutura-rust-v<version>-arm64.dmg
```

Both installers read `uname -m`, so they expect the package for *your*
architecture and refuse rather than guess. Leave out `--from` and the installer
uses the newest package sitting beside it, choosing the right architecture on
its own. `--dry-run` (or `SUTURA_DRY_RUN=1`) prints what would be written and
changes nothing.

> **Downloading from Releases is not live yet.** The Rust packages are built by
> the *Build Rust distribution artifacts* workflow, which uploads them as
> workflow artifacts and deliberately creates no release. Until the first Rust
> release is published, the installers work only with a package you supply
> yourself. This section is written for the flow that will be in place, and no
> step here depends on a link that does not exist yet.

### Integrity check

Every package ships a `<package>.sha256` next to it, and the updater **requires**
it: an update whose digest does not match is rejected before anything on disk is
touched (`sha256 mismatch (expected …, got …); not installed`).

The shell installers are more permissive, which is worth knowing:

| | With `.sha256` present | Without it |
|---|---|---|
| `install-rust.sh`, `install-macos-rust.sh` | verified, and a mismatch **stops** the install | prints `note: no …sha256 beside the package; skipping integrity check` and **continues** |
| Updater and `--repair-install` | verified, a mismatch is fatal | cannot proceed at all |

So if you downloaded a package by hand, keep its `.sha256` beside it. Verify it
yourself first if you prefer:

```sh
sha256sum -c Sutura-rust-v<version>-x86_64.tar.gz.sha256   # Linux
shasum -a 256 -c Sutura-rust-v<version>-arm64.dmg.sha256   # macOS
```

### What gets installed

Linux — everything under your home directory, nothing system-wide:

| Path | What |
|---|---|
| `~/.local/share/sutura/bin/sutura` | the CLI |
| `~/.local/share/sutura/bin/sutura-gui` | the GUI |
| `~/.local/share/sutura/bin/ftetwild` | the fTetWild binary, when bundled |
| `~/.local/bin/sutura`, `~/.local/bin/sutura-gui` | symlinks on `PATH` |
| `~/.local/share/applications/sutura-app.desktop` | desktop entry |
| `~/.local/share/kio/servicemenus/sutura.desktop` | Dolphin right-click entry |
| `~/.local/share/icons/hicolor/*/apps/sutura.png` | icons |

macOS:

| Path | What |
|---|---|
| `~/Applications/Sutura.app` | the whole application bundle |
| `~/.local/bin/sutura`, `~/.local/bin/sutura-gui` | symlinks into the bundle |
| `~/Library/Services/Sutura Quick Action.workflow` | Finder Quick Action |
| `~/.local/share/sutura/macos-quick-action.sh` | the Quick Action's helper |

The installer adds `~/.local/bin` to the startup file your shell reads
(`.zshrc`/`.zprofile` for zsh, `.bashrc`/`.bash_profile` for bash, else
`.profile`) with a `# added by Sutura installer` marker, so uninstalling can
remove exactly that line.

**An existing `data/` directory is preserved** across a reinstall: it is set
aside, the program files are replaced, and the data is put back.

There is **no licence acceptance prompt**. `LICENSE` and `ATTRIBUTION.md` ship
inside the package, and the PolyForm Noncommercial terms apply as before; a
commercial licence still needs a separate agreement.

The macOS app is ad-hoc signed, not notarized, so the first launch shows the
*unidentified developer* warning. Open it with **right-click → Open** once, or:

```sh
xattr -dr com.apple.quarantine ~/Applications/Sutura.app
```

### Speed and size

| | Linux x86_64 | Linux aarch64 | macOS arm64 | macOS Intel |
|---|---|---|---|---|
| Download size | — | — | — | — |
| Installed size | — | — | — | — |
| Repair time, typical mesh | — | — | — | — |

These figures are deliberately blank: they will be filled from measurements
against the release package, and are not estimated here.

## Updating

Updates are installed by **`sutura --update`**, or by the GUI's update panel.

```sh
sutura --check-update   # report only: is there a newer release?
sutura --update         # install it, after asking
```

`--check-update` never installs anything. It prints `update available: <tag>`,
`no update available`, or a licence notice when the release crosses a licence
boundary — in that last case it deliberately refuses and points you at the
releases page.

`--update` **asks before doing anything**, and the answer defaults to **no**: a
bare Enter, `n`, or anything not recognised leaves your install alone. `--yes`
answers for you. A non-interactive run (a script, CI) also declines and says so,
because there is nobody to ask.

What an update does:

1. Downloads the package and verifies its `sha256`. Nothing is written until the
   digest matches.
2. Copies the current install to a backup.
3. Unpacks and swaps in the new version, keeping `data/` untouched.
4. Runs the new binary with `--version` and checks it answers with the expected
   version.
5. If that check fails, **puts the previous version back** and tells you where
   from.

### Backups and rolling back

Two backups are kept, in `~/.local/share/sutura.backup/<version>/`. `data/` is
never copied into a backup, so no backup can ever restore over your history.

```sh
sutura --rollback   # restore the newest backup, after asking
```

`--rollback` restores the **newest** backup only. There is no command to pick an
older one: the third-newest is evicted when a third update happens. That is also
why a Python-era backup is kept elsewhere — see
[MIGRATE.md](../../MIGRATE.md).

## Repairing a damaged install

```sh
sutura --repair-install   # reinstall this version over itself
```

It re-downloads the package for the version you are **running** — never the
newest one, so it cannot silently update or downgrade — verifies it, swaps it in
with the same backup and rollback as an update, and asks first (default: no).
The GUI has the same thing as **Repair installation**.

## Release notes

```sh
sutura --whats-new   # the notes saved by the last install, offline
```

The notes are written once a new version has installed successfully, so a failed
update cannot leave you reading notes for a version you do not have. On a fresh
install there may be nothing saved yet, in which case the command points at the
releases page.

## Uninstalling

```sh
bash uninstall-rust.sh           # remove the program, keep your data
bash uninstall-rust.sh --purge   # also remove data, settings and backups
SUTURA_DRY_RUN=1 bash uninstall-rust.sh   # show what would be removed
```

Without `--purge` it removes the program, the launchers, the desktop entries,
the icons, the macOS bundle and Quick Action, and the `PATH` line the installer
added. It **keeps** `~/.local/share/sutura/data` and the update backups, and
says where they are.

`--purge` additionally removes `~/.local/share/sutura` (data included),
`~/.config/sutura`, and `~/.local/share/sutura.backup*` — both the update
backups and a quarantined Python-era backup, if one exists.

## Upgrading from the Python version

A Python install is migrated, not reinstalled: history and learning data are
carried over, the previous install is saved where you can get back to it, and
nothing is removed until the new build has passed its health check. See
[Moving from the Python version](../../MIGRATE.md).
