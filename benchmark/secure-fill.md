# Secure key fill surface - threat model and evidence log (2026-09-20)

Purpose: deliver Agrim's authorized Gemini key from the Instinct vault into
REX's FileSecretStore inside the benchmark execution environment, with zero
disclosure, zero relay, zero spend.

## Route

GitHub Codespaces on Agrim's personal account (owner of the private repo
REX-codebase/rex-harness). A new dedicated 2-core codespace runs:
- rex-key-fill (127.0.0.1:8788): one-shot fill server (our code,
  crates/rex-providers/src/bin/rex-key-fill.rs). One-time CSRF token bound
  to the served form; masked password field; Cache-Control no-store; no
  request/body/access logging; writes FileSecretStore dir 0700 / file 0600;
  process exits after one successful fill (route gone).
- rex-dev-server (127.0.0.1:8787, --store-file same dir): readback
  verification via GET /api/providers has_key (never returns the value).

Both ports are forwarded by Codespaces with PRIVATE visibility: the
https://<codespace>-PORT.app.github.dev URLs require GitHub authentication
as Agrim's account. No tunnel, no public exposure, no third party.

## Trust boundaries (explicit)

1. GitHub edge proxy terminates TLS for *.app.github.dev; plaintext hops
   inside GitHub's fabric to the codespace. Agrim already trusts GitHub
   with the entire private codebase; this extends that trust to one
   transient secret in transit. No persistent logging at the edge is
   configured by us or visible to us.
2. The Instinct vault fills only into the browser field; the value passes
   browser -> GitHub edge -> codespace process -> 0600 file. It is never
   returned by any endpoint, never printed, never in the repo, Drive,
   chat, or shell history (the fill is a form POST; no command line
   carries it).
3. The codespace VM is Agrim's own, private, covered by $0 stop-usage
   budgets (evidence below) - it cannot charge.

## $0 evidence (live reads, 2026-09-20 ~08:55 IST)

- github.com/settings/billing: net $0.00/month; all metered usage
  discounted by included usage.
- github.com/settings/billing/budgets: 5 account budgets, including
  Product "Codespaces" - Stop usage: Yes, $0 budget, $0 spent
  (screenshot in task transcript). If included quota is exhausted, usage
  STOPS; no charge path exists.
- Pre-existing codespace "refactored space fishstick" (uncommitted
  changes, predates task): NOT touched.

## Canary protocol (must pass before the real fill)

Canary value "CANARY-*" through the full browser path, then verify:
- application logs (rex-key-fill.log, rex-dev-server.log): no canary
- page source/DOM/AX reads after submit: no canary
- process args / env dumps (ps, /proc): no canary
- shell history: no canary (fill is a form POST, never typed in a shell)
- repo, artifacts, Drive: no canary
- store perms 0600/dir 0700; second POST refused (server exited)

## Evidence log

- [x] Local sandbox canary (2026-09-20 08:57 IST): all checks pass
  (token 32 hex chars, one-shot exit, 0600/0700, zero log hits, 403 on
  bad token, server alive for retry).
- [x] Codespace canary through cloud browser (2026-09-20 09:39 IST): CANARY-KEY-12345 via private forwarded-port form; stored 0600, server self-exited; bench-verify-canary: 0 hits in app logs/shell history/process args/env/repo/home artifacts; store cleaned after
- [x] Real vault fill + has_key=true readback (2026-09-20 09:44 IST): entry "Gemini API key (AI Studio, Zapia project)" login/password via one-shot masked form; response {"ok":true,"stored":true,"perms":"0600","server":"exiting"}; /api/providers has_key=true for gemini only; no value echo anywhere
- [x] Fill route removed (process exit verified): one-shot server self-terminated after real fill ("fill complete; exiting (route closed)" in log); bench-stop-key-fill confirmed KEY-FILL-STOPPED; /health dead; unauthenticated external access 302 to GitHub login (Private visibility)
