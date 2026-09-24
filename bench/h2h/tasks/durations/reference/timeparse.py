import re

_RE = re.compile(r"(?:(\d+)h)?(?:(\d+)m)?(?:(\d+)s)?")


def parse_duration(text):
    t = text.strip()
    m = _RE.fullmatch(t)
    if not t or not m:
        raise ValueError(text)
    h, mi, s = (int(g or 0) for g in m.groups())
    return h * 3600 + mi * 60 + s
