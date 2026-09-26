#!/usr/bin/env python3
"""Fetch a typical-user 3D print repair corpus from Thingi10K.

Selects 50 representative user models with mild defects (and clean controls)
from the Thingi10K dataset via direct downloads from the Hugging Face mirror
(https://huggingface.co/datasets/Thingi10K/Thingi10K).

Strata (50 meshes):
  - 20 'open' (not closed, not self-intersecting)
  - 20 'self-intersecting' (self_intersecting True)
  - 10 'clean control' (closed, not self-intersecting, solid)

Common filters:
  - num_facets: 1,000 .. 300,000
  - num_components <= 20
  - License: CC0 / Public Domain / CC-BY / CC-BY-SA (no NC, no ND)
  - Excludes all 87 Thingi10K IDs already used in tests/real-world-samples/
    and the 115-mesh benchmark corpus.

Stdlib only: runs under any Python 3.8+.
"""
import argparse
import csv
import os
import random
import shutil
import sys
import urllib.request

HF_BASE = "https://huggingface.co/datasets/Thingi10K/Thingi10K/resolve/main"

# The 87 Thingi10K IDs already used in Sutura:
# - 32 in tests/real-world-samples/ (documented in tests/real-world-samples/ATTRIBUTION.md)
# - 63 in the 115-mesh benchmark corpus (/tmp/sutura_corpus_100/thingi10k_*.stl,
#   downloaded via scripts/fetch_benchmark_corpus.sh from release benchmark-corpus-v1)
# - 8 overlap between the two sets (100045, 100077, 100173, 100827, 1038439, 1038441, 224108, 502009)
EXCLUDED_IDS = frozenset([
    39507, 40886, 42844, 46012, 55280, 55772, 57854, 59226, 60916, 63785,
    70561, 71691, 81221, 100045, 100077, 100173, 100281, 100423, 100643, 100728,
    100827, 101170, 101187, 101250, 101558, 101560, 101951, 102041, 102625, 103143,
    103144, 103284, 103286, 103289, 103742, 103821, 104401, 104431, 104445, 105382,
    105637, 105688, 105691, 105692, 105696, 105803, 136634, 145065, 224108, 228302,
    235725, 248395, 260537, 313444, 331105, 475828, 502009, 1004825, 1004826, 1005277,
    1005285, 1005289, 1017012, 1018273, 1018274, 1018275, 1018295, 1020669, 1036653, 1036655,
    1038432, 1038433, 1038434, 1038439, 1038441, 1038442, 1038443, 1038444, 1043461, 1044251,
    1053374, 1053874, 1054518, 1064115, 1066896, 1066897, 1356633,
])

ALLOWED_LICENSES = frozenset([
    "Creative Commons - Attribution",
    "Creative Commons - Attribution - Share Alike",
    "Creative Commons - Public Domain Dedication",
    "Public Domain",
])


class DownloadTracker:
    def __init__(self):
        self.total_bytes = 0

    def download(self, url, dest_path):
        """Download url to dest_path if not already present. Returns bytes downloaded in this call."""
        if os.path.exists(dest_path) and os.path.getsize(dest_path) > 0:
            return 0
        req = urllib.request.Request(url, headers={"User-Agent": "Sutura-Corpus-Fetcher/1.0"})
        with urllib.request.urlopen(req) as resp, open(dest_path, "wb") as out:
            data = resp.read()
            out.write(data)
            n = len(data)
            self.total_bytes += n
            return n


def load_metadata(cache_dir, tracker):
    """Download and parse Thingi10K metadata CSVs."""
    os.makedirs(cache_dir, exist_ok=True)
    meta_files = [
        "metadata/geometry_data.csv",
        "metadata/input_summary.csv",
        "metadata/contextual_data.csv",
    ]
    for rel in meta_files:
        fn = os.path.basename(rel)
        local = os.path.join(cache_dir, fn)
        url = f"{HF_BASE}/{rel}"
        bytes_dl = tracker.download(url, local)
        if bytes_dl > 0:
            print(f"Downloaded {fn} ({bytes_dl / 1024:.1f} KB)")

    # 1. input_summary.csv -> file_id -> Thing ID and original link extension
    summary_path = os.path.join(cache_dir, "input_summary.csv")
    file_to_thing = {}
    file_exts = {}
    with open(summary_path, "r", encoding="utf-8", errors="replace") as fh:
        for row in csv.DictReader(fh):
            try:
                fid = int(row["ID"])
                file_to_thing[fid] = int(row["Thing ID"])
                link = row.get("Link", "")
                ext = link.split(".")[-1].lower() if link else "stl"
                file_exts[fid] = ext
            except (ValueError, KeyError):
                continue

    # 2. contextual_data.csv -> Thing ID -> author and license
    ctx_path = os.path.join(cache_dir, "contextual_data.csv")
    thing_meta = {}
    with open(ctx_path, "r", encoding="utf-8", errors="replace") as fh:
        for row in csv.DictReader(fh):
            try:
                tid = int(row["Thing ID"])
                thing_meta[tid] = {
                    "author": row.get("Author", "").strip(),
                    "license": row.get("License", "").strip(),
                }
            except (ValueError, KeyError):
                continue

    # 3. geometry_data.csv -> mesh geometric metrics
    geom_path = os.path.join(cache_dir, "geometry_data.csv")
    with open(geom_path, "r", encoding="utf-8", errors="replace") as fh:
        geom_rows = list(csv.DictReader(fh))

    return geom_rows, file_to_thing, file_exts, thing_meta


