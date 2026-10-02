# Contributing to Sutura

## 1. License and contribution terms

Sutura is licensed under the **PolyForm Noncommercial 1.0.0** license for all
releases from v0.2.0 onward (releases between v0.1.0 and v0.1.9 remain under
Apache 2.0; see the LICENSE file and the README "License" section for the
exact terms).

**Acceptance of terms.** By submitting a contribution — a pull request, a
patch, a file, or any other copyrightable work — you agree that your
contribution is licensed to the project under the same terms as the project
itself (PolyForm Noncommercial 1.0.0). The project does not accept
contributions on any other or additional terms, and no contribution is
incorporated unless those terms apply.

**Rights of the contributor.** The contributor retains ownership of the work
contributed; nothing in this policy transfers copyright. The license granted
to the project is non-exclusive, so the same work may be published or
licensed elsewhere by the contributor, subject to any third-party rights.

**Rights of the project.** The project may incorporate, modify, distribute
and maintain contributions as part of Sutura under the project's license, and
may remove or refuse any contribution that does not satisfy the license
compatibility or technical requirements below. Acceptance of a contribution
does not obligate the project to keep it.

**Scope of use.** Personal and non-commercial use of Sutura (hobby, research,
education, personal 3D printing) is permitted under the license. Commercial
use requires separate permission from the maintainer; contributors are not
exempt from this and should not assume that contributing grants a commercial
license.

## 2. License compatibility — copyleft dependency policy

Sutura's repair pipeline **imports `pymeshlab` (GPL-3.0) in-process**. Because
the pipeline links against it in the same process, the combined repair
pipeline is treated as a **GPL-derivative work** for compatibility purposes.
This has two consequences that every contributor must respect:

- **No copying of copyleft code.** You must not copy source code from
  GPL-3.0 (or any copyleft) projects into Sutura — including modified
  copies, translations, or near-verbatim reuse. This includes pymeshlab
  itself and any third-party GPL code whose terms would propagate into the
  project's files.
- **Attribution is free; copying is not.** You may draw **inspiration at the
  algorithm or formula level** from copyleft projects, reimplement the idea
  independently from the public scientific literature, and cite the source.
  This is the established policy of the project (precedent: the feature
  evaluations inspired by IvanNik17, GPL-3.0, were reimplemented from the
  public formulas of Weinmann et al. 2015, Wang et al. 2012, Ioannou et al.
  2012, Rabbani et al. 2006 and Hanoeka et al. 2019, with attribution and
  without copying code).

**Symmetry of the rule.** The same boundaries apply in both directions:

- For **inbound** contributions: the project will not accept code that is
  copied from GPL/copyleft sources, and will ask you to either remove it or
  reimplement it cleanly with attribution. This protects the project from
  inheriting obligations it cannot honour under its license.
- For **outbound** use: you, as a contributor, retain the freedom to
  reimplement public formulas and algorithms for your own purposes, and to
  reuse your own contributions elsewhere — while respecting that the code
  inside this repository is PolyForm Noncommercial and may not be copied
  into copyleft projects in violation of its terms.

If you are unsure whether a specific dependency or code fragment is
compatible, open an issue and ask before contributing it.

## 3. Development workflow

- **Setup.** Run `install.sh` (Linux) or `install-macos.sh` (macOS) — each
  creates the two virtual environments (PyMeshLab/VCG stage 1 and
  manifold3d stage 2) and the `sutura` CLI.
- **Tests.** The suite is script-based (no test framework); run the
  `tests/test_*.py` scripts under the venv, the layered-3MF check
  (`tests/make_layered_multiobject_3mf.py --check`), and the manual harnesses
  (40-mesh corpus, 115-mesh strict-watertight benchmark,
  `scripts/calibrate_classifier.py`, `tests/torture_tests.py`) when a change
  touches the corresponding area.
- **Branches and PRs.** Work on a feature branch off `main` and open a pull
  request. One logical change per PR.
- **Standing rules.** CLI features must have a GUI counterpart and vice versa
  (see `docs/cli-gui-parity-notes.md`); a user-visible change must update the
  README (EN and TR), the CHANGELOG and the "Version history" section when it
  adds a feature; the pre-push secret-scan hook runs on every push.

## 4. Pull request checklist

- Tests pass (including the suites relevant to the change).
- User-visible behaviour changes update README.md, README.tr.md and, for
  feature additions, the "Version history" section; CHANGELOG.md is updated.
- Commit identity is `Krateian <arifgokdas@gmail.com>`; commits are in
  English, one logical change each.
- No secrets, no large corpus files (the benchmark corpus lives outside the
  repo and is published as a release asset), no copied copyleft code.

## 5. Releasing

Releases advance by one patch number (`vX.Y.Z` -> `vX.Y.(Z+1)`) unless the
maintainer explicitly decides on a minor bump; changes accumulate in
`CHANGELOG.md` between releases. The release is a single, manual step run from
the maintainer's machine with the maintainer's own `gh` (a user token).

1. Bump the version constant in `sutura/repair.py`, update `CHANGELOG.md`
   (and the README "Version history" row when the release adds a feature), and
   push `main`.
2. Publish the release (this creates the tag `vX.Y.Z` at `main` and publishes
   the release as the user):

   ```sh
   gh release create vX.Y.Z --target main --title vX.Y.Z --notes-file <notes>
   ```

3. The `release: published` event then starts four workflows:
   - `build-appimage.yml`, `build-macos.yml`, `build-sutura-geom.yml` attach
     the AppImage, `.dmg` and prebuilt wheels + `sutura_geom-SHA256SUMS` to the
     existing release, preserving its body (they never pass `name`/`body`/
     `draft`, so a manually written release body is never clobbered).
   - `publish-orcacloud.yml` publishes the plugin to OrcaCloud (it is skipped
     when the plugin file did not change since the previous tag).

Never create or publish the release from a workflow with the automatic
`GITHUB_TOKEN`. GitHub does not start workflow runs for `GITHUB_TOKEN`-authored
events, so `release: published` would never fire and the OrcaCloud publish
would silently never run.

Verify with `gh release view vX.Y.Z --json body,assets` and the repository's
Actions runs. To backfill a single build artifact onto an existing release, run
that build workflow manually with its `tag` input. A failed OrcaCloud publish
is retried by turning the release back into a draft and publishing it again
(this re-fires `release: published`); the OrcaCloud side only accepts an OIDC
token from a `release` event, so a `workflow_dispatch` re-run is rejected.
