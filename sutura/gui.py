#!/usr/bin/env python3
"""Sutura - small GUI frontend.

Pick one or more mesh files, run the two-stage repair on each, and read
back what was fixed. Results are always written to new "_fixed" files.
"""
import tkinter as tk
from tkinter import ttk, filedialog
import subprocess
import threading
import json
import os
import sys

SUTURA = os.path.expanduser('~/.local/bin/sutura')

BG = '#1e2327'
PANEL = '#16191c'
FG = '#dbe4ea'
MUTED = '#8b98a5'
ACCENT = '#14b8a6'
BORDER = '#3a444c'

MONO = ('DejaVu Sans Mono', 9)
UI_FONT = ('Noto Sans', 10)


def run_repair(path):
    r = subprocess.run([SUTURA, path], capture_output=True, text=True, timeout=600)
    try:
        return json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        return {'error': r.stderr.strip() or r.stdout.strip()}


def format_report(data):
    if data.get('error') and 'stage1' not in data:
        return 'ERROR: %s' % data['error']
    s1 = data.get('stage1', {})
    lines = []
    lines.append('Output file: %s' % data.get('output'))
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed              : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining           : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed  : %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed             : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components      : %d' % s1.get('components', 0))
    lines.append('  Two-manifold              : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    if 'stage2' in data:
        s2 = data['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            lines.append('  ERROR: %s' % s2['error'])
        else:
            lines.append('  Input triangles : %d' % s2.get('input_faces', 0))
            lines.append('  Output triangles: %d' % s2.get('output_faces', 0))
            if s2.get('shells_merged'):
                lines.append('  Shells merged   : %d' % s2['shells_merged'])
            lines.append('  Volume (after)  : %.4f' % s2.get('volume_after', 0))
    if 'objects' in data:
        lines.append('')
        lines.append('3MF objects repaired: %d' % data.get('objects', 0))
        for i, rep in enumerate(data.get('object_reports', [])):
            s1o = rep.get('stage1', {})
            ok = s1o.get('two_manifold') and s1o.get('holes_remaining', 0) == 0
            lines.append('  object %d: %s (%d hole(s) remaining, two-manifold=%s)' % (
                i, 'watertight' if ok else 'partial',
                s1o.get('holes_remaining', 0), 'YES' if s1o.get('two_manifold') else 'NO'))
    return '\n'.join(lines)


def summarize(data):
    if data.get('error') and 'stage1' not in data:
        return 'ERROR'
    s1 = data.get('stage1', {})
    if s1.get('two_manifold') and s1.get('holes_remaining', 0) == 0:
        return 'watertight'
    if s1.get('two_manifold'):
        return '%d hole(s)' % s1.get('holes_remaining', 0)
    return 'partial'


def load_icon():
    for size in (48, 32, 64, 128):
        path = os.path.expanduser(
            '~/.local/share/icons/hicolor/%dx%d/apps/sutura.png' % (size, size))
        if os.path.exists(path):
            try:
                return tk.PhotoImage(file=path)
            except tk.TclError:
                pass
    return None


class App:
    def __init__(self, root):
        self.root = root
        root.title('Sutura')
        root.configure(bg=BG)
        root.geometry('760x600')
        root.minsize(600, 460)
        self._style()

        icon = load_icon()
        if icon is not None:
            root.iconphoto(True, icon)
        self._icon = icon

        self.files = []

        # --- file list -----------------------------------------------------
        top = ttk.Frame(root)
        top.pack(fill='both', expand=True, padx=12, pady=(12, 4))

        self.tree = ttk.Treeview(top, columns=('status',), show='tree headings', height=8)
        self.tree.heading('#0', text='Files')
        self.tree.heading('status', text='Result')
        self.tree.column('#0', width=560)
        self.tree.column('status', width=110, anchor='center')
        self.tree.pack(side='left', fill='both', expand=True)

        scroll = ttk.Scrollbar(top, orient='vertical', command=self.tree.yview)
        self.tree.configure(yscrollcommand=scroll.set)
        scroll.pack(side='right', fill='y')

        # --- buttons -------------------------------------------------------
        btns = ttk.Frame(root)
        btns.pack(fill='x', padx=12, pady=(4, 4))
        ttk.Button(btns, text='Add files…', command=self.add_files).pack(side='left', padx=(0, 6))
        ttk.Button(btns, text='Remove selected', command=self.remove_selected).pack(side='left', padx=(0, 6))
        ttk.Button(btns, text='Clear', command=self.clear_files).pack(side='left')

        self.repair_btn = ttk.Button(btns, text='Repair', style='Accent.TButton',
                                     command=self.repair, state='disabled')
        self.repair_btn.pack(side='right')

        # --- status / progress ---------------------------------------------
        mid = ttk.Frame(root)
        mid.pack(fill='x', padx=12, pady=(4, 4))
        self.progress = ttk.Progressbar(mid, mode='determinate', maximum=100)
        self.progress.pack(side='left', fill='x', expand=True, padx=(0, 10))
        self.status = tk.StringVar(value='Ready')
        ttk.Label(mid, textvariable=self.status, foreground=MUTED).pack(side='left')

        # --- per-file report log -------------------------------------------
        log_frame = ttk.Frame(root)
        log_frame.pack(fill='both', expand=True, padx=12, pady=(4, 12))
        self.log = tk.Text(log_frame, wrap='word', bg=PANEL, fg=FG, insertbackground=FG,
                           font=MONO, relief='flat', padx=10, pady=8)
        self.log.pack(side='left', fill='both', expand=True)
        lscroll = ttk.Scrollbar(log_frame, orient='vertical', command=self.log.yview)
        self.log.configure(yscrollcommand=lscroll.set)
        lscroll.pack(side='right', fill='y')

    def _style(self):
        style = ttk.Style(self.root)
        try:
            style.theme_use('clam')
        except tk.TclError:
            pass
        style.configure('.', background=BG, foreground=FG, fieldbackground=PANEL,
                        bordercolor=BORDER, lightcolor=BORDER, darkcolor=BORDER,
                        font=UI_FONT)
        style.configure('TFrame', background=BG)
        style.configure('TLabel', background=BG, foreground=FG)
        style.configure('TButton', background='#2a3238', foreground=FG,
                        bordercolor=BORDER, padding=(12, 6), font=UI_FONT)
        style.map('TButton', background=[('active', '#39434b'), ('pressed', ACCENT)],
                  foreground=[('pressed', '#0b0f11')])
        style.configure('Accent.TButton', background=ACCENT, foreground='#0b0f11')
        style.map('Accent.TButton',
                  background=[('active', '#17c9b4'), ('pressed', '#0f8a7c')])
        style.configure('Treeview', background=PANEL, fieldbackground=PANEL,
                        foreground=FG, rowheight=24)
        style.map('Treeview', background=[('selected', ACCENT)],
                  foreground=[('selected', '#0b0f11')])
        style.configure('Treeview.Heading', background='#2a3238', foreground=FG,
                        relief='flat')
        style.configure('Horizontal.TProgressbar', background=ACCENT,
                        troughcolor=PANEL, bordercolor=BORDER)

    def add_files(self):
        paths = filedialog.askopenfilenames(
            title='Select mesh files',
            filetypes=[('Mesh files', '*.stl *.STL *.3mf *.3MF'), ('All files', '*.*')])
        for p in paths:
            if p not in self.files:
                self.files.append(p)
                self.tree.insert('', 'end', iid=p, text=p, values=('pending',))
        self.refresh_state()

    def remove_selected(self):
        for iid in self.tree.selection():
            if iid in self.files:
                self.files.remove(iid)
            self.tree.delete(iid)
        self.refresh_state()

    def clear_files(self):
        self.files.clear()
        self.tree.delete(*self.tree.get_children())
        self.refresh_state()

    def refresh_state(self):
        self.repair_btn.config(state='normal' if self.files else 'disabled')

    def repair(self):
        if not self.files:
            return
        self.repair_btn.config(state='disabled')
        self.progress.configure(maximum=len(self.files), value=0)
        self.status.set('Repairing…')
        self.log.delete('1.0', 'end')
        for iid in self.tree.get_children():
            self.tree.set(iid, 'status', '')
        threading.Thread(target=self._work, args=(list(self.files),), daemon=True).start()

    def _work(self, paths):
        for idx, path in enumerate(paths, start=1):
            self.root.after(0, lambda p=path: self.tree.set(p, 'status', '…'))
            data = run_repair(path)
            summary = summarize(data)
            self.root.after(0, lambda p=path, s=summary: self.tree.set(p, 'status', s))
            report = format_report(data)
            self.root.after(0, lambda p=path, r=report: self._append_log(p, r))
            self.root.after(0, lambda i=idx: self.progress.configure(value=i))
            self.root.after(0, lambda i=idx, n=len(paths): self.status.set(
                'Repairing… %d/%d' % (i, n)))
        self.root.after(0, self._done)

    def _append_log(self, path, report):
        self.log.insert('end', '%s\n%s\n\n' % (path, report))
        self.log.see('end')

    def _done(self):
        self.status.set('Done')
        self.repair_btn.config(state='normal')


if __name__ == '__main__':
    root = tk.Tk()
    app = App(root)
    if len(sys.argv) > 1:
        for p in sys.argv[1:]:
            if p not in app.files:
                app.files.append(p)
                app.tree.insert('', 'end', iid=p, text=p, values=('pending',))
        app.refresh_state()
    root.mainloop()