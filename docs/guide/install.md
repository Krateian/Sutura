# Install and requirements

[← Back to README](../../README.md) · [Türkçe](tr/install.md)

## Requirements

Linux:

* `python3` (>= 3.11) with venv support, for the repair venv. If the system
  Python cannot create a venv, the installer downloads a standalone Python
  3.11 automatically.
* `sutura_geom` is installed from a verified prebuilt wheel, so no Rust,
  cmake or C++ compiler is needed on supported platforms. A source build is
  only a fallback when the toolchain is already present.
* KDE Plasma for the Dolphin service menu (optional; CLI and GUI work anywhere)

macOS (Apple Silicon / Intel):

* Miniforge/conda. If conda is missing, `install-macos.sh` downloads and
  installs Miniforge (~80 MB) automatically, in the same run; an existing
  conda installation is reused. Homebrew is not required.
* Python 3.11 is created in the `sutura-env` conda environment automatically.

Python 3.11 is used as a fallback runtime for the fTetWild tier only when the
main interpreter is outside 3.10–3.14; a normal install with a recent
`python3` does not need it.

* Arch: Python 3.11 is not in the official repositories; `install.sh` downloads a standalone fallback runtime automatically when needed (`SUTURA_NO_PYTHON_DOWNLOAD=1` to opt out)
* Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
* Fedora: `sudo dnf install python3.11`

## Install

### Linux

One line (fetches the latest `main` and installs):

```sh
curl -fsSL https://raw.githubusercontent.com/Krateian/Sutura/main/install.sh | bash
```

Or from a clone:

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install.sh
```

This creates the main virtualenv under `~/.local/share/sutura` (plus a
`venv311` fallback only when the main interpreter is outside 3.10–3.14),
installs the CLI wrapper at `~/.local/bin/sutura`, installs the hicolor app
icons, and registers the Dolphin service menu. Re-running is safe. The
installer adds `~/.local/bin` to the startup file your shell reads (`.zshrc`
for zsh, `.bashrc`/`.bash_profile` for bash) and prints the absolute
`~/.local/bin/sutura` path for the current terminal.

`./uninstall.sh` removes the installed files and PATH line; user data
(`~/.local/share/sutura/data`), configs (`~/.config/sutura`) and the
`sutura-env` conda environment are kept. Sutura removes `sutura-env` only when
its installer created it: interactive runs print the exact path and size and
ask first; non-interactive runs keep it unless `--remove-env` (or `--purge`) is
given. `SUTURA_UNINSTALL_SKIP_CONDA=1` always keeps it. `--purge` (with
confirmation when interactive) also removes user data, configs and the
Sutura-created env. Older installs without the ownership marker keep the env;
the uninstaller prints the manual removal command.

**AppImage (optional).** A self-contained AppImage with a single Python 3.14
runtime bundled for Stage 1, Stage 2 and the GUI — no virtualenvs and no
system `python3.11` needed. Each tagged release ships a prebuilt
`Sutura-x86_64.AppImage` on the
[GitHub releases page](https://github.com/Krateian/Sutura/releases); you can
also build it yourself with `scripts/build_appimage.sh` (produces
`dist/Sutura-x86_64.AppImage`). Make it executable and run:

```sh
chmod +x Sutura-x86_64.AppImage
./Sutura-x86_64.AppImage            # GUI
./Sutura-x86_64.AppImage model.stl  # CLI (writes model_fixed.stl)
```

Unlike the `install.sh` flow, the AppImage build cannot update itself in
place; grab a new AppImage from the releases page above. The Dolphin
right-click service menu is still installed by `install.sh`.

Installation uses pip inside isolated virtualenvs — no AUR, no yay/paru
required, nothing touches your system package manager. The GUI needs
PySide6 (~79 MB download, part of the `venv`); total installed size for
the main virtualenv is roughly 800 MB (the fTetWild tier adds about 6 MB,
see below).

**Rust extension (`sutura_geom`).** The Rust extension `sutura_geom` (~1 MB)
is installed automatically by `install.sh` and `install-macos.sh` from a
verified prebuilt abi3 wheel. The installer retries the release asset briefly
and then uses the latest compatible abi3 wheel; no Rust or cmake is needed on
supported platforms. A source build from `rust/sutura-geom` runs only when no
wheel is available and the toolchain is already present. AppImage and macOS
.app/.dmg bundles include the extension. It accelerates Method #13 Graft (morphology shell wrap),
the Flap surface-hole fill (`flap_fill`), the Dressing coat
(`dressing_coat`) and the Tamponade interior plug (`tamponade`), and carries the
exact indirect predicates. **The extension is required:** when neither a
prebuilt wheel nor an existing source toolchain is available, the installer
stops with one named platform message; startup and `sutura --self-check`
likewise fail hard and name the missing extension instead of degrading
silently. Use `SUTURA_GEOM_WHEEL=<path>` for a local wheel, or
`SUTURA_GEOM_FROM_SOURCE=1` to force compilation when the toolchain exists.
Source builds require Rust >= 1.89 (the `kiddo` 6.3 MSRV).
The former `SUTURA_NO_GEOM=1` opt-out has been removed.

**fTetWild fallback tier (installed by default).** The fTetWild fallback
needs only `pytetwild` — no PyVista or VTK — so it is about **6 MB installed**.
`install.sh` installs it into the main virtualenv by default (it is also
bundled in the AppImage and the macOS `.dmg`). Once installed, the tier is on
by default for meshes stage 1 cannot close (see *Optional tiers* above). To
skip it:

```sh
SUTURA_WITH_FTETWILD=0 ./install.sh
```

Without it the default changes nothing (and the GUI checkbox has no
effect); only `--experimental-fallback-ftetwild` reports an explicit skip.

The tier can also be managed after install, without re-running the installer:
`sutura ftetwild install` / `sutura ftetwild uninstall` (with `--dry-run`,
`--yes`) or the GUI's Options → **Engines** tab install/remove it in the
Sutura environment only, showing the download and on-disk sizes first. Both
refuse to change a bundled or system layout (AppImage, `.dmg`, system Python)
and report why.

On Arch, `install.sh` automatically downloads a standalone Python 3.11 runtime only as the fTetWild fallback interpreter when no in-range `python3` is present (set `SUTURA_NO_PYTHON_DOWNLOAD=1` to disable).

### macOS

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install-macos.sh
```

