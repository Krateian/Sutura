---
name: Bug report
about: Report a mesh that fails to repair or repairs incorrectly
title: ''
labels: bug
assignees: ''
---

## Mesh source

How was the mesh produced? (slicer, scanner, CAD export — please be specific,
e.g. "Bambu Studio export", "photogrammetry in Meshroom", "OpenSCAD export")

## Command used

```sh
# the exact command you ran, e.g.:
sutura <file> --human
```

## Output

Paste the `--human` report (or the JSON from the plain command) here:

```
```

## Expected vs. actual behaviour

What did you expect to happen, and what happened instead?

## Diagnostics archive

Please generate and attach a diagnostics archive instead of hand-typed environment info:
- CLI: run `sutura --diagnostics sutura-diag.zip`
- GUI: click the **Diagnostics** button

Attach `sutura-diag.zip` to this issue.

## Crash log (optional)

If Sutura crashed or terminated unexpectedly, attach `crash.log (see Troubleshooting)`.

## Attach the mesh (if possible)

If you can share the file, attach or link it. A reproducible file is the
fastest way to diagnose.
