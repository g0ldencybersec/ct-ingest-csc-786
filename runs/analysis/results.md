# Results


## E0: encoding analysis (917,504 entries, 748,158 x509 / 169,346 precert)

| Rep. | Representation | Total MB | Bytes/entry | vs. A' |
|---|---|---:|---:|---:|
| A | Legacy JSON, uncompressed | 4,956.2 | 5,402 | 3.488 |
| A' | Legacy JSON, gzip as served by the log | 1,421.1 | 1,549 | 1.000 |
| A6 | Legacy JSON, gzip -6 applied locally | 1,421.1 | 1,549 | 1.000 |
| B | Legacy binary (no base64, no JSON) | 3,691.6 | 4,024 | 2.598 |
| B6 | Legacy binary, gzip -6 per 1,000 entries | 967.6 | 1,055 | 0.681 |
| C | Tiles decompressed + each issuer once | 1,376.2 | 1,500 | 0.968 |
| C' | Tiles as served (gzip) + issuers as served | 792.9 | 864 | 0.558 |

| Component | MB | Bytes/entry | Share of A |
|---|---:|---:|---:|
| Base64 overhead (A − B, part) | 1,233.4 | 1,344 | 24.9% |
| JSON framing (A − B, part) | 31.2 | 34 | 0.6% |
| Legacy chain bytes (full DER per entry) | 2,379.3 | 2,593 | 48.0% |
| Tiles chain bytes (fingerprints + issuers once per block) | 65.7 | 72 | 1.3% |
| Leaf/TimestampedEntry bytes, legacy | 1,137.6 | 1,240 | 23.0% |
| TimestampedEntry bytes, tiles | 1,135.8 | 1,238 | 22.9% |

