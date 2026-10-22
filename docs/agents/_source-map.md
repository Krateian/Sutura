# AGENTS.md source map

- Source: `origin/main:AGENTS.md`
- Source commit: **5e28b78**
- Source size: **147159 bytes / 681 lines**
- Rule: every source range is assigned exactly once. Blank/heading lines are
  assigned to the target document header. Content is moved unchanged unless the
  row says `rewritten-summary`.
- Verification: for each `moved` row, `git show 5e28b78:AGENTS.md | sed -n
  'A,Bp'` must match the corresponding section in the target file (header
  differences only). This `diff -u` check is a required gate.

| Source lines | Bytes | Target | Status |
|---|---:|---|---|
| 1-14 | 708 | `docs/agents/workflow.md` | moved (verbatim original overview; AGENTS has a short summary) |
| 15-34 | 1116 | `docs/agents/workflow.md` | moved (skills source; AGENTS has a short index) |
| 35-38 | 1515 | `docs/agents/pipeline-stages.md` | moved |
| 39-40 | 3142 | `docs/agents/packaging-updater-selfrepair.md` | moved (flagged REVIEW) |
| 41-43 | 4218 | `docs/agents/pipeline-stages.md` | moved |
| 44 | 2229 | `docs/agents/gui-viewer.md` | moved |
| 45-48 | 5942 | `docs/agents/diagnosis-classifier.md` | moved |
| 49-54 | 12251 | `docs/agents/gui-viewer.md` | moved |
| 55-56 | 6071 | `docs/agents/diagnosis-classifier.md` | moved |
| 57-58 | 5094 | `docs/agents/si-repair.md` | moved |
| 59-63 | 8026 | `docs/agents/rust-geom.md` | moved |
| 64 | 5257 | `docs/agents/tiers-graft-dressing.md` | moved |
| 65-66 | 5054 | `docs/agents/si-repair.md` | moved |
| 67 | 4110 | `docs/agents/rust-geom.md` | moved |
| 68-69 | 9176 | `docs/agents/closing-holes.md` | moved |
| 70-71 | 4077 | `docs/agents/pipeline-stages.md` | moved |
| 72-74 | 46 | `AGENTS.md` | blank/heading only |
| 75-103 | 10019 | `docs/agents/tiers-registry.md` | moved |
| 104-107 | 10271 | `docs/agents/tiers-graft-dressing.md` | moved |
| 108-130 | 9328 | `docs/agents/closing-holes.md` | moved |
| 131-144 | 3977 | `docs/agents/diagnosis-classifier.md` | moved |
| 145-153 | 3294 | `docs/agents/closing-holes.md` | moved |
| 154-166 | 4414 | `docs/agents/tiers-registry.md` | moved |
| 167-183 | 3953 | `docs/agents/pipeline-stages.md` | moved |
| 184 | 42 | `AGENTS.md` | blank only |
| 185-186 | 3069 | `docs/agents/packaging-updater-selfrepair.md` | moved |
| 187-219 | 4715 | `docs/agents/pipeline-stages.md` | moved (flagged DUP for 211-218) |
| 220-246 | 1849 | `docs/agents/tiers-registry.md` | moved |
| 247-299 | 4623 | `docs/agents/tiers-registry.md` | moved |
| 300-309 | 4304 | `docs/agents/rust-geom.md` | moved |
| 310-378 | 4606 | `docs/agents/legacy-and-duplicates.md` | moved |
| 379-414 | 2174 | `docs/agents/legacy-and-duplicates.md` | moved |
| 415-436 | 1169 | `docs/agents/verification-bench.md` | moved (flagged DUP) |
| 437-466 | 12722 | `docs/agents/verification-bench.md` | moved (flagged OUTDATED?) |
| 467-475 | 1940 | `docs/agents/workflow.md` | moved |
| 476-558 | 5489 | `docs/agents/workflow.md` | moved (flagged DUP) |
| 559-584 | 1596 | `docs/agents/workflow.md` | moved |
| 585-674 | 5525 | `docs/agents/workflow.md` | moved (flagged DUP) |
| 675-681 | 404 | `docs/agents/workflow.md` | moved |

## Target sizes

| Target | Bytes |
|---|---:|
| `AGENTS.md` | 6290 |
| `pipeline-stages.md` | 15431 |
| `diagnosis-classifier.md` | 16519 |
| `tiers-registry.md` | 7504 |
| `tiers-graft-dressing.md` | 23814 |
| `si-repair.md` | 9339 |
| `closing-holes.md` | 10254 |
| `gui-viewer.md` | 8464 |
| `packaging-updater-selfrepair.md` | 4039 |
| `rust-geom.md` | 15607 |
| `verification-bench.md` | 14334 |
| `workflow.md` | 17053 |
| `legacy-and-duplicates.md` | 8509 |

All topic files are below the 25 KB limit. The `AGENTS.md` byte budget is
separate: **≤ 12288 bytes**.

## Migration measurement

| Metric | Before | After |
|---|---:|---:|
| `AGENTS.md` bytes | 147159 | **6290** |
| `AGENTS.md` lines | 681 | **102** |
| `AGENTS.md` estimated tokens (bytes / 4) | ~36789 | **~1572** |
| Topic docs bytes | 0 | 155317 |
| Topic docs files | 0 | 12 |

The deterministic gate is byte/line/token; the OpenCode `/new` context
percentage is a manual observation (baseline reported 40-50%) and is not
claimed as reproducible. Verification result: **37/37 source ranges** found in
their target files; **13/13** `docs/agents/*.md` links in `AGENTS.md` resolve.
