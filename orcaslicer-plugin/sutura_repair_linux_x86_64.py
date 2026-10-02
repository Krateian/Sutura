# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy"]
#
# [tool.orcaslicer.plugin]
# name = "Sutura Repair"
# description = "Sutura mesh repair in an OrcaSlicer dock panel: repairs the model parts on the plate with the Sutura CLI and loads the repaired copy back as a new object."
# author = "Krateian"
# version = "0.7.2"
# ///
"""Sutura Repair - OrcaSlicer script plugin (v2, dock panel).

The plugin reads the objects on the plate through the ``orca.host`` API,
exports each object's model parts to a single STL in global (world)
coordinates, repairs it with the separately-installed Sutura CLI, and loads
the repaired copy back into the slicer. A dockable HTML panel (embedded from
``orcaslicer-plugin/panel/panel.html``) lists the objects, exposes the repair
presets and per-object Analyze / Repair actions, streams job phases back to
the page through the fixed message protocol, and drives the settings. A
debounced lifecycle scan pushes at most one warning notification per broken
object per session.

numpy IS a declared plugin dependency (``dependencies = ["numpy"]``), so
OrcaSlicer's bundled uv installs it at plugin install time; the zero-copy
``vertices()``/``triangles()`` accessors and the 4x4 transform matrices need
it. A numpy-free fallback path is kept (``vertex(i)``/``triangle(i)``) so the
plugin still imports and reads geometry even where numpy is unavailable, but
without numpy the host cannot hand out transform matrices, so volume/instance
placements are then treated as identity. Repair itself always runs in the
external Sutura CLI (``~/.local/bin/sutura``), which carries its own
numpy/pymeshlab environment.

EXPERIMENTAL - the OrcaSlicer Python plugin system exists only in nightly
builds / releases NEWER than 2.4.2; stable 2.4.2 has no "Plugins" menu.

Scope: the export contains every ``is_model_part()`` volume of an object,
with each volume's local-to-object matrix and the first instance's
object-to-world matrix applied, in that order. Parameter modifiers, negative
volumes and support blockers are skipped (and reported). Multi-part objects
become ONE STL containing all model parts, i.e. the union surface Sutura is
asked to repair. The staging root is derived from the plugin's own install
location (walk up from this file to the ``orca_plugins`` component and take
its parent, i.e. OrcaSlicer's data dir) -- the host exposes no
``data_dir()`` API. Each run stages its input under
``<data_dir>/orca_plugins/.sutura_work/<uuid>/`` and writes the repaired file
to the persistent ``<data_dir>/orca_plugins/.sutura_work/out/`` as
``<object-name>_sutura_<timestamp>.stl`` with any source mesh extension stripped
from the object name (kept so OrcaSlicer can still read it after an asynchronous
load-back; pruned after seven days). The repaired copy keeps the exported world
coordinates and is added as a NEW object, so it lands on top of the original;
press A (Arrange) to separate it, and Ctrl/Cmd+Z removes it (OrcaSlicer records
a "Load File" undo snapshot). The original object is never modified.
"""

import json
import os
import shutil
import struct
import subprocess
import sys
import threading
import time
import uuid

try:  # numpy is a declared plugin dependency; the fallback keeps this file importable without it
    import numpy as _np
except Exception:  # noqa: BLE001
    _np = None

import orca

# The intensity presets the panel offers; must match sutura/triage.py INTENSITIES.
_INTENSITIES = ('quick', 'balanced', 'thorough', 'extreme')

# The default Sutura CLI. Overridable via SUTURA_CLI or the plugin's sutura_cli setting.
_DEFAULT_CLI = os.path.expanduser('~/.local/bin/sutura')

# OrcaSlicer binary used for --single-instance (Linux). macOS uses the native
# bundle-id `open -b com.orcaslicer.OrcaSlicer` instead. Overridable via ORCA_BIN.
_ORCA_BIN = os.environ.get('ORCA_BIN', 'orca-slicer')

_REPAIR_TIMEOUT = 600
_ANALYZE_TIMEOUT = 300
_SCAN_DEBOUNCE = 1.0
_SCAN_EVENTS = frozenset(('ObjectAdded', 'ObjectChanged', 'ProjectOpened'))

# The dock panel is NOT created from on_load(): OrcaSlicer starts on the Home
# tab and restores the Plater/AUI layout after the plugin loads, so a pane
# created then stays hidden even after show(). Creation is deferred to the first
# of these scene events, on the UI thread, once the layout exists. NewProject is
# included even though it does not trigger a rescan.
_PANEL_CREATE_EVENTS = frozenset(('ProjectOpened', 'NewProject', 'ObjectAdded'))

# Source mesh extensions stripped from an object name before building the
# repaired-output filename (avoids ``broken_cube.stl_sutura_...stl``).
_MESH_EXTENSIONS = ('.stl', '.obj', '.3mf')

# The dock page, embedded verbatim from orcaslicer-plugin/panel/panel.html
# (sha256 6e4a6189a8c061a701daa84bfeb1035b3ba2f6d5b6ce5b9e96193bb6af88862a).
# It is embedded so the published single-file plugin is self-contained; whenever
# panel/panel.html changes, copy its full contents back into this raw string and
# update the digest.
_EMBEDDED_PANEL_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Sutura — OrcaSlicer Mesh Repair</title>
<style>
:root {
  --orca-bg: #fff; --orca-fg: #1f2429; --orca-muted: #6b7580;
  --orca-border: #d9dee3; --orca-accent: #009688; --orca-accent-fg: #fff;
  --orca-font: system-ui, -apple-system, sans-serif;
}
@media (prefers-color-scheme: dark) {
  :root {
    --orca-bg: #2b2d30; --orca-fg: #e4e6e8; --orca-muted: #9aa0a6;
    --orca-border: #3d4043; --orca-accent: #009688; --orca-accent-fg: #fff;
  }
}
* { box-sizing: border-box; margin: 0; padding: 0; }
body {
  background: var(--orca-bg); color: var(--orca-fg); font-family: var(--orca-font);
  font-size: 12px; line-height: 1.4; height: 100vh; width: 100%; display: flex; flex-direction: column;
  overflow-x: hidden; user-select: none;
}
button, input { font-family: inherit; font-size: inherit; }
:focus-visible { outline: 2px solid var(--orca-accent); outline-offset: 1px; }

