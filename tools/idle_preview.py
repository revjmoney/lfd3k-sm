#!/usr/bin/env python3
"""Idle show preview: the GIF and the item contact sheet from panel_check --dump.

  py -3.14 tools/idle_preview.py <build>/idle <mockups dir>

Reads items.txt and show.txt ("label TAB FB:<2048 hex>" per line, written by
tools/panel_check) and writes idle-show.gif (one frame per 100 ms, with the transition
or item name printed small under the panel: preview only, never on the device) and
idle-items.png (every item). Needs Pillow.
"""
import os
import sys

from PIL import Image, ImageDraw

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fb2png import BG, FG, parse_fb  # noqa: E402

W, H = 128, 64
LABEL_FG = (0xC8, 0xA0, 0x30)


def fb_image(fb, scale, y0=0, y1=H):
    im = Image.new("RGB", (W, y1 - y0), BG)
    px = im.load()
    for y in range(y0, y1):
        page, bit = y >> 3, y & 7
        for x in range(W):
            if (fb[page * W + x] >> bit) & 1:
                px[x, y - y0] = FG
    return im.resize((W * scale, (y1 - y0) * scale), Image.NEAREST)


def read(path):
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            label, _, rest = line.rstrip("\n").partition("\t")
            out.append((label, parse_fb(rest)))
    return out


def main(argv):
    if len(argv) != 3:
        print(__doc__)
        return 2
    src, dst = argv[1], argv[2]
    os.makedirs(dst, exist_ok=True)

    # contact sheet: the band (y 10..31) of every item, 3x, name on the left
    items = read(os.path.join(src, "items.txt"))
    s, cols = 3, 3
    cw, ch = W * s + 110, 22 * s + 8
    rows = (len(items) + cols - 1) // cols
    sheet = Image.new("RGB", (cw * cols, ch * rows), (0, 0, 0))
    d = ImageDraw.Draw(sheet)
    for i, (label, fb) in enumerate(items):
        x, y = (i % cols) * cw, (i // cols) * ch
        d.text((x + 6, y + ch // 2 - 6), label, fill=LABEL_FG)
        sheet.paste(fb_image(fb, s, 10, 32), (x + 104, y + 4))
    out = os.path.join(dst, "idle-items.png")
    sheet.save(out, optimize=True)
    print("wrote", out)

    # the show: 2x panel plus a small caption strip with the effect name
    frames = []
    for label, fb in read(os.path.join(src, "show.txt")):
        im = Image.new("RGB", (W * 2, H * 2 + 14), (0, 0, 0))
        im.paste(fb_image(fb, 2), (0, 0))
        ImageDraw.Draw(im).text((3, H * 2 + 2), label, fill=LABEL_FG)
        frames.append(im.convert("P", palette=Image.Palette.ADAPTIVE, colors=8))
    out = os.path.join(dst, "idle-show.gif")
    frames[0].save(out, save_all=True, append_images=frames[1:], duration=100, loop=0, optimize=True)
    print("wrote", out, len(frames), "frames")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
