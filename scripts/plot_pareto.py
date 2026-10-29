#!/usr/bin/env python3
"""Render measured Silesia extraction/ratio frontiers and their Markdown table."""
import argparse
import json
from pathlib import Path
import re
import statistics

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


def frontier(points):
    return sorted([p for p in points if not any(
        q["seconds"] <= p["seconds"] and q["ratio"] >= p["ratio"]
        and (q["seconds"] < p["seconds"] or q["ratio"] > p["ratio"])
        for q in points)], key=lambda p: p["seconds"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="contains sweep.json and classic-formats.json")
    args = parser.parse_args()
    root = args.directory
    native = json.loads((root / "sweep.json").read_text())
    classic = json.loads((root / "classic-formats.json").read_text())
    dense_path = root / "dense-sweep.json"
    if dense_path.exists() and dense_path.stat().st_size:
        dense = json.loads(dense_path.read_text())
        if dense["input_bytes"] != native["input_bytes"]:
            raise ValueError("dense native corpus differs")
        native["configurations"].update(dense["configurations"])
    if native["input_bytes"] != classic["input_bytes"] or native["corpus"] != classic["corpus"]:
        raise ValueError("corpora differ")
    points = []
    for label, row in native["configurations"].items():
        level, block = map(int, re.fullmatch(r"level(-?\d+)-block(\d+)", label).groups())
        points.append({"label": f"L{level} / {block} KiB", "level": level, "block": block,
            "ratio": native["input_bytes"] / row["archive_bytes"],
            "seconds": row["extraction"]["median_seconds"], "measurement": row["extraction"],
            "row": row, "native": True})
    for kind, row in classic["formats"].items():
        points.append({"label": kind.upper() + (" L1" if kind != "tar" else ""), "ratio": classic["input_bytes"] / row["archive_bytes"],
            "seconds": row["zipper"]["median_seconds"], "measurement": row["zipper"],
            "row": row, "native": False})
    dense_classic_path = root / "dense-classic.json"
    if (root / "dense-classic-current.json").exists():
        dense_classic_path = root / "dense-classic-current.json"
    if dense_classic_path.exists() and dense_classic_path.stat().st_size:
        dense_classic = json.loads(dense_classic_path.read_text())
        if dense_classic["input_bytes"] != native["input_bytes"]:
            raise ValueError("dense classic corpus differs")
        for kind, row in dense_classic["formats"].items():
            points.append({"label": f"{kind.upper()} L{dense_classic['level']}",
                "ratio": native["input_bytes"] / row["archive_bytes"],
                "seconds": row["zipper"]["median_seconds"], "measurement": row["zipper"],
                "row": row, "native": False})
            for engine, suffix in [("zipper-native", "stream"), ("zipper-native-fast", "buffered")]:
                if engine in row:
                    points.append({"label": f"ZIP L9 {suffix}",
                        "ratio": native["input_bytes"] / row["archive_bytes"],
                        "seconds": row[engine]["median_seconds"], "measurement": row[engine],
                        "row": row, "native": False, "native_zip": True})
    native_zip_path = root / "native-zip.json"
    if (root / "fast-zip.json").exists():
        native_zip_path = root / "fast-zip.json"
    if (root / "fast-zip-current.json").exists():
        native_zip_path = root / "fast-zip-current.json"
    if native_zip_path.exists():
        native_zip = json.loads(native_zip_path.read_text())
        if native_zip["input_bytes"] != native["input_bytes"]:
            raise ValueError("native ZIP corpus differs")
        row = native_zip["formats"]["zip"]
        for engine, suffix in [("zipper-native", "stream"), ("zipper-native-fast", "buffered")]:
            if engine in row:
                points.append({"label": f"ZIP L1 {suffix}",
                    "ratio": native["input_bytes"] / row["archive_bytes"],
                    "seconds": row[engine]["median_seconds"],
                    "measurement": row[engine], "row": row, "native": False,
                    "native_zip": True})
    tar_path = root / "tar-tuned.json"
    if tar_path.exists() and tar_path.stat().st_size:
        tar = json.loads(tar_path.read_text())["corpora"]["silesia"]
        if tar["input_bytes"] != native["input_bytes"]:
            raise ValueError("tar corpus differs")
        for tag, engine in [("T1", "tar-1MiB"), ("T2", "tar+pzstd-1MiB"), ("T3", "tar+zstd-1MiB")]:
            row = tar["engines"][engine]
            times = [s["extraction_seconds"] for s in row["samples"]]
            points.append({"label": tag, "engine": engine, "native": False, "comparator": True,
                "ratio": native["input_bytes"] / row["archive_bytes"], "row": row,
                "seconds": statistics.median(times),
                "measurement": {"min_seconds": min(times), "max_seconds": max(times)}})
    zip_tar_path = root / "zip-vs-tar.json"
    if zip_tar_path.exists():
        zip_tar = json.loads(zip_tar_path.read_text())
        if zip_tar["input_bytes"] != native["input_bytes"]:
            raise ValueError("bsdtar ZIP corpus differs")
        row = zip_tar["formats"]["zip"]
        m = row["bsdtar-1MiB"]
        points.append({"label": "T4", "engine": "bsdtar ZIP, 1 MiB", "native": False, "comparator": True,
            "ratio": native["input_bytes"] / row["archive_bytes"], "row": row,
            "seconds": m["median_seconds"], "measurement": m})
    native_points = [p for p in points if p["native"]]
    all_front = frontier(points)
    native_front = frontier(native_points)
    colors = {-3: "#7c3aed", 1: "#0284c7", 3: "#059669", 6: "#d97706",
              9: "#db2777", 15: "#7c2d12", 19: "#334155"}
    plt.rcParams.update({"font.size": 10, "axes.spines.top": False, "axes.spines.right": False,
                         "svg.fonttype": "none", "svg.hashsalt": "zipper-pareto"})
    fig, axes = plt.subplots(1, 2, figsize=(14, 7.5), gridspec_kw={"width_ratios": [1.1, 1]})
    for ax, subset, front, title in [(axes[0], points, all_front, "All measured formats"),
                                      (axes[1], native_points, native_front, "Native settings — detail")]:
        seen = set()
        for p in subset:
            measurement = p["measurement"]
            buffered = "buffered" in p["label"]
            label = (f".zipper level {p['level']}" if p["native"] else
                     ("ZIP native buffered" if buffered else "ZIP native stream") if p.get("native_zip") else "7-Zip backend")
            color = colors[p["level"]] if p["native"] else "#e11d48" if p.get("native_zip") else "#334155"
            if p.get("comparator"):
                label, color = "bsdtar baselines (T1–T4)", "#15803d"
            ax.errorbar(p["seconds"] * 1000, p["ratio"],
                xerr=[[1000 * (p["seconds"] - measurement["min_seconds"])],
                      [1000 * (measurement["max_seconds"] - p["seconds"])]],
                fmt="o" if p["native"] else "D", color=color, capsize=2,
                markerfacecolor="white" if p.get("native_zip") and not buffered else color,
                markersize=5, elinewidth=0.8, alpha=0.8,
                label=label if label not in seen else None)
            seen.add(label)
            if not p["native"]:
                if p.get("comparator"):
                    ax.annotate(p["label"], (p["seconds"] * 1000, p["ratio"]),
                                xytext=(5, -15), textcoords="offset points", fontsize=9, color=color)
                elif buffered:
                    ax.annotate(p["label"], (p["seconds"] * 1000, p["ratio"]),
                        xytext=(p["seconds"] * (900 if "L9" in p["label"] else 1350),
                                p["ratio"] + (0.45 if "L9" in p["label"] else -0.52)),
                        ha="center", fontsize=9, arrowprops={"arrowstyle": "-", "color": "#94a3b8", "lw": 0.8})
                else:
                    ax.annotate(p["label"], (p["seconds"] * 1000, p["ratio"]),
                                xytext=(7, 8), textcoords="offset points", fontsize=9)
        ax.plot([p["seconds"] * 1000 for p in front], [p["ratio"] for p in front],
                "--", color="#111827", linewidth=1.3, label="Observed median frontier")
        ax.set(title=title, xlabel="Extraction time (ms) — lower is better",
               ylabel="Original bytes / archive bytes — higher is better")
        ax.grid(alpha=0.2)
        ax.margins(x=0.12, y=0.15)
    axes[0].set_xscale("log")
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="lower center", bbox_to_anchor=(0.5, 0.065), ncol=4, fontsize=9)
    # Numbered frontier markers connect the detailed plot to an exact table.
    for i, p in enumerate(native_front, 1):
        axes[1].annotate(str(i), (p["seconds"] * 1000, p["ratio"]),
                         xytext=(0, -16 if i in (4, 6) else 10), textcoords="offset points",
                         ha="center", fontsize=9, fontweight="bold")
    fig.suptitle("Zipper: decompression time versus compression ratio", fontsize=16, fontweight="bold")
    fig.text(0.5, 0.025,
        "Silesia · Apple M5 · 7 measured runs + warmup · bars: observed min–max · all recovered bytes verified\n"
        "Native: 4 codec workers; 7-Zip defaults; bsdtar + optional 4-worker pzstd. Local results; metadata support differs.",
        ha="center", fontsize=9, color="#475569")
    fig.tight_layout(rect=[0, 0.20, 1, 0.94])
    for extension in ["svg", "png", "pdf"]:
        fig.savefig(root / f"pareto.{extension}", dpi=180)
    svg = root / "pareto.svg"
    svg.write_text("\n".join(line.rstrip() for line in svg.read_text().splitlines()) + "\n")
    lines = ["# Decompression time versus compression ratio", "",
        "![Measured Pareto frontier](pareto.svg)", "",
        "[PNG](pareto.png) · [PDF](pareto.pdf) · [Native samples](sweep.json) · [Higher native levels](dense-sweep.json) · [Classic-format samples](classic-formats.json)", "",
        "A point is on the observed Pareto frontier if no other measured configuration extracts in less or equal median time while producing a smaller or equal archive, with at least one strict improvement. Better points are toward the upper left. Ratio means original bytes divided by complete archive bytes. Lines only connect observed points; they do not imply intermediate configurations exist. Error bars show observed minima and maxima, not confidence intervals. Overlapping ranges do not establish a reliable speed ranking.", "",
        "## Native frontier (numbers on the detail plot)", "",
        "| Point | Setting | Ratio | Extraction median (ms) | Range (ms) |", "| --- | --- | ---: | ---: | ---: |"]
    for i, p in enumerate(native_front, 1):
        m = p["measurement"]
        lines.append(f"| {i} | {p['label']} | {p['ratio']:.3f}× | {p['seconds']*1000:.2f} | {m['min_seconds']*1000:.2f}–{m['max_seconds']*1000:.2f} |")
    lines += ["", "## All native configurations", "",
        "| Level / block | Ratio | Archive bytes | Compression MiB/s | Extraction ms | Range ms | Peak creation RSS MiB |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for p in native_points:
        r, m = p["row"], p["measurement"]
        lines.append(f"| {p['label']} | {p['ratio']:.3f}× | {r['archive_bytes']:,} | {r['compression']['mib_s']:.0f} | {p['seconds']*1000:.2f} | {m['min_seconds']*1000:.2f}–{m['max_seconds']*1000:.2f} | {r['compression']['peak_rss_bytes']/2**20:.1f} |")
    lines += ["", "## Zipper handling classic formats", "",
        "Archives are created by 7-Zip 26.05: ZIP/Deflate and 7z/LZMA2 (levels labeled in the table), and uncompressed TAR. Default extraction delegates to that backend. ZIP stream uses the opt-in parallel Rust ZIP/zlib-rs reader; ZIP buffered adds libdeflate with up to 64 MiB of entry buffers per worker. Both request four workers and read the same archive bytes as their matched direct backend. Native ZIP preserves bytes/directories only, so it is not yet a metadata-equivalent replacement. The plot uses Zipper extraction time, not direct-backend time. [Current level-1 samples](fast-zip-current.json) and [current level-9 samples](dense-classic-current.json) use work-stealing revision `afb77d0` and include in-process RSS. Earlier static-scheduling trials remain in `fast-zip.json` and `dense-classic.json`. Current extraction-only trials prepare one archive per format before timing repeated reads; their null creation times are intentional.", "",
        "| Format / reader | Ratio | Archive bytes | Zipper median ms | Zipper range ms | Direct 7-Zip median ms | Native reader RSS MiB |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for p in points:
        if not p["native"] and not p.get("comparator"):
            r, m = p["row"], p["measurement"]
            memory = f"{m['peak_rss_bytes']/2**20:.1f}" if "peak_rss_bytes" in m else "—"
            lines.append(f"| {p['label']} | {p['ratio']:.3f}× | {r['archive_bytes']:,} | {p['seconds']*1000:.2f} | {m['min_seconds']*1000:.2f}–{m['max_seconds']*1000:.2f} | {r['7zip-direct']['median_seconds']*1000:.2f} | {memory} |")
    lines += ["", "## Tuned bsdtar baselines", "",
        "These green points use 1 MiB tar record buffers. T2 uses four pzstd workers; T3 uses ordinary zstd decoding. Compressed pipelines consume all padding (`--ignore-zeros`) so the decoder finishes successfully. T4 extracts the same ZIP used in its matched Zipper comparison. Tar round trips use SHA-256 manifests; ZIP comparisons use direct byte checks. Timing sessions differ from the native level sweep. [Tar samples](tar-tuned.json), [identical-ZIP samples](zip-vs-tar.json), and [matched comparison details](../../docs/tar-comparison.md).", "",
        "| Point | Reader | Ratio | Archive bytes | Median ms | Range ms |",
        "| --- | --- | ---: | ---: | ---: | ---: |"]
    for p in points:
        if p.get("comparator"):
            m = p["measurement"]
            lines.append(f"| {p['label']} | {p['engine']} | {p['ratio']:.3f}× | {p['row']['archive_bytes']:,} | {p['seconds']*1000:.2f} | {m['min_seconds']*1000:.2f}–{m['max_seconds']*1000:.2f} |")
    lines += ["", "## Method and limits", "",
        f"Silesia contains {native['input_bytes']:,} bytes in 12 files. Data came from a [pinned public mirror](https://github.com/MiloszKrajewski/SilesiaCorpus/tree/3f3fa2cdbbb3795c903b74e774acb309e1360337); [provenance and SHA-256 hashes](corpus.json) make the inputs auditable. Each native setting ran seven measured rounds after a warmup, with shuffled order. Classic formats ran in a separate interleaved session; native/classic cross-session timing differences are less controlled than comparisons within each session. Every extraction was compared directly byte for byte, including tree membership and file sizes. Verification was outside the timed interval.", "",
        "Times include process startup, index/metadata work and filesystem writes, and exclude explicit extraction sync. They are application extraction times, not in-memory codec speeds. Cache flushing and thermal/storage isolation were not performed. Native uses four codec workers plus I/O threads; classic extraction uses 7-Zip's default thread allocation for both paths. Tar/ZIP/7z metadata semantics differ from native Zipper. No cross-machine leadership or equal-feature comparison is claimed. Repeat on Linux/x86 and larger corpora before changing defaults.", "",
        "The sweep exposes a genuine tradeoff: larger blocks can improve higher-level ratio, but consume much more memory and often extract more slowly. The default remains level 1 / 1024 KiB. Compression speed, memory, selective extraction cost, and metadata support are separate axes not optimized by this two-dimensional frontier.", "",
        "## Reproduce", "", "```sh",
        "python3 scripts/fetch_silesia.py /tmp/zipper-corpora/silesia", "cargo build --release",
        "python3 scripts/sweep.py /tmp/zipper-corpora/silesia --runs 7 --threads 4 > benchmarks/silesia/sweep.json",
        "python3 scripts/sweep.py /tmp/zipper-corpora/silesia --levels 9 15 19 --blocks-kib 1024 4096 16384 --runs 7 --threads 4 > benchmarks/silesia/dense-sweep.json",
        "python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --runs 7 --threads 4 > benchmarks/silesia/classic-formats.json",
        "python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --formats zip --native-fast --reuse-archives --runs 7 --threads 4 > benchmarks/silesia/fast-zip-current.json",
        "python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --formats zip 7z --level 9 --native-fast --reuse-archives --runs 7 --threads 4 > benchmarks/silesia/dense-classic-current.json",
        "python3 scripts/compare_zstd.py /tmp/zipper-corpora --corpus silesia --include-plain-tar --tuned-tar --runs 7 > benchmarks/silesia/tar-tuned.json",
        "python3 scripts/compare_formats.py /tmp/zipper-corpora/silesia --formats zip --native-fast --tar-reader tar --reuse-archives --runs 7 --threads 4 > benchmarks/silesia/zip-vs-tar.json",
        "# With matplotlib installed in your plotting environment:",
        "python3 scripts/plot_pareto.py benchmarks/silesia", "```", "",
        "Copy the adjacent generated `silesia.manifest.json` to `benchmarks/silesia/corpus.json` when refreshing the data. The fetch destination must be new. Classic format runs require 7zz on PATH."]
    (root / "README.md").write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
