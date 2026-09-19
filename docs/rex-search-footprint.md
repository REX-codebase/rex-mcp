# REX-search resource footprint

Measured 2026-09-19 on commit `9e2210d3659a90e70c34ff2ec3e3e89f78638cca`, Linux x86_64, Rust 1.98.1, release profile. This isolates `rex-search`; Tauri and the OS webview are not started.

## Representative harness

A minimal release executable constructed the real `SearchEngine`, or ran it against a loopback HTTP fixture through the same fetch, robots, extraction, ranking, and bounded-frontier paths. Loopback was enabled only in the benchmark build, matching the crate's existing `cfg(test)` fixture approach; production SSRF policy was not changed. Each HTML page contained roughly 8 KiB of deterministic text. The per-origin delay was set to zero so CPU and engine latency were measured without the intentional 750 ms politeness wait. Five runs were taken per search case.

## Results

| Case | Result |
| --- | ---: |
| Minimal representative binary, stripped | 3,003,656 bytes (2.86 MiB) |
| Empty Rust executable, stripped | 349,968 bytes (0.33 MiB) |
| Approximate incremental engine/dependency contribution | 2,653,688 bytes (2.53 MiB) |
| Idle RSS after constructing `SearchEngine` | 1,220 KiB |
| 1 page, median engine-reported latency | 2.61 ms |
| 1 page, median process wall time | 5.16 ms |
| 1 page, median CPU time / average CPU | 3.65 ms / 70.0% of one core |
| 1 page, median peak RSS | 5,288 KiB |
| 12 pages, median engine-reported latency | 15.03 ms |
| 12 pages, median process wall time | 17.69 ms |
| 12 pages, median CPU time / average CPU | 7.89 ms / 43.6% of one core |
| 12 pages, median peak RSS | 5,292 KiB |

Real public-web latency will be dominated by DNS, TLS, server response, payload size, and the configured 750 ms per-origin politeness delay. These local-fixture latency numbers are an engine floor, not a promise for live searches.

## Local-index disk growth

The JSON index was populated with deterministic documents containing 10,000 content characters each.

| Documents | File size |
| --- | ---: |
| 1 | 10,188 bytes |
| 10 | 101,862 bytes |
| 100 | 1,018,872 bytes |

That is approximately 10.19 KB per 10,000-character document, including URL, title, ID, timestamp, provenance, JSON quoting, and formatting. The current index rewrites the complete JSON file on each upsert; the 100-document sequential upsert plus query took about 253 ms locally. This is appropriate for a small operator-controlled index, not a large corpus.

## Boundary

The engine itself is lightweight by desktop-app standards: low-single-digit MiB executable contribution and roughly 5.2 MiB peak RSS in these bounded runs. This does not describe the complete REX desktop process. Tauri, the system webview, frontend assets, model/provider clients, and OS allocator/platform differences must be measured separately in a packaged app.