def select_corpus(geom_rows, file_to_thing, file_exts, thing_meta, seed=42):
    """Filter and deterministically sample 50 meshes across three strata."""
    open_cand = []
    si_cand = []
    clean_cand = []

    for r in geom_rows:
        try:
            fid = int(r["file_id"])
        except (ValueError, KeyError):
            continue

        if fid in EXCLUDED_IDS:
            continue

        # Facet count filter: 1,000 .. 300,000
        num_faces = int(r["num_faces"])
        if not (1000 <= num_faces <= 300000):
            continue

        # Connected components filter: <= 20
        num_comp = int(r["num_connected_components"])
        if num_comp > 20:
            continue

        # License filter
        tid = file_to_thing.get(fid)
        if not tid or tid not in thing_meta:
            continue
        lic = thing_meta[tid]["license"]
        if lic not in ALLOWED_LICENSES:
            continue

        # Keep STL files directly
        ext = file_exts.get(fid, "stl")
        if ext != "stl":
            continue

        # Geometric properties:
        # closed: boundary edges == 0
        num_b = int(r["num_boundary_edges"])
        closed = (num_b == 0)

        # self_intersecting: num_self_intersections > 0 or num_coplanar_intersecting_faces > 0
        num_si = int(r["num_self_intersections"])
        num_coplanar = int(r["num_coplanar_intersecting_faces"])
        si = (num_si > 0 or num_coplanar > 0)

        # solid: solid == 1
        solid = (int(r["solid"]) == 1)

        author = thing_meta[tid]["author"]
        meta = {
            "id": fid,
            "num_facets": num_faces,
            "license": lic,
            "author": author,
            "ext": ext,
        }

        # Stratum criteria:
        # 1. 'open': not closed, not self-intersecting
        if not closed and not si:
            open_cand.append(meta)
        # 2. 'self-intersecting': self_intersecting == True
        if si:
            si_cand.append(meta)
        # 3. 'clean control': closed, not self-intersecting, solid
        if closed and not si and solid:
            clean_cand.append(meta)

    # Sort candidates by ID for deterministic sampling
    open_cand.sort(key=lambda x: x["id"])
    si_cand.sort(key=lambda x: x["id"])
    clean_cand.sort(key=lambda x: x["id"])

    print(f"Qualified STL candidates: open={len(open_cand)}, SI={len(si_cand)}, clean={len(clean_cand)}")

    rng = random.Random(seed)
    sel_open = rng.sample(open_cand, 20)
    sel_si = rng.sample(si_cand, 20)
    sel_clean = rng.sample(clean_cand, 10)

    selected = []
    for x in sel_open:
        x["stratum"] = "open"
        selected.append(x)
    for x in sel_si:
        x["stratum"] = "self-intersecting"
        selected.append(x)
    for x in sel_clean:
        x["stratum"] = "clean control"
        selected.append(x)

    selected.sort(key=lambda x: x["id"])
    return selected


def main():
    parser = argparse.ArgumentParser(
        description="Select and fetch the 50-mesh typical-user repair corpus."
    )
    parser.add_argument(
        "--dest",
        default="/tmp/sutura_typical_corpus",
        help="Target directory for downloaded STL meshes (default: /tmp/sutura_typical_corpus)",
    )
    parser.add_argument(
        "--cache-dir",
        default="/tmp/thingi10k_cache",
        help="Directory to cache metadata and raw downloads (default: /tmp/thingi10k_cache)",
    )
    parser.add_argument(
        "--manifest",
        default=os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "tests",
            "typical-corpus-ids.csv",
        ),
        help="Path to output manifest CSV (default: tests/typical-corpus-ids.csv)",
    )
    parser.add_argument("--seed", type=int, default=42, help="RNG seed for deterministic sampling")
    args = parser.parse_args()

    tracker = DownloadTracker()
    print("Loading Thingi10K metadata...")
    geom_rows, file_to_thing, file_exts, thing_meta = load_metadata(args.cache_dir, tracker)

    print(f"Selecting 50 meshes (seed={args.seed})...")
    selected = select_corpus(geom_rows, file_to_thing, file_exts, thing_meta, seed=args.seed)

    # Write manifest CSV
    os.makedirs(os.path.dirname(args.manifest), exist_ok=True)
    with open(args.manifest, "w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(["id", "stratum", "num_facets", "license", "author"])
        for item in selected:
            writer.writerow([item["id"], item["stratum"], item["num_facets"], item["license"], item["author"]])
    print(f"Wrote manifest: {args.manifest} ({len(selected)} entries)")

    # Download raw meshes and write to dest
    os.makedirs(args.dest, exist_ok=True)
    print(f"Fetching {len(selected)} meshes into {args.dest}...")
    for idx, item in enumerate(selected, 1):
        fid = item["id"]
        ext = item["ext"]
        cached_file = os.path.join(args.cache_dir, f"{fid}.{ext}")
        url = f"{HF_BASE}/raw_meshes/{fid}.{ext}"
        bytes_dl = tracker.download(url, cached_file)
        dest_stl = os.path.join(args.dest, f"thingi10k_{fid}.stl")
        shutil.copyfile(cached_file, dest_stl)
        size_kb = os.path.getsize(dest_stl) / 1024.0
        dl_note = f" (downloaded {bytes_dl / 1024.0:.1f} KB)" if bytes_dl > 0 else " (cached)"
        print(f"  [{idx:2d}/50] thingi10k_{fid}.stl [{item['stratum']}] {item['num_facets']} facets, {size_kb:.1f} KB{dl_note}")

    print()
    print(f"Done: {len(selected)} meshes placed in {args.dest}")
    print(f"Total downloaded in this session: {tracker.total_bytes / (1024 * 1024):.2f} MB")


if __name__ == "__main__":
    main()
