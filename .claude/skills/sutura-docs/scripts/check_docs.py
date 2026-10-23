#!/usr/bin/env python3
"""Check Sutura user docs: links, anchors, EN/TR parity, landing index.

Usage: python3 check_docs.py [REPO_ROOT]   (default: current directory)
Exit code 0 = clean, 1 = problems found. Read-only; changes nothing.
"""
import os
import re
import sys

ROOT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else ".")
EN = os.path.join(ROOT, "docs", "guide")
TR = os.path.join(EN, "tr")
LANDING = [os.path.join(ROOT, "README.md"), os.path.join(ROOT, "README.tr.md")]
problems = []


def slug(text):
    text = text.strip().lower()
    text = "".join(c for c in text if c.isalnum() or c in " -_")
    return text.replace(" ", "-")


def outline(path):
    """Return (anchors, header levels) of a markdown file, fences skipped."""
    anchors, levels, seen, fence = set(), [], {}, False
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            if line.lstrip().startswith("```"):
                fence = not fence
                continue
            if fence:
                continue
            m = re.match(r"^(#{1,6}) (.*)", line)
            if m:
                levels.append(len(m.group(1)))
                a = slug(m.group(2))
                n = seen.get(a, 0)
                seen[a] = n + 1
                anchors.add(a if n == 0 else "%s-%d" % (a, n))
            for m in re.finditer(r'<a (?:name|id)="([^"]+)"', line):
                anchors.add(m.group(1))
    return anchors, levels


def check_links(path):
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    base = os.path.dirname(path)
    rel = os.path.relpath(path, ROOT)
    for m in re.finditer(r"\]\(([^)\s]+)\)|src=\"([^\"]+)\"|href=\"([^\"]+)\"", text):
        target = m.group(1) or m.group(2) or m.group(3)
        if re.match(r"^(https?:|mailto:|data:)", target):
            continue
        p, _, anchor = target.partition("#")
        dest = os.path.normpath(os.path.join(base, p)) if p else path
        if not os.path.exists(dest):
            problems.append("missing target  %s -> %s" % (rel, target))
            continue
        if anchor and dest.endswith(".md") and anchor not in outline(dest)[0]:
            problems.append("missing anchor  %s -> %s" % (rel, target))


def pages(folder):
    return sorted(f for f in os.listdir(folder) if f.endswith(".md"))


en_pages, tr_pages = pages(EN), pages(TR)
for name in sorted(set(en_pages) ^ set(tr_pages)):
    side = "TR" if name in en_pages else "EN"
    problems.append("no %s twin     docs/guide/%s" % (side, name))

for name in sorted(set(en_pages) & set(tr_pages)):
    en_file, tr_file = os.path.join(EN, name), os.path.join(TR, name)
    en_lv, tr_lv = outline(en_file)[1], outline(tr_file)[1]
    if en_lv != tr_lv:
        problems.append(
            "outline differs docs/guide/%s (EN %d headers, TR %d) - TR twin not in sync"
            % (name, len(en_lv), len(tr_lv)))
    with open(en_file, encoding="utf-8") as fh:
        head = fh.read(400)
    if "../../README.md" not in head or "tr/%s" % name not in head:
        problems.append("nav line       docs/guide/%s (back + Türkçe link)" % name)
    with open(tr_file, encoding="utf-8") as fh:
        head = fh.read(400)
    if "../../../README.tr.md" not in head or "../%s" % name not in head:
        problems.append("nav line       docs/guide/tr/%s (back + English link)" % name)

for landing in LANDING:
    with open(landing, encoding="utf-8") as fh:
        text = fh.read()
    for name in en_pages:
        if "docs/guide/%s" % name not in text or "docs/guide/tr/%s" % name not in text:
            problems.append("not indexed    %s lacks docs/guide/%s or its TR twin"
                            % (os.path.basename(landing), name))

for f in LANDING + [os.path.join(EN, n) for n in en_pages] + [os.path.join(TR, n) for n in tr_pages]:
    check_links(f)
for extra in ("CHANGELOG.md", "CONTRIBUTING.md"):
    if os.path.exists(os.path.join(ROOT, extra)):
        check_links(os.path.join(ROOT, extra))

for p in problems:
    print(p)
print("docs check: %d page(s) per language, %d problem(s)" % (len(en_pages), len(problems)))
sys.exit(1 if problems else 0)
