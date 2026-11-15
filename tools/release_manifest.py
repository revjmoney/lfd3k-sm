#!/usr/bin/env python3
"""The release images of one firmware env, laid out for the AXIS installer (stdlib only).

    python -I tools/release_manifest.py --env heltec_v4 --build firmware/.pio/build/heltec_v4 \
        --out rel/heltec_v4 --release v0.5.0

Copies the images a flash needs from a PlatformIO build folder to --out and writes
--out/manifest.json in the shape the AXIS installer page (site/installer-logic.js planFlash)
and its PC script (tools/axis_flash.py) read:

    {release, env, chip "ESP32-S3", layout "multimode" | "single", flash_size (bytes),
     slot ("ota_1", multimode only), table "partitions.bin",
     app_partition {offset, size}, images [{role, file, offset, size, sha256}]}

heltec_v4 is the multimode modem slot: the only image is the app at ota_1 (0x4A0000), and
partitions.bin ships beside it so the installer can compare tables, but it is never written.
heltec_v4_single and heltec_v3_single are the modem alone on the board: boot loader at 0x0,
partition table at 0x8000, otadata at the table's otadata partition and the app in the first
app partition. Offsets come from the build's own partitions.bin.

Stops (exit 1, nothing written to --out) when the build does not match the env: an unknown env,
a missing image, the wrong layout, an app that is not the LFD3K-SM program, an app bigger than
its partition, or a boot loader whose header names a different flash size than the board has.
Opens no serial port and flashes nothing.
"""
import argparse
import hashlib
import json
import os
import shutil
import sys

CHIP = "ESP32-S3"
PROJECT = "lfd3k_sm"  # firmware/CMakeLists.txt project(); the installer checks the app header for it
MB = 1024 * 1024
MULTIMODE_MARKER = "picker_nvs"  # only the multimode layout has this partition
MODEM_SLOT = "ota_1"  # firmware/platformio.ini env heltec_v4: board_build.app_partition_name = ota_1
BOOTLOADER_OFFSET = 0x0  # where the ESP32-S3 ROM loads the boot loader
TABLE_OFFSET = 0x8000
# env: (layout, flash size in bytes). The installer tells a V3 (8 MB) from a V4 (16 MB) by this.
ENVS = {
    "heltec_v4": ("multimode", 16 * MB),
    "heltec_v4_single": ("single", 16 * MB),
    "heltec_v3_single": ("single", 8 * MB),
}
# Image header byte 3, high nibble (esptool's flash size codes)
FLASH_SIZE_CODE = {0: 1 * MB, 1: 2 * MB, 2: 4 * MB, 3: 8 * MB, 4: 16 * MB, 5: 32 * MB}


class Stopped(Exception):
    """The build does not fit the env; nothing is written."""


def _u32(b, i):
    return b[i] | (b[i + 1] << 8) | (b[i + 2] << 16) | (b[i + 3] << 24)


def _cstring(b, start, length):
    raw = bytes(b[start:start + length]).split(b"\0", 1)[0]
    try:
        text = raw.decode("ascii")
    except UnicodeDecodeError:
        return None
    return text if all(32 <= ord(c) <= 126 for c in text) else None


def parse_table(data):
    """The partition table bytes as entries (same as the installer's parseTable), or None."""
    out = []
    for i in range(0, len(data) - 31, 32):
        if data[i] != 0xAA or data[i + 1] != 0x50:
            break
        out.append({"type": data[i + 2], "subtype": data[i + 3], "offset": _u32(data, i + 4),
                    "size": _u32(data, i + 8), "label": _cstring(data, i + 12, 16) or ""})
    return out or None


def app_header(data):
    """{project, version} from the app's esp_app_desc_t (as the installer's parseAppHeader), or None."""
    if len(data) < 0x70 or data[0] != 0xE9 or _u32(data, 0x20) != 0xABCD5432:
        return None
    project = _cstring(data, 0x50, 32)
    if not project:
        return None
    return {"project": project, "version": _cstring(data, 0x30, 32) or ""}


def image_flash_size(data):
    """The flash size an image header names (bytes), or None."""
    if len(data) < 4 or data[0] != 0xE9:
        return None
    return FLASH_SIZE_CODE.get(data[3] >> 4)


def _sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(65536), b""):
            h.update(block)
    return h.hexdigest()


def _read(path):
    if not os.path.isfile(path):
        raise Stopped("the build has no %s" % path)
    with open(path, "rb") as fh:
        return fh.read()


