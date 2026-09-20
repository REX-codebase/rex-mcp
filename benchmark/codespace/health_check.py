#!/usr/bin/env python3
"""One-shot Gemini health check: exactly ONE official generateContent call.

Reads the key from the same FileSecretStore the REX provider uses
($XDG_CONFIG_HOME/rex-harness/secrets.json or ~/.config/rex-harness/secrets.json,
provider "gemini") and calls the same endpoint/header shape as
crates/rex-providers/src/live.rs, with a fixed tiny prompt. NOT a benchmark
task: label health-check/non-scoring. No retries. The key is never printed
or written; outputs are scrubbed defensively anyway.
"""
import json, os, sys, time, urllib.request, urllib.error

MODEL = "gemini-3.5-flash-lite"
PROMPT = "Reply exactly OK"
LABEL = "health-check/non-scoring"

def store_path():
    xdg = os.environ.get("XDG_CONFIG_HOME")
    base = os.path.join(xdg, "rex-harness") if xdg else os.path.join(os.path.expanduser("~"), ".config", "rex-harness")
    return os.path.join(base, "secrets.json")

def main():
    rec = {
        "label": LABEL,
        "model": MODEL,
        "prompt": PROMPT,
        "ts_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "store": store_path(),
        "attempts": 1,
    }
    try:
        with open(store_path()) as fh:
            key = json.load(fh).get("gemini")
    except Exception as e:
        rec["result"] = "store_error"
        rec["detail"] = str(e)
        return emit(rec, None)
    if not key:
        rec["result"] = "no_key"
        return emit(rec, None)
    body = json.dumps({
        "contents": [{"role": "user", "parts": [{"text": PROMPT}]}],
        "generationConfig": {"maxOutputTokens": 8, "temperature": 0},
    }).encode()
    req = urllib.request.Request(
        f"https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent",
        data=body,
        headers={"x-goog-api-key": key, "content-type": "application/json"},
        method="POST",
    )
    status, text = None, ""
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            status, text = resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        status, text = e.code, e.read().decode("utf-8", "replace")
    except Exception as e:
        rec["result"] = "transport_error"
        rec["detail"] = f"{type(e).__name__}: {e}"
        return emit(rec, key)
    rec["http_status"] = status
    try:
        payload = json.loads(text)
    except Exception:
        payload = None
    if payload and 200 <= status < 300:
        rec["result"] = "ok"
        cand = (payload.get("candidates") or [{}])[0]
        parts = ((cand.get("content") or {}).get("parts")) or []
        rec["reply_text"] = "".join(p.get("text", "") for p in parts)[:80]
        rec["usage"] = payload.get("usageMetadata")
        rec["finish_reason"] = cand.get("finishReason")
    else:
        rec["result"] = "provider_error"
        err = (payload or {}).get("error") or {}
        rec["google_error"] = {k: err.get(k) for k in ("code", "status", "message")}
        for d in err.get("details") or []:
            t = d.get("@type", "")
            if t.endswith("QuotaFailure"):
                rec["quota_violations"] = d.get("violations")
            elif t.endswith("RetryInfo"):
                rec["retry_info"] = d.get("retryDelay")
            elif t.endswith("ErrorInfo"):
                rec["error_info"] = {k: d.get(k) for k in ("reason", "domain")}
    return emit(rec, key)

def emit(rec, key):
    out = json.dumps(rec, indent=1, sort_keys=True)
    if key:
        assert key not in out, "refusing to write output containing the key"
        out = out.replace(key, "***")
    dest = os.path.join(os.path.expanduser("~"), "bench", "results", "health-check-20260920.json")
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    with open(dest, "w") as fh:
        fh.write(out + "\n")
    # stdout gets only non-sensitive fields
    print(json.dumps({k: rec.get(k) for k in ("label", "model", "ts_utc", "result", "http_status", "reply_text", "usage", "google_error", "quota_violations", "retry_info", "error_info")}, sort_keys=True))

if __name__ == "__main__":
    sys.exit(main() or 0)
