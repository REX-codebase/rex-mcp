# REX-search 0.2 architecture

REX-search is a native Rust evidence layer for agents. It is an honest, bounded crawler and operator-controlled local index, not a web-scale search engine.

## Live retrieval contract

An agent supplies public seed URLs, a query, page/result budgets, and a subdomain policy. REX-search can discover same-scope URLs from ordinary links, robots-declared or conventional sitemaps, and RSS/Atom links declared by fetched HTML. Every candidate still goes through the same crawl budget, destination safety, scope, robots, rate-limit, and body-size controls before it becomes evidence.

Each result records the requested URL, final URL, full redirect chain, title, relevant excerpt, bounded content, retrieval time, HTTP status, content type, discovery kind and parent, robots decision, score, and exact failure state. The response reports attempts, denials, redirects, sitemap/feed discoveries, budget truncation, and its partial-coverage warning.

## Redirect and network safety

- Automatic HTTP redirects are disabled in the client. REX-search handles at most five hops itself.
- Before every redirect hop, it rechecks HTTP(S), the operator-approved host/subdomain scope, that hop's origin-specific robots policy, and the public destination constraint. Out-of-scope, private, malformed, missing-location, over-limit, robots-denied, and HTTP failures remain separate states.
- The custom resolver returns only the public IP addresses it validated directly to the connector. There is no second DNS lookup between policy validation and connect. TLS still verifies the original hostname.
- Loopback, private, link-local, `.local`, and `.internal` targets are blocked. A named user-agent is sent. Requests time out after 15 seconds, bodies are capped at 2 MiB, crawl budget is capped at 64 pages, discovery documents and extracted URLs are separately capped, and each origin is delayed at least 750 ms.
- A 5xx, timeout, or unreachable robots file fails closed. Rules support named/wildcard groups, merged equally specific groups, `*`, terminal `$`, query matching, longest specificity, and Allow on ties.

## Discovery boundary

Sitemap and feed ingestion improve discovery only inside sites the operator already selected. They do not discover the whole web. Sitemap indexes are parsed as bounded URL lists rather than recursively expanded without limit. Feeds are fetched only when an in-scope HTML page declares RSS/Atom. REX-search does not execute JavaScript, submit forms, log in, bypass paywalls, solve challenges, scrape a human search engine, or fetch private networks.

## Operator-controlled local index

`LocalIndex` stores only documents explicitly added by the operator or agent through the desktop commands:

- `search_index_upsert`: add or replace an identified document with URL, content, timestamp, and provenance.
- `search_index_query`: rank and excerpt only those local documents.
- `search_index_remove`: immediately remove one document.

The index is a private JSON file under the app configuration directory, written through a temporary file and rename. There is no background crawling, automatic retention, hidden global corpus, or claim of complete coverage. The operator owns source selection, refresh, and deletion.

## Verification

- `cargo test -p rex-search --locked`
- `cargo test -p rex-providers --all-targets --locked`
- `cargo metadata --locked`
- `scripts/tauri-linux-env.sh cargo check --workspace --all-targets --locked`
- `npm run build`

Tests cover robots behavior, private targets, scope, safe text extraction, ranking, sitemap/feed URL extraction, query-aware robots matching, and local-index upsert/query/deletion. Redirect integration is additionally exercised by the typed full workspace check. A production deployment should add a local adversarial HTTP fixture to exercise each hop and DNS outcome end to end.

## Standards and sources (checked 2026-09-19)

- Robots Exclusion Protocol, RFC 9309: https://www.rfc-editor.org/rfc/rfc9309.html
- HTTP semantics, RFC 9110: https://datatracker.ietf.org/doc/html/rfc9110
- Sitemap protocol: https://www.sitemaps.org/protocol.html
- RSS 2.0: https://www.rssboard.org/rss-specification
- Atom, RFC 4287: https://www.rfc-editor.org/rfc/rfc4287.html