def plan(env, build_dir):
    """The manifest (without sha256 and sizes) and the files it names, checked against the env."""
    if env not in ENVS:
        raise Stopped("unknown env %s (known: %s)" % (env, ", ".join(sorted(ENVS))))
    layout, flash_size = ENVS[env]
    table = parse_table(_read(os.path.join(build_dir, "partitions.bin")))
    if not table:
        raise Stopped("%s has no partition table" % os.path.join(build_dir, "partitions.bin"))
    multimode = any(e["label"] == MULTIMODE_MARKER for e in table)
    if multimode != (layout == "multimode"):
        raise Stopped("%s should have the %s layout, but its partition table is %s" %
                      (env, layout, "multimode" if multimode else "a single app table"))
    apps = sorted((e for e in table if e["type"] == 0), key=lambda e: e["offset"])
    if layout == "multimode":
        app_part = next((e for e in apps if e["label"] == MODEM_SLOT), None)
        if not app_part:
            raise Stopped("the multimode table has no app partition %s" % MODEM_SLOT)
    else:
        if not apps:
            raise Stopped("the partition table has no app partition")
        app_part = apps[0]

    firmware = _read(os.path.join(build_dir, "firmware.bin"))
    hdr = app_header(firmware)
    if not hdr or hdr["project"] != PROJECT:
        raise Stopped("firmware.bin is %s, not the %s program" %
                      (("project " + hdr["project"]) if hdr else "not an ESP-IDF app", PROJECT))
    if len(firmware) > app_part["size"]:
        raise Stopped("firmware.bin is %d bytes, %s holds %d" % (len(firmware), app_part["label"], app_part["size"]))

    files = []
    if layout == "single":
        bootloader = _read(os.path.join(build_dir, "bootloader.bin"))
        if image_flash_size(bootloader) != flash_size:
            raise Stopped("bootloader.bin says %s of flash, %s has %d MB" %
                          ("%d MB" % (image_flash_size(bootloader) // MB) if image_flash_size(bootloader)
                           else "an unknown size", env, flash_size // MB))
        ota = next((e for e in table if e["type"] == 1 and e["subtype"] == 0), None)
        if not ota:
            raise Stopped("the single app table has no otadata partition")
        files += [("bootloader", "bootloader.bin", BOOTLOADER_OFFSET),
                  ("partitions", "partitions.bin", TABLE_OFFSET),
                  ("otadata", "ota_data_initial.bin", ota["offset"])]
    files.append(("app", "firmware.bin", app_part["offset"]))
    for _role, name, _offset in files:
        _read(os.path.join(build_dir, name))
    manifest = {"release": "", "env": env, "chip": CHIP, "layout": layout, "flash_size": flash_size}
    if layout == "multimode":
        manifest["slot"] = MODEM_SLOT
    manifest["table"] = "partitions.bin"
    manifest["app_partition"] = {"offset": app_part["offset"], "size": app_part["size"]}
    return manifest, files, hdr


def write(env, build_dir, out_dir, release):
    """Check the build, then copy its images to out_dir and write out_dir/manifest.json."""
    manifest, files, hdr = plan(env, build_dir)
    manifest["release"] = release
    os.makedirs(out_dir, exist_ok=True)
    images = []
    for role, name, offset in files:
        src = os.path.join(build_dir, name)
        shutil.copyfile(src, os.path.join(out_dir, name))
        images.append({"role": role, "file": name, "offset": offset, "size": os.path.getsize(src),
                       "sha256": _sha256(src)})
    if manifest["layout"] == "multimode":  # the table to compare with, never written
        shutil.copyfile(os.path.join(build_dir, "partitions.bin"), os.path.join(out_dir, "partitions.bin"))
    manifest["images"] = images
    with open(os.path.join(out_dir, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(manifest, fh, indent=1)
        fh.write("\n")
    return manifest, hdr


def main(argv=None):
    ap = argparse.ArgumentParser(prog="release_manifest")
    ap.add_argument("--env", required=True, choices=sorted(ENVS))
    ap.add_argument("--build", required=True, help="the PlatformIO build folder, firmware/.pio/build/<env>")
    ap.add_argument("--out", required=True)
    ap.add_argument("--release", required=True, help="the release tag, for example v0.5.0")
    args = ap.parse_args(argv)
    try:
        m, hdr = write(args.env, args.build, args.out, args.release)
    except (OSError, Stopped) as e:
        print("release_manifest: %s" % e, file=sys.stderr)
        return 1
    print("%s %s: %s layout, %d MB, app %s %s" % (args.release, args.env, m["layout"], m["flash_size"] // MB,
                                                   hdr["project"], hdr["version"]))
    for img in m["images"]:
        print("  %-10s %-22s 0x%-7X %8d bytes  %s" % (img["role"], img["file"], img["offset"], img["size"],
                                                     img["sha256"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
