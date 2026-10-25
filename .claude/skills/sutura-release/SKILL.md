---
name: sutura-release
description: Use for any Sutura release, version bump, merge to main, "release et" request, or end-of-feature wrap-up — the full checklist so nothing is forgotten. Canonical copy lives in this repo at .claude/skills/sutura-release/SKILL.md.
---

# Sutura release checklist

Run every item in order and tick it in the task list. The maintainer should
never have to remind any of these. This file is the single source of truth:
Claude, OpenCode workers and any other agent read this same file.

## 0. Standing product rules (check before prep)
- Every finished improvement ships ON by default (opt-out). Never propose
  leaving one off. Only passes that can deform the shape (e.g. Dressing) stay
  opt-in behind a warning.
- Judge outputs by SHAPE, never bytes: reload holes, non-manifold edges,
  components / closed shells, slicer (Orca) part count + manifold, two-sided
  distance to the input within tol = clamp(bbox_diag/10000, 0.0125, 0.05) mm,
  outside leak. File size and byte identity do not matter.
- No measured mesh is worse. A deliberate trade is recorded with its numbers
  and approved by the lead; it is never hidden as a metric exception.
  thingi10k_100827 (71-face low-poly with flipped faces) is the canary: always
  include it. When fTetWild is adopted on both sides, measure the canary with
  the fTetWild tier off and record that.
- Verification is done by a worker that did not write the code.
- FDM focus; resin is out of scope.
- Code navigation goes through graphify (`graphify query "<question>"`,
  `graphify explain`, `graphify path`) instead of raw grep/file crawling;
  run `graphify update .` after every commit. See the sutura-dev skill.

## 1. Code and defaults
- A default change must reach every entry point: CLI flag + env, GUI checkbox
  (with migration of legacy saved settings), Dolphin ServiceMenu, macOS Quick
  Action, OrcaSlicer plugin.
- Grep the whole repo (sutura/, docs/ incl. docs/guide/, README.md, README.tr.md, CHANGELOG.md,
  GUI tooltips EN + TR, orcaslicer plugin, share/) for stale wording:
  "opt-in", "off by default", old pipeline order, old method count.
- GUI labels and tooltips match current behaviour in BOTH languages.
- New methods appear in --list-methods, the GUI "Use method" menu and the
  method tags / badges.

## 2. Tests that pass on CI, not only on this machine
- No test reads /tmp or any machine-local path. Fixtures live in the repo
  (rust/sutura-geom/tests/fixtures/, tests/fixtures/) and load through
  CARGO_MANIFEST_DIR or repo-relative paths. Never commit the private sample.
- Tests handle CI without the compiled Rust extension (fallback paths).
- cargo test, full Python suite, fixtures-v2 never-worse, offscreen GUI
  launch, --version.
- Real-mesh default vs opt-out comparison on the smoke set (shape verdict,
  runtime delta).
- The release-level test is the final gate in §4; the unit suite is not a
  substitute.

## 3. Docs and visuals
- Version bumped everywhere: sutura/repair.py VERSION, engine __init__,
  diagnosis and the OrcaSlicer plugin header.
- CHANGELOG [x.y.z] with date, written in plain language for ordinary users.
  Every new behaviour AND its limitations are documented in the changelog and
  the limitations page; a new rule is never left only in the code.
- User docs in sync, EN `docs/guide/` and TR `docs/guide/tr/`: feature-status,
  version-history, how-it-works pipeline order; README landing pages
  (`README.md`, `README.tr.md`): method count, highlights, install commands.
  EN and TR must state the same facts, not two different explanations.
- No speed or latency claims unless the release measured them; describe
  behaviour ("skips methods…"), not time saved.
- `python3 .claude/skills/sutura-docs/scripts/check_docs.py .` reports zero
  problems.
- Screenshots: regenerate only when the visible GUI changed
  (`git diff --stat origin/main..release -- sutura/gui.py assets`), with
  LANG=en_US.UTF-8 and offscreen; look at every PNG (defect visible in
  before/after, options dialog shows the current defaults).

## 4. Final gate (release-level, independent)
Run this on the release candidate against `origin/main` before the release.
Each side uses its own isolated `sutura_geom` build; never compare with a
shared installed extension. The harness is
`.claude/skills/sutura-verify/scripts/verify_default_vs_optout.py`
(`--quality --no-visual --exclude-nondeterministic`; see the sutura-verify
skill for the exact invocation and cache).

- **Sets**: the 40 tracked real-world samples + the 36-mesh oracle set
  (quality); the 32-mesh J2 set; the 26 clean anchors (`--byte
  --deterministic`).
