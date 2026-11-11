#!/usr/bin/env python3
"""Layout checker for the LFD3K-SM multimode 16 MB flash (stdlib only).

usage: check_layout.py [--csv CSV] [--flash-size BYTES] [LABEL=PATH ...]

LABEL is a partition label from the CSV (factory, ota_0, camdb, ...), or one
of the special labels `bootloader` (limit 0x8000) and `partitions` (the binary
table at 0x8000, compared entry by entry with the CSV).
Exit 0 when every check passes, 1 otherwise.
"""
import argparse
import os
import struct
import sys

FLASH_DEFAULT = 16 * 1024 * 1024
APP_ALIGN = 0x10000
DATA_ALIGN = 0x1000
BOOT_OFFSET = 0x0
TABLE_OFFSET = 0x8000
TABLE_SIZE = 0x1000
FIRST_PART = 0x9000
IMAGE_MAGIC = 0xE9
APP_DESC_MAGIC = 0xABCD5432
APP_DESC_OFFSET = 0x20  # 24 byte image header + 8 byte first segment header
ESP32S3_CHIP_ID = 9

TYPES = {"app": 0, "data": 1}
SUBTYPES = {
    "app": {"factory": 0, "ota_0": 0x10, "ota_1": 0x11, "ota_2": 0x12,
            "ota_3": 0x13, "test": 0x20},
    "data": {"ota": 0, "phy": 1, "nvs": 2, "coredump": 3, "nvs_keys": 4,
             "efuse": 5, "spiffs": 0x82, "fat": 0x81},
}


def num(text):
    t = text.strip().lower()
    mult = 1
    if t.endswith("k"):
        mult, t = 1024, t[:-1]
    elif t.endswith("m"):
        mult, t = 1024 * 1024, t[:-1]
    return int(t, 0) * mult


class Part:
    def __init__(self, name, typ, sub, off, size):
        self.name, self.typ, self.sub, self.off, self.size = name, typ, sub, off, size

    @property
    def end(self):
        return self.off + self.size

    def type_num(self):
        return TYPES[self.typ]

    def sub_num(self):
        s = SUBTYPES.get(self.typ, {}).get(self.sub)
        return s if s is not None else num(self.sub)


def parse_csv(path):
    parts = []
    with open(path, "r", encoding="utf-8") as f:
        for ln, raw in enumerate(f, 1):
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            cols = [c.strip() for c in line.split(",")]
            if len(cols) < 5:
                raise ValueError("%s:%d: need at least 5 columns" % (path, ln))
            name, typ, sub, off, size = cols[:5]
            if typ not in TYPES:
                raise ValueError("%s:%d: unknown type %r" % (path, ln, typ))
            parts.append(Part(name, typ, sub, num(off), num(size)))
    return parts


def parse_table_bin(blob):
    """Decode a binary partition table into (name, type, subtype, off, size)."""
    out = []
    for i in range(0, len(blob) - 31, 32):
        ent = blob[i:i + 32]
        if ent[:2] == b"\xaa\x50":
            typ, sub, off, size = struct.unpack("<BBII", ent[2:12])
            name = ent[12:28].split(b"\0", 1)[0].decode("ascii", "replace")
            out.append((name, typ, sub, off, size))
        else:
            break  # MD5 entry (0xEBEB) or erased (0xFF) ends the table
    return out


class Report:
    def __init__(self):
        self.rows = []
        self.failed = False

    def ok(self, check, detail=""):
        self.rows.append((check, "ok", detail))

    def fail(self, check, detail):
        self.rows.append((check, "FAIL", detail))
        self.failed = True

    def print(self):
        w = max([len(r[0]) for r in self.rows] + [5])
        for check, status, detail in self.rows:
            print("%-*s  %-4s  %s" % (w, check, status, detail))


def check_table(parts, flash, rep):
    for p in parts:
        align = APP_ALIGN if p.typ == "app" else DATA_ALIGN
        if p.off % align:
            rep.fail("align " + p.name, "offset 0x%X not 0x%X aligned" % (p.off, align))
        else:
            rep.ok("align " + p.name, "0x%X on 0x%X" % (p.off, align))
        if p.size % DATA_ALIGN:
            rep.fail("size " + p.name, "size 0x%X not a 0x1000 multiple" % p.size)
        if p.off < FIRST_PART:
            rep.fail("low " + p.name, "starts below 0x9000 (0x%X)" % p.off)
        if p.end > flash:
            rep.fail("range " + p.name, "ends 0x%X past flash 0x%X" % (p.end, flash))
    names = [p.name for p in parts]
    for n in sorted(set(names)):
        if names.count(n) > 1:
            rep.fail("unique " + n, "label used twice")
    srt = sorted(parts, key=lambda p: p.off)
    bad = False
    for a, b in zip(srt, srt[1:]):
        if a.end > b.off:
            rep.fail("overlap", "%s (0x%X..0x%X) overlaps %s (0x%X)" %
                     (a.name, a.off, a.end, b.name, b.off))
            bad = True
    if not bad:
        rep.ok("overlap", "none among %d partitions" % len(parts))
    if not any(r[1] == "FAIL" and r[0].split(" ")[0] in ("low", "range") for r in rep.rows):
        rep.ok("bounds", "all within 0x%X..0x%X" % (FIRST_PART, flash))


