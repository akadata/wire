"""Generate SVG benchmark graphs from measured CSVs (single source of truth).

Reads akadata-report/perf_c.csv and perf_h324_normal.csv, writes
akadata-report/svg/c_throughput.svg and c_latency.svg.
No numbers are typed by hand: every bar/whisker/label comes from the CSVs,
except the explicitly-labeled historical phantom claim (202.15, old headline).
"""
import csv
import statistics
from pathlib import Path

REPORT = Path("/scratch/wire/akadata-report")
SVGDIR = REPORT / "svg"
SVGDIR.mkdir(exist_ok=True)


def load(csv_name, wanted):
    rows = {}
    with open(REPORT / csv_name) as fh:
        for r in csv.DictReader(fh):
            if r["series"] in wanted:
                rows.setdefault(r["series"], []).append(
                    (float(r["mops"]), float(r["ns_per_task"])))
    for s, v in rows.items():
        assert len(v) == 10, (s, len(v))
    return rows


c = load("perf_c.csv", ("c_o3", "c_native"))
rust = load("perf_h324_normal.csv", ("base_v6", "new_v6"))


def stats(vals):
    return {
        "mean": statistics.mean(vals),
        "min": min(vals),
        "max": max(vals),
    }


SERIES = [
    # key, label, color, kind
    ("c_o3", "C  -O3  (plain, no tuning)", "#2e7d32"),
    ("c_native", "C  -O3 -march=native", "#66bb6a"),
    ("new_v6", "Rust  improved drain_all", "#1565c0"),
    ("base_v6", "Rust  honest baseline", "#64b5f6"),
]

W, H = 760, 420
LEFT, RIGHT, TOP, BOT = 70, 30, 60, 104
PLOT_W = W - LEFT - RIGHT
PLOT_H = H - TOP - BOT


