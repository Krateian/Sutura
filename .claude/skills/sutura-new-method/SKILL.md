---
name: sutura-new-method
description: Use whenever adding, renaming or removing a Sutura repair method (a new tier, slug, number, CLI flag, GUI toggle, or registry entry) — the mandatory end-to-end checklist that keeps the registry, CLI, GUI, installers, packaging, docs and tests in lockstep so nothing silently degrades.
---

# Adding a repair method to Sutura

Why: a method is not just its algorithm. It has a registry number and slug, a
CLI surface and env opt-out, a GUI checkbox/tooltip in EN+TR, a Use-method
menu entry and order tag, an install/packaging entry, a public-sample fixture,
CI-safe tests that run WITHOUT the Rust extension, and rows in both READMEs.
Skip any one and an install, the GUI, or a reviewer silently loses the method.
Every step below is required unless marked optional.

## 0. Design first
- Read `docs/agents/tiers-registry.md` → "How to add a repair method" and the registry code
  (`graphify query "repair method registry"`, `graphify explain RepairMethod`).
- Decide `num`, canonical `id` (slug), `display_name`, `family`,
  `invents_geometry`, and whether it needs user input (`needs_user_input`
  excludes it from the auto-ranking). Improvements ship ON by default unless
  they deform the shape, in which case they stay opt-in.

## 1. Implement
- `sutura/sutura_engine/methods/builtin/<slug>.py`, exporting `METHOD` (or
  `METHODS`, or a `RepairMethod` subclass). One module per slug, named after
  the slug. Keep the algorithm out of `repair.py` where possible.
- Conform to `RepairMethodProtocol` (`sutura_engine.methods.protocol`); reuse
  `RepairContext` / `MethodResult`.
- `available()` returns `(bool, reason)`; put the dependency check in
  `sutura_engine/methods/availability.py` and import it (a missing dep must
  degrade to unavailable, never crash).
- Lazy-import heavy deps; the engine top-level stays stdlib-only.

## 2. Registry + CLI
- Auto-discovery scans `builtin/`, so the module registers itself; add any
  alias/number mappings in `methods/__init__.py` if needed.
- `--list-methods` must show it; accept `--methods <num|slug>` and the alias.
- Add the CLI flag AND an env opt-out (`--no-<slug>` / `SUTURA_<SLUG>=0`), and
  default ON for non-deforming methods. Wire a single resolution helper
  (`resolve_<slug>`) so CLI, env and the library agree.
- Thread the flag through `repair_file` / `repair_3mf` / `process_file` and the
  `--human` report line.

## 3. GUI (CLI/GUI parity is a standing rule)
- Add the batch-wide checkbox in the Options dialog (the right tab) and to
  `_options_defaults`; `RepairWorker` maps it to the CLI flag.
- Add the EN + TR label and tooltip strings in `gui.py`; the tooltip must state
  the default and how to turn it off (never a stale "opt-in").
- The method appears automatically in the right-click "Use method" menu and the
  Method column tag/badge; verify it lists the number and the badge shows.
- Decide whether the tier is also a right-click action (like Try Dressing).

## 4. Surfaces that invoke the CLI
- Dolphin/KDE service, macOS Quick Action (`share/`) and the OrcaSlicer plugin
  (`orcaslicer-plugin/`) shell out to the CLI: confirm a new default needs no
  change, and that a new flag is plumbed where those surfaces pass options.

## 5. Packaging / install lists (easy to forget)
- Add the module to the explicit lists in `install.sh`, `install-macos.sh`,
  `scripts/build_appimage.sh`, `sutura/updater.py` (`APP_MODULES`) and
  `.github/workflows/build-macos.yml` when it is a top-level module. Engine
  modules ship inside the copied `sutura_engine` package automatically.

## 6. Tests (CI-safe)
- A plain script under `tests/` (no framework) that runs WITHOUT the Rust
  extension (fake/stub the tier) so CI covers the Python surface; note the
  extension-only path as a skip. Cover adoption guards and the no-op path.
- Add the suite to `.github/workflows/ci.yml`; generate fixtures with a script
  in `tests/` (never commit machine-local or private files).
- Verify with the sutura-verify skill: default vs opt-out, no worse on every
  mesh, `thingi10k_100827` included.

## 7. Docs + versioning
- User docs (EN `docs/guide/` and TR `docs/guide/tr/`, kept in sync): the
  method table row in `methods.md` (#, slug, display name, family, what it
  does, invents geometry), the percentage in `feature-status.md`, a section in
  `methods.md` / `how-it-works.md` if it is substantial, and a
  `version-history.md` line for a user-visible feature. Update the method count
  in the README landing pages (`README.md`, `README.tr.md`) when it changes.
  Keep the academic/neutral tone.
- `CHANGELOG.md` entry under the unreleased section. Do NOT bump the version or
  tag without explicit maintainer approval.

## 8. Naming (surgical)
- Names matter: prefer the domain metaphor used elsewhere (Tamponade / Stent,
  Flap, Graft, Dressing) over generic labels, and keep slug, module name,
  display name, CLI flag and GUI label consistent. Pick the slug first — it is
  the module name, the `--methods` id and the alias source.
