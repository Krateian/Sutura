# AGENTS.md

Sutura: two-stage STL/3MF mesh repair for 3D printing. Stage 1 is the native
`sutura_geom` + `sutura_engine` filter chain; Stage 2 is the Manifold solid
rebuild linked into the same Rust extension. Default repair stays fast and
simple; the exact core, the mesh classifier and the experimental tiers are
opt-in behind CLI flags and the GUI Options window. Repo: `Krateian/Sutura`,
branch `main`.

## Hard rules (read every session)

1. **Plan first.** Non-trivial work starts with a short plan and a
   **Challenges** section (what is wrong/risky/unnecessary, each with evidence).
   Do not edit before explicit approval.
2. **No unapproved changes.** Never push, tag, release, merge a PR or bump the
   version without a named go-ahead. Local commits are allowed only when asked.
3. **Graphify first.** Before grep/read of source, run `graphify query`,
   `graphify explain` or `graphify path`; read only the ranges the graph points
   to. Run `graphify update .` after commits. If graphify is unavailable,
   stale or unhelpful, fall back to targeted grep immediately.
4. **Ponytail ladder.** Before writing code: (1) is it needed? (2) is it already
   in the codebase (`graphify`)? (3) do stdlib/platform provide it? (4) does an
   installed dependency provide it? (5) for a new tool/module, does
   `patent --fast --json "<idea>"` show a ready implementation? (6) will one
   line do? Only then write minimal code. The plan's Challenges section records
   these answers; `patent` is only for new tool/module ideas.
5. **Karpathy surgical change.** Touch only the requested lines. Delete dead
   code you created; do **not** delete existing dead code — mark it in
   `docs/agents/legacy-and-duplicates.md`. If a refactor touches far more lines
   than the requested behaviour change, justify it in the commit/PR. This rule
   is for code; an approved documentation migration is an explicit exception.
6. **Git identity and push.** Commit as `Krateian
   <294938791+Krateian@users.noreply.github.com>`. Stage explicit paths only
   (no `git add -A`, no `-a`, no stash). Push over HTTPS with the `gh` token,
   never SSH. English commit messages, no AI attribution.
7. **Private data.** Never copy, name or log a private sample, path, user or IP
   in tracked files. Use the existing `_scrub` / `priv_<sha8>` rules.
8. **Shape, not bytes.** Judge repairs by holes, non-manifold edges, parts/
   shells, Orca parts+manifold, two-sided distance and leak. Never accept a
   result worse than the baseline on any gate mesh. T0 is CI; T1/T2/T3 follow
   `.claude/skills/sutura-verify/SKILL.md`.
9. **Default ON.** Improvements ship enabled with an opt-out; shape-deforming
   or user-input tiers stay opt-in behind a warning. Every CLI feature needs a
   GUI control (and vice versa) unless documented in
   `docs/cli-gui-parity-notes.md`.
10. **Version thrift.** Patches bump `0.0.1`; a minor bump is the owner's
    explicit decision. Batch small fixes; do not release per commit. Releases
    follow `.claude/skills/sutura-release/SKILL.md`.
11. **Documentation tone.** Academic, professional, third-person, neutral.
    No first-person narration, nicknames or real personal names. Keep
    `README.md` and `README.tr.md` in sync for user-visible changes.

## Architecture map

| Area | One line |
|---|---|
| `sutura/repair.py` | CLI entry: JSON/`--human`, validate/dry-run, Stage 1→2 flow, P-WELD, reports. |
| `sutura/sutura_engine/` | Method registry, triage, adapters, diagnosis, health, graft/dressing/mirror/wall. |
| `rust/sutura-geom/` | Mandatory `sutura_geom`: predicates, winding/SDF, morph, arrangement, splint, ray-stab. |
| `sutura/gui.py` + `heatmap.py` | PySide6 GUI shells out to the CLI; CPU rasterizer/viewer, no PyMeshLab in-process. |
| `classification.py` / `confidence.py` | Stdlib-only category/issues/confidence shared by CLI and GUI. |
| `docs/agents/` | Topic details split from this file; open only the row that matches your trigger. |

## Read this when

Do **not** bulk-read `docs/agents/`. Pick the one row that matches the task.

| Trigger | Read |
|---|---|
| CLI/entry points, Stage 1/2, deep-repair ladder, P-WELD, engine layers, 3MF/read-only/history | `docs/agents/pipeline-stages.md` |
| classification, confidence, health/defects, mesh classifier, learning triage | `docs/agents/diagnosis-classifier.md` |
| method numbers/slugs/aliases, registry, **adding a method**, Tamponade/Flap, repeat/transplant | `docs/agents/tiers-registry.md` |
| Graft, Dressing, Dressing opt-in UX | `docs/agents/tiers-graft-dressing.md` |
| self-intersection: Autorefine, SI excise/re-cap, local exact, SI mode | `docs/agents/si-repair.md` |
| hole closing, proxy/scaffold, fTetWild, mirror completion, wall thicken | `docs/agents/closing-holes.md` |
| GUI, heatmap, before/after, interactive viewer, Qt subprocess rules | `docs/agents/gui-viewer.md` |
| install.sh, AppImage, updater/rollback, Orca plugin, macOS/Linux packaging | `docs/agents/packaging-updater-selfrepair.md` |
| Rust core, `sutura_geom`, morphology, splint, ray-stabbing, build/performance | `docs/agents/rust-geom.md` |
| T0-T3, test scripts, benchmarks, shape validation | `docs/agents/verification-bench.md` |
| conventions, process discipline, README upkeep, release, cleanup | `docs/agents/workflow.md` |
| backlog, resolved backlog, duplicate/outdated markers | `docs/agents/legacy-and-duplicates.md` |

Source/line reconciliation for the split lives in
`docs/agents/_source-map.md` (source commit `5e28b78`).

## Skills

Read these when the task matches; they are the single source for their areas:

- `.claude/skills/sutura-dev/SKILL.md` — daily rules, validation, git hygiene.
- `.claude/skills/sutura-release/SKILL.md` — full release checklist.
- `.claude/skills/sutura-verify/SKILL.md` — independent shape verification.
- `.claude/skills/sutura-new-method/SKILL.md` — adding a repair method.

When a rule changes, edit the skill or the matching `docs/agents/` file; do not
grow this index into a second copy.

## Maintenance

Keep this file an **index plus hard rules**, not a manual. New detail goes to the
matching `docs/agents/` file and the trigger table gains one row. If a source
block moves, update `docs/agents/_source-map.md` and re-run the `diff -u` gate.
