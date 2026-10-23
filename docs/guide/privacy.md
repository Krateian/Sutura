# Usage history (privacy)

[← Back to README](../../README.md) · [Türkçe](tr/privacy.md)

## Usage history (anonymous, opt-out)

Sutura records a *technical usage history* of repairs so the community can
steer the engine (mesh_classifier + stage-1/2 tuning) toward the cases it
actually meets. Every shared history helps make the repair engine smarter
over time — more real-world meshes means better classifier tuning and
fewer edge cases missed.

* **What is recorded:** mesh size (vertex/face counts), defect counts (holes,
  non-manifold, self-intersections), classifier result (type + confidence),
  repair mode, applied filter count, final category, repair-confidence score,
  repair timing, and a geometry-only fingerprint for deduplication.
* **What is NEVER recorded:** file names, file paths, user names, IP
  addresses, machine identifiers, environment variables, or any personal
  data. The fingerprint is derived from mesh geometry only.
* **Default:** on. Disable from the CLI with `sutura --no-history ...`, or
  uncheck the box in the first-run dialog (GUI). The preference lives in
  `~/.config/sutura/config.json` (`history_enabled`).
* **Sharing:** `sutura export-history` prints an anonymous summary plus the
  full JSON records in one command, ready to paste into a GitHub issue. Use
  `--summary-only` for just the summary, `--last N` to limit to the most
  recent records, and `--clear` to wipe the file. Nothing is ever uploaded
  automatically.
* **Where it lives:** the data files (`history.jsonl`,
  `triage_learning.json`) live in `~/.local/share/sutura/data/`, separate
  from the application code; set `SUTURA_DATA_DIR` to store them elsewhere
  (useful for the AppImage, whose bundled code mount is read-only). The
  uninstaller keeps this directory by default; `uninstall.sh --purge`
  removes it.
