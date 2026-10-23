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
  (git hooks also refresh graphify-out/; `graphify hook install` writes only
  post-commit/post-checkout, so a local `post-merge` hook refreshes after
  fast-forward merges/pulls, which fire neither).
- Graphify is AST-only: its call liveness misses `getattr`/dynamic dispatch,
  module-attribute calls (`m.close()`), and names re-exported through star
  shims (`sutura/wall_thickness.py`: `from sutura_engine.wall import *`).
  For deletion claims, confirm with a targeted `git grep '<symbol>'` of the
  symbol name and check the star re-export shims before deleting.
- Every worker prompt must tell the worker to use graphify and to refresh it
  after commits.

## Ponytail ladder (before new code)
- Ask, in order: is it needed? -> is it already in the codebase (`graphify`)?
  -> do stdlib/platform provide it? -> does an installed dependency provide it?
  -> for a new tool/module, does `patent --fast --json "<idea>"` show a ready
  implementation? -> will one line do? Only then write minimal code.
- The plan's Challenges section records these answers in one line each.
  `patent` is only for a genuinely new tool/module idea; do not call it for
  ordinary fixes.

## Surgical changes (Karpathy)
- Touch only the requested lines. Delete dead code you created; do **not**
  delete existing dead code — mark it in `docs/agents/legacy-and-duplicates.md`.
- If a refactor touches far more lines than the requested behaviour change,
  justify it in the commit/PR. An approved documentation migration is an
  explicit exception; code changes are not.

## Anatomy / dead-code rule
- `scripts/anatomy.sh` cross-checks vulture (static, `--min-confidence 60`)
  against coverage.py (dynamic) over `sutura/`. It writes `vulture.txt`,
  `vulture_whitelisted.txt`, the coverage HTML/summary and
  `dead_code_candidates.txt` (symbols vulture flags AND coverage never ran)
  under `SUTURA_ANATOMY_OUT` (default `/tmp/sutura-anatomy`). Read-only.
- `scripts/vulture_whitelist.py` suppresses only framework false positives:
  Qt event handlers, PEP 562 `__getattr__` shims and protocol dunders. Do not
  add real code to it to quiet a finding.
- Delete a symbol only when ALL four hold:
  1. vulture flags it (`vulture_whitelisted.txt`);
  2. coverage never executed it (`dead_code_candidates.txt`);
  3. `git grep -w '<name>'` is clean of call sites, re-exports and tests;
  4. the import walk is clean — no dynamic `getattr`/registry string, no star
     shim (`sutura/wall_thickness.py`, `mirror_repair.py`).
  Any single failed check means keep it. The candidates list is a starting
  point for manual review, never an auto-delete list.

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
- Big tasks only (a new feature or a phase start): the plan also answers "What
  would we lose if we skipped this entirely or did it much simpler?" — not for
  small fixes.
- One rebuttal round: if the worker disagrees with the lead's answer it may
  reply once with new evidence; the lead then decides.
- Cross-model review: for big or risky work (security, user data, a phase
  plan) a second worker on a different model reviews the plan or the finished
  diff read-only and reports findings ranked by severity before merge.
- The lead does not rubber-stamp: when accepting a batch of challenges it
  verifies the riskiest ones itself.
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
- Keep the user docs in sync with any user-visible change, in the same work:
  short landing pages `README.md` / `README.tr.md`, detail in `docs/guide/`
  (EN) and `docs/guide/tr/` (TR), always updated as a pair. Release notes and docs in plain language.
- Every feature: check the macOS side (install-macos.sh, conda) as well as
  Linux; say explicitly if something cannot work on macOS.
- Clean-room only: no code from CGAL, VCG/MeshLab, MeshFix or Blender; papers
  are fine and credited as "inspired by".

## Git
- Commit identity Krateian <294938791+Krateian@users.noreply.github.com>; commit by explicit paths
  only (no `git add -A`, no `commit -a`, no stash). English commit messages
  with a human-readable body. No AI attribution anywhere.
- Never push, tag, release or bump the version without the maintainer's
  approval. Push over HTTPS with the gh token, never SSH.
- Destructive commands (rm -rf, rsync --delete, branch/tag deletion) need
  confirmation first.
- Never copy or name the private sample in tracked files.
- Releases: follow the sutura-release skill.
