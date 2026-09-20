# Security audit (pre-v0.3) — 2026-09-20

Scope: a reasonable on-release scan, not a comprehensive pentest. Goal:
find cheap/low-risk issues and fix them directly; report judgment-call or
large changes to the maintainer. No code was changed in this audit — all
findings below are either clean or low-severity recommendations.

## Summary

| # | Area | Result | Severity |
|---|---|---|---|
| 1 | Dependency CVE scan (`pip-audit`) | clean | — |
| 2 | Shell / subprocess injection patterns | clean | — |
| 3 | 3MF XML (XXE) + ZIP (zip-slip) parsing | clean (by design) | — |
| 4 | Path traversal / symlink / output writes | clean | — |
| 5 | Install scripts (curl\|tar, checksums) | 1 recommendation | low |
| 6 | Hardcoded secrets in working tree | clean | — |
| 7 | GitHub Actions (pinning, secret leaks) | 1 recommendation | low |

## 1. Dependency security (`pip-audit`)

`pip-audit` (installed temporarily) was run against the active environment
(pymeshlab, numpy, manifold3d, trimesh, PySide6) and against
`requirements.txt` / `requirements-311.txt` / `requirements-gui.txt`:

- Environment: **no known vulnerabilities found**.
- Requirements files: **no known vulnerabilities found**.

No dependency bumps required.

## 2. Shell / subprocess injection

Scanned the whole codebase for `shell=True`, `os.system`, `eval(`, `exec(`,
`/bin/sh`, `popen(...shell=...)`. The only `exec` matches are Qt's
`QDialog.exec()` / `QApplication.exec()` — not Python `exec`.

Every `subprocess.run`/`Popen` call (repair stage-2 bridge, updater,
GUI CLI shell-out, heatmap/viewer renderers) uses an **argument list**, never
a shell string, and no user-controlled path is interpolated into a shell.
→ **No injection surface.** No fix needed.

## 3. 3MF XML (XXE) + ZIP (zip-slip)

- **XML:** the 3MF parser does NOT use an XML parser at all —
  `repair.read_3mf_units` and `repair.parse_3mf_meshes` read the `.model`
  content as text and use regular expressions (`re.search`/`re.findall`).
  XML entities are therefore never resolved, so an XXE payload cannot trigger
  an external entity fetch. **XXE-safe by construction.** No fix needed.
- **ZIP:** `zipfile.ZipFile` is used read-only (`read`, `namelist`,
  `writestr`) — there is no `extract`/`extractall`, so **zip-slip (path
  traversal via `../` archive entries) is not possible.** No fix needed.
- **Zip-bomb memory — FIXED in FAZ 14:** `repair_3mf` read every archive
  entry fully into memory. It now checks the zip header's declared
  `file_size` against `_3MF_MAX_ENTRY_BYTES` (1 GiB) BEFORE reading each
  entry, so a crafted 3MF with a huge declared uncompressed size fails with a
  controlled error message instead of exhausting memory.

## 4. Path traversal / output writes

The repair output path is always `stem + '_fixed' + ext` in the **same
directory** as the input (repair.py `process_file`). No user-supplied path is
joined onto an unrelated base directory for writing, and no symlink-following
write path was found in the working tree. The fetch script writes only to a
directory the user explicitly passes. → **Clean.**

## 5. Install scripts — checksum verification (LOW, recommendation)

`install.sh` fetches the repo tarball from the official GitHub HTTPS URL and
pipes it to `tar` (`curl -fsSL "$REPO_URL" | tar xz`); `scripts/build_appimage.sh`
downloads python-build-standalone tarballs and `appimagetool.AppImage` via
`curl -fL`. None of these verify a **checksum or signature** of the download.

HTTPS against the official repo is the standard trust anchor for open-source
tools and is reasonable, but pinned SHA-256 checksums (or the AppImage
signature) would harden supply-chain risk. **Not fixed now** (maintainer
decision): adding hashes means maintaining them across version bumps, and it
is a recommendation, not a defect in current behaviour.

**FAZ 14 update (implemented):**

- **python-build-standalone — now verified.** `scripts/build_appimage.sh`
  downloads the project's `SHA256SUMS` from the same release and verifies both
  Python tarballs before extraction (`sha256sum -c`, must get 2 OK, else the
  build aborts). The checksum file itself is fetched over HTTPS from the same
  release — the standard trust anchor.
- **appimagetool — no official checksum exists (verified 2026-09).** The
  `continuous` release publishes only the AppImage binaries, no SHA-256 /
  signature asset, so there is nothing legitimate to pin to. The script now
  documents this explicitly (comment) and continues to fetch over HTTPS from
  the official repo. If AppImage ever publishes a checksum/signature, wire it
  up here.
- **install.sh repo tarball — not pinned (documented).** `install.sh` fetches
  the repo tarball from a **live git reference** (the current default branch /
  latest commit), whose content changes on every commit, so a fixed SHA-256
  pin is architecturally impossible without moving to a tagged/checksummed
  release artifact. This is left as-is and recorded as a known limitation of
  the live-branch install path (the official HTTPS GitHub tarball remains the
  trust anchor).

## 6. Hardcoded secrets

Scanned the working tree for private keys, API keys, passwords and token
formats (`ghp_`, `AKIA…`, `BEGIN … PRIVATE KEY`, `secret=`…). **None found.**
The pre-push secret-scan hook (`.git/hooks` / `scripts/pre-push-security-check.sh`)
enforces this on every push. Git history was out of scope per the task; note:
if the repo is ever made public-wide, a `git log -p` secret sweep would be
worthwhile.

## 7. GitHub Actions (LOW, recommendation)

All workflow actions are pinned to **major versions** (`actions/checkout@v7`,
`codeql-action@v4`, `softprops/action-gh-release@v3`,
`conda-incubator/setup-miniconda@v3`). Major-version pinning is the common,
acceptable practice and is not a defect. The tighter `@<commit-sha>` pin is a
hardening option (avoids a tag being moved). **Not changed** — the user marked
this as a suggestion only.

No workflow `run:` step echoes secrets or environment variables, and no
`secrets.*` are referenced in logs. → No secret-leak risk.

## Decisions

- **Fixed in FAZ 14:** 3MF zip-bomb decompressed-size cap (`repair_3mf` checks
  the zip header `file_size` against `_3MF_MAX_ENTRY_BYTES` = 1 GiB before
  reading each entry; a bomb fails with a controlled error, never a crash);
  python-build-standalone downloads now SHA-256-verified in
  `scripts/build_appimage.sh`.
- **Reported for the maintainer (unchanged):** GitHub-Actions commit-SHA
  pinning (low, suggestion only), 3MF zip-bomb entry cap already done above,
  install.sh live-branch tarball pin (architecturally impossible — documented
  above), appimagetool checksum (no official asset exists — documented above).
- All five code-scan areas (deps, shell, XML/ZIP, paths, secrets) are clean.