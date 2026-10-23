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
- Never worse on any mesh. thingi10k_100827 (71-face low-poly with flipped
  faces) is the canary: always include it.
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

## 3. Docs and visuals
- Version bumped everywhere (sutura/repair.py VERSION, engine __init__,
  diagnosis, OrcaSlicer plugin).
- CHANGELOG [x.y.z] with date, written in plain language for ordinary users.
- User docs in sync, EN `docs/guide/` and TR `docs/guide/tr/`: feature-status,
  version-history, how-it-works pipeline order; README landing pages
  (`README.md`, `README.tr.md`): method count, highlights, install commands.
- Screenshots regenerated (LANG=en_US.UTF-8, offscreen) and every PNG looked
  at: the defect must be visible in before/after, the options dialog shows
  the current defaults.
- Release body: plain language, correct pipeline order, known limitations,
  screenshots.

## 4. Git and GitHub
- main is protected (required_linear_history, no force push). Never push a
  branch with merge commits to main. Push release/vX.Y.Z, open a PR, wait
  for ALL checks green, squash-merge.
- Tag the squash commit on main (annotated, "Sutura vX.Y.Z"); push over
  HTTPS with the gh token, never SSH.
- Pre-push scan: tokens, personal paths, hostnames, the private sample name,
  AI attribution. Commits by Krateian only; no AI attribution in commits,
  PR bodies or releases.
- gh release create --title "Sutura vX.Y.Z" -F body; after the release
  workflows (AppImage, dmg, wheels, OrcaCloud) finish, re-check that the body
  is intact and the assets are attached.

## 5. After the release
- Repo About description and topics are current (method count, core).
- Orca Cloud listing shows the new version.
- Open Dependabot PRs: merge after the release once green.
- Cleanup: list worktrees, merged branches and temp output, and ask the
  maintainer before deleting anything.
- Write the handoff note for the next session.
