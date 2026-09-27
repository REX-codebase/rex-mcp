#!/usr/bin/env python3
"""Diagnostic same-crop pixel deltas for a claimed local visual selection.

Optional developer utility, not a visual-quality or semantic-segmentation gate.
Requires Pillow (`python -m pip install Pillow`). Coordinates are CSS-independent
image pixels in a JSON manifest; the two source frames must be aligned first.
"""
import argparse
import json
from pathlib import Path


def changed_fraction(before, after, box, threshold):
    """Return changed pixels and total pixels for an RGB image crop."""
    x0, y0, x1, y1 = box
    if not (0 <= x0 < x1 <= before.width and 0 <= y0 < y1 <= before.height):
        raise ValueError(f"Invalid/out-of-frame region: {box}")
    a = before.crop(box).convert("RGB").tobytes()
    b = after.crop(box).convert("RGB").tobytes()
    changed = sum(
        max(abs(a[i] - b[i]), abs(a[i + 1] - b[i + 1]), abs(a[i + 2] - b[i + 2])) > threshold
        for i in range(0, len(a), 3)
    )
    return changed, len(a) // 3


def inspect(before, after, manifest, threshold):
    if before.size != after.size:
        raise ValueError(f"Different image dimensions: {before.size} vs {after.size}; align same crops first")
    if not 0 <= threshold <= 255:
        raise ValueError("threshold must be between 0 and 255")
    regions = manifest.get("regions")
    if not isinstance(regions, dict) or not regions:
        raise ValueError("manifest needs a nonempty regions object")
    result = {}
    for name, box in regions.items():
        if not isinstance(box, list) or len(box) != 4 or any(type(v) is not int for v in box):
            raise ValueError(f"Region {name} must be four integer coordinates [left,top,right,bottom]")
        changed, total = changed_fraction(before, after, box, threshold)
        result[name] = {"changed_pixels": changed, "total_pixels": total,
                        "changed_fraction": round(changed / total, 6)}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("manifest", type=Path,
                        help='JSON with {"regions":{"subject":[x0,y0,x1,y1],"stable_anchor":[...]}}')
    parser.add_argument("--threshold", type=int, default=20,
                        help="largest per-channel RGB delta treated as unchanged (default 20)")
    args = parser.parse_args()
    try:
        from PIL import Image
    except ImportError as exc:
        parser.error("Pillow required for this optional diagnostic: python -m pip install Pillow")
    try:
        with Image.open(args.before) as before, Image.open(args.after) as after:
            manifest = json.loads(args.manifest.read_text())
            result = inspect(before, after, manifest, args.threshold)
            print(json.dumps({"before": str(args.before), "after": str(args.after),
                              "size": list(before.size), "threshold": args.threshold,
                              "regions": result,
                              "note": "Pixel-change diagnostic only: camera drift, light, compression and motion can alter stable regions; changed pixels do not prove a local material change or design quality."}, indent=2))
    except (ValueError, OSError, json.JSONDecodeError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
