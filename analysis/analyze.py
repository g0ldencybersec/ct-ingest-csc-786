#!/usr/bin/env python3
"""Analysis of E0 and C1 results. Standard library only (matplotlib optional,
for figures).

  python3 analysis/analyze.py --e0 runs/e0-*.json --c1 runs/c1-* --out runs/analysis

Writes <out>/results.md (tables), <out>/results.json (numbers), and figures
if matplotlib is installed.

C1 statistics: within each unit (same block, network, cache state), arms are
paired. For each metric we report the median of per-unit ratios
(tiles / legacy-gzip primary; legacy-identity / legacy-gzip diagnostic), a 95%
percentile-bootstrap CI of that median (10,000 resamples, fixed seed), and an
exact two-sided Wilcoxon signed-rank test on log ratios.
"""
import argparse, glob, json, math, os, random, statistics
from collections import defaultdict

BOOT = 10_000
RNG_SEED = 786

# ---------------------------------------------------------------- statistics

def wilcoxon_exact(x):
    """Two-sided exact Wilcoxon signed-rank p-value for H0: median(x) = 0.
    Zeros dropped; ties get average ranks (exact enumeration over doubled
    ranks keeps it exact for tied data). Returns (W+, n, p)."""
    x = [v for v in x if v != 0]
    n = len(x)
    if n == 0:
        return (0.0, 0, 1.0)
    order = sorted(range(n), key=lambda i: abs(x[i]))
    ranks = [0.0] * n
    i = 0
    while i < n:
        j = i
        while j + 1 < n and abs(x[order[j + 1]]) == abs(x[order[i]]):
            j += 1
        for k in range(i, j + 1):
            ranks[order[k]] = (i + j + 2) / 2
        i = j + 1
    r2 = [int(round(2 * r)) for r in ranks]          # integer doubled ranks
    wplus2 = sum(r2[i] for i in range(n) if x[i] > 0)
    total = sum(r2)
    counts = defaultdict(int)
    counts[0] = 1
    for r in r2:                                      # distribution of sum of random subset
        nxt = defaultdict(int)
        for s, c in counts.items():
            nxt[s] += c
            nxt[s + r] += c
        counts = nxt
    allc = 2 ** n
    mean2 = total / 2
    dev = abs(wplus2 - mean2)
    tail = sum(c for s, c in counts.items() if abs(s - mean2) >= dev - 1e-9)
    return (wplus2 / 2, n, min(1.0, tail / allc))


def boot_median_ci(xs, seed=RNG_SEED, b=BOOT):
    if len(xs) < 2:
        return (float("nan"), float("nan"))
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(xs, k=len(xs))) for _ in range(b))
    return (meds[int(0.025 * b)], meds[int(0.975 * b) - 1])


def q(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = (len(xs) - 1) * p
    f = math.floor(k)
    c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)

# ---------------------------------------------------------------- E0

