---
name: sutura-dev
description: Use for any Sutura code work — features, fixes, reviews, worker prompts, tests. Daily rules (graphify navigation, worker discipline, validation, git hygiene). Canonical copy lives in this repo at .claude/skills/sutura-dev/SKILL.md.
---

# Sutura development rules

Single source of truth for Claude, OpenCode workers and other agents.

## Code navigation: graphify first
- Use `graphify query "<question>"` (also `graphify explain <symbol>`,
  `graphify path <a> <b>`) to find code before any grep or file crawl.
  Narrow with context filters when the answer is too long.
- Run `graphify update .` after every commit so the graph stays current
  (git hooks also refresh graphify-out/).
- Every worker prompt must tell the worker to use graphify and to refresh it
  after commits.

## Workers (tmux session `sutura`)
- Claude plans, reviews and decides; workers execute. Grunt work (long test
  runs, repetitive verification) goes to a worker; Claude spot-checks the real
  output, not the worker's summary.
- Every worker prompt asks for a short plan before file changes.
- A worker stops at 45% context; it writes its progress to a /tmp file and is
  then relaunched from that file (relaunching is not ctrl+x n). Gemini/agy:
  /clear.
- Never switch worker models without asking; expensive models need explicit
  permission.
- Prompts for workers are shown to the maintainer in their own clean code
  block, never mixed with commentary.
- Check workers every 10-15 minutes; always schedule the next check.

## Challenges (objections) — mandatory
- Every worker plan opens with a "Challenges" section: what in the brief or
  design the worker thinks is wrong, risky or unnecessary. No minimum count —
  if there are none, say so in one sentence with the reason (no padding).
- Each challenge carries evidence (a `file:line`, or the output of a command
  actually run); without evidence it is only an idea and does not change the
  decision.
- The lead answers every challenge explicitly (accepted / rejected / partly,
  with reason) and says plainly when its own decision was wrong.
- Typical valid rejections: scope creep; collision with another worker's files;
  a new knob/flag/env var where an existing one already suffices.
- No file changes before the lead's "approved".

## Validation
- Judge repairs by shape, not bytes (holes, non-manifold, parts/shells, Orca
  parts + manifold, distance to input within tol, leak). Never worse on any
  mesh; include thingi10k_100827.
- Iterate on a small representative set; full corpus only at gates. No
  115-mesh benchmark until the desktop PC is back.
- Corpus/test output goes to /tmp, never into the repo. Tests must not depend
  on machine-local files.
- Independent verification by a worker that did not write the change.

## Product rules
- Improvements ship ON by default (opt-out); shape-deforming passes stay
  opt-in behind a warning.
- Keep README.md and README.tr.md in sync with any user-visible change, in the
  same work. Release notes and docs in plain language.
- Every feature: check the macOS side (install-macos.sh, conda) as well as
  Linux; say explicitly if something cannot work on macOS.
- Clean-room only: no code from CGAL, VCG/MeshLab, MeshFix or Blender; papers
  are fine and credited as "inspired by".

## Git
- Commit identity Krateian <arifgokdas@gmail.com>; commit by explicit paths
  only (no `git add -A`, no `commit -a`, no stash). English commit messages
  with a human-readable body. No AI attribution anywhere.
- Never push, tag, release or bump the version without the maintainer's
  approval. Push over HTTPS with the gh token, never SSH.
- Destructive commands (rm -rf, rsync --delete, branch/tag deletion) need
  confirmation first.
- Never copy or name the private sample in tracked files.
- Releases: follow the sutura-release skill.
