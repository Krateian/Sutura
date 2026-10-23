# Workflow

Trigger: Conventions, process discipline, README upkeep, release checklist, cleanup.

Source: `origin/main:AGENTS.md` @ 5e28b78, ranges 1-14, 15-34, 467-475, 476-558, 559-584, 585-674, 675-681.

## Original project overview (verbatim)

# AGENTS.md

Sutura: two-stage STL/3MF mesh repair for 3D printing. Stage 1 = the native
`sutura_geom` + `sutura_engine` filter chain (VCG `meshing_*` semantics,
reimplemented in Rust/Python), stage 2 = the Manifold solid rebuild statically
linked into the same Rust extension. Git repo, branch `main`, remote
`Krateian/Sutura`.

Project goal: a fast and effective repair tool first, in the spirit of CGAL
and Netfabb. The default path stays simple and quick; the extras for advanced
users — the own exact geometry core (`rust/sutura-geom`), the own mesh-type
classifier, the opt-in experimental tiers — sit behind flags and the GUI's
Options window, never in the way of the plain repair.

## Conventions

- Input files are never overwritten — output always gets a `_fixed` suffix (or `-o`).
- Robustness is a hard requirement: truncated files, wrong-ext-mismatched content, NaN/Inf coordinates, empty/degenerate meshes must return a clear JSON `error` and exit 1, never a crash or a silent "clean" report.
- The native Stage-1 chain lives in `stage1_chain`; `delete_fallback_chain` is tried only when the main chain leaves non-manifold edges/vertices, and is kept only if it improves them.
- **CLI/GUI feature parity (standing rule, FAZ14):** every feature available in the CLI must also be available in the GUI, and vice versa. When adding a feature, add it to BOTH by default; only if one side is technically impossible (e.g. a GUI-only visual feature) is it omitted — and that must be explicitly noted (see `docs/cli-gui-parity-notes.md`). A new CLI flag with no GUI control, or a GUI control with no CLI flag, is a gap unless documented.
- **Documentation tone (standing rule):** documentation — README.md, README.tr.md, CHANGELOG.md, `docs/*` and AGENTS.md included — is written in an academic/professional, third-person, neutral engineering-report tone. Casual or first-person narration ("I did", "we tried", "yaptim", "denedim") and real personal names or nicknames are not used; the same tone is kept when adding a new FAZ/section.
- **Git commit identity:** user.name `Krateian`, user.email `294938791+Krateian@users.noreply.github.com` (bu Sutura repo'su icin local git config'te ayarli). Gecmis commit'lerde bazi yanlis/eskimis kimlik girdileri var (ornegin `<krateian@<hostname>.local>`; eskiden gercek isimle atilanlar da dahil) - gecmis duzeltilmedi, bundan sonraki TUM commit'ler Krateian kimligiyle atilmali. Yeni bir makinede/ortamda calisirken once `git config --get user.name` / `user.email` kontrol edilmeli, yanlissa yukarida verilen degerlerle duzeltilmeli.

## Process discipline