def e0_section(path):
    d = json.load(open(path))
    t = d["total"]
    n = t["entries"]
    A = t["legacy_json_bytes"]
    Ag = t["legacy_gzip_served_bytes"]
    A6 = t["legacy_gzip_local6_bytes"]
    B = t["legacy_raw_bytes"]
    B6 = t["legacy_binary_gzip6_bytes"]
    C = t["tile_decoded_bytes"] + t["issuer_der_bytes"]
    Cg = t["tile_served_bytes"] + t["issuer_served_bytes"]
    per = lambda x: x / n
    rows = [
        ("A",  "Legacy JSON, uncompressed", A),
        ("A'", "Legacy JSON, gzip as served by the log", Ag),
        ("A6", "Legacy JSON, gzip -6 applied locally", A6),
        ("B",  "Legacy binary (no base64, no JSON)", B),
        ("B6", "Legacy binary, gzip -6 per 1,000 entries", B6),
        ("C",  "Tiles decompressed + each issuer once", C),
        ("C'", "Tiles as served (gzip) + issuers as served", Cg),
    ]
    md = [f"## E0: encoding analysis ({n:,} entries, {t['x509']:,} x509 / {t['precert']:,} precert)\n",
          "| Rep. | Representation | Total MB | Bytes/entry | vs. A' |",
          "|---|---|---:|---:|---:|"]
    for k, name, v in rows:
        md.append(f"| {k} | {name} | {v/1e6:,.1f} | {per(v):,.0f} | {v/Ag:.3f} |")
    chain_legacy = t["legacy_extra_chain_bytes"]
    chain_tiles = t["tile_fingerprint_bytes"] + t["issuer_der_bytes"]
    dec = [
        ("Base64 overhead (A − B, part)", t["legacy_base64_overhead_bytes"]),
        ("JSON framing (A − B, part)", t["legacy_json_framing_bytes"]),
        ("Legacy chain bytes (full DER per entry)", chain_legacy),
        ("Tiles chain bytes (fingerprints + issuers once per block)", chain_tiles),
        ("Leaf/TimestampedEntry bytes, legacy", t["legacy_leaf_input_bytes"]),
        ("TimestampedEntry bytes, tiles", t["tile_timestamped_entry_bytes"]),
    ]
    md += ["", "| Component | MB | Bytes/entry | Share of A |", "|---|---:|---:|---:|"]
    for name, v in dec:
        md.append(f"| {name} | {v/1e6:,.1f} | {per(v):,.0f} | {v/A:.1%} |")
    factors = {
        "total_uncompressed_legacy_over_tiles_served (A/C')": A / Cg,
        "operational_legacy_gzip_over_tiles_served (A'/C')": Ag / Cg,
        "encoding_effect_uncompressed (A/C)": A / C,
        "  of_which_base64_and_json (A/B)": A / B,
        "  of_which_chain_dedup_and_layout (B/C)": B / C,
        "compression_effect_legacy (A/A')": A / Ag,
        "compression_effect_tiles (C/C')": C / Cg,
        "gzip_on_legacy_binary_vs_tiles_served (B6/C')": B6 / Cg,
        "chain_repetition_factor (legacy chain / tiles chain)": chain_legacy / chain_tiles,
    }
    md += ["", "| Factor | Ratio |", "|---|---:|"]
    for k, v in factors.items():
        md.append(f"| {k.strip()} | {v:.2f}× |")
    enc = {"legacy (gzip requested)": t["legacy_gzip_content_encodings"],
           "tiles": t["tile_content_encodings"], "tile gzip layers": t["tile_gzip_layers"]}
    md += ["", "Response encodings actually received: " + "; ".join(f"{k}: {v}" for k, v in enc.items())]
    return "\n".join(md), {"rows": {k: v for k, _, v in rows}, "factors": factors, "entries": n, "encodings": enc}

# ---------------------------------------------------------------- C1

METRICS = [
    ("wall_secs", "Wall time (s)", lambda s: s["wall_secs"]),
    ("wire_mb", "Bytes on wire (MB)", lambda s: s["wire_bytes"] / 1e6),
    ("requests", "Requests", lambda s: s["requests"]),
    ("client_cpu", "Client CPU (s)", lambda s: s["client"]["cpu_user_secs"] + s["client"]["cpu_sys_secs"]),
    ("client_rss_mb", "Client peak RSS (MB)", lambda s: s["client"]["max_rss_kib"] / 1024),
    ("server_cpu", "Server CPU (s)", lambda s: (s.get("server") or {}).get("cpu_secs")),
]


def load_c1(dirs):
    trials, metas = {}, []
    for d in dirs:
        mp = os.path.join(d, "meta.json")
        if os.path.exists(mp):
            metas.append(json.load(open(mp)))
        p = os.path.join(d, "trials.jsonl")
        if not os.path.exists(p):
            continue
        for line in open(p):
            line = line.strip()
            if line:
                s = json.loads(line)
                trials[(d, s.get("trial_id"))] = s       # last line wins (resumed runs)
    return list(trials.values()), metas


