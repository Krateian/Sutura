---
name: sutura-docs
description: Use whenever Sutura user documentation must be added to or updated — a new feature, flag, method, install step, limitation or release note — to put it on the right docs/guide page in English and Turkish and keep the README landing pages, links and index consistent.
---

# Sutura documentation upkeep

The user docs are split so that nothing has to be rewritten from scratch:
short landing pages, one topic page per subject, and a Turkish twin for every
English page. This skill says where new information goes and how to keep the
two languages and all links in step. Canonical copy lives in this repo at
`.claude/skills/sutura-docs/SKILL.md`; `docs/agents/workflow.md` (README
upkeep) holds the same rules in short form.

## Layout

| File | Holds | Edit when |
|---|---|---|
| `README.md`, `README.tr.md` | Landing page: logo, badges, highlights, install, quick use, 4-step pipeline summary, documentation index, maintainer note, contributing, sponsors, license | Only when its own content changes (see below) |
| `docs/guide/<page>.md` | Full detail on one topic, English | Any user-visible change in that topic |
| `docs/guide/tr/<page>.md` | Turkish translation of the same page, same file name | Every time its English twin changes |
| `CHANGELOG.md` | Every change in detail, English only, `## [Unreleased]` until a release | Every user-visible change |

Topic pages and what belongs on each:

| Page | Put here |
|---|---|
| `how-it-works.md` | Pipeline order and what each stage does, in plain words |
| `methods.md` | Repair methods (#1–#N), intensity presets, external engines, per-method flags |
| `feature-status.md` | Maturity table and percentages per area |
| `install.md` | Requirements, installers, AppImage, macOS, extras, updating, uninstalling |
| `usage.md` | CLI options, GUI behaviour, report fields, Dolphin, Quick Action, OrcaSlicer plugin |
| `troubleshooting.md` | Symptom → cause → fix entries |
| `limitations.md` | Robustness notes and known limitations |
| `mesh-classifier.md` | Mechanical/organic detection and type-aware behaviour |
| `architecture.md` | Libraries, Rust core, module layout |
| `privacy.md` | Anonymous usage history and opt-out |
| `testing.md` | Tests and benchmarks for developers (never the private local benchmark) |
| `version-history.md` | One short entry per feature release (stingy: 0.9.x fixes, then 1.0) |

## Where does new information go

1. Pick the single page whose topic owns the change. If two pages touch it,
   write the detail once on the owner page and link to it from the other
   (`[Methods](methods.md#anchor)`), never copy paragraphs.
2. A new section on an existing page is preferred over a new page. Create a new
   page only for a topic big enough to stand alone; then add it to both
   documentation tables in `README.md` and `README.tr.md` (one row, EN and TR
   links) and give it the standard header (see below).
3. Touch the landing pages only for: method count or highlights, install
   one-liners, the quick-use examples, the 4-step pipeline summary, the
   documentation table, the maintainer note, license. Everything else stays on
   the topic pages.
4. Add a `CHANGELOG.md` line under `## [Unreleased]` (Added / Changed / Fixed),
   plain language for ordinary users.
5. New repair method: follow `sutura-new-method` for the code side; on the docs
   side it touches `methods.md`, `how-it-works.md` (if the pipeline order
   changes), `feature-status.md`, both landing pages (method count) and the
   repo About text.

## Writing rules

- Plain language for ordinary 3D-printing users; precise terms where needed,
  no marketing. Understated tone in `feature-status.md`: never round up.
- English is the source. Translate the same change into the Turkish twin in
  the same session, without being asked. The Turkish page must have the same
  headings in the same order and the same number of headings, so that both
  outlines match.
- Every page starts with:
  - EN: `# Title`, blank line, `[← Back to README](../../README.md) · [Türkçe](tr/<page>.md)`
  - TR: `# Başlık`, blank line, `[← README'ye dön](../../../README.tr.md) · [English](../<page>.md)`
- Relative paths: from `docs/guide/` the repo root is `../../`, from
  `docs/guide/tr/` it is `../../../`. Cross-page links use the file name plus a
  GitHub anchor (`usage.md#cli-options`). If a heading is renamed, search both
  languages for links to its old anchor.
- Never name private test samples, and never mention the local reference-engine
  benchmark. Public `core_*` / Thingi10K IDs are fine.
- No AI or tool attribution anywhere: docs, commits, PR bodies.
- Screenshots in `assets/` are regenerated with `scripts/generate_screenshots.py`
  (English UI) only when the visible GUI changed.

## Check before committing

Run the read-only checker from the repo root:

```sh
python3 .claude/skills/sutura-docs/scripts/check_docs.py .
```

It reports:

- missing link targets and anchors in the landing pages, every guide page,
  `CHANGELOG.md` and `CONTRIBUTING.md`;
- a guide page without its EN or TR twin;
- EN/TR outlines that differ (a sign the translation lags behind);
- a missing back/language line;
- a guide page that is not in both documentation tables.

It must end with `0 problem(s)` before a commit. Then commit as Krateian, open
a PR without any footer, and merge on green.

## Release time

`sutura-release` covers the release itself. On the docs side, at every release:

- audit `feature-status.md` against the git log since the previous tag;
- add a `version-history.md` entry in both languages;
- move `## [Unreleased]` to the version heading in `CHANGELOG.md`;
- check the landing pages (method count, install commands, badges) and the
  repo About text.
