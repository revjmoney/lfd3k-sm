#!/usr/bin/env python3
"""Turn the *.fb.txt files written by panel_check --dump into PNG mockups.

  python panel_mockups.py <dir with .fb.txt> <output dir>

Uses fb2png.parse_fb and fb2png.fb_to_png (the same PNG writer as the bench tools).
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fb2png import fb_to_png, parse_fb  # noqa: E402


def main(argv):
    if len(argv) != 3:
        print(__doc__)
        return 2
    src, dst = argv[1], argv[2]
    os.makedirs(dst, exist_ok=True)
    names = sorted(n for n in os.listdir(src) if n.endswith(".fb.txt"))
    if not names:
        print("error: no .fb.txt files in", src, file=sys.stderr)
        return 1
    for n in names:
        with open(os.path.join(src, n), encoding="ascii") as f:
            png = fb_to_png(parse_fb(f.read()))
        out = os.path.join(dst, n[: -len(".fb.txt")] + ".png")
        with open(out, "wb") as f:
            f.write(png)
        print("wrote", out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