`install-macos.sh` reuses an existing conda installation, or downloads and
installs Miniforge (~80 MB) automatically when conda is missing; it then
creates a `sutura-env` conda environment (Python 3.11),
installs PySide6/sutura_geom from pip
(PySide6 6.11.2 on the Python 3.11 environment; Linux/Python 3.12+ installs use 6.12.0, and this pin keeps macOS 13 support),
verifies the imports, copies the app files to `~/.local/share/sutura/`, and
creates `~/.local/bin/sutura` (CLI) and `~/.local/bin/sutura-gui` launchers.
It also creates a native **`~/Applications/Sutura.app`** so you can launch the
GUI straight from **Spotlight** (press `Cmd+Space`, type *Sutura*, Enter) —
no terminal needed. It is macOS-only and re-runnable. The fTetWild fallback
tier (~6 MB installed, see the Linux section) is installed by default;
`SUTURA_WITH_FTETWILD=0 ./install-macos.sh` skips it.

The installer also adds a Finder **Quick Action** ("Sutura — Repair"). It
appears for any selected file; select one or more STL/3MF files, right-click →
*Quick Actions* → *Sutura — Repair*. Each file is repaired with the installed
CLI (the `.dmg` app can use its bundled CLI) and a native macOS
notification reports the result — a single file shows `Health: X/100  Risk:
Y/100  Status: <label>`, a batch shows one summary (`N/M repaired, K failed —
see log`). The log for each run is written to
`~/Library/Logs/Sutura/sutura-<timestamp>.log`. Non-STL/3MF files are skipped
and reported. For a `.dmg` app the Quick Action uses the bundled
`sutura-cli`; on a conda install it uses the installed `~/.local/bin/sutura`
(or the conda environment) directly. If the app is a downloaded
(quarantined) copy, the notification tells you to right-click → Open
`Sutura.app` once first.

**Removing a macOS install:** `./uninstall.sh` now handles macOS too. It removes
the app, launchers, Quick Action, logs and icons. `sutura-env` is kept unless
Sutura's installer created it and you confirm interactively (or pass
`--remove-env`/`--purge`); the exact env path and size are printed before
asking, and non-interactive runs keep it by default. User data
(`~/.local/share/sutura/data`) and configs (`~/.config/sutura`) are kept by
default; `--purge` (with confirmation when interactive) also removes them and
the owned env. Older installs without the ownership marker keep the env and
the uninstaller prints the manual removal command. The same script removes the
PATH line it added to `.zshrc`/`.zprofile`/`.bashrc`/`.bash_profile`.

The installer also writes `~/.local/bin` to the startup file your shell reads
(`.zshrc` for zsh, `.bashrc`/`.bash_profile` for bash), and prints the absolute
working path (`~/.local/bin/sutura`) for the current terminal.

Each tagged release also ships a **macOS .dmg** (`Sutura-vX.Y.Z.dmg`, built
by the `Build macOS .app/.dmg` workflow) — a self-contained
`Sutura.app`. Drag it to `/Applications` (or double-click to open) and launch
from Spotlight the same way. The .dmg is **unsigned** (no Apple Developer
Program / notarization applied yet), so the first open shows macOS's
*"unidentified developer"* warning. Open it with **right-click → Open**, or clear the
quarantine attribute first:

```sh
xattr -dr com.apple.quarantine Sutura.app
```