- **Determinism**: 37402, 39344 and 41937 twice each with
  `--no-fallback-ftetwild`; byte-identical output is required. fTetWild is an
  external stochastic solver and is outside the guarantee; a mesh that adopts
  it on both sides is SKIPPED-NONDET, not a silent pass.
- **Visual**: the 5-mesh main-vs-candidate set (the worst-5 and the release
  set), comparing input, baseline and candidate outputs; identical output
  bytes count as identical. A fTetWild-adopted mesh is the documented
  exception.
- **T0**: the full CI list, `cargo test --release` and `--features cdt-check`,
  `check_docs.py`, `--version`, `--self-check`, `--list-methods` and the
  offscreen GUI import.
- **Acceptance**: no measured mesh is worse; an intended trade is recorded
  with its measured numbers and approved by the lead. A baseline timeout with
  a candidate result is UNBLOCKED, not worse. The summary must say PASS.
- Keep the run directory (`final-gate.md`, `determinism.md`, the VQA sheet
  and the SI-excise comparison). Read `final-gate.md`, not a gate job's stdout:
  a job log can still show a pre-fix FAIL; re-run the summary after any fix so
  the recorded board is the final PASS.

### Qt offscreen under the queue runner
The `sq` runner only sources `~/.sq/env`, so a Python that creates a
`QApplication` cannot find PySide6's platform plugins. Set the paths
explicitly (the same block the VQA runner uses):

```bash
QT_PLUGINS="$("$PY" -c 'import PySide6, os; print(os.path.join(os.path.dirname(PySide6.__file__), "Qt", "plugins"))')"
export QT_QPA_PLATFORM=offscreen
export QT_QPA_PLATFORM_PLUGIN_PATH="$QT_PLUGINS/platforms"
export QT_PLUGIN_PATH="$QT_PLUGINS"
```

The symptom is `qt.qpa.plugin: Could not find the Qt platform plugin
"offscreen"` (often rc=134). The measurement harness itself should run in the
measurement venv (no Qt) or with the same variables; never run it in the conda
environment without them.

## 4b. Security check (mandatory before release)

Run this after §4 and before the release commit/push. Use the repository's
existing tools; do not install a new scanner for a release.

- **Pre-push secret scan (release range).** Run the tracked hook against the
  exact range that will be pushed:

  ```sh
  printf 'refs/heads/release %s refs/heads/main %s\n' \
    "$(git rev-parse HEAD)" "$(git rev-parse origin/main)" \
    | scripts/pre-push-security-check.sh
  ```

  It must print `pre-push security scan: clean`. It scans the diff for token
  formats, `/home/` absolute paths outside `tests/`, and public IPv4
  addresses. The installed `.git/hooks/pre-push` runs the same check on every
  push and is the last line of defence, not a substitute for this run.
- **Repository-wide path scan.** The hook is diff-only. From the release
  worktree, scan tracked files for host-specific and local-development paths:

  ```sh
  git grep -nE '/Users/|/home/|/tmp/(scorecard|release-|bigrun|vqa-real)|Desktop/|Downloads/' \
    -- ':!tests/' ':!scripts/pre-push-security-check.sh' \
       ':!.claude/skills/sutura-release/SKILL.md' ':!docs/security-audit-2026-09.md'
  ```

  It must return no matches. A legitimate hit is recorded with its path and
  reason in the release note; an unexplained hit blocks the release.
- **Personal information.** The personal names and the place/workplace terms
  are deliberately not written in this file. Set the local pattern from the
  maintainer's notes and scan tracked files; the `LICENSE` copyright line is
  the only allowed name mention:

  ```sh
  : "${PERSONAL_PATTERN:?set PERSONAL_PATTERN to the local name/place/workplace terms}"
  git grep -n -I -i -E "$PERSONAL_PATTERN" -- . ':!LICENSE'
  ```

  Check e-mail addresses separately and allow only `users.noreply.github.com`:

  ```sh
  git grep -n -I -E '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' -- . \
    | grep -v 'users\.noreply\.github\.com' || true
  ```

  The historical `krateian@<hostname>.local` placeholder in
  `docs/agents/workflow.md` is not a real address; record it as a false
  positive or remove it only with the maintainer's approval. Every other hit
  is either fixed in a separate comment-only commit or recorded with a reason.