| Factor | Ratio |
|---|---:|
| total_uncompressed_legacy_over_tiles_served (A/C') | 6.25× |
| operational_legacy_gzip_over_tiles_served (A'/C') | 1.79× |
| encoding_effect_uncompressed (A/C) | 3.60× |
| of_which_base64_and_json (A/B) | 1.34× |
| of_which_chain_dedup_and_layout (B/C) | 2.68× |
| compression_effect_legacy (A/A') | 3.49× |
| compression_effect_tiles (C/C') | 1.74× |
| gzip_on_legacy_binary_vs_tiles_served (B6/C') | 1.22× |
| chain_repetition_factor (legacy chain / tiles chain) | 36.19× |

Response encodings actually received: legacy (gzip requested): {'gzip': 924}; tiles: {'gzip': 3584}; tile gzip layers: {'1': 3584}

## C1: repeated trials on identical data

Trials: 120 recorded, 120 analyzed.

Block: 131,072 entries per trial (aligned subtree, root-verified). Concurrency 4. WAN profile: 25ms each way, 50mbit each way. Server: 8 vCPU, cache 1024 MB. Client: 2 vCPU.

### Per condition (median [IQR])

| Net | Cache | Arm | n | Wall s | MB on wire | Requests | Client CPU s | Client RSS MB | Server CPU s | Steal % (server) |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| lan | cold | legacy-gzip | 10 | 12.75 [12.07–13.03] | 182.26 [175.88–184.68] | 132 | 8.22 [7.91–8.88] | 37.32 [36.43–40.30] | 53.87 [52.38–55.95] | 3.2 |
| lan | cold | tiles | 10 | 4.65 [4.24–4.81] | 114.54 [114.54–125.14] | 617 | 2.70 [2.63–2.97] | 17.29 [17.03–17.35] | 16.77 [15.53–17.41] | 2.2 |
| lan | cold | legacy-identity | 10 | 8.55 [8.05–9.40] | 702.82 [701.11–714.70] | 132 | 6.56 [6.47–6.75] | 48.26 [45.31–50.61] | 36.10 [34.54–37.49] | 2.2 |
| lan | warm | legacy-gzip | 10 | 10.78 [9.93–11.93] | 199.82 [175.88–255.94] | 132 | 8.43 [8.07–8.72] | 40.14 [39.17–42.10] | 39.04 [33.50–41.78] | 0.6 |
| lan | warm | tiles | 10 | 3.58 [3.52–3.73] | 109.31 [93.47–123.70] | 634 | 2.62 [2.41–2.78] | 16.93 [16.44–17.30] | 10.42 [8.83–11.86] | 1.1 |
| lan | warm | legacy-identity | 10 | 6.72 [6.56–6.82] | 714.70 [704.51–741.27] | 132 | 6.36 [6.24–6.56] | 49.19 [46.79–50.33] | 18.69 [17.66–20.37] | 0.7 |
| wan | cold | legacy-gzip | 10 | 38.33 [35.37–40.53] | 188.14 [185.49–217.37] | 132 | 12.45 [11.99–13.09] | 44.22 [41.24–45.08] | 53.76 [52.21–55.68] | 0.8 |
| wan | cold | tiles | 10 | 22.87 [22.19–23.23] | 117.41 [112.56–120.94] | 643 | 5.24 [4.99–5.30] | 16.20 [15.89–16.46] | 17.05 [16.26–18.91] | 0.4 |
| wan | cold | legacy-identity | 10 | 123.97 [121.65–125.22] | 702.82 [694.16–708.79] | 132 | 21.43 [21.04–21.95] | 82.33 [79.57–84.78] | 35.99 [34.76–38.17] | 0.4 |
| wan | warm | legacy-gzip | 10 | 32.35 [30.70–36.34] | 182.26 [173.75–210.00] | 132 | 11.98 [11.68–12.77] | 42.08 [39.70–44.10] | 33.62 [33.26–35.30] | 0.3 |
| wan | warm | tiles | 10 | 22.79 [22.13–23.40] | 117.41 [113.06–126.76] | 617 | 5.19 [4.91–5.42] | 16.20 [16.01–16.36] | 9.39 [9.16–10.07] | 0.3 |
| wan | warm | legacy-identity | 10 | 123.65 [120.63–124.84] | 709.62 [701.11–714.70] | 132 | 21.53 [20.86–22.59] | 79.62 [78.36–81.94] | 19.23 [18.01–19.73] | 0.2 |

### Paired ratios (median of per-unit ratios, 95% bootstrap CI, exact Wilcoxon p)

Ratio < 1 means the first arm costs less.

| Comparison | Net | Cache | Metric | n | Median ratio | 95% CI | p |
|---|---|---|---|---:|---:|---|---:|
| tiles / legacy-gzip (primary) | lan | cold | Wall time (s) | 10 | 0.355 | 0.343–0.380 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | cold | Bytes on wire (MB) | 10 | 0.628 | 0.572–0.730 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | cold | Requests | 10 | 4.674 | 4.386–4.871 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | cold | Client CPU (s) | 10 | 0.329 | 0.313–0.353 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | cold | Server CPU (s) | 10 | 0.298 | 0.290–0.332 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | warm | Wall time (s) | 10 | 0.344 | 0.303–0.375 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | warm | Bytes on wire (MB) | 10 | 0.554 | 0.365–0.730 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | warm | Requests | 10 | 4.803 | 4.386–5.311 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | warm | Client CPU (s) | 10 | 0.320 | 0.273–0.351 | 0.00195 |
| tiles / legacy-gzip (primary) | lan | warm | Server CPU (s) | 10 | 0.273 | 0.258–0.296 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | cold | Wall time (s) | 10 | 0.605 | 0.535–0.662 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | cold | Bytes on wire (MB) | 10 | 0.636 | 0.497–0.648 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | cold | Requests | 10 | 4.871 | 4.636–4.928 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | cold | Client CPU (s) | 10 | 0.406 | 0.388–0.444 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | cold | Server CPU (s) | 10 | 0.327 | 0.298–0.341 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | warm | Wall time (s) | 10 | 0.703 | 0.594–0.769 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | warm | Bytes on wire (MB) | 10 | 0.638 | 0.516–0.730 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | warm | Requests | 10 | 4.674 | 4.386–4.898 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | warm | Client CPU (s) | 10 | 0.426 | 0.386–0.465 | 0.00195 |
| tiles / legacy-gzip (primary) | wan | warm | Server CPU (s) | 10 | 0.279 | 0.273–0.290 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | cold | Wall time (s) | 10 | 0.657 | 0.632–0.732 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | cold | Bytes on wire (MB) | 10 | 3.847 | 3.505–4.113 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | cold | Requests | 10 | 1.000 | 1.000–1.000 | 1 |
| legacy-identity / legacy-gzip (diagnostic) | lan | cold | Client CPU (s) | 10 | 0.801 | 0.734–0.832 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | cold | Server CPU (s) | 10 | 0.663 | 0.625–0.693 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | warm | Wall time (s) | 10 | 0.628 | 0.564–0.684 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | warm | Bytes on wire (MB) | 10 | 3.520 | 2.896–4.113 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | warm | Requests | 10 | 1.000 | 1.000–1.000 | 1 |
| legacy-identity / legacy-gzip (diagnostic) | lan | warm | Client CPU (s) | 10 | 0.757 | 0.721–0.793 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | lan | warm | Server CPU (s) | 10 | 0.507 | 0.468–0.541 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | cold | Wall time (s) | 10 | 3.274 | 2.986–3.579 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | cold | Bytes on wire (MB) | 10 | 3.775 | 3.178–3.822 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | cold | Requests | 10 | 1.000 | 1.000–1.000 | 1 |
| legacy-identity / legacy-gzip (diagnostic) | wan | cold | Client CPU (s) | 10 | 1.721 | 1.579–1.786 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | cold | Server CPU (s) | 10 | 0.671 | 0.646–0.719 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | warm | Wall time (s) | 10 | 3.824 | 3.230–4.034 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | warm | Bytes on wire (MB) | 10 | 3.847 | 3.164–4.113 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | warm | Requests | 10 | 1.000 | 1.000–1.000 | 1 |
| legacy-identity / legacy-gzip (diagnostic) | wan | warm | Client CPU (s) | 10 | 1.738 | 1.575–1.939 | 0.00195 |
| legacy-identity / legacy-gzip (diagnostic) | wan | warm | Server CPU (s) | 10 | 0.551 | 0.517–0.575 | 0.00195 |

### Response encoding actually received

| Arm | Accept-Encoding sent | Request kind | Content-Encoding received (count) |
|---|---|---|---|
| legacy-gzip | gzip | get-entries | gzip: 5,280 |
| legacy-identity | identity | get-entries | none: 5,280 |
| tiles | gzip | issuer | gzip: 4,640 |
| tiles | gzip | tile | gzip: 20,480 |


Figures: c1-wall_secs.png, c1-wire_mb.png, c1-client_cpu.png, c1-server_cpu.png, c1-summary.png
