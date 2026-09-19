# REX-search 0.1 architecture

REX-search is a native Rust live-evidence layer for agents. It is separate from the browser and uses no commercial search API. Version 0.1 is an honest **bounded seed crawler**, not a web-scale search engine: an agent supplies public seed URLs from task context, user input, feeds, or a future local index; REX-search discovers allowed same-site links, fetches current pages, extracts readable text, ranks it, and returns structured evidence.

## Contract

Input: query, explicit seed URLs, page/result budgets, and whether subdomains are in scope.

Output per page:
- requested and final URL
- title, relevant excerpt, bounded extracted content
- retrieval timestamp, HTTP status, content type
- discovery kind and parent URL
- robots decision
- relevance score
- exact fetch state and non-secret error

The response also states its coverage model, attempts, denials, budget truncation, and the warning that this is not complete-web coverage. Failures remain first-class evidence instead of disappearing.

## Safety and protocol policy

- HTTP(S) only; loopback, link-local, private, `.local`, and `.internal` targets are blocked before requests and after redirects.
- `robots.txt` is checked per origin. A 5xx, timeout, or unreachable robots file fails closed for the current run. Rules use longest-match precedence for the `REX-search` and `*` groups.
- A named user-agent is sent, bodies are capped at 2 MiB, redirects at 5, requests at 15 seconds, crawl budget at 64 pages, and each origin is delayed at least 750 ms.
- Only text/HTML is extracted. Scripts, styles, and noscript blocks are discarded. REX-search does not execute JavaScript, submit forms, log in, bypass paywalls, solve challenges, or fetch private networks.
- Rate limits and HTTP/network/content failures have separate states. The engine never labels a partial crawl as the whole web.
- Stored indexing is intentionally absent in 0.1. Any later index needs retention, deletion, recrawl, canonicalization, copyright, privacy, and operator-control policy before it ships.

## Discovery boundaries

Direct crawling is good at fresh retrieval within known sites and bad at global discovery. Scraping a human search engine would be fragile and against the goal. Querying a public index or self-hosted metasearch can be added later only as an explicitly named discovery source; it must not be presented as native coverage. A responsible path is:

1. 0.1: explicit seeds + bounded same-site discovery (implemented).
2. 0.2: sitemap and RSS/Atom seed ingestion, with the same robots and fetch policy.
3. 0.3: optional local index over operator-selected sources, with recrawl and deletion controls.
4. Optional public-index adapter (for example Common Crawl), labeled as third-party discovery and followed by a live origin fetch before evidence is returned.

## Standards and sources (checked 2026-09-19)

- Robots Exclusion Protocol, RFC 9309: https://www.rfc-editor.org/rfc/rfc9309.html
- HTTP semantics including 429 and Retry-After, RFC 9110: https://datatracker.ietf.org/doc/html/rfc9110
- HTTP caching, RFC 9111: https://www.rfc-editor.org/rfc/rfc9111.html
- Sitemap protocol: https://www.sitemaps.org/protocol.html
- Robots meta and `X-Robots-Tag`: https://developers.google.com/search/docs/crawling-indexing/robots-meta-tag
- W3C Ethical Web Principles: https://www.w3.org/TR/2024/STMT-ethical-web-principles-20241212/
- Common Crawl public index (future optional discovery, not used by 0.1): https://commoncrawl.org/get-started

## Verification

`cargo test -p rex-search`

Tests cover robots precedence, named-agent matching, private-target rejection, origin/subdomain scope, safe text extraction, and query ranking. The current build environment used for this increment has no Rust toolchain, so tests are committed but not falsely reported as executed. Cargo syntax and dependency integration still need CI or a Rust-equipped machine to confirm.