- **Codebase exploration (default, not a hard rule):** `graphify` is
  a real CLI (PyPI package `graphifyy`,
  https://github.com/Graphify-Labs/graphify), not just a plugin
  convention. Reach for `graphify query "<question>"` /
  `graphify explain "<symbol>"` / `graphify path "A" "B"` first for focused
  codebase-understanding questions — a graph query returns a small, focused
  answer instead of dumping whole files, which saves tokens and context.
  However, if graphify is not installed, errors out, returns no useful
  result, or a tiny well-known file can be read more directly, fall back to
  raw grep/find/cat immediately. The goal is efficiency, not a rigid rule
  that blocks work when the graph is stale or unavailable. Output lives in
  `graphify-out/` (git-ignored, local per clone; the git hooks rebuild it, or run `graphify update .` once after a fresh clone).
  **On a new machine (graphify is pipx-installed per-machine, not
  repo-tracked), check first:** `which graphify` — if missing, run
  `pipx install graphifyy` (or `uv tool install graphifyy`), then
  `graphify install --platform opencode` (registers the OpenCode plugin +
  skill) and `graphify update .` (builds the graph fresh for this repo).
  Git hooks (`post-commit`/`post-checkout`, installed via
  `graphify hook install`) auto-refresh the graph on commit/checkout; run
  `graphify update .` manually after uncommitted changes if the graph
  needs to be current before that.
- **Plan before touching files (standing rule — needs re-enforcing, it
  slips):** for any non-trivial task, present a plan and get explicit
  approval before making file changes. Do not start editing on a task's
  first pass without that approval step.
- **Challenges (objections) — mandatory (standing rule):** every worker plan
  opens with a "Challenges" section — what in the brief or design the worker
  thinks is wrong, risky or unnecessary. No minimum count; if there are none,
  the worker says so in one sentence with the reason (no padding). Each
  challenge must carry evidence (a `file:line`, or the output of a command
  actually run); without evidence it is only an idea and does not change the
  decision. The lead answers every challenge explicitly (accepted / rejected /
  partly, with reason) and states plainly when its own decision was wrong.
  Typical valid rejections: scope creep, collision with another worker's
  files, a new knob/flag/env var where an existing one already suffices. Big
  tasks only (a new feature or a phase start) answer in the plan: "What would
  we lose if we skipped this entirely or did it much simpler?" — not for small
  fixes. One rebuttal round: if the worker disagrees with the lead's answer it
  may reply once with new evidence, then the lead decides. Cross-model review:
  for big or risky work (security, user data, a phase plan) a second worker on
  a different model reviews the plan or the finished diff read-only and reports
  findings ranked by severity before merge. The lead does not rubber-stamp:
  when accepting a batch of challenges it verifies the riskiest ones itself. No
  file changes before the lead's "approved".
- **Merge and release approval (standing rule):** opening a PR and
  iterating on it — algorithm/math work, benchmarking, pushing more
  commits to an open PR branch — needs no per-step approval. Merging
  that PR (`gh pr merge`, including a backgrounded/nohup script that
  auto-merges once CI is green) and anything under the Release
  checklist below (bumping the version, tagging, pushing to main,
  publishing a GitHub Release) needs the user's explicit go-ahead,
  asked for by name, before it starts. Green CI is not that
  go-ahead. A backlog note that "the next release is a vX.Y.0
  candidate" is not that go-ahead either — it is a note for a human
  to act on, not a queued task. Leave the PR open and say it's ready;
  do not self-merge and do not start the release checklist on your
  own judgment.
- **Push/commit discipline:** do not commit and push each small fix
  individually. Batch small fixes together and ship them under one
  version bump/release (see Release checklist below).
- **Versioning (standing rule):** releases advance in small steps — the
  patch number goes up by 0.0.1 per release (v0.4.1 -> v0.4.2 -> ...),
  even when the batch contains a user-visible behaviour change. Changes
  accumulate under `[Unreleased]` in CHANGELOG.md and ship together. A
  minor bump (vX.(Y+1).0) is the user's explicit decision, never inferred
  from the size or kind of the batch. **v0.5.0 (2026-09-26) is such an
  explicit decision:** the owner chose a minor bump (not v0.4.2) for the
  Triage Engine milestone (intensity presets, user profiles, external
  engines, fTetWild manager, deep-repair ladder, tabbed Options window);
  the previously unreleased `[0.4.2]` CHANGELOG batch was folded into the
  `[0.5.0]` section.
- **Triage Engine module lists (standing rule):** `sutura/triage.py`,
  `sutura/engines.py` and `sutura/ftetwild_manager.py` must stay in the
  explicit module lists of `install.sh`, `install-macos.sh`,
  `scripts/build_appimage.sh`, `sutura/updater.py` and
  `.github/workflows/build-macos.yml` (PyInstaller `--hidden-import`).
  `engines.py` and `ftetwild_manager.py` are imported inside functions, so a
  missing file silently degrades the engine layer to a no-op.
- **Test/corpus output location:** corpus and test-run files always go to
  `/tmp`, never committed to the repo (see also Cleanup discipline below).

## Heavy jobs

Long tests, corpus runs, benchmarks and release builds go through the `sq`
queue (one heavy job at a time, results in `~/.sq/`); workers do not wait on
them. Full rules: `.claude/skills/sutura-dev/SKILL.md` (Heavy jobs).

## README upkeep

- **Layout (since 23 Oct 2026):** `README.md` / `README.tr.md` are short landing
  pages (badges, highlights, install, quick use, short pipeline, documentation
  index, maintainer note, license). All detail lives in `docs/guide/*.md`
  (English) and `docs/guide/tr/*.md` (Turkish), one page per topic with the same
  file names in both languages. Edit the topic page, not the landing page;
  touch the landing page only when its own content changes (method count,
  highlights, install commands, page list).
- The full procedure (which page owns what, EN/TR rules, link checker) is the
  `sutura-docs` skill: `.claude/skills/sutura-docs/SKILL.md`. Run
  `python3 .claude/skills/sutura-docs/scripts/check_docs.py .` before a docs
  commit; it must report 0 problems.
- If a user-visible behaviour is added or changed in this session (CLI flag,
  GUI behaviour, install step, new feature), update the matching
  `docs/guide/` page in the same session — without the user asking.
- The Turkish pages are translations of the English ones and must be kept in
  sync: any user-visible addition/change to a `docs/guide/` page also updates
  its `docs/guide/tr/` twin (and `README.md` changes update `README.tr.md`) in
  the same session, without being asked.
- The README GUI screenshots (`assets/*.png`) are regenerated with
  `scripts/generate_screenshots.py` whenever a visible behaviour changes. The
  generator forces the English locale (`LANG=en_US.UTF-8`) regardless of the
  system locale — screenshots are always the English UI (standing rule).
- Feature Status table audit: at every stable release (v0.1.x) AND every
  minor-series transition (vX.Y.9 -> vX.(Y+1).0), compare the
  `docs/guide/feature-status.md` / `docs/guide/tr/feature-status.md` table and narrative sections against the
  actual codebase (git log between the previous and current tag) — not
  just for the version being released, but to catch any earlier version's
  documentation debt too (a release can ship without its README catching
  up; check for that gap explicitly). Bump percentages only when the
  honest state changed (new tests, calibration, scope) — keep the
  understated tone, never round up for optics. Also re-run
  scripts/generate_screenshots.py and refresh assets/ if the GUI's visible
  state changed since the screenshots were last generated (check via
  git log --follow on the asset files). This audit runs automatically at
  every release — the user does not need to ask or remind.

## Release checklist

> The canonical, current checklist is `.claude/skills/sutura-release/SKILL.md`.
> Read it first; the notes below are older background.

Get the user's explicit go-ahead to start a release, asked for by name
("should I cut vX.Y.Z now?"), before step 1 — green CI, a merged PR, or
a backlog note that a release is pending are not that go-ahead (see
Merge and release approval above). Once given, run ALL of the
following in order, without being asked separately for each step:

1. Bump the version constant (repair.py) — batch bumps, don't bump on every
   commit (see Conventions).
2. Update CHANGELOG.md with a real entry for this version.
3. Commit the version bump with a summary body.
4. Tag the release (vX.Y.Z).
5. **Version-history standing rule (FAZ15):** if this version ADDS a
   user-facing feature/capability, add a short entry to
   `docs/guide/version-history.md` AND `docs/guide/tr/version-history.md` (one line, what
   came in, based on the CHANGELOG Added/Changed user-visible items). Fix-only
   versions do NOT touch that section.
6. Feature Status table audit (`docs/guide/feature-status.md` + TR) against the actual commits
   since the previous tag — bump percentages only when the honest state
   changed; check the test-suite list in the 'Test coverage' row is current.
7. Re-run scripts/generate_screenshots.py if the GUI's visible state changed
   since assets/ was last generated (check via git log --follow).
8. Publish the release from the maintainer's machine with the maintainer's own
   `gh` (a user token):
   `gh release create vX.Y.Z --target main --title vX.Y.Z --notes-file <notes>`.
   This creates the tag `vX.Y.Z` at `main` AND publishes the release as the
   user, which is what fires the `release: published` workflows (the three
   build workflows attach the AppImage/.dmg/prebuilt wheels and
   `publish-orcacloud.yml` publishes the plugin). Never create or publish the
   release from a workflow with the automatic `GITHUB_TOKEN` — GitHub does not
   start workflow runs for `GITHUB_TOKEN`-authored events, so
   `release: published` would never fire and the OrcaCloud publish would
   silently never run.
9. Verify the release notes actually landed as written: after
   `gh release create` / `gh release edit`, run
   `gh release view <tag> --json body` and read the ACTUAL body back.
   Confirm it contains the real highlights list for this version — not a
   generic template blurb, not empty, not truncated. A `gh release
   create`/`edit` call returning success is not proof the content is
   correct; heredocs and multi-line --notes strings can silently get
   mangled by shell quoting. If the body is wrong, edit it again and
   re-verify — do not consider the release done until the displayed body
   matches what was intended.
10. **Verify prebuilt wheels workflow (`build-sutura-geom.yml`):** ensure that
   `build-sutura-geom.yml` is green and its prebuilt wheels (`sutura_geom` abi3
   wheels for Linux and macOS) plus `sutura_geom-SHA256SUMS` are attached to the
   release before announcing it (installers, updater, and bundles depend on these
   assets).

11. **OrcaCloud plugin auto-publish (standing, automatic since 2026-09):**
   every published GitHub Release triggers
   `.github/workflows/publish-orcacloud.yml` via OIDC trusted publishing.
   The plugin's OrcaCloud version is NOT tracked independently anymore -- it
   follows this release's own tag (`github.event.release.tag_name`), and the
   changelog field is this release's own body text. The workflow diffs
   `orcaslicer-plugin/` between this tag and the previous one and SKIPS the
   OrcaCloud publish step entirely if the plugin file did not change -- so a
   release with no plugin changes does not spam a new, unrelated Cloud
   version. No manual Cloud upload/paste step is needed anymore; do not
   revert to doing it by hand out of habit. If the plugin file DID change,
   verify after the fact that the new version actually appears on the Orca
   Cloud listing page (Actions tab -> workflow run -> check the "Publish
   release to OrcaCloud" step didn't get skipped and returned 2xx).
   Since v0.4.1 the step prints the API response and fails on anything but
   HTTP 201 (it used to report success on a 400). The metadata JSON is sent
   with `curl --form-string` because `-F` splits the value at `;`. The
   OrcaCloud side only accepts OIDC tokens from the `release` event, so a
   failed publish is retried by turning the release into a draft and
   publishing it again (same tag, re-fires `published`), never by
   `workflow_dispatch` (which is rejected with HTTP 401). The OrcaCloud
   **listing description** is a free-text field in the OrcaCloud
   Create/Edit-plugin web dialog, not a repository file and not part of the
   publish API payload, so it is maintained manually in the OrcaCloud UI.

This checklist is the release process — steps 5 and 6 are not optional
extras to be reminded about separately; they are part of doing a release at
all.

General principle for this whole checklist: a command exiting 0 means
the command ran, not that its output is correct. For every step that
produces user-facing content (CHANGELOG entry, commit body, README
section, GitHub Release notes), read back what was actually written
before moving to the next step. This applies beyond releases too —
whenever a tool call's success is being used as evidence that content is
correct, verify the actual content instead of trusting the exit status.

## Cleanup discipline

- At the end of any session that created temporary/debug files (test scripts,
  probe files, generated meshes, log files used only for diagnosis), delete
  them without being asked — both in `/tmp` and in the repo. Before finishing,
  run `git status` and a quick `ls` of any temp working directories, and remove
  anything not meant to persist.


## Agent skills (full source block, lines 15-34)

## Agent skills (read first)

Two skill files in `.claude/skills/` are the single source of truth for how
agents work in this repo. Claude Code loads them automatically; OpenCode and
other agents must read them before starting:

- `.claude/skills/sutura-dev/SKILL.md` — daily rules: graphify-first code
  navigation (`graphify query`, `graphify update .` after commits), worker
  discipline, shape-based validation, product defaults, git hygiene.
- `.claude/skills/sutura-release/SKILL.md` — the full release checklist.
- `.claude/skills/sutura-verify/SKILL.md` — independent shape-based
  verification of any repair change (default vs opt-out: holes, non-manifold,
  components/shells, Orca parts+manifold, two-sided distance, leak, runtime);
  the harness lives in its `scripts/`.
- `.claude/skills/sutura-new-method/SKILL.md` — the end-to-end checklist for
  adding a repair method (registry, CLI flag + env opt-out, GUI checkbox +
  tooltip EN/TR + Use-method menu, packaging lists, CI-safe tests, READMEs).

When a rule changes, edit the skill file; do not duplicate it here.