- **Private sample name.** The name is deliberately not written in this file.
  Set it from the maintainer's local notes and scan tracked files:

  ```sh
  : "${PRIVATE_SAMPLE:?set PRIVATE_SAMPLE to the local private sample name}"
  git grep -nF -- "$PRIVATE_SAMPLE" -- ':!tests/' ':!docs/security-audit-2026-09.md'
  ```

  It must return no matches. If the name is unavailable, record that the
  check could not run and do not report the release as clean.
- **CodeQL.** No open code-scanning alerts:

  ```sh
  gh api 'repos/Krateian/Sutura/code-scanning/alerts?state=open&per_page=100'
  gh run list --workflow=codeql.yml --branch main --limit 1
  ```

  The alert list must be `[]`; the latest completed `main` run must be
  `success`. If the API is unavailable (missing scope or 403/404), record the
  exact error and do not treat an unqueried list as clean. An alert with a
  written reason and a decision (fix, justified dismissal, accepted risk) is
  allowed; an unexplained alert blocks the release.
- **Dependabot.** No open dependency alerts or Dependabot PRs that affect the
  release:

  ```sh
  gh api 'repos/Krateian/Sutura/dependabot/alerts?state=open&per_page=100'
  gh pr list --state open --author 'app/dependabot'
  ```

  The alert list must be `[]`. An open Dependabot PR is merged after the
  release when green; if it is left open, write the reason. A dependency alert
  with a written reason and decision is allowed; an unexplained alert blocks
  the release.
- **Attribution.** The release commits are authored by Krateian only; no AI
  attribution in commits, tags or release notes.
- **Record one line** in the release note:
  `Security: pre-push clean; repo path/name scan clean; CodeQL 0 open; Dependabot 0 open.`
  If any item is not clean, write the exception and the lead's decision.

## 5. Git and GitHub
- **main is not protected** (`gh api repos/Krateian/Sutura/branches/main/protection`
  returns 404 "Branch not protected"). The release does not open a PR.
- Prepare the release branch/worktree from `origin/main`; run the final gate
  on it; bump the version and docs on it.
- Save the history bundle before squashing:
  `git bundle create <repo>/release-<x.y.z>-history.bundle <release-branch>`
  and verify it.
- **One squash commit on `origin/main`**:
  `git switch -c release/vX.Y.Z-squash origin/main`, `git merge --squash
  release/vX.Y.Z`, commit `release: x.y.z`, then confirm
  `git diff --stat release/vX.Y.Z` is empty (the tree must match).
- **One push.** The token never appears in the URL or the output: use the
  oauth2 credential helper.

  ```bash
  git -c credential.helper='!f() { echo username=oauth2; echo password=$(gh auth token); }; f' \
      push https://github.com/Krateian/Sutura.git HEAD:main
  ```

  The output must show the plain URL only. Never force-push and never push a
  branch with merge commits to main.
- The §4b security check is complete; the pre-push hook repeats the
  secret/path scan. Commits by Krateian only; no AI attribution in commits,
  tags or releases.
- Tag the squash commit (annotated, "Sutura vX.Y.Z") and push the tag with the
  same credential helper; confirm `git rev-parse vX.Y.Z^{}` is the squash
  commit.
- `gh release create vX.Y.Z --title "Sutura vX.Y.Z" --notes-file <file>`; read
  the body back (`gh release view vX.Y.Z --json body`) and check it against §6.

## 6. Release notes
- Plain language; the reader is a user, not a maintainer.
- **No unmeasured claims.** The "how it was verified" paragraph stays a
  placeholder until the final gate; fill it with the measured counts and the
  accepted trades (with numbers). Never publish "no mesh was worse" before
  the gate reports it.
- **No private sample name, path, user or IP.** Public corpus IDs only.
- **No AI signature or attribution** in the notes, commits, tags or release.
- State the known limitations and the fTetWild exception explicitly.
- Read the published body back with `gh release view --json body`; a create
  command exiting 0 is not proof that the text is correct.

## 7. After the release
- Wait for the release workflows: AppImage, macOS .app/.dmg, wheels +
  `sutura_geom-SHA256SUMS`, manifest. Check every asset with
  `gh release view vX.Y.Z --json assets`.
- OrcaCloud: the `Publish release to OrcaCloud` workflow step must not be
  skipped. Read the log for `HTTP 201`, `"version":"x.y.z"` and
  `"public":true`; the listing description is the only manual field.
- Repo About description and topics are current (method count, core).
- Open Dependabot PRs: merge only after the release, when green; none is a
  valid outcome.
- Cleanup: list worktrees, merged branches and temp output, and ask the
  maintainer before deleting anything; never bulk-delete. Keep the gate
  evidence and the history bundle.
- Write the handoff note for the next session.