def app_header_checks(blob, label, rep):
    if len(blob) < 0x24:
        rep.fail("magic " + label, "file too short (%d bytes)" % len(blob))
        return
    if blob[0] != IMAGE_MAGIC:
        rep.fail("magic " + label, "first byte 0x%02X, want 0xE9" % blob[0])
        return
    chip = struct.unpack("<H", blob[12:14])[0]
    desc = struct.unpack("<I", blob[APP_DESC_OFFSET:APP_DESC_OFFSET + 4])[0]
    if desc != APP_DESC_MAGIC:
        rep.fail("magic " + label, "descriptor 0x%08X at 0x20, want 0xABCD5432" % desc)
        return
    if chip != ESP32S3_CHIP_ID:
        rep.fail("magic " + label, "chip id %d, want 9 (ESP32-S3)" % chip)
        return
    rep.ok("magic " + label, "0xE9, ESP32-S3, descriptor 0xABCD5432 at 0x20")


def check_image(label, path, parts, by_name, rep):
    try:
        with open(path, "rb") as f:
            blob = f.read()
    except OSError as e:
        rep.fail("file " + label, "cannot read %s: %s" % (path, e))
        return
    n = len(blob)
    if label == "bootloader":
        limit = TABLE_OFFSET - BOOT_OFFSET
        if n > limit:
            rep.fail("fit bootloader", "%d bytes over 0x%X" % (n, limit))
        else:
            rep.ok("fit bootloader", "%d of %d bytes at 0x0" % (n, limit))
        if blob[:1] != bytes([IMAGE_MAGIC]):
            rep.fail("magic bootloader", "first byte not 0xE9")
        else:
            rep.ok("magic bootloader", "0xE9")
        return
    if label == "partitions":
        if n > TABLE_SIZE:
            rep.fail("fit partitions", "%d bytes over 0x1000" % n)
        else:
            rep.ok("fit partitions", "%d of %d bytes at 0x8000" % (n, TABLE_SIZE))
        got = parse_table_bin(blob)
        want = [(p.name, p.type_num(), p.sub_num(), p.off, p.size) for p in parts]
        if got == want:
            rep.ok("table = csv", "%d entries match the CSV" % len(got))
        else:
            diff = ["has %s" % (g,) for g in got if g not in want]
            diff += ["lacks %s" % (w,) for w in want if w not in got]
            rep.fail("table = csv", "partitions.bin differs: " + "; ".join(diff[:4]))
        return
    p = by_name.get(label)
    if p is None:
        rep.fail("slot " + label, "no such partition in the CSV")
        return
    if n > p.size:
        rep.fail("fit " + label, "%d bytes > slot size %d at 0x%X" % (n, p.size, p.off))
    else:
        rep.ok("fit " + label, "%d of %d bytes (%.1f%%) at 0x%X" %
               (n, p.size, 100.0 * n / p.size, p.off))
    if p.typ == "app":
        app_header_checks(blob, label, rep)


def main(argv):
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--csv", default=os.path.join(here, "..", "multimode", "partitions-multimode.csv"))
    ap.add_argument("--flash-size", type=num, default=FLASH_DEFAULT)
    ap.add_argument("images", nargs="*", help="label=path")
    a = ap.parse_args(argv)

    rep = Report()
    try:
        parts = parse_csv(a.csv)
    except (OSError, ValueError) as e:
        print("cannot read table: %s" % e)
        return 1
    by_name = {p.name: p for p in parts}
    check_table(parts, a.flash_size, rep)

    for spec in a.images:
        if "=" not in spec:
            rep.fail("arg " + spec, "expected label=path")
            continue
        label, path = spec.split("=", 1)
        check_image(label, path, parts, by_name, rep)

    rep.print()
    print("RESULT: %s" % ("FAIL" if rep.failed else "PASS"))
    return 1 if rep.failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