/* Header */
.header {
  flex: none; display: flex; align-items: center; justify-content: space-between;
  padding: 8px 12px; border-bottom: 1px solid var(--orca-border); width: 100%; min-width: 0;
}
.brand { display: flex; align-items: center; gap: 8px; font-weight: 700; font-size: 14px; }
.status-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--orca-muted); flex: none; }
.status-dot.online { background: #10b981; box-shadow: 0 0 0 2px rgba(16,185,129,0.25); }
.status-dot.offline { background: #ef4444; box-shadow: 0 0 0 2px rgba(239,68,68,0.25); }
.btn-icon {
  width: 26px; height: 26px; border-radius: 4px; border: none; background: transparent;
  color: var(--orca-muted); cursor: pointer; display: flex; align-items: center; justify-content: center; flex: none;
}
.btn-icon:hover { color: var(--orca-fg); background: var(--orca-border); }

/* Preset Segmented Bar */
.preset-bar { flex: none; display: flex; padding: 6px 12px; width: 100%; min-width: 0; }
.segmented {
  display: flex; width: 100%; min-width: 0; border: 1px solid var(--orca-border);
  border-radius: 6px; padding: 2px; gap: 2px;
}
.seg-btn {
  flex: 1; min-width: 0; padding: 4px 1px; font-size: 11px; font-weight: 500; border: none;
  border-radius: 4px; background: transparent; color: var(--orca-muted); cursor: pointer;
  text-align: center; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.seg-btn:hover { color: var(--orca-fg); }
.seg-btn.active { background: var(--orca-accent); color: var(--orca-accent-fg); font-weight: 600; }

/* Content Area */
.content-area { flex: 1; overflow-y: auto; overflow-x: hidden; min-height: 0; width: 100%; }

/* CLI Missing */
.cli-missing { padding: 24px 16px; text-align: center; display: flex; flex-direction: column; align-items: center; gap: 8px; }
.cli-missing-badge { font-size: 24px; }
.cli-missing-title { font-size: 13px; font-weight: 600; }
.cli-missing-desc { font-size: 11px; color: var(--orca-muted); max-width: 260px; }
.code-box { display: flex; align-items: center; border: 1px solid var(--orca-border); border-radius: 6px; padding: 4px 8px; width: 100%; max-width: 280px; gap: 6px; }
.code-text { flex: 1; font-family: monospace; font-size: 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: left; }
.btn-copy { font-size: 10px; padding: 2px 6px; border-radius: 4px; border: 1px solid var(--orca-border); background: transparent; color: var(--orca-fg); cursor: pointer; }
.btn-copy:hover { background: var(--orca-border); }

/* Object Rows */
.empty-state { padding: 32px 16px; text-align: center; color: var(--orca-muted); }
.object-list { display: flex; flex-direction: column; width: 100%; }
.object-row {
  border-bottom: 1px solid var(--orca-border); padding: 8px 12px;
  display: flex; flex-direction: column; gap: 4px; width: 100%; min-width: 0;
}
.object-row:hover { background: rgba(128,128,128,0.04); }
.row-header { display: flex; align-items: center; gap: 6px; min-width: 0; width: 100%; }
.row-checkbox { accent-color: var(--orca-accent); cursor: pointer; width: 14px; height: 14px; flex: none; }
.row-name { font-weight: 500; font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0; }
.row-sub { display: flex; align-items: center; justify-content: space-between; gap: 8px; min-width: 0; width: 100%; margin-top: 1px; }
.row-meta { font-size: 11px; color: var(--orca-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0; }
.row-actions { display: flex; align-items: center; gap: 4px; flex: none; }

/* Buttons & Badges */
.btn-accent, .btn-accent-sm {
  border-radius: 4px; border: 1px solid var(--orca-accent); background: var(--orca-accent);
  color: var(--orca-accent-fg); cursor: pointer; font-weight: 600; font-size: 11px;
}
.btn-accent { padding: 6px 12px; }
.btn-accent-sm { padding: 2px 8px; font-weight: 500; white-space: nowrap; }
.btn-accent:hover:not(:disabled), .btn-accent-sm:hover:not(:disabled) { opacity: 0.9; }
.btn-accent:disabled { opacity: 0.45; cursor: not-allowed; }
.btn-quiet {
  padding: 2px 8px; font-size: 11px; border-radius: 4px; border: 1px solid var(--orca-border);
  background: transparent; color: var(--orca-fg); cursor: pointer; white-space: nowrap;
}
.btn-quiet:hover { background: var(--orca-border); }

.badge { display: inline-flex; align-items: center; font-size: 10px; font-weight: 500; padding: 1px 5px; border-radius: 4px; white-space: nowrap; flex: none; }
.badge-ok, .chip-success { color: #10b981; background: rgba(16,185,129,0.12); border: 1px solid rgba(16,185,129,0.3); }
.badge-warn { color: #f59e0b; background: rgba(245,158,11,0.12); border: 1px solid rgba(245,158,11,0.35); }
.badge-bad, .chip-failed { color: #ef4444; background: rgba(239,68,68,0.12); border: 1px solid rgba(239,68,68,0.3); }

/* Running & Result */
.running-box { margin-top: 4px; display: flex; flex-direction: column; gap: 4px; background: rgba(128,128,128,0.05); padding: 6px 8px; border-radius: 4px; }
.progress-bar-track { height: 3px; width: 100%; background: var(--orca-border); border-radius: 2px; overflow: hidden; position: relative; }
.progress-bar-bar { position: absolute; top: 0; bottom: 0; width: 35%; background: var(--orca-accent); border-radius: 2px; animation: progress-indet 1.3s infinite ease-in-out; }
@keyframes progress-indet { 0% { left: -35%; width: 35%; } 50% { left: 35%; width: 45%; } 100% { left: 100%; width: 25%; } }
.running-row { display: flex; align-items: center; justify-content: space-between; font-size: 11px; }
.running-status { color: var(--orca-fg); display: flex; align-items: center; gap: 6px; }
.running-time { color: var(--orca-muted); font-variant-numeric: tabular-nums; }
.result-row { margin-top: 3px; display: flex; align-items: center; justify-content: space-between; gap: 6px; }
.chip { display: inline-flex; align-items: center; font-size: 11px; font-weight: 500; padding: 2px 6px; border-radius: 4px; white-space: nowrap; }
.chip-cancelled { color: var(--orca-muted); background: var(--orca-border); }
.link-show-file { background: transparent; border: none; color: var(--orca-accent); cursor: pointer; font-size: 11px; text-decoration: underline; padding: 2px 4px; }
.result-hint { margin-top: 4px; color: var(--orca-muted); font-size: 11px; line-height: 1.4; }

/* Analysis Drawer */
.analysis-drawer { margin-top: 6px; padding: 8px; border-radius: 6px; border: 1px solid var(--orca-border); display: flex; flex-direction: column; gap: 6px; }
.analysis-top { display: flex; align-items: center; justify-content: space-between; }
.analysis-heading { font-size: 11px; font-weight: 600; text-transform: uppercase; color: var(--orca-muted); }
.btn-close-analysis { background: transparent; border: none; color: var(--orca-muted); cursor: pointer; font-size: 11px; }
.analysis-error { color: #ef4444; font-size: 11px; line-height: 1.35; }
.defect-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 4px; }
.defect-cell { background: rgba(128,128,128,0.05); border: 1px solid var(--orca-border); border-radius: 4px; padding: 4px 6px; text-align: center; }
.defect-val { font-size: 13px; font-weight: 700; font-variant-numeric: tabular-nums; }
.defect-val.warn { color: #f59e0b; }
.defect-val.bad { color: #ef4444; }
.defect-val.zero { color: var(--orca-muted); }
.defect-lbl { font-size: 9px; color: var(--orca-muted); text-transform: uppercase; }
.rec-list { display: flex; flex-direction: column; gap: 4px; }
.rec-row { display: flex; flex-direction: column; gap: 2px; }
.rec-meta { display: flex; justify-content: space-between; font-size: 11px; }
.rec-name { color: var(--orca-fg); font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.rec-conf { color: var(--orca-muted); font-variant-numeric: tabular-nums; font-weight: 600; margin-left: 6px; }
.rec-bar-track { height: 4px; background: var(--orca-border); border-radius: 2px; overflow: hidden; }
.rec-bar-fill { height: 100%; background: var(--orca-accent); border-radius: 2px; }

/* Sticky Footer */
.footer { flex: none; border-top: 1px solid var(--orca-border); background: var(--orca-bg); padding: 8px 12px; display: flex; flex-direction: column; gap: 6px; width: 100%; }
.footer-actions { display: flex; gap: 8px; width: 100%; min-width: 0; }
.btn-block { flex: 1; text-align: center; min-width: 0; white-space: nowrap; }
.settings-details { font-size: 11px; }
.settings-summary { cursor: pointer; color: var(--orca-muted); user-select: none; }
.settings-summary:hover { color: var(--orca-fg); }
.settings-content { margin-top: 4px; display: flex; flex-direction: column; gap: 6px; }
.setting-label { display: flex; align-items: center; gap: 6px; cursor: pointer; color: var(--orca-fg); }
.setting-label input { accent-color: var(--orca-accent); cursor: pointer; }
.disclaimer { font-size: 10px; color: var(--orca-muted); text-align: center; }
.hidden { display: none !important; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0,0,0,0); border: 0; }
</style>
</head>
<body>

  <!-- Header -->
  <header class="header">
    <div class="brand">
      <span>Sutura</span>
      <span id="cli-dot" class="status-dot" title="Checking CLI..." aria-label="CLI status"></span>
    </div>
    <button type="button" id="btn-refresh" class="btn-icon" title="Refresh scene objects" aria-label="Refresh">
      <svg viewBox="0 0 16 16" width="13" height="13" fill="currentColor">
        <path d="M13.65 2.35A8 8 0 1 0 16 8h-2a6 6 0 1 1-1.76-4.24l-2.24 2.24H16V0l-2.35 2.35z"/>
      </svg>
    </button>
  </header>

  <!-- Preset Selector -->
  <div class="preset-bar" id="preset-bar">
    <div class="segmented" role="radiogroup" aria-label="Repair Preset">
      <button type="button" class="seg-btn" data-preset="quick" title="Fast repair, skips heavy solid rebuild">Quick</button>
      <button type="button" class="seg-btn active" data-preset="balanced" title="Default two-stage repair with manifold3d rebuild">Balanced</button>
      <button type="button" class="seg-btn" data-preset="thorough" title="Aggressive repair with lower thresholds for stubborn defects">Thorough</button>
      <button type="button" class="seg-btn" data-preset="extreme" title="Maximum hole filling and aggressive component merging">Extreme</button>
    </div>
  </div>

  <!-- Main Scrollable Area -->
  <div class="content-area">
    <div id="cli-missing-view" class="cli-missing hidden" role="alert">
      <div class="cli-missing-badge">⚠️</div>
      <div class="cli-missing-title">Sutura is not installed</div>
      <p class="cli-missing-desc">Install Sutura via bash to enable mesh repair, solid rebuilds, and defect diagnostics.</p>
      <div class="code-box">
        <span class="code-text" id="install-cmd">curl -fsSL https://raw.githubusercontent.com/Krateian/Sutura/main/install.sh | bash</span>
        <button type="button" id="btn-copy-install" class="btn-copy" title="Copy install command">Copy</button>
      </div>
      <button type="button" id="btn-check-cli" class="btn-accent" style="margin-top: 6px;">Check again</button>
    </div>

    <div id="object-list" class="object-list" role="region" aria-label="Objects on plate"></div>
    <div id="empty-state" class="empty-state hidden">No objects on the plate.</div>
  </div>

  <!-- Sticky Footer -->
  <footer class="footer" id="panel-footer">
    <div class="footer-actions">
      <button type="button" id="btn-repair-selected" class="btn-accent btn-block" disabled>Repair selected (0)</button>
      <button type="button" id="btn-select-broken" class="btn-quiet">Select broken</button>
    </div>
    <details class="settings-details" id="settings-details">
      <summary class="settings-summary">Settings</summary>
      <div class="settings-content">
        <label class="setting-label">
          <input type="checkbox" id="chk-notify-broken" checked>
          <span>Notify about broken meshes</span>
        </label>
        <label class="setting-label">
          <input type="checkbox" id="chk-open-startup" checked>
          <span>Open panel at startup</span>
        </label>
      </div>
    </details>
    <div class="disclaimer">Repaired copies are added as new objects; originals stay unchanged.</div>
  </footer>

  <div id="aria-status" class="sr-only" aria-live="polite"></div>

<script>
(function () {
  'use strict';

  const $ = id => document.getElementById(id);
  const on = (id, evt, fn) => $(id)?.addEventListener(evt, fn);

  const state = {
    cli: { found: false, path: '', version: '' },
    settings: { preset: 'balanced', notify_broken: true, open_panel_at_startup: true },
    objects: [],
    selectedIds: new Set(),
    jobs: new Map(),
    analyses: new Map(),
    expandedAnalyses: new Set()
  };

  const send = msg => { if (window.orca?.postMessage) window.orca.postMessage(msg); };
  const announce = txt => { const el = $('aria-status'); if (el) el.textContent = txt; };
  const esc = s => !s ? '' : String(s).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));

  const formatTris = n => typeof n !== 'number' ? '0 tris' : n >= 1e6 ? (n/1e6).toFixed(1)+'M tris' : n >= 1e3 ? (n/1e3).toFixed(1)+'k tris' : n+' tris';
  const renderMetaLine = o => `${o.parts > 1 ? o.parts + ' parts' : '1 part'} · ${formatTris(o.triangles)}${o.modifiers > 0 ? ' · ' + o.modifiers + ' mod' : ''}`;
  const renderStatusBadge = o => o.status === 'warn' ? `<span class="badge badge-warn">${o.orca_fixed_errors || 1} fixed by Orca</span>` : (o.status === 'bad' || o.manifold === false) ? '<span class="badge badge-bad">Not manifold</span>' : '<span class="badge badge-ok">OK</span>';

  function updateCliDot() {
    const dot = $('cli-dot');
    if (!dot) return;
    const ok = !!(state.cli?.found);
    dot.className = 'status-dot ' + (ok ? 'online' : 'offline');
    dot.title = ok ? (state.cli.version || state.cli.path || 'Sutura CLI ready') : 'Sutura CLI not found';
    $('cli-missing-view')?.classList.toggle('hidden', ok);
    $('preset-bar')?.classList.toggle('hidden', !ok);
    $('panel-footer')?.classList.toggle('hidden', !ok);
    if (ok) renderObjectList();
    else { if ($('object-list')) $('object-list').innerHTML = ''; $('empty-state')?.classList.add('hidden'); }
  }

  function updatePresetButtons() {
    document.querySelectorAll('.seg-btn').forEach(btn => {
      btn.classList.toggle('active', btn.getAttribute('data-preset') === state.settings.preset);
    });
  }

  function updateSettingsUI() {
    const s = state.settings;
    if ($('chk-notify-broken') && s.notify_broken !== undefined) $('chk-notify-broken').checked = !!s.notify_broken;
    if ($('chk-open-startup') && s.open_panel_at_startup !== undefined) $('chk-open-startup').checked = !!s.open_panel_at_startup;
    updatePresetButtons();
  }

  function updateFooter() {
    const btn = $('btn-repair-selected');
    if (!btn) return;
    const n = state.selectedIds.size;
    btn.textContent = `Repair selected (${n})`;
    btn.disabled = n === 0;
  }

  function renderObjectRowHtml(obj) {
    const id = String(obj.id);
    const isSel = state.selectedIds.has(id);
    const job = state.jobs.get(id);
    const isRunning = job && ['queued', 'exporting', 'repairing', 'loading'].includes(job.phase);
    const hasResult = job && ['done', 'failed', 'cancelled'].includes(job.phase);
    const analysis = state.analyses.get(id);
    const isExp = state.expandedAnalyses.has(id) && !!analysis;

    let h = `<div class="object-row" id="row-${esc(id)}" data-id="${esc(id)}">
      <div class="row-header">
        <input type="checkbox" class="row-checkbox" data-id="${esc(id)}"${isSel ? ' checked' : ''} aria-label="Select ${esc(obj.name)}">
        <span class="row-name" title="${esc(obj.name)}">${esc(obj.name)}</span>
        ${renderStatusBadge(obj)}
      </div>
      <div class="row-sub">
        <span class="row-meta">${renderMetaLine(obj)}</span>
        <div class="row-actions">
          ${!isRunning ? `
            <button type="button" class="btn-quiet btn-analyze" data-id="${esc(id)}">Analyze</button>
            <button type="button" class="btn-accent-sm btn-repair" data-id="${esc(id)}">Repair</button>
          ` : ''}
        </div>
      </div>`;

    if (isRunning) {
      const phase = job.phase ? (job.phase[0].toUpperCase() + job.phase.slice(1)) : 'Working';
      const time = job.elapsed_s != null ? `${typeof job.elapsed_s === 'number' ? job.elapsed_s.toFixed(1) : job.elapsed_s}s` : '';
      h += `<div class="running-box" role="status">
        <div class="progress-bar-track"><div class="progress-bar-bar"></div></div>
        <div class="running-row">
          <div class="running-status"><span>${esc(phase)}...</span><span class="running-time">${esc(time)}</span></div>
          <button type="button" class="btn-quiet btn-cancel" data-id="${esc(id)}">Cancel</button>
        </div>
      </div>`;
    }

    if (hasResult) {
      h += `<div class="result-row">`;
      if (job.phase === 'done') {
        const method = job.result?.method || 'repaired';
        h += `<span class="chip chip-success">Watertight ✓ · ${esc(method)}</span>
              <button type="button" class="link-show-file" data-id="${esc(id)}">Show file</button>`;
      } else if (job.phase === 'failed') {
        h += `<span class="chip chip-failed" title="${esc(job.message)}">Failed — ${esc(job.message || 'Error')}</span>`;
      } else if (job.phase === 'cancelled') {
        h += `<span class="chip chip-cancelled">Cancelled</span>`;
      }
      h += `</div>`;
      if (job.phase === 'done') {
        h += `<div class="result-hint">Added as a new object — press <b>A</b> (Arrange) to separate it from the original; <b>Ctrl/Cmd+Z</b> removes it.</div>`;
      }
    }

    if (isExp) {
      h += `<div class="analysis-drawer">
        <div class="analysis-top">
          <span class="analysis-heading">Mesh Diagnostics</span>
          <button type="button" class="btn-close-analysis" data-id="${esc(id)}" title="Close diagnostics">✕</button>
        </div>`;
      if (analysis.error) {
        h += `<div class="analysis-error">${esc(analysis.error)}</div>`;
      } else {
        const d = analysis.defects || {};
        const holes = d.holes || 0, nm = d.non_manifold || 0, si = d.self_intersections || 0;
        const recs = Array.isArray(analysis.recommended) ? analysis.recommended.slice(0, 3) : [];

        h += `<div class="defect-grid">
          ${[['Holes',holes,holes>0?'warn':'zero'],['Non-manifold',nm,nm>0?'bad':'zero'],['Self-intersect',si,si>0?'warn':'zero']].map(([l,v,c])=>`<div class="defect-cell"><div class="defect-val ${c}">${v}</div><div class="defect-lbl">${l}</div></div>`).join('')}
        </div>`;

        if (recs.length) {
          h += `<div class="rec-list">`;
          recs.forEach(rec => {
            const pct = Math.round((rec.confidence || 0) * 100);
            const name = rec.name || rec.method || 'Recommended';
            h += `<div class="rec-row">
              <div class="rec-meta"><span class="rec-name" title="${esc(name)}">${esc(name)}</span><span class="rec-conf">${pct}%</span></div>
              <div class="rec-bar-track"><div class="rec-bar-fill" style="width:${Math.min(100, Math.max(0, pct))}%"></div></div>
            </div>`;
          });
          h += `</div>`;
        }
      }
      h += `</div>`;
    }

    h += `</div>`;
    return h;
  }

  function renderObjectList() {
    const c = $('object-list'), e = $('empty-state');
    if (!c || !e) return;
    if (!state.cli?.found) { c.innerHTML = ''; e.classList.add('hidden'); return; }
    if (!state.objects?.length) { c.innerHTML = ''; e.classList.remove('hidden'); return; }
    e.classList.add('hidden');
    c.innerHTML = state.objects.map(renderObjectRowHtml).join('');
  }

  function updateSingleRow(id) {
    const row = $('row-' + id);
    const obj = state.objects.find(o => String(o.id) === String(id));
    if (!row || !obj) { renderObjectList(); return; }
    const t = document.createElement('div');
    t.innerHTML = renderObjectRowHtml(obj);
    if (t.firstElementChild) row.parentNode.replaceChild(t.firstElementChild, row);
  }

  $('object-list')?.addEventListener('click', e => {
    const t = e.target;
    if (t.classList.contains('row-checkbox')) {
      const id = t.getAttribute('data-id');
      if (t.checked) state.selectedIds.add(id); else state.selectedIds.delete(id);
      updateFooter();
      return;
    }
    const bA = t.closest('.btn-analyze');
    if (bA) {
      const id = bA.getAttribute('data-id');
      announce('Analyzing object ' + id);
      send({ command: 'analyze', id });
      return;
    }
    const bR = t.closest('.btn-repair');
    if (bR) {
      const id = bR.getAttribute('data-id');
      announce('Repairing object ' + id);
      send({ command: 'repair', ids: [id], preset: state.settings.preset });
      return;
    }
    const bC = t.closest('.btn-cancel');
    if (bC) {
      const id = bC.getAttribute('data-id');
      announce('Cancelling repair for object ' + id);
      send({ command: 'cancel', id });
      return;
    }
    const bS = t.closest('.link-show-file');
    if (bS) {
      send({ command: 'open_output', id: bS.getAttribute('data-id') });
      return;
    }
    const bCl = t.closest('.btn-close-analysis');
    if (bCl) {
      const id = bCl.getAttribute('data-id');
      state.expandedAnalyses.delete(id);
      updateSingleRow(id);
      return;
    }
  });

  $('preset-bar')?.addEventListener('click', e => {
    const btn = e.target.closest('.seg-btn');
    if (!btn) return;
    const p = btn.getAttribute('data-preset');
    if (!p || p === state.settings.preset) return;
    state.settings.preset = p;
    updatePresetButtons();
    persistSettings();
  });

  on('btn-refresh', 'click', () => { announce('Refreshing objects'); send({ command: 'refresh' }); });
  on('btn-check-cli', 'click', () => { announce('Checking Sutura CLI'); send({ command: 'refresh' }); });

  on('btn-copy-install', 'click', () => {
    const txt = $('install-cmd')?.textContent || '';
    if (!txt) return;
    const btn = $('btn-copy-install');
    const done = () => { if (btn) { btn.textContent = 'Copied!'; setTimeout(() => { btn.textContent = 'Copy'; }, 1500); } };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(txt).then(done).catch(done);
    } else {
      const ta = document.createElement('textarea');
      ta.value = txt; ta.style.position = 'fixed'; ta.style.opacity = '0';
      document.body.appendChild(ta); ta.select();
      try { document.execCommand('copy'); done(); } catch (err) {}
      document.body.removeChild(ta);
    }
  });

  on('btn-repair-selected', 'click', () => {
    const ids = Array.from(state.selectedIds);
    if (!ids.length) return;
    announce(`Repairing ${ids.length} selected objects`);
    send({ command: 'repair', ids, preset: state.settings.preset });
  });

  on('btn-select-broken', 'click', () => {
    state.selectedIds.clear();
    state.objects.forEach(obj => {
      if (obj.status !== 'ok' || obj.manifold === false || (obj.orca_fixed_errors && obj.orca_fixed_errors > 0)) {
        state.selectedIds.add(String(obj.id));
      }
    });
    renderObjectList();
    updateFooter();
    announce(`Selected ${state.selectedIds.size} broken objects`);
  });

  function persistSettings() {
    send({
      command: 'settings',
      values: {
        preset: state.settings.preset,
        notify_broken: state.settings.notify_broken,
        open_panel_at_startup: state.settings.open_panel_at_startup
      }
    });
  }

  on('chk-notify-broken', 'change', e => { state.settings.notify_broken = e.target.checked; persistSettings(); });
  on('chk-open-startup', 'change', e => { state.settings.open_panel_at_startup = e.target.checked; persistSettings(); });

  function onMessage(msg) {
    if (!msg) return;
    switch (msg.command) {
      case 'state':
        if (msg.cli) state.cli = msg.cli;
        if (msg.settings) { Object.assign(state.settings, msg.settings); updateSettingsUI(); }
        if (Array.isArray(msg.objects)) {
          state.objects = msg.objects;
          const v = new Set(msg.objects.map(o => String(o.id)));
          state.selectedIds.forEach(id => { if (!v.has(id)) state.selectedIds.delete(id); });
          updateFooter();
        }
        updateCliDot();
        break;
      case 'job':
        if (msg.id != null) {
          const id = String(msg.id);
          state.jobs.set(id, msg);
          updateSingleRow(id);
          if (msg.phase === 'done') announce(`Object ${id} repair complete: ${msg.result?.method || 'watertight'}`);
          else if (msg.phase === 'failed') announce(`Object ${id} repair failed: ${msg.message || 'error'}`);
        }
        break;
      case 'analysis':
        if (msg.id != null) {
          const id = String(msg.id);
          state.analyses.set(id, msg);
          state.expandedAnalyses.add(id);
          updateSingleRow(id);
          announce(msg.error
            ? `Analysis failed for object ${id}: ${msg.error}`
            : `Analysis ready for object ${id}`);
        }
        break;
    }
  }

  if (window.orca?.onMessage) window.orca.onMessage(onMessage);

  send({ command: 'hello' });
  send({ command: 'refresh' });

  window.__sutura_panel = {
    getState: () => state,
    onMessage
  };
})();
</script>
</body>
</html>
"""

_VERSION_CACHE = {}


# --------------------------------------------------------------------------- geometry helpers

def _to_float3(value):
    """Normalise a vertex to [float, float, float] from a list/tuple/np row."""
    return [float(value[0]), float(value[1]), float(value[2])]


def _to_int3(value):
    """Normalise a triangle to [int, int, int] from a list/tuple/np row."""
    return [int(value[0]), int(value[1]), int(value[2])]


def _cross(a, b):
    """3D cross product of two 3-vectors (pure Python)."""
    return [a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0]]


def _write_binary_stl(path, verts, tris):
    """Write a binary STL from plain sequences (list/tuple/np array).

    Pure stdlib (struct); face normals are computed from the geometry."""
    v = [_to_float3(x) for x in verts]
    t = [_to_int3(x) for x in tris]
    with open(path, 'wb') as f:
        f.write(b'sutura repair staging'.ljust(80, b'\0'))
        f.write(struct.pack('<I', len(t)))
        for tri in t:
            a, b, c = v[tri[0]], v[tri[1]], v[tri[2]]
            n = _cross([b[i] - a[i] for i in range(3)],
                       [c[i] - a[i] for i in range(3)])
            ln = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]) ** 0.5
            if ln > 0:
                n = [x / ln for x in n]
            f.write(struct.pack('<3f', n[0], n[1], n[2]))
            for p in (a, b, c):
                f.write(struct.pack('<3f', p[0], p[1], p[2]))
            f.write(struct.pack('<H', 0))


def _strip_mesh_extension(stem):
    """Drop a trailing source mesh extension (``.stl``/``.obj``/``.3mf``).

    OrcaSlicer object names commonly carry the imported file's extension
    (``broken_cube.stl``); leaving it in place would produce a double extension
    in the repaired output (``broken_cube.stl_sutura_...stl``)."""
    text = str(stem or '')
    lowered = text.lower()
    for ext in _MESH_EXTENSIONS:
        if lowered.endswith(ext):
            return text[:-len(ext)]
    return text


def _unique_output_name(stem, ext='stl'):
    """A readable repaired-output filename: <stem>_sutura_<timestamp>.<ext>."""
    stem = _strip_mesh_extension(stem) or 'mesh'
    ts = time.strftime('%Y%m%d-%H%M%S')
    return '%s_sutura_%s.%s' % (stem, ts, ext)


# The audit hook denies a path with a component containing any of these
# substrings, so no staging/output component may include them.
_FORBIDDEN_PATH_PARTS = ('conf', 'config', 'secret', 'cert')

# Staging directories older than this are removed on plugin load.
_STALE_STAGING_SECONDS = 7 * 24 * 3600


def _audit_safe_component(name):
    """A path component free of the audit hook's denied substrings."""
    lowered = name.lower()
    if any(part in lowered for part in _FORBIDDEN_PATH_PARTS):
        return 'mesh'
    return name


def _data_dir():
    """OrcaSlicer's data dir, derived from this plugin's install location.

    Cloud plugins live under ``<data_dir>/orca_plugins/_subscribed/<user>/``
    and local plugins under ``<data_dir>/orca_plugins/``; walk up from this
    file to the ``orca_plugins`` component and take its parent. Returns None
    when the plugin is not installed under such a tree (the host exposes no
    ``data_dir()`` API, so there is nothing else to fall back to)."""
    try:
        path = os.path.abspath(__file__)
    except NameError:
        return None
    parts = path.split(os.sep)
    for i in range(len(parts) - 1, -1, -1):
        if parts[i] == 'orca_plugins':
            parent = os.sep.join(parts[:i])
            return parent or None
    return None


def _work_root():
    """The audit-allowed staging root: ``<data_dir>/orca_plugins/.sutura_work``."""
    data_dir = _data_dir()
    if not data_dir:
        return None
    return os.path.join(data_dir, 'orca_plugins', '.sutura_work')


def _staging_dir():
    """A fresh per-job staging dir under the allowed root, or None."""
    root = _work_root()
    if root is None:
        return None
    path = os.path.join(root, uuid.uuid4().hex)
    try:
        os.makedirs(path, exist_ok=True)
    except OSError:
        return None
    return path


def _out_dir():
    """Persistent repaired-output dir: ``<work_root>/out``.

    Unlike the per-job input staging, this survives the job so OrcaSlicer can
    still read the repaired file after an asynchronous load-back."""
    root = _work_root()
    if root is None:
        return None
    return os.path.join(root, 'out')


def _unique_output_path(stem):
    """A persistent, readable, unique output path in ``_out_dir()``, or None.

    ``<object-name>_sutura_<YYYYmmdd-HHMMSS>.stl``; a counter is appended only
    when that name already exists (e.g. two repairs of the same object within
    one second)."""
    out_dir = _out_dir()
    if out_dir is None:
        return None
    try:
        os.makedirs(out_dir, exist_ok=True)
    except OSError:
        return None
    base = _unique_output_name(_audit_safe_component(stem or 'mesh'))
    path = os.path.join(out_dir, base)
    if not os.path.exists(path):
        return path
    root, ext = os.path.splitext(base)
    for n in range(2, 1000):
        candidate = os.path.join(out_dir, '%s_%d%s' % (root, n, ext))
        if not os.path.exists(candidate):
            return candidate
    return os.path.join(out_dir, '%s_%s%s' % (root, uuid.uuid4().hex[:6], ext))


def _remove_path(path):
    try:
        if os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path, ignore_errors=True)
        else:
            os.remove(path)
    except Exception:  # noqa: BLE001
        pass


def _cleanup_staging(path):
    """Delete a job's per-job input staging dir (the repaired output lives in
    the persistent ``_out_dir()`` and is left alone)."""
    _remove_path(path)


def _purge_stale_staging(max_age=_STALE_STAGING_SECONDS):
    """Remove per-job staging dirs and persistent outputs older than
    ``max_age`` seconds (run on plugin load)."""
    now = time.time()
    for folder in (_work_root(), _out_dir()):
        if not folder or not os.path.isdir(folder):
            continue
        try:
            entries = os.listdir(folder)
        except Exception:  # noqa: BLE001
            continue
        for entry in entries:
            full = os.path.join(folder, entry)
            try:
                if now - os.path.getmtime(full) > max_age:
                    _remove_path(full)
            except Exception:  # noqa: BLE001
                continue


def _safe_call(obj, method, default=None):
    try:
        value = getattr(obj, method)()
        return default if value is None else value
    except Exception:  # noqa: BLE001
        return default


def _safe_bool(obj, method, default=True):
    value = _safe_call(obj, method, default)
    try:
        return bool(value)
    except Exception:  # noqa: BLE001
        return default


def _safe_name(obj):
    """A filesystem-safe object name, or 'object'.

    ``ModelObject.name`` is a read-only attribute (not a method), so it is
    read directly rather than through ``_safe_call``."""
    try:
        name = str(getattr(obj, 'name', '') or '')
    except Exception:  # noqa: BLE001
        name = ''
    name = ''.join(ch for ch in name if ch.isalnum() or ch in '._- ').strip()
    return name or 'object'


def _as_rows(matrix):
    """A 4x4 affine matrix as nested Python rows (accepts a numpy array)."""
    if _np is not None and isinstance(matrix, _np.ndarray):
        return matrix.tolist()
    return [[float(matrix[i][j]) for j in range(4)] for i in range(4)]


def _matmul(a, b):
    """4x4 row-major matrix product (a @ b); either side may be None."""
    if a is None:
        return b
    if b is None:
        return a
    ar, br = _as_rows(a), _as_rows(b)
    return [[sum(ar[i][k] * br[k][j] for k in range(4)) for j in range(4)]
            for i in range(4)]


def _det3(matrix):
    """Determinant of the upper-left 3x3 of a 4x4 affine matrix."""
    m = _as_rows(matrix)
    return (m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]))


def _apply_matrix(verts, matrix):
    """Apply a 4x4 row-major affine matrix to vertices (list or np array)."""
    m = _as_rows(matrix)
    a, b, c = m[0], m[1], m[2]
    if _np is not None and isinstance(verts, _np.ndarray):
        v = _np.asarray(verts, dtype=_np.float64)
        out = _np.empty_like(v)
        out[:, 0] = v[:, 0] * a[0] + v[:, 1] * a[1] + v[:, 2] * a[2] + a[3]
        out[:, 1] = v[:, 0] * b[0] + v[:, 1] * b[1] + v[:, 2] * b[2] + b[3]
        out[:, 2] = v[:, 0] * c[0] + v[:, 1] * c[1] + v[:, 2] * c[2] + c[3]
        return out
    return [[a[0] * v[0] + a[1] * v[1] + a[2] * v[2] + a[3],
             b[0] * v[0] + b[1] * v[1] + b[2] * v[2] + b[3],
             c[0] * v[0] + c[1] * v[1] + c[2] * v[2] + c[3]] for v in verts]


def _read_mesh(mesh):
    """Read (verts, tris) from a TriangleMesh.

    Uses the numpy zero-copy accessors when numpy is available, else the
    numpy-free ``vertex(i)``/``triangle(i)`` element accessors."""
    if _np is not None:
        try:
            verts = mesh.vertices()
            tris = mesh.triangles()
            return _np.asarray(verts, dtype=_np.float64), _np.asarray(tris, dtype=_np.int64)
        except Exception:  # noqa: BLE001 - fall through to the numpy-free path
            pass
    nv = int(mesh.vertex_count())
    nt = int(mesh.triangle_count())
    verts = [_to_float3(mesh.vertex(i)) for i in range(nv)]
    tris = [_to_int3(mesh.triangle(i)) for i in range(nt)]
    return verts, tris


def _volume_matrix(volume):
    """Volume-local to object matrix, or None when numpy is unavailable."""
    try:
        return volume.matrix()
    except Exception:  # noqa: BLE001 - matrix() needs numpy
        return None


def _instance_matrix(obj):
    """Object-to-world matrix of the first instance, or None when unavailable."""
    try:
        instances = obj.instances()
    except Exception:  # noqa: BLE001
        return None
    if not instances:
        return None
    try:
        return instances[0].matrix()
    except Exception:  # noqa: BLE001
        return None


def _model_part_volumes(obj):
    """(model_part_volumes, skipped_count) for one object.

    Only ``is_model_part()`` volumes are exported; parameter modifiers,
    negative volumes and support blockers are skipped."""
    try:
        volumes = list(obj.volumes())
    except Exception:  # noqa: BLE001
        return [], 0
    parts = [v for v in volumes if _safe_bool(v, 'is_model_part', False)]
    return parts, max(len(volumes) - len(parts), 0)


def _export_object_stl(obj, path):
    """Export one object's model parts to a binary STL in world coordinates.

    Every model part is transformed by ``instance(0).matrix() @ volume.matrix()``
    and appended to the same mesh; the triangle winding is flipped when the
    combined transform is left-handed (negative determinant), so a mirrored
    part keeps outward-facing normals. Returns a small info dict."""
    parts, skipped = _model_part_volumes(obj)
    inst_m = _instance_matrix(obj)
    verts_all, tris_all, offset = [], [], 0
    for volume in parts:
        mesh = _safe_call(volume, 'mesh', None)
        if mesh is None:
            continue
        try:
            verts, tris = _read_mesh(mesh)
        except Exception:  # noqa: BLE001 - skip an unreadable volume
            continue
        if len(verts) == 0 or len(tris) == 0:
            continue
        combined = _matmul(inst_m, _volume_matrix(volume))
        if combined is not None:
            verts = _apply_matrix(verts, combined)
            if _det3(combined) < 0:
                tris = (tris[:, [0, 2, 1]] if _np is not None and isinstance(tris, _np.ndarray)
                        else [[t[0], t[2], t[1]] for t in tris])
        if _np is not None and isinstance(tris, _np.ndarray):
            for t in tris:
                tris_all.append([int(t[0]) + offset, int(t[1]) + offset, int(t[2]) + offset])
        else:
            for t in tris:
                tris_all.append([t[0] + offset, t[1] + offset, t[2] + offset])
        verts_all.extend(verts)
        offset += len(verts)
    if not tris_all:
        raise ValueError('object has no exportable model-part mesh')
    _write_binary_stl(path, verts_all, tris_all)
    return {'parts': len(parts), 'skipped': skipped,
            'vertices': len(verts_all), 'triangles': len(tris_all)}


# --------------------------------------------------------------------------- CLI

def _cli_version(path):
    """Version string of the Sutura CLI, cached per path; '' when unknown."""
    if path in _VERSION_CACHE:
        return _VERSION_CACHE[path]
    version = ''
    try:
        proc = subprocess.run([path, '--version'], capture_output=True,
                              text=True, timeout=15)
        text = ((proc.stdout or '') + (proc.stderr or '')).strip()
        if text:
            version = text.split()[-1]
    except Exception:  # noqa: BLE001
        version = ''
    _VERSION_CACHE[path] = version
    return version


def _resolve_cli(cfg=None):
    """Resolve the Sutura CLI: (path, found, version).

    Precedence: the SUTURA_CLI environment variable, then the plugin's
    ``sutura_cli`` setting, then ``~/.local/bin/sutura``, then PATH."""
    cfg = cfg or {}
    env = os.environ.get('SUTURA_CLI')
    path = env or (cfg.get('sutura_cli') or '') or _DEFAULT_CLI
    if path and os.path.exists(path):
        return path, True, _cli_version(path)
    found = shutil.which('sutura')
    if found:
        return found, True, _cli_version(found)
    return (path or 'sutura'), False, ''


def _parse_last_json(text):
    """The last complete JSON object line of a CLI stdout stream, or None."""
    for line in reversed((text or '').splitlines()):
        line = line.strip()
        if line.startswith('{') and line.endswith('}'):
            try:
                return json.loads(line)
            except Exception:  # noqa: BLE001
                continue
    return None


def _run_cli(cmd, cancel_event, timeout, on_proc=None):
    """Run a CLI command, cancellable and bounded; returns (status, report, msg).

    status is 'ok' | 'failed' | 'cancelled'. The last stdout JSON line is
    parsed into ``report`` when present."""
    try:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)
    except FileNotFoundError:
        return 'failed', None, 'Sutura CLI not found at %s' % cmd[0]
    if on_proc is not None:
        try:
            on_proc(proc)
        except Exception:  # noqa: BLE001
            pass

    # Drain both pipes on reader threads. Polling the process while only one
    # pipe is read can deadlock: the child blocks writing a report larger than
    # the pipe buffer, never exits, and a valid large report looks like a
    # timeout (and cancel never gets a chance). communicate() cannot be
    # cancelled, so the pipes are drained here and the wait loop below stays
    # responsive to cancel_event/timeout.
    out_chunks, err_chunks = [], []

    def _drain(stream, sink):
        try:
            for line in iter(stream.readline, ''):
                sink.append(line)
        except Exception:  # noqa: BLE001
            pass
        finally:
            try:
                stream.close()
            except Exception:  # noqa: BLE001
                pass

    readers = [
        threading.Thread(target=_drain, args=(proc.stdout, out_chunks), daemon=True),
        threading.Thread(target=_drain, args=(proc.stderr, err_chunks), daemon=True),
    ]
    for reader in readers:
        reader.start()

    start = time.time()
    try:
        while True:
            if cancel_event is not None and cancel_event.is_set():
                _terminate(proc)
                return 'cancelled', None, 'Cancelled'
            if proc.poll() is not None:
                break
            if time.time() - start > timeout:
                _terminate(proc)
                return 'failed', None, 'timed out after %ds' % timeout
            time.sleep(0.2)
    except Exception as exc:  # noqa: BLE001
        _terminate(proc)
        return 'failed', None, str(exc)
    for reader in readers:
        reader.join(timeout=5)
    out = ''.join(out_chunks)
    err = ''.join(err_chunks)
    report = _parse_last_json(out)
    if proc.returncode == 0:
        return 'ok', report or {}, ''
    if report and 'error' in report:
        return 'failed', report, str(report.get('error'))
    return 'failed', report, (err or out or 'exit %d' % proc.returncode).strip()


def _terminate(proc):
    # The pipes are drained by _run_cli's reader threads, so never call
    # communicate() here (it would race those readers and can block). Just
    # kill and reap.
    try:
        if proc.poll() is None:
            proc.kill()
    except Exception:  # noqa: BLE001
        pass
    try:
        proc.wait(timeout=5)
    except Exception:  # noqa: BLE001
        try:
            proc.kill()
        except Exception:  # noqa: BLE001
            pass


def _run_repair(cli, src, out, preset, cancel_event, timeout=_REPAIR_TIMEOUT, on_proc=None):
    """Repair ``src`` into ``out`` with the given intensity preset."""
    return _run_cli([cli, src, '--intensity', preset, '-o', out],
                    cancel_event, timeout, on_proc)


def _run_analyze(cli, src, cancel_event=None, timeout=_ANALYZE_TIMEOUT, on_proc=None):
    """Read-only analysis: defects, mesh type and ranked method recommendations."""
    return _run_cli([cli, src, '--analyze'], cancel_event, timeout, on_proc)


def _job_result(report, out_path):
    """Map a Sutura repair report to the panel's job-result contract."""
    report = report or {}
    stage1 = report.get('stage1') or {}
    stage2 = report.get('stage2') or {}
    defects = report.get('defects') or {}
    holes_before = 0
    if isinstance(defects, dict):
        try:
            holes_before = len(defects.get('holes') or [])
        except TypeError:
            holes_before = int(defects.get('holes') or 0)
    watertight = report.get('category') == 'watertight'
    holes_after = stage1.get('holes_remaining')
    if holes_after is None:
        holes_after = 0 if watertight else None
    if stage2.get('ok'):
        method = 'two-stage rebuild'
    elif stage1.get('two_manifold'):
        method = 'stage 1 close'
    else:
        method = 'stage 1'
    return {'watertight': bool(watertight), 'method': method,
            'holes_before': holes_before, 'holes_after': holes_after,
            'output_path': out_path}


def _analysis_message(report):
    """Map a Sutura --analyze report to the panel's analysis contract."""
    report = report or {}
    analysis = report.get('analysis') or {}
    if not analysis and report.get('object_analyses'):
        analysis = (report.get('object_analyses') or [{}])[0]
    recommended = []
    for rec in (report.get('recommendations') or []):
        recommended.append({
            'method': rec.get('id') or rec.get('method') or str(rec.get('num', '')),
            'name': rec.get('name') or rec.get('id') or 'method',
            'confidence': rec.get('score', rec.get('confidence', 0.0)),
        })
    return {'defects': {
        'holes': int(analysis.get('boundary_loops') or 0),
        'non_manifold': int(analysis.get('non_manifold_edges') or 0),
        'self_intersections': int(analysis.get('self_intersections') or 0),
    }, 'recommended': recommended}


def _analyze_failure_text(msg, report=None):
    """A clear message when ``--analyze`` failed or is unsupported.

    An older Sutura CLI rejects the unknown flag with a usage message on
    stderr and a non-zero exit; surface that as an explicit "update Sutura"
    error instead of leaving the panel waiting."""
    text = (msg or '').strip()
    lowered = text.lower()
    unsupported = ('--analyze' in text
                   and any(marker in lowered for marker in (
                       'unrecognized arguments', 'invalid choice', 'no such option',
                       'unknown option', 'usage:')))
    if unsupported:
        return ('This Sutura CLI version does not support --analyze; '
                'update Sutura and retry.')
    if not text and isinstance(report, dict):
        text = str(report.get('error') or '')
    return text or 'Analysis failed'


def _running_orca_processes():
    """Number of distinct OrcaSlicer processes running (pgrep -f). Returns
    None when the check itself fails (treated as 'cannot verify')."""
    try:
        r = subprocess.run(['pgrep', '-f', 'OrcaSlicer'],
                           capture_output=True, text=True)
        pids = [l.strip() for l in (r.stdout or '').splitlines() if l.strip()]
        return len(pids)
    except Exception:  # noqa: BLE001
        return None


def _load_back(out_path):
    """Best-effort: reload the repaired file into the slicer.

    Linux: OrcaSlicer --single-instance <path> (path-based binary, no name
    ambiguity). macOS: `open -b com.orcaslicer.OrcaSlicer <path>` -- BUNDLE-ID
    matching prefers the running process; skipped when MORE THAN ONE
    OrcaSlicer process is running (ambiguous). Returns True when the launch
    succeeded (rc 0), False when it was skipped or failed; the repaired file
    lives in the persistent ``_out_dir()`` either way, so the caller can post
    a message pointing at it. Never crashes the worker.

    TODO(owner live test): the export is in WORLD coordinates, so OrcaSlicer
    may re-centre the imported object or, on some builds, drop it; the exact
    placement after reload has not been verified on a real nightly and needs
    the owner's live check."""
    try:
        if sys.platform == 'darwin':
            n = _running_orca_processes()
            if n is not None and n > 1:
                sys.stderr.write('[sutura] load-back skipped: %d OrcaSlicer '
                                 'processes running (ambiguous)\n' % n)
                return False
            cmd = ['open', '-b', 'com.orcaslicer.OrcaSlicer', out_path]
        else:
            cmd = [_ORCA_BIN, '--single-instance', out_path]
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write('[sutura] load-back error: %s\n' % exc)
        return False
    # Run (not Popen) so the exact command and its return code are logged; the
    # file now lives in the persistent out/ dir, so a slow reader is safe.
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except FileNotFoundError:
        sys.stderr.write('[sutura] load-back failed: binary not found for %s\n'
                         % ' '.join(cmd))
        return False
    except subprocess.TimeoutExpired:
        sys.stderr.write('[sutura] load-back timed out: %s\n' % ' '.join(cmd))
        return False
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write('[sutura] load-back error: %s\n' % exc)
        return False
    detail = ''
    if proc.stderr:
        detail = ' stderr=%s' % proc.stderr.strip()[:500]
    sys.stderr.write('[sutura] load-back: %s -> rc=%d%s\n'
                     % (' '.join(cmd), proc.returncode, detail))
    return proc.returncode == 0


def _reveal(path):
    """Best-effort: reveal a repaired file in the platform file browser."""
    try:
        if sys.platform == 'darwin':
            subprocess.Popen(['open', '-R', path])
        elif sys.platform.startswith('win'):
            subprocess.Popen(['explorer', '/select,', path])
        else:
            subprocess.Popen(['xdg-open', os.path.dirname(path) or '.'])
    except Exception:  # noqa: BLE001
        pass


# --------------------------------------------------------------------------- plugin

class SuturaRepair(orca.script.ScriptPluginCapabilityBase):
    """The panel capability: dock HTML UI, repair/analyze jobs, notifications."""

    # --- lazy instance state (avoids depending on a base __init__) --------
    def _st(self):
        state = getattr(self, '_sutura_state', None)
        if state is None:
            state = {'panel': None, 'jobs': {}, 'notified': set(),
                     'scan_timer': None, 'lock': threading.RLock()}
            self._sutura_state = state
        return state

    def get_name(self):
        return "Sutura Repair"

    # --- config -----------------------------------------------------------
    def get_default_config(self):
        return {'preset': 'balanced', 'open_panel_at_startup': True,
                'notify_broken': True, 'sutura_cli': ''}

    def _settings(self):
        cfg = dict(self.get_default_config())
        try:
            raw = self.get_config()
            stored = json.loads(raw) if raw else {}
            if isinstance(stored, dict):
                cfg.update(stored)
        except Exception:  # noqa: BLE001
            pass
        if cfg.get('preset') not in _INTENSITIES:
            cfg['preset'] = 'balanced'
        return cfg

    def _persist_settings(self, values):
        cfg = self._settings()
        for key in ('preset', 'open_panel_at_startup', 'notify_broken',
                    'sutura_cli'):
            if key in values:
                cfg[key] = values[key]
        if cfg.get('preset') not in _INTENSITIES:
            cfg['preset'] = 'balanced'
        try:
            self.save_config(json.dumps(cfg))
        except Exception:  # noqa: BLE001
            pass
        return cfg

    # --- panel ------------------------------------------------------------
    def _log(self, text):
        """Best-effort diagnostic line on stderr (the host has no log API)."""
        try:
            sys.stderr.write('[sutura] %s\n' % text)
        except Exception:  # noqa: BLE001
            pass

    def _open_panel(self):
        st = self._st()
        panel = st.get('panel')
        if panel is not None:
            try:
                panel.show()
                self._post_state_async()
                st['panel_create_pending'] = False
                return True
            except Exception:  # noqa: BLE001
                pass
        try:
            panel = orca.host.ui.create_dock_panel(
                html=_EMBEDDED_PANEL_HTML, title='Sutura', width=340, height=560,
                on_message=self.on_message, on_close=self._on_panel_close, dock='right')
        except Exception as exc:  # noqa: BLE001
            self._log('dock panel creation failed: %s' % exc)
            return False
        if panel is None:
            self._log('dock panel creation returned no panel')
            return False
        st['panel'] = panel
        st['panel_create_pending'] = False
        return True

    def _on_panel_close(self):
        self._st()['panel'] = None

    def _post(self, message):
        panel = self._st().get('panel')
        if panel is None:
            return
        try:
            if panel.is_open():
                panel.post(message)
        except Exception:  # noqa: BLE001
            pass

    def _post_state(self):
        try:
            self._post(self._build_state())
        except Exception:  # noqa: BLE001
            pass

    def _post_state_async(self):
        threading.Thread(target=self._post_state, daemon=True).start()

    def _iter_objects(self):
        try:
            model = orca.host.model()
        except Exception:  # noqa: BLE001
            return []
        if model is None:
            return []
        try:
            return list(model.objects())
        except Exception:  # noqa: BLE001
            return []

    def _find_object(self, obj_id):
        target = str(obj_id)
        for obj in self._iter_objects():
            if str(_safe_call(obj, 'id', '')) == target:
                return obj
        return None

    def _object_summary(self, obj):
        volumes = []
        try:
            volumes = list(obj.volumes())
        except Exception:  # noqa: BLE001
            volumes = []
        parts = [v for v in volumes if _safe_bool(v, 'is_model_part', False)]
        modifiers = max(len(volumes) - len(parts), 0)
        manifold = all(_safe_bool(v, 'is_manifold', True) for v in parts) if parts else True
        errors = int(_safe_call(obj, 'mesh_errors_count', 0) or 0)
        if not manifold:
            status = 'bad'
        elif errors > 0:
            status = 'warn'
        else:
            status = 'ok'
        try:
            name = getattr(obj, 'name', '') or '(unnamed)'
        except Exception:  # noqa: BLE001
            name = '(unnamed)'
        return {
            'id': _safe_call(obj, 'id', ''),
            'name': name,
            'parts': len(parts),
            'modifiers': modifiers,
            'triangles': int(_safe_call(obj, 'facets_count', 0) or 0),
            'orca_fixed_errors': errors,
            'manifold': bool(manifold),
            'status': status,
        }

    def _build_state(self):
        cfg = self._settings()
        cli_path, found, version = _resolve_cli(cfg)
        objects = []
        for obj in self._iter_objects():
            try:
                objects.append(self._object_summary(obj))
            except Exception:  # noqa: BLE001
                continue
        return {
            'command': 'state',
            'cli': {'found': bool(found), 'path': cli_path or '', 'version': version or ''},
            'settings': {'preset': cfg.get('preset', 'balanced'),
                         'notify_broken': bool(cfg.get('notify_broken', True)),
                         'open_panel_at_startup': bool(cfg.get('open_panel_at_startup', True))},
            'objects': objects,
        }

    # --- lifecycle / proactive scan --------------------------------------
    def on_load(self):
        try:
            _purge_stale_staging()
        except Exception:  # noqa: BLE001
            pass
        if not self._settings().get('open_panel_at_startup', True):
            return
        # Do NOT create the panel here: OrcaSlicer is still on the Home tab and
        # restores the Plater/AUI layout after load, so a pane created now stays
        # hidden even after show(). Arm a lazy creation on the first scene event.
        self._st()['panel_create_pending'] = True
        self._log('dock panel creation deferred to the first scene event')

    def on_unload(self):
        st = self._st()
        timer = st.get('scan_timer')
        if timer is not None:
            try:
                timer.cancel()
            except Exception:  # noqa: BLE001
                pass
        for job in list(st['jobs'].values()):
            cancel = job.get('cancel')
            if cancel is not None:
                cancel.set()
        panel = st.get('panel')
        if panel is not None:
            try:
                panel.close()
            except Exception:  # noqa: BLE001
                pass

    def on_lifecycle_event(self, event, ctx=None):
        # Runs synchronously on the UI thread: only enqueue, never do work here.
        name = getattr(event, 'name', None) or str(event)
        self._maybe_create_panel(name)
        if name in _SCAN_EVENTS:
            self._schedule_scan()

    def _maybe_create_panel(self, name):
        """Create the dock panel lazily on the first scene event (see on_load).

        A panel created before the Plater/AUI layout is restored stays hidden,
        so a pre-existing handle (created at load in an older build, or by a
        direct execute()) is closed and replaced by one created now, inside the
        UI thread with the layout in place."""
        st = self._st()
        if not st.get('panel_create_pending'):
            return
        if name not in _PANEL_CREATE_EVENTS:
            return
        st['panel_create_pending'] = False  # once only, success or not
        panel = st.get('panel')
        if panel is not None:
            try:
                panel.close()
                self._log('closed the panel created before startup restore')
            except Exception:  # noqa: BLE001
                pass
            st['panel'] = None
        if self._open_panel():
            self._log('dock panel created on %s' % name)
        else:
            self._log('dock panel still unavailable on %s; giving up' % name)

    def _schedule_scan(self):
        st = self._st()
        with st['lock']:
            timer = st.get('scan_timer')
            if timer is not None:
                try:
                    timer.cancel()
                except Exception:  # noqa: BLE001
                    pass
            timer = threading.Timer(_SCAN_DEBOUNCE, self._debounced_scan)
            timer.daemon = True
            st['scan_timer'] = timer
            timer.start()

    def _debounced_scan(self):
        self._post_state()
        self._notify_broken_objects()

    def _notify_broken_objects(self):
        st = self._st()
        if not self._settings().get('notify_broken', True):
            return
        for obj in self._iter_objects():
            oid = str(_safe_call(obj, 'id', ''))
            if not oid or oid in st['notified']:
                continue
            errors = int(_safe_call(obj, 'mesh_errors_count', 0) or 0)
            parts, _skipped = _model_part_volumes(obj)
            not_manifold = any(not _safe_bool(v, 'is_manifold', True) for v in parts)
            if errors <= 0 and not not_manifold:
                continue
            st['notified'].add(oid)
            name = _safe_name(obj)
            if errors > 0:
                text = ('%s: %d mesh error(s) repaired by Orca \u2014 check with Sutura'
                        % (name, errors))
            else:
                text = '%s: non-manifold mesh \u2014 check with Sutura' % name
            try:
                orca.host.ui.push_notification(
                    orca.host.ui.NotificationLevel.WarningNotificationLevel,
                    text, 'Repair with Sutura',
                    lambda oid=oid: self._on_notification_click(oid))
            except Exception:  # noqa: BLE001
                pass

    def _on_notification_click(self, oid):
        self._handle_repair({'ids': [oid],
                             'preset': self._settings().get('preset', 'balanced')})
        return True

    # --- execute (Run from the Plugins dialog) ---------------------------
    def execute(self):
        if self._open_panel():
            return orca.ExecutionResult.success("Sutura panel opened.")
        return orca.ExecutionResult.failure(
            orca.PluginResult.RecoverableError,
            "Sutura Repair: could not open the dock panel on this OrcaSlicer build.")

    # --- message protocol -------------------------------------------------
    def on_message(self, message):
        try:
            command = (message or {}).get('command')
        except Exception:  # noqa: BLE001
            return
        if command in ('hello', 'refresh'):
            self._post_state_async()
        elif command == 'repair':
            self._handle_repair(message)
        elif command == 'analyze':
            self._handle_analyze(message)
        elif command == 'cancel':
            self._handle_cancel(message)
        elif command == 'settings':
            self._handle_settings(message.get('values') or {})
        elif command == 'open_output':
            self._handle_open_output(message.get('id'))

    def _emit_job(self, oid, phase, elapsed, message, result=None):
        st = self._st()
        job = st['jobs'].setdefault(oid, {})
        job['phase'] = phase
        if phase in ('done', 'failed', 'cancelled'):
            job['running'] = False
        payload = {'command': 'job', 'id': oid, 'phase': phase,
                   'elapsed_s': round(float(elapsed), 1), 'message': message or ''}
        if result is not None:
            payload['result'] = result
        self._post(payload)

    def _handle_repair(self, message):
        ids = list(message.get('ids') or [])
        preset = message.get('preset') or self._settings().get('preset', 'balanced')
        if preset not in _INTENSITIES:
            preset = 'balanced'
        st = self._st()
        for oid in ids:
            oid = str(oid)
            job = st['jobs'].get(oid)
            if job is not None and job.get('running'):
                continue
            cancel = threading.Event()
            st['jobs'][oid] = {'cancel': cancel, 'proc': None, 'running': True,
                               'phase': 'queued', 'output_path': None, 'preset': preset}
            self._emit_job(oid, 'queued', 0.0, 'Queued')
            threading.Thread(target=self._job_worker, args=(oid, preset, cancel),
                             daemon=True).start()

    def _job_worker(self, oid, preset, cancel):
        st = self._st()
        job = st['jobs'].get(oid) or {}
        start = time.time()
        dest = None
        try:
            cli, found, _ver = _resolve_cli(self._settings())
            if not found:
                self._emit_job(oid, 'failed', 0.0,
                               'Sutura CLI not found (install with install.sh)')
                return
            obj = self._find_object(oid)
            if obj is None:
                self._emit_job(oid, 'failed', time.time() - start,
                               'object is no longer in the scene')
                return
            dest = _staging_dir()
            if dest is None:
                self._emit_job(oid, 'failed', time.time() - start,
                               'Cannot locate the OrcaSlicer data directory '
                               '(no orca_plugins component in the plugin path); '
                               'repair is disabled')
                return
            in_path = os.path.join(dest, 'input.stl')
            self._emit_job(oid, 'exporting', time.time() - start, 'Exporting mesh...')
            _export_object_stl(obj, in_path)
            out_path = _unique_output_path(_safe_name(obj))
            if out_path is None:
                self._emit_job(oid, 'failed', time.time() - start,
                               'Cannot create the repaired-output directory '
                               'under the OrcaSlicer data dir')
                return
            job['output_path'] = out_path
            self._emit_job(oid, 'repairing', time.time() - start, 'Repairing with Sutura...')
            status, report, msg = _run_repair(
                cli, in_path, out_path, preset, cancel,
                on_proc=lambda p: job.__setitem__('proc', p))
            if status == 'cancelled':
                self._emit_job(oid, 'cancelled', time.time() - start, 'Cancelled')
                return
            if status != 'ok':
                self._emit_job(oid, 'failed', time.time() - start, msg or 'repair failed')
                return
            self._emit_job(oid, 'loading', time.time() - start, 'Loading result...')
            if _load_back(out_path):
                self._emit_job(oid, 'done', time.time() - start, 'Repaired',
                               _job_result(report, out_path))
            else:
                self._emit_job(oid, 'done', time.time() - start,
                               'Repaired; automatic load-back failed, file kept at %s'
                               % out_path, _job_result(report, out_path))
        except Exception as exc:  # noqa: BLE001 - never crash the worker
            self._emit_job(oid, 'failed', time.time() - start, str(exc))
        finally:
            job['running'] = False
            if dest is not None:
                _cleanup_staging(dest)

    def _handle_cancel(self, message):
        oid = str(message.get('id'))
        job = self._st()['jobs'].get(oid)
        if job is None:
            return
        cancel = job.get('cancel')
        if cancel is not None:
            cancel.set()
        proc = job.get('proc')
        if proc is not None:
            _terminate(proc)

    def _handle_analyze(self, message):
        oid = str(message.get('id'))
        threading.Thread(target=self._analyze_worker, args=(oid,), daemon=True).start()

    def _analyze_worker(self, oid):
        dest = None
        try:
            cli, found, _ver = _resolve_cli(self._settings())
            if not found:
                self._post({'command': 'analysis', 'id': oid,
                            'error': 'Sutura CLI not found (install with install.sh)'})
                return
            obj = self._find_object(oid)
            if obj is None:
                self._post({'command': 'analysis', 'id': oid,
                            'error': 'object is no longer in the scene'})
                return
            dest = _staging_dir()
            if dest is None:
                self._post({'command': 'analysis', 'id': oid,
                            'error': 'Cannot locate the OrcaSlicer data directory '
                                     '(no orca_plugins component in the plugin path)'})
                return
            in_path = os.path.join(dest, 'input.stl')
            _export_object_stl(obj, in_path)
            status, report, msg = _run_analyze(cli, in_path)
            if status != 'ok':
                self._post({'command': 'analysis', 'id': oid,
                            'error': _analyze_failure_text(msg, report)})
                return
            payload = _analysis_message(report)
            payload.update({'command': 'analysis', 'id': oid})
            self._post(payload)
        except Exception as exc:  # noqa: BLE001
            self._post({'command': 'analysis', 'id': oid, 'error': str(exc)})
        finally:
            if dest is not None:
                _cleanup_staging(dest)

    def _handle_settings(self, values):
        self._persist_settings(values)
        self._post_state_async()

    def _handle_open_output(self, oid):
        job = self._st()['jobs'].get(str(oid))
        path = (job or {}).get('output_path')
        if path and os.path.exists(path):
            _reveal(path)
        elif path:
            try:
                orca.host.ui.message(
                    'The repaired file is no longer on disk -- it was pruned '
                    'after seven days or removed manually.',
                    title='Sutura Repair', buttons='ok', icon='info')
            except Exception:  # noqa: BLE001
                pass


@orca.plugin
class SuturaRepairPlugin(orca.base):
    def register_capabilities(self):
        # Declare ONLY the filesystem READ paths that actually exist: a
        # request for a non-existent path is rejected. The subprocess
        # (ProcessCreate) spawn is audited reactively and its persisted grant
        # is keyed to the exact command line (which contains unique staging
        # paths), so this does NOT remove the per-run subprocess prompt.
        candidates = [os.environ.get('SUTURA_CLI'), _DEFAULT_CLI, shutil.which('sutura')]
        fs_read = [p for p in dict.fromkeys(candidates) if p and os.path.exists(p)]
        if fs_read:
            try:
                orca.request_permissions(fs_read=fs_read)
            except Exception:  # noqa: BLE001
                pass
        orca.register_capability(SuturaRepair)