def svg_bar(title, subtitle, unit, series_stats, phantom=None,
            lower_better=False, filename="out.svg"):
    vmax = max(s["max"] for s in series_stats.values())
    if phantom:
        vmax = max(vmax, phantom["value"])
    vmax *= 1.12
    n = len(series_stats)
    slot = PLOT_W / n
    bar_w = min(110, slot * 0.52)

    def y(v):
        return TOP + PLOT_H - (v / vmax) * PLOT_H

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" '
        'font-family="DejaVu Sans, sans-serif">',
        f'<rect width="{W}" height="{H}" fill="#ffffff"/>',
        f'<text x="{W/2}" y="30" text-anchor="middle" font-size="19" '
        f'font-weight="bold" fill="#111">{title}</text>',
        f'<text x="{W/2}" y="50" text-anchor="middle" font-size="12" '
        f'fill="#555">{subtitle}</text>',
    ]
    # gridlines (4)
    for i in range(5):
        v = vmax * i / 4
        yy = y(v)
        parts.append(
            f'<line x1="{LEFT}" y1="{yy}" x2="{W-RIGHT}" y2="{yy}" '
            'stroke="#ddd"/>')
        parts.append(
            f'<text x="{LEFT-8}" y="{yy+4}" text-anchor="end" font-size="11" '
            f'fill="#555">{v:.0f}</text>')
    parts.append(
        f'<text x="16" y="{TOP+PLOT_H/2}" text-anchor="middle" font-size="12" '
        f'fill="#333" transform="rotate(-90 16 {TOP+PLOT_H/2})">{unit}</text>')

    for i, (key, label, color) in enumerate(SERIES):
        s = series_stats[key]
        cx = LEFT + slot * i + slot / 2
        parts.append(
            f'<rect x="{cx-bar_w/2:.1f}" y="{y(s["mean"]):.1f}" '
            f'width="{bar_w:.1f}" height="{TOP+PLOT_H-y(s["mean"]):.1f}" '
            f'fill="{color}" rx="3"/>')
        # min-max whisker
        parts.append(
            f'<line x1="{cx:.1f}" y1="{y(s["min"]):.1f}" x2="{cx:.1f}" '
            f'y2="{y(s["max"]):.1f}" stroke="#111" stroke-width="2"/>')
        parts.append(
            f'<line x1="{cx-12:.1f}" y1="{y(s["min"]):.1f}" '
            f'x2="{cx+12:.1f}" y2="{y(s["min"]):.1f}" stroke="#111" '
            'stroke-width="2"/>')
        parts.append(
            f'<line x1="{cx-12:.1f}" y1="{y(s["max"]):.1f}" '
            f'x2="{cx+12:.1f}" y2="{y(s["max"]):.1f}" stroke="#111" '
            'stroke-width="2"/>')
        parts.append(
            f'<text x="{cx:.1f}" y="{y(s["mean"])-14:.1f}" text-anchor="middle"'
            f' font-size="13" font-weight="bold" fill="#111">'
            f'{s["mean"]:.2f}</text>')
        # two-line label
        a, b = label.split("  ", 1)
        parts.append(
            f'<text x="{cx:.1f}" y="{H-BOT+22}" text-anchor="middle" '
            f'font-size="11" fill="#222">{a}</text>')
        parts.append(
            f'<text x="{cx:.1f}" y="{H-BOT+37}" text-anchor="middle" '
            f'font-size="11" fill="#222">{b}</text>')
        parts.append(
            f'<text x="{cx:.1f}" y="{H-BOT+52}" text-anchor="middle" '
            f'font-size="10" fill="#666">mean {s["mean"]:.2f} · '
            f'min–max {s["min"]:.1f}–{s["max"]:.1f}</text>')

    if phantom:
        # floating dashed annotation in the empty upper area (NOT a bar:
        # the old headline is off-scale and counted phantom work)
        bx, bw, bt, bh = LEFT + PLOT_W / 2, 300, TOP + 8, 72
        parts.append(
            f'<rect x="{bx-bw/2}" y="{bt}" width="{bw}" height="{bh}" '
            'fill="#eeeeee" rx="6" stroke="#888" stroke-dasharray="6,4"/>')
        parts.append(
            f'<text x="{bx}" y="{bt+26}" text-anchor="middle" font-size="15" '
            f'font-weight="bold" fill="#333">old headline: '
            f'{phantom["value"]:.2f}*</text>')
        parts.append(
            f'<text x="{bx}" y="{bt+46}" text-anchor="middle" font-size="11" '
            'fill="#333">*counted ~996k tasks never accepted — off chart, '
            'not comparable</text>')
        parts.append(
            f'<text x="{bx}" y="{bt+62}" text-anchor="middle" font-size="11" '
            'fill="#333">(see BENCHMARKS.md §1)</text>')

    note = ("lower is better" if lower_better else "higher is better")
    parts.append(
        f'<text x="{W-RIGHT}" y="{H-8}" text-anchor="end" font-size="10" '
        f'fill="#777">bars = mean of 10 runs, whiskers = min–max · {note} · '
        'Homer, i9-13900K</text>')
    parts.append("</svg>")
    (SVGDIR / filename).write_text("\n".join(parts))
    print("wrote", filename)


mops_stats, ns_stats = {}, {}
for key, _, _ in SERIES:
    src = c if key.startswith("c_") else rust
    mops_stats[key] = stats([v[0] for v in src[key]])
    ns_stats[key] = stats([v[1] for v in src[key]])

print({k: round(v["mean"], 2) for k, v in mops_stats.items()})
print({k: round(v["mean"], 2) for k, v in ns_stats.items()})

svg_bar(
    "Task dispatch throughput — C vs Rust (1M tasks, honest 1M/1M/1M)",
    "same box, same load window, serial back-to-back runs · v6 harness",
    "Mops/s", mops_stats,
    phantom={"value": 202.15},
    filename="c_throughput.svg",
)
svg_bar(
    "Latency per dispatch — C vs Rust (ns/task)",
    "same box, same load window, serial back-to-back runs · v6 harness",
    "ns / task", ns_stats, lower_better=True,
    filename="c_latency.svg",
)
