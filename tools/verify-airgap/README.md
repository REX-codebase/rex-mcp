# Air-gapped receipt verification

`verify_receipt.py` checks a REX run receipt's Ed25519 certificate on a
machine with **no REX installed, no dependencies, and no network**.
Copy this one file plus the receipt anywhere Python 3.8+ exists:

```sh
python3 verify_receipt.py RECEIPT.json [--public-key BASE64]
```

- Exit `0`: `VALID` — the receipt bytes are exactly what the signer signed.
- Exit `1`: `INVALID` — with a reason (tampered, unsigned, bad signature,
  wrong pinned key).
- Exit `2`: usage error.

## What it checks

1. The receipt carries a `certificate` with `alg: "Ed25519"`.
2. The canonical JSON of the receipt (minus `certificate`) — recursively
   key-sorted, compact, matching `serde_json` — hashes to the recorded
   `sha256`. Any post-signing edit fails here.
3. The Ed25519 signature verifies against the certificate's `public_key`
   (pure-Python RFC 8032 implementation, stdlib only).
4. If `--public-key` is given, the signer must match the pin.

## Trust notes

- A VALID receipt proves the holder of that machine key signed those exact
  bytes. It does not prove the claims inside the receipt are true — only
  that they haven't changed since signing.
- The Ed25519 implementation here was cross-checked against OpenSSL
  signatures and the group-order identity `[L]G = 1`, and the canonicalizer
  was tested against receipts signed by the real Rust signer (including
  unicode strings, nested objects, and float edge cases like `1e-7`).
- Float formatting follows `serde_json`/ryu shortest-repr rules. Receipts
  are overwhelmingly strings, ints, and small floats, which round-trip
  exactly.
