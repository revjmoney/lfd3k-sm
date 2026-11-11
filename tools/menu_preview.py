"""Replay the setup menu's ANSI output on an 80x24 CP437 screen and draw it.

Reads the .ans files written by `cargo run -p lfd3k-sim --example
menu_preview -- <dir>`, replays each whole session through a strict little
terminal (cursor moves, SGR colors, clear, erase line, cursor show and
hide, CR, LF, wrap and scroll; any other escape is an error), checks that
the incremental session, the slow (few bytes per millisecond) session and
a fresh full paint of the same state end on the same screen, and writes a
PNG per snapshot. Box and block characters are drawn as shapes; text uses
Consolas (preview only).

Usage: py -3.14 tools/menu_preview.py <dir>
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

COLS, ROWS = 80, 24
CW, CH = 16, 32  # cell size in the PNG (2x of an 8x16 VGA cell)

VGA = [
    (0, 0, 0), (170, 0, 0), (0, 170, 0), (170, 85, 0),
    (0, 0, 170), (170, 0, 170), (0, 170, 170), (170, 170, 170),
    (85, 85, 85), (255, 85, 85), (85, 255, 85), (255, 255, 85),
    (85, 85, 255), (255, 85, 255), (85, 255, 255), (255, 255, 255),
]


class Term:
    def __init__(self):
        self.cells = [[(0x20, 7, 0)] * COLS for _ in range(ROWS)]
        self.r = self.c = 0
        self.fg, self.bg, self.bold = 7, 0, False
        self.pending_wrap = False
        self.cursor = True

    def attr(self):
        return (self.fg + (8 if self.bold else 0), self.bg)

    def scroll(self):
        self.cells.pop(0)
        self.cells.append([(0x20, 7, self.bg)] * COLS)

    def lf(self):
        if self.r == ROWS - 1:
            self.scroll()
        else:
            self.r += 1

    def put(self, b):
        if self.pending_wrap:
            self.pending_wrap = False
            self.c = 0
            self.lf()
        fg, bg = self.attr()
        self.cells[self.r][self.c] = (b, fg, bg)
        if self.c == COLS - 1:
            self.pending_wrap = True
        else:
            self.c += 1

    def sgr(self, params):
        if not params:
            params = [0]
        for p in params:
            if p == 0:
                self.fg, self.bg, self.bold = 7, 0, False
            elif p == 1:
                self.bold = True
            elif 30 <= p <= 37:
                self.fg = p - 30
            elif 40 <= p <= 47:
                self.bg = p - 40
            else:
                raise ValueError(f"unknown SGR {p}")

    def feed(self, data):
        i = 0
        n = len(data)
        while i < n:
            b = data[i]
            if b == 0x1B:
                if i + 1 >= n or data[i + 1] != ord("["):
                    raise ValueError(f"lone ESC at {i}")
                j = i + 2
                while j < n and (0x30 <= data[j] <= 0x3F):
                    j += 1
                if j >= n:
                    raise ValueError(f"cut CSI at {i}")
                body = data[i + 2:j].decode("ascii")
                final = chr(data[j])
                self.csi(body, final, i)
                i = j + 1
                continue
            if b == 0x0D:
                self.c = 0
                self.pending_wrap = False
            elif b == 0x0A:
                self.pending_wrap = False
                self.lf()
            elif b == 0x08:
                self.c = max(0, self.c - 1)
            elif b < 0x20 or b == 0x7F:
                raise ValueError(f"control byte {b:#x} at {i}")
            else:
                self.put(b)
            i += 1

    def csi(self, body, final, at):
        self.pending_wrap = False
        if body.startswith("?"):
            if body == "?25" and final == "l":
                self.cursor = False
            elif body == "?25" and final == "h":
                self.cursor = True
            else:
                raise ValueError(f"unknown private CSI {body}{final} at {at}")
            return
        params = [int(p) if p else 0 for p in body.split(";")] if body else []
        if final == "H":
            r = (params[0] if len(params) > 0 and params[0] else 1) - 1
            c = (params[1] if len(params) > 1 and params[1] else 1) - 1
            if not (0 <= r < ROWS and 0 <= c < COLS):
                raise ValueError(f"cursor off screen {body}H at {at}")
            self.r, self.c = r, c
        elif final == "m":
            self.sgr(params)
        elif final == "J" and params == [2]:
            self.cells = [[(0x20, 7, self.bg)] * COLS for _ in range(ROWS)]
        elif final == "K" and params in ([], [0]):
            for c in range(self.c, COLS):
                self.cells[self.r][c] = (0x20, 7, self.bg)
        else:
            raise ValueError(f"unknown CSI {body}{final} at {at}")


def replay(path):
    t = Term()
    t.feed(path.read_bytes())
    return t


# Box drawing: (up, down, left, right) with 0 none, 1 single, 2 double.
BOX = {
    0xB3: (1, 1, 0, 0), 0xBA: (2, 2, 0, 0), 0xC4: (0, 0, 1, 1), 0xCD: (0, 0, 2, 2),
    0xDA: (0, 1, 0, 1), 0xBF: (0, 1, 1, 0), 0xC0: (1, 0, 0, 1), 0xD9: (1, 0, 1, 0),
    0xC9: (0, 2, 0, 2), 0xBB: (0, 2, 2, 0), 0xC8: (2, 0, 0, 2), 0xBC: (2, 0, 2, 0),
    0xB5: (1, 1, 2, 0), 0xC6: (1, 1, 0, 2),
}
SHADE = {0xB0: 0.25, 0xB1: 0.5, 0xB2: 0.75, 0xDB: 1.0}


def draw_box(d, x, y, spec, col):
    up, down, left, right = spec
    cx, cy = x + CW // 2, y + CH // 2
    g = 3  # half the gap between double lines
    x1, y1 = x + CW - 1, y + CH - 1

    def v(x0, ya, yb):
        d.rectangle([x0 - 1, min(ya, yb), x0, max(ya, yb)], fill=col)

    def h(y0, xa, xb):
        d.rectangle([min(xa, xb), y0 - 1, max(xa, xb), y0], fill=col)

    doubles = (up == 2) + (down == 2) + (left == 2) + (right == 2)
    if doubles == 2 and up + down + left + right == 4 and not (up and down) and not (left and right):
        sx = 1 if right else -1  # horizontal arm side
        sy = 1 if down else -1  # vertical arm side
        ex = x1 if right else x
        ey = y1 if down else y
        # Outer lines (away from the arms) and inner lines.
        v(cx - sx * g, cy - sy * g, ey)
        v(cx + sx * g, cy + sy * g, ey)
        h(cy - sy * g, cx - sx * g, ex)
        h(cy + sy * g, cx + sx * g, ex)
        return
    for part, ya, yb in ((up, y, cy), (down, cy, y1)):
        if part == 1:
            v(cx, ya, yb)
        elif part == 2:
            v(cx - g, ya, yb)
            v(cx + g, ya, yb)
    for part, xa, xb in ((left, x, cx), (right, cx, x1)):
        if part == 1:
            h(cy, xa, xb)
        elif part == 2:
            h(cy - g, xa, xb)
            h(cy + g, xa, xb)


def render(t, out):
    img = Image.new("RGB", (COLS * CW, ROWS * CH))
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype("C:/Windows/Fonts/consola.ttf", 26)
    for r in range(ROWS):
        for c in range(COLS):
            ch, fg, bg = t.cells[r][c]
            x, y = c * CW, r * CH
            d.rectangle([x, y, x + CW - 1, y + CH - 1], fill=VGA[bg])
            col = VGA[fg]
            if ch in SHADE:
                f = SHADE[ch]
                if f >= 1.0:
                    d.rectangle([x, y, x + CW - 1, y + CH - 1], fill=col)
                else:
                    step = {0.25: 4, 0.5: 2, 0.75: 2}[f]
                    for py in range(y, y + CH, 2):
                        for px in range(x + ((py // 2) % 2 if f == 0.5 else 0), x + CW, step):
                            d.point((px, py), fill=col)
                            if f == 0.75:
                                d.point((px + 1, py + 1), fill=col)
                                d.point((px, py + 1), fill=col)
                continue
            if ch in BOX:
                draw_box(d, x, y, BOX[ch], col)
                continue
            if ch == 0x20:
                continue
            u = bytes([ch]).decode("cp437")
            d.text((x + CW // 2, y + CH // 2), u, font=font, fill=col, anchor="mm")
    img.save(out)


def text(t):
    return "\n".join(
        bytes(cell[0] for cell in row).decode("cp437") for row in t.cells
    )


def main():
    folder = Path(sys.argv[1] if len(sys.argv) > 1 else "docs/phase4-bench/menu")
    bad = 0
    for ans in sorted(folder.glob("[0-9]*.ans")):
        t = replay(ans)
        for kind in ("slow", "fresh"):
            other = ans.with_name(f"{kind}_{ans.name}")
            if not other.exists() or other.stat().st_size == 0:
                continue
            o = replay(other)
            if o.cells != t.cells:
                bad += 1
                print(f"MISMATCH {ans.name} vs {kind}")
                for r in range(ROWS):
                    if o.cells[r] != t.cells[r]:
                        print(f"  row {r + 1}:")
                        print("   ", bytes(x[0] for x in t.cells[r]).decode("cp437"))
                        print("   ", bytes(x[0] for x in o.cells[r]).decode("cp437"))
        png = ans.with_suffix(".png")
        render(t, png)
        print(f"{png.name}: ok")
    if "--text" in sys.argv:
        for ans in sorted(folder.glob("[0-9]*.ans")):
            print(f"== {ans.name}")
            print(text(replay(ans)))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
