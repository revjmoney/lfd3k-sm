#!/usr/bin/env python3
"""RAM segment check of every multimode app image (stdlib only).

usage: check_images.py [--esptool PATH] [--allow-missing] [LABEL=PATH ...]

With no images given it checks the five apps the install image carries, at
the paths tools/build_multimode.ps1 uses. For each image it runs
`esptool image-info` (reads the file, opens no port) and fails when a RAM
segment overlaps the loader memory of the multimode 2nd stage bootloader.

Why: the picker's own image verify (esp_image_verify) does not run the 2nd
stage bootloader's load address check, so an image whose static RAM reaches
into the bootloader's own memory would pass the picker and AT+MODE and then
only fail inside the bootloader ("overlaps bootloader"), which sends the board
back to the picker with no message. IDF 4.4 apps (Arduino core 2.x, as RNode
and Simian use) are linked to allow static RAM up to 0x403CC700, 4 KB higher
than the IDF 5.4 bootloader permits, so this has to be checked per image.

Loader memory of the IDF 5.4.1 bootloader on the ESP32-S3, from
components/bootloader/subproject/main/ld/esp32s3/bootloader.ld (PlatformIO
framework-espidf 3.50401.0):

  bootloader_usable_dram_end       = 0x3fce9700   (end of ROM shared buffers)
  bootloader_dram_seg_end          = usable_end - 0x2000 (stack margin) = 0x3fce7700
  bootloader_dram_seg_start        = seg_end - 0x5000                   = 0x3fce2700
  bootloader_iram_loader_seg_start = dram_seg_start - 0x7000 + 0x6f0000 = 0x403cb700
  (the linker script asserts the last value is 0x403CB700 = SRAM_IRAM_END of apps)

So the loader text lives at IRAM view 0x403CB700.., its data at DRAM view
0x3FCE2700..0x3FCE7700, and everything the bootloader may use ends at
0x3FCE9700 (IRAM view 0x403D9700). An app image may place static RAM below
0x3FCDB700 (DRAM view) or 0x403CB700 (IRAM view) and nothing at or above it up
to the end of that region. The two views alias the same SRAM, offset 0x6F0000.
This tool forbids:

  DRAM view  [0x3FCDB700, 0x3FCE9700)
  IRAM view  [0x403CB700, 0x403D9700)

Segments elsewhere (flash-mapped DROM/IROM, RTC memory) are not checked here.
Exit 0 when every image passes, 1 otherwise.
"""
import argparse
import os
import re
import subprocess
import sys

IRAM_DRAM_OFFSET = 0x6F0000
LOADER_START_IRAM = 0x403CB700
LOADER_START_DRAM = LOADER_START_IRAM - IRAM_DRAM_OFFSET  # 0x3FCDB700
LOADER_END_DRAM = 0x3FCE9700
LOADER_END_IRAM = LOADER_END_DRAM + IRAM_DRAM_OFFSET  # 0x403D9700
FORBIDDEN = (
    ("DRAM", LOADER_START_DRAM, LOADER_END_DRAM),
    ("IRAM", LOADER_START_IRAM, LOADER_END_IRAM),
)
# Internal SRAM as the CPU sees it (any view), to tell RAM from flash and RTC.
SRAM_DRAM = (0x3FC88000, 0x3FD00000)
SRAM_IRAM = (0x40370000, 0x403E0000)

ROW = re.compile(r"^\s*(\d+)\s+0x([0-9a-fA-F]+)\s+0x([0-9a-fA-F]+)\s+0x([0-9a-fA-F]+)\s*(.*?)\s*$")


def parse_segments(text):
    """Segment rows of an `image-info` report: [(index, length, load, types)]."""
    segs = []
    inside = False
    for line in text.splitlines():
        if line.startswith("Segments Information"):
            inside = True
            continue
        if inside:
            m = ROW.match(line)
            if m:
                segs.append((int(m.group(1)), int(m.group(2), 16), int(m.group(3), 16), m.group(5)))
            elif segs and not line.strip():
                break
    return segs


def is_sram(load, length):
    end = load + length
    for lo, hi in (SRAM_DRAM, SRAM_IRAM):
        if lo <= load and end <= hi:
            return True
    return False


