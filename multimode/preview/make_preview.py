"""Turns preview.exe output into multimode/preview/logo.gif, menu.png and a
logo contact sheet. OLED look: white pixels on black, 4x scale with a thin
pixel gap. Run:  py -3.14 multimode/preview/make_preview.py logo.raw menu.raw
"""
import os
import sys

from PIL import Image

W, H, S = 128, 64, 4
ON = (220, 235, 255)
OFF = (6, 8, 12)


def frames(path):
    data = open(path, "rb").read()
    return [data[i:i + 1024] for i in range(0, len(data), 1024)]


def to_img(fb, scale=S):
    img = Image.new("RGB", (W * scale, H * scale), OFF)
    pix = img.load()
    for page in range(8):
        for x in range(W):
            b = fb[page * W + x]
            for bit in range(8):
                if b >> bit & 1:
                    y = page * 8 + bit
                    for dy in range(scale - 1):
                        for dx in range(scale - 1):
                            pix[x * scale + dx, y * scale + dy] = ON
    return img


def sheet(imgs, cols, pad=8):
    w, h = imgs[0].size
    rows = (len(imgs) + cols - 1) // cols
    out = Image.new("RGB", (cols * (w + pad) + pad, rows * (h + pad) + pad), (40, 40, 48))
    for i, im in enumerate(imgs):
        out.paste(im, (pad + (i % cols) * (w + pad), pad + (i // cols) * (h + pad)))
    return out


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    logo = frames(sys.argv[1])
    menu = frames(sys.argv[2])
    imgs = [to_img(f) for f in logo]
    imgs[0].save(os.path.join(here, "logo.gif"), save_all=True, append_images=imgs[1:],
                 duration=40, loop=0)
    picks = [0, 3, 8, 15, 22, 30, 38, 45, 50, 55, 62, 70, 80, 95, 110, 125]
    sheet([imgs[i] for i in picks if i < len(imgs)], 4).save(os.path.join(here, "logo_sheet.png"))
    sheet([to_img(f) for f in menu], 2).save(os.path.join(here, "menu.png"))
    print("logo frames", len(logo), "menu frames", len(menu))


if __name__ == "__main__":
    main()
