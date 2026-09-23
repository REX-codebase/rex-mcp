#!/usr/bin/env python3
"""Verify a REX run receipt's Ed25519 certificate with no REX installed.

Air-gap story: copy this one file (plus the receipt) to any machine with
Python 3.8+ and run it. No dependencies, no network, no REX.

    python3 verify_receipt.py RECEIPT.json [--public-key BASE64]

It recomputes the canonical JSON of the receipt (minus `certificate`),
checks the recorded SHA-256, and verifies the Ed25519 signature against
the signer's public key embedded in the certificate.

Exit 0: VALID. Exit 1: INVALID (with a reason). Exit 2: usage error.
"""

import argparse
import base64
import hashlib
import json
import sys

ALG = "Ed25519"

# ---------------------------------------------------------------------------
# Ed25519 verification (RFC 8032), verify-only, public-domain construction.
# ---------------------------------------------------------------------------

_Q = (1 << 255) - 19
_L = (1 << 252) + 27742317777372353535851937790883648493
_D = (-121665 * pow(121666, _Q - 2, _Q)) % _Q
_I = pow(2, (_Q - 1) // 4, _Q)


def _xrecover(y):
    xx = (y * y - 1) * pow(_D * y * y + 1, _Q - 2, _Q) % _Q
    x = pow(xx, (_Q + 3) // 8, _Q)
    if (x * x - xx) % _Q != 0:
        x = (x * _I) % _Q
    if x & 1:
        x = _Q - x
    return x


def _edwards(p, q):
    # Twisted Edwards, a = -1: y3 uses (y1*y2 + x1*x2), not minus.
    (x1, y1), (x2, y2) = p, q
    d = _D * x1 * x2 * y1 * y2
    x3 = (x1 * y2 + x2 * y1) * pow(1 + d, _Q - 2, _Q)
    y3 = (y1 * y2 + x1 * x2) * pow(1 - d, _Q - 2, _Q)
    return (x3 % _Q, y3 % _Q)


_IDENT = (0, 1)
_GY = 4 * pow(5, _Q - 2, _Q) % _Q
_G = (_xrecover(_GY), _GY)


def _scalarmult(p, e):
    r = _IDENT
    while e:
        if e & 1:
            r = _edwards(r, p)
        p = _edwards(p, p)
        e >>= 1
    return r


def _encodepoint(p):
    x, y = p
    return ((y & ((1 << 255) - 1)) | ((x & 1) << 255)).to_bytes(32, "little")


def _decodepoint(s):
    if len(s) != 32:
        raise ValueError("bad point length")
    y = int.from_bytes(s, "little") & ((1 << 255) - 1)
    x = _xrecover(y)
    if (x & 1) != (s[31] >> 7):
        x = _Q - x
    if (-x * x + y * y - 1 - _D * x * x * y * y) % _Q != 0:
        raise ValueError("point not on curve")
    return (x, y)


def ed25519_verify(public_key: bytes, message: bytes, signature: bytes) -> bool:
    """True iff signature is a valid Ed25519 signature of message."""
    try:
        if len(public_key) != 32 or len(signature) != 64:
            return False
        s = int.from_bytes(signature[32:], "little")
        if s >= _L:
            return False
        a = _decodepoint(public_key)
        rs = signature[:32]
        r = _decodepoint(rs)
        h = int.from_bytes(hashlib.sha512(rs + public_key + message).digest(), "little") % _L
        lhs = _scalarmult(_G, s)
        rhs = _edwards(r, _scalarmult(a, h))
        return _encodepoint(lhs) == _encodepoint(rhs)
    except ValueError:
        return False


# ---------------------------------------------------------------------------
# Canonical JSON: recursively key-sorted, compact, matching serde_json.
# ---------------------------------------------------------------------------


def _fmt_float(f: float) -> str:
    if f != f or f in (float("inf"), float("-inf")):
        raise ValueError("non-finite floats cannot be canonicalized")
    s = repr(f)
    if "e" in s:
        mant, exp = s.split("e")
        neg = exp.startswith("-")
        exp = exp.lstrip("+-").lstrip("0") or "0"
        s = "{0}e{1}{2}".format(mant, "-" if neg else "", exp)
    return s


def _canon(obj) -> str:
    if obj is None:
        return "null"
    if obj is True:
        return "true"
    if obj is False:
        return "false"
    if isinstance(obj, int):
        return str(obj)
    if isinstance(obj, float):
        return _fmt_float(obj)
    if isinstance(obj, str):
        return json.dumps(obj, ensure_ascii=False)
    if isinstance(obj, (list, tuple)):
        return "[" + ",".join(_canon(x) for x in obj) + "]"
    if isinstance(obj, dict):
        items = sorted(obj.items(), key=lambda kv: kv[0].encode("utf-8"))
        return (
            "{"
            + ",".join(json.dumps(k, ensure_ascii=False) + ":" + _canon(v) for k, v in items)
            + "}"
        )
    raise TypeError("not JSON-serializable: {0}".format(type(obj)))


def canonical_bytes(obj) -> bytes:
    return _canon(obj).encode("utf-8")


# ---------------------------------------------------------------------------
# Verifier
# ---------------------------------------------------------------------------


class Invalid(Exception):
    pass


def verify_receipt(path: str, pin=None) -> dict:
    with open(path, "r", encoding="utf-8") as f:
        receipt = json.load(f)
    if not isinstance(receipt, dict):
        raise Invalid("receipt is not a JSON object")
    cert = receipt.get("certificate")
    if not isinstance(cert, dict):
        raise Invalid("no certificate: receipt is unsigned")

    if cert.get("alg") != ALG:
        raise Invalid("unsupported algorithm: {0!r}".format(cert.get("alg")))

    try:
        public_key = base64.b64decode(cert["public_key"], validate=True)
        signature = base64.b64decode(cert["signature"], validate=True)
    except Exception:
        raise Invalid("certificate keys are not valid base64")

    body = {k: v for k, v in receipt.items() if k != "certificate"}
    msg = canonical_bytes(body)

    want_sha = cert.get("sha256", "")
    got_sha = hashlib.sha256(msg).hexdigest()
    if want_sha != got_sha:
        raise Invalid("digest mismatch: receipt was modified after signing")

    if pin and pin != cert.get("public_key"):
        raise Invalid("signer public key does not match --public-key pin")

    if not ed25519_verify(public_key, msg, signature):
        raise Invalid("signature invalid")

    return {
        "run_id": receipt.get("run_id"),
        "status": receipt.get("status"),
        "public_key": cert.get("public_key"),
    }


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Verify a REX run receipt offline.")
    ap.add_argument("receipt", help="path to the receipt JSON")
    ap.add_argument("--public-key", default=None, help="pin the expected signer (base64)")
    args = ap.parse_args(argv)

    try:
        info = verify_receipt(args.receipt, args.public_key)
    except Invalid as e:
        print("INVALID: {0}".format(e))
        return 1
    except FileNotFoundError:
        print("error: no such file: {0}".format(args.receipt), file=sys.stderr)
        return 2
    except (json.JSONDecodeError, ValueError, TypeError) as e:
        print("INVALID: malformed receipt ({0})".format(e))
        return 1

    print("VALID: run {0} ({1})".format(info["run_id"], info["status"]))
    print("signer: {0}".format(info["public_key"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