def check_segments(segs):
    """Returns (problems, highest_ram_end_by_view). problems is a list of strings."""
    problems = []
    top = {"DRAM": 0, "IRAM": 0}
    for idx, length, load, types in segs:
        end = load + length
        if length == 0:
            continue
        for view, lo, hi in FORBIDDEN:
            if load < hi and end > lo:
                problems.append(
                    "segment %d (%s) 0x%08X..0x%08X overlaps the bootloader's loader memory "
                    "(%s view 0x%08X..0x%08X)" % (idx, types, load, end, view, lo, hi))
        if is_sram(load, length):
            view = "IRAM" if SRAM_IRAM[0] <= load < SRAM_IRAM[1] else "DRAM"
            top[view] = max(top[view], end)
    return problems, top


def image_problems(text):
    """Problems found in a whole `image-info` report."""
    problems = []
    segs = parse_segments(text)
    if not segs:
        problems.append("no segment table in the esptool output")
    seg_problems, top = check_segments(segs)
    problems += seg_problems
    if "Chip ID: 9 (ESP32-S3)" not in text:
        problems.append("not an ESP32-S3 image")
    for m in re.finditer(r"^(Checksum|Validation hash):.*$", text, re.M):
        if "(valid)" not in m.group(0):
            problems.append(m.group(0).strip())
    return problems, segs, top


def run_image_info(esptool, path):
    here = os.path.dirname(os.path.abspath(__file__))
    for sub in ("image-info", "image_info"):  # esptool 5 / esptool 4
        r = subprocess.run([sys.executable, "-I", esptool, sub, path], capture_output=True,
                           text=True, cwd=here, timeout=120)
        if r.returncode == 0 and "Segments Information" in r.stdout:
            return r.stdout
        last = r.stdout + r.stderr
    raise RuntimeError("esptool image-info failed: " + last.strip()[-300:])


def default_images(root):
    return [
        ("PICKER", os.path.join(root, "multimode", "picker", ".pio", "build", "picker", "firmware.bin")),
        ("MODEM", os.path.join(root, "firmware", ".pio", "build", "heltec_v4", "firmware.bin")),
        ("SIMIAN", r"F:\simian-radar\.pio\build\heltec_v4_multimode\firmware.bin"),
        ("MESH", r"F:\meshtastic-axis\.pio\build\heltec-v4-axis\firmware-heltec-v4-axis-2.7.26.54e0d8d.bin"),
        ("RNODE", os.path.join(root, "multimode", "out", "rnode", "rnode_firmware_heltec32v4pa.bin")),
    ]


def main(argv):
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(here)
    ap = argparse.ArgumentParser()
    ap.add_argument("--esptool", default=os.path.join(os.path.expanduser("~"), ".platformio",
                                                      "packages", "tool-esptoolpy", "esptool.py"))
    ap.add_argument("--allow-missing", action="store_true", help="skip images that are not built")
    ap.add_argument("images", nargs="*", help="label=path")
    a = ap.parse_args(argv)

    imgs = []
    for spec in a.images:
        if "=" not in spec:
            print("expected label=path, got %r" % spec)
            return 1
        imgs.append(tuple(spec.split("=", 1)))
    if not imgs:
        imgs = default_images(root)

    print("forbidden: DRAM view 0x%08X..0x%08X, IRAM view 0x%08X..0x%08X (bootloader loader memory)" %
          (LOADER_START_DRAM, LOADER_END_DRAM, LOADER_START_IRAM, LOADER_END_IRAM))
    failed = False
    for label, path in imgs:
        if not os.path.isfile(path):
            if a.allow_missing:
                print("%-7s SKIP  %s not found" % (label, path))
                continue
            print("%-7s FAIL  %s not found" % (label, path))
            failed = True
            continue
        try:
            text = run_image_info(a.esptool, path)
        except (RuntimeError, OSError, subprocess.SubprocessError) as e:
            print("%-7s FAIL  %s" % (label, e))
            failed = True
            continue
        problems, segs, top = image_problems(text)
        ram = [s for s in segs if is_sram(s[2], s[1]) and s[1]]
        head_d = LOADER_START_DRAM - top["DRAM"] if top["DRAM"] else None
        head_i = LOADER_START_IRAM - top["IRAM"] if top["IRAM"] else None
        info = "%d segments, %d in SRAM; DRAM ends 0x%08X (headroom %s), IRAM ends 0x%08X (headroom %s)" % (
            len(segs), len(ram), top["DRAM"], "%d B" % head_d if head_d is not None else "-",
            top["IRAM"], "%d B" % head_i if head_i is not None else "-")
        if problems:
            failed = True
            print("%-7s FAIL  %s" % (label, info))
            for p in problems:
                print("          " + p)
        else:
            print("%-7s PASS  %s" % (label, info))
    print("RESULT: %s" % ("FAIL" if failed else "PASS"))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
