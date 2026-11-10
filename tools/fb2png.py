#!/usr/bin/env python3
"""Convert an AT+FB reply to a PNG (128x64 OLED, 4x scale, cyan on near-black).

The reply is the line "FB:" followed by 2048 uppercase hex digits (1024 bytes,
SSD1306 page order: 8 pages x 128 columns, bit 0 = top row of the page).
Standard library only.

  python fb2png.py saved.txt out.png      (file holding the FB: line)
  python fb2png.py - out.png              (read stdin)
"""
import re
import struct
import sys
import zlib

W, H, SCALE = 128, 64, 4
FG = (0x40, 0xE8, 0xFF)
BG = (0x05, 0x0A, 0x12)


def parse_fb(text):
    m = re.search(r"FB:\s*([0-9A-Fa-f]{2048})", text)
    if not m:
        raise ValueError("no FB: line with 2048 hex digits found")
    return bytes.fromhex(m.group(1))


def fb_to_png(fb):
    if len(fb) != 1024:
        raise ValueError("framebuffer must be 1024 bytes")
    fg, bg = bytes(FG), bytes(BG)
    rows = []
    for y in range(H):
        page, bit = y >> 3, y & 7
        line = bytearray()
        for x in range(W):
            on = (fb[page * W + x] >> bit) & 1
            line += (fg if on else bg) * SCALE
        row = b"\x00" + bytes(line)
        rows.extend([row] * SCALE)
    raw = b"".join(rows)

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", W * SCALE, H * SCALE, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main(argv):
    if len(argv) != 3:
        print(__doc__)
        return 2
    if argv[1] == "-":
        src = sys.stdin.read()
    else:
        with open(argv[1], encoding="ascii", errors="replace") as f:
            src = f.read()
    try:
        png = fb_to_png(parse_fb(src))
    except ValueError as e:
        print("error:", e, file=sys.stderr)
        return 1
    with open(argv[2], "wb") as f:
        f.write(png)
    print("wrote", argv[2])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