def c1_section(dirs):
    trials, metas = load_c1(dirs)
    excluded = defaultdict(int)
    good = []
    for s in trials:
        if not s.get("verified"):
            excluded["not verified"] += 1
        elif (s.get("server") or {}).get("restarted_during_trial"):
            excluded["server restarted during trial"] += 1
        else:
            good.append(s)
    retried = sum(1 for s in good if s.get("retries", 0) > 0)
    cells = defaultdict(list)
    for s in good:
        m = s["meta"]
        cells[(m["net"], m["cache"], s["arm"])].append(s)

    md = [f"## C1: repeated trials on identical data\n",
          f"Trials: {len(trials)} recorded, {len(good)} analyzed"
          + (f"; excluded: {dict(excluded)}" if excluded else "")
          + (f"; {retried} analyzed trials had ≥1 HTTP retry" if retried else "") + ".",
          ""]
    if metas:
        m = metas[0]
        md.append(f"Block: {good[0]['entries']:,} entries per trial (aligned subtree, root-verified). "
                  f"Concurrency {m['concurrency']}. WAN profile: {m['wan_profile']['delay_each_way']} each way, "
                  f"{m['wan_profile']['rate_each_way']} each way. Server: {m['server'].get('nproc')} vCPU, "
                  f"cache {m['server'].get('cache_mb')} MB. Client: {m['client'].get('nproc')} vCPU.\n" if good else "")

    # Per-cell descriptive table.
    md += ["### Per condition (median [IQR])\n",
           "| Net | Cache | Arm | n | Wall s | MB on wire | Requests | Client CPU s | Client RSS MB | Server CPU s | Steal % (server) |",
           "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
    desc = {}
    order_arm = {"legacy-gzip": 0, "tiles": 1, "legacy-identity": 2}
    for key in sorted(cells, key=lambda k: (k[0], k[1], order_arm.get(k[2], 9))):
        ss = cells[key]
        row = {"n": len(ss)}
        cols = []
        for mk, _, f in METRICS:
            vals = [f(s) for s in ss if f(s) is not None]
            row[mk] = {"median": q(vals, .5), "q1": q(vals, .25), "q3": q(vals, .75)} if vals else None
            if mk in ("requests",):
                cols.append(f"{q(vals,.5):,.0f}" if vals else "–")
            else:
                cols.append(f"{q(vals,.5):,.2f} [{q(vals,.25):,.2f}–{q(vals,.75):,.2f}]" if vals else "–")
        steal = [s["server"]["steal_frac"] for s in ss if (s.get("server") or {}).get("steal_frac") is not None]
        row["server_steal_frac_median"] = q(steal, .5) if steal else None
        cols.append(f"{100*q(steal,.5):.1f}" if steal else "–")
        desc["/".join(key)] = row
        md.append(f"| {key[0]} | {key[1]} | {key[2]} | {len(ss)} | " + " | ".join(cols) + " |")

    # Paired ratios.
    units = defaultdict(dict)
    for s in good:
        m = s["meta"]
        units[(m["net"], m["cache"], m["unit"])][s["arm"]] = s
    paired = {}
    md += ["", "### Paired ratios (median of per-unit ratios, 95% bootstrap CI, exact Wilcoxon p)\n",
           "Ratio < 1 means the first arm costs less.\n",
           "| Comparison | Net | Cache | Metric | n | Median ratio | 95% CI | p |",
           "|---|---|---|---|---:|---:|---|---:|"]
    for (a, b, label) in [("tiles", "legacy-gzip", "tiles / legacy-gzip (primary)"),
                          ("legacy-identity", "legacy-gzip", "legacy-identity / legacy-gzip (diagnostic)")]:
        for net in sorted({k[0] for k in units}):
            for cache in sorted({k[1] for k in units}):
                us = [u for k, u in units.items() if k[0] == net and k[1] == cache and a in u and b in u]
                if not us:
                    continue
                for mk, mname, f in METRICS:
                    if mk == "client_rss_mb":
                        continue
                    rs = []
                    for u in us:
                        x, y = f(u[a]), f(u[b])
                        if x is not None and y is not None and x > 0 and y > 0:
                            rs.append(x / y)
                    if len(rs) < 2:
                        continue
                    med = statistics.median(rs)
                    lo, hi = boot_median_ci(rs)
                    _, n, p = wilcoxon_exact([math.log(r) for r in rs])
                    paired[f"{a}/{b}|{net}|{cache}|{mk}"] = {"n": n, "median": med, "ci": [lo, hi], "p": p}
                    md.append(f"| {label} | {net} | {cache} | {mname} | {len(rs)} | {med:.3f} | {lo:.3f}–{hi:.3f} | {p:.3g} |")

    # Encodings actually received, per arm and request kind.
    enc = defaultdict(lambda: defaultdict(int))
    for s in good:
        for kind, v in s.get("by_kind", {}).items():
            for e, c in v.get("content_encoding_received", {}).items():
                enc[(s["arm"], s["accept_encoding_sent"], kind)][e] += c
    md += ["", "### Response encoding actually received\n", "| Arm | Accept-Encoding sent | Request kind | Content-Encoding received (count) |", "|---|---|---|---|"]
    for k in sorted(enc):
        md.append(f"| {k[0]} | {k[1]} | {k[2]} | " + ", ".join(f"{e}: {c:,}" for e, c in sorted(enc[k].items())) + " |")

    return "\n".join(md), {"descriptive": desc, "paired": paired, "excluded": dict(excluded),
                           "encodings": {"|".join(k): dict(v) for k, v in enc.items()}}, cells, units


def figures(cells, units, out):
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        return []
    files = []
    colors = {"legacy-gzip": "#2F5FA8", "tiles": "#E07A2E", "legacy-identity": "#9AA4B2"}
    conds = sorted({(k[0], k[1]) for k in cells})
    arms = [a for a in ["legacy-gzip", "tiles", "legacy-identity"] if any(k[2] == a for k in cells)]
    for mk, mname, f in [m for m in METRICS if m[0] in ("wall_secs", "wire_mb", "client_cpu", "server_cpu")]:
        fig, ax = plt.subplots(figsize=(8, 4))
        w = 0.8 / max(1, len(arms))
        for i, a in enumerate(arms):
            data = [[f(s) for s in cells.get((n, c, a), []) if f(s) is not None] for n, c in conds]
            pos = [j + (i - (len(arms) - 1) / 2) * w for j in range(len(conds))]
            bp = ax.boxplot([d if d else [float("nan")] for d in data], positions=pos, widths=w * 0.9,
                            patch_artist=True, showfliers=True)
            for box in bp["boxes"]:
                box.set_facecolor(colors.get(a, "#888")); box.set_alpha(0.85)
            ax.plot([], [], color=colors.get(a, "#888"), lw=8, label=a)
        ax.set_xticks(range(len(conds)))
        ax.set_xticklabels([f"{n} / {c}" for n, c in conds])
        ax.set_ylabel(mname)
        ax.set_ylim(bottom=0)
        ax.legend(frameon=False, fontsize=9)
        ax.spines[["top", "right"]].set_visible(False)
        fig.tight_layout()
        p = os.path.join(out, f"c1-{mk}.png")
        fig.savefig(p, dpi=200)
        plt.close(fig)
        files.append(p)
    return files


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--e0", help="E0 report JSON")
    ap.add_argument("--c1", nargs="*", default=[], help="C1 run directories (globs ok)")
    ap.add_argument("--out", default="runs/analysis")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    md, js = ["# Results\n"], {}
    if a.e0:
        t, j = e0_section(a.e0)
        md.append(t); js["e0"] = j
    dirs = sorted({d for g in a.c1 for d in glob.glob(g) if os.path.isdir(d)})
    if dirs:
        t, j, cells, units = c1_section(dirs)
        md.append(t); js["c1"] = j
        figs = figures(cells, units, a.out)
        if figs:
            md.append("\nFigures: " + ", ".join(os.path.basename(f) for f in figs))
    open(os.path.join(a.out, "results.md"), "w").write("\n\n".join(md) + "\n")
    json.dump(js, open(os.path.join(a.out, "results.json"), "w"), indent=2, default=str)
    print(f"wrote {a.out}/results.md")


if __name__ == "__main__":
    main()
