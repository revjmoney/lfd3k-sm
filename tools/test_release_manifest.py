#!/usr/bin/env python3
"""Tests of the release manifest writer (tools/release_manifest.py) and the gh-pages release
store (tools/ghpages.py), on made-up build folders.

    python -I -m unittest discover -s tools -p "test_release_manifest.py"

Stdlib only. Nothing here opens a port, runs a flasher or touches git.
"""
import hashlib
import json
import os
import struct
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import ghpages  # noqa: E402
import release_manifest as rm  # noqa: E402

MB = 1024 * 1024
SINGLE = [("nvs", 1, 2, 0x9000, 0x5000), ("otadata", 1, 0, 0xE000, 0x2000),
          ("ota_0", 0, 0x10, 0x10000, 0x180000), ("ota_1", 0, 0x11, 0x190000, 0x180000),
          ("storage", 1, 0x83, 0x310000, 0x100000)]
MULTI = [("nvs", 1, 2, 0x9000, 0xF000), ("otadata", 1, 0, 0x18000, 0x2000),
         ("picker_nvs", 1, 2, 0x1A000, 0x6000), ("factory", 0, 0, 0x20000, 0x80000),
         ("ota_0", 0, 0x10, 0xA0000, 0x400000), ("ota_1", 0, 0x11, 0x4A0000, 0x180000),
         ("ota_2", 0, 0x12, 0x620000, 0x180000), ("lfd3k_nvs", 1, 2, 0x9A0000, 0x10000)]


def table(rows):
    out = b""
    for label, ptype, sub, off, size in rows:
        out += struct.pack("<BBBBII16sI", 0xAA, 0x50, ptype, sub, off, size, label.encode(), 0)
    return out + b"\xff" * 32


def app(project="lfd3k_sm", size=0x1000):
    b = bytearray(size)
    b[0] = 0xE9
    struct.pack_into("<I", b, 0x20, 0xABCD5432)
    b[0x30:0x30 + 6] = b"v0.5.0"
    b[0x50:0x50 + len(project)] = project.encode()
    return bytes(b)


def bootloader(size_code):
    return bytes([0xE9, 3, 2, (size_code << 4) | 0xF]) + b"\0" * 60


def build(folder, rows, size_code=4, project="lfd3k_sm", otadata=True, app_size=0x1000):
    os.makedirs(folder, exist_ok=True)
    files = {"partitions.bin": table(rows), "firmware.bin": app(project, app_size),
             "bootloader.bin": bootloader(size_code)}
    if otadata:
        files["ota_data_initial.bin"] = b"\xff" * 0x2000
    for name, data in files.items():
        with open(os.path.join(folder, name), "wb") as fh:
            fh.write(data)
    return folder


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, env, rows, **kw):
        src = build(os.path.join(self.root, "b-" + env), rows, **kw)
        out = os.path.join(self.root, "out-" + env)
        m, _hdr = rm.write(env, src, out, "v0.5.0")
        return m, out

    def test_multimode_is_the_modem_slot_only(self):
        m, out = self.write("heltec_v4", MULTI)
        self.assertEqual(m["chip"], "ESP32-S3")
        self.assertEqual(m["layout"], "multimode")
        self.assertEqual(m["slot"], "ota_1")
        self.assertEqual(m["flash_size"], 16777216)
        self.assertEqual(m["table"], "partitions.bin")
        self.assertEqual([(i["role"], i["offset"]) for i in m["images"]], [("app", 0x4A0000)])
        self.assertTrue(os.path.isfile(os.path.join(out, "partitions.bin")))
        with open(os.path.join(out, "manifest.json"), encoding="utf-8") as fh:
            self.assertEqual(json.load(fh), m)

    def test_single_v4_and_v3(self):
        for env, code, size in (("heltec_v4_single", 4, 16777216), ("heltec_v3_single", 3, 8388608)):
            m, out = self.write(env, SINGLE, size_code=code)
            self.assertEqual(m["layout"], "single")
            self.assertEqual(m["flash_size"], size)
            self.assertNotIn("slot", m)
            self.assertEqual([(i["role"], i["offset"]) for i in m["images"]],
                             [("bootloader", 0x0), ("partitions", 0x8000), ("otadata", 0xE000), ("app", 0x10000)])
            self.assertEqual(m["app_partition"], {"offset": 0x10000, "size": 0x180000})
            for img in m["images"]:
                with open(os.path.join(out, img["file"]), "rb") as fh:
                    data = fh.read()
                self.assertEqual(img["size"], len(data))
                self.assertEqual(img["sha256"], hashlib.sha256(data).hexdigest())

    def refused(self, env, rows, text, **kw):
        self.n = getattr(self, "n", 0) + 1
        src = build(os.path.join(self.root, "bad%d" % self.n), rows, **kw)
        out = os.path.join(self.root, "out-bad%d" % self.n)
        with self.assertRaises(rm.Stopped) as cm:
            rm.write(env, src, out, "v0.5.0")
        self.assertIn(text, str(cm.exception))
        self.assertFalse(os.path.exists(out), "nothing is written when a check fails")

    def test_refusals(self):
        self.refused("heltec_v4", SINGLE, "should have the multimode layout")
        self.refused("heltec_v4_single", MULTI, "should have the single layout")
        self.refused("heltec_v4_single", SINGLE, "not the lfd3k_sm program", project="axis-bbs")
        self.refused("heltec_v3_single", SINGLE, "bootloader.bin says 16 MB", size_code=4)
        self.refused("heltec_v4_single", SINGLE, "ota_data_initial.bin", otadata=False)
        self.refused("heltec_v4", MULTI, "holds", app_size=0x180001)
        self.assertEqual(rm.main(["--env", "heltec_v4", "--build", os.path.join(self.root, "none"),
                                  "--out", os.path.join(self.root, "o"), "--release", "v1"]), 1)


class GhPagesTests(unittest.TestCase):
    def test_release_store_keeps_three_and_other_files(self):
        with tempfile.TemporaryDirectory() as root:
            ghp = os.path.join(root, "ghp")
            os.makedirs(ghp)
            with open(os.path.join(ghp, "README.md"), "w") as fh:
                fh.write("kept\n")
            src = os.path.join(root, "rel")
            os.makedirs(os.path.join(src, "heltec_v4"))
            for tag in ("v0.5.0", "v0.10.0", "v0.6.0", "v0.6.0-rc1"):
                ghpages.add_release(ghp, tag, src)
            self.assertEqual(ghpages.releases(ghp), ["v0.10.0", "v0.6.0", "v0.6.0-rc1"])
            with open(os.path.join(ghp, "releases", "index.json"), encoding="utf-8") as fh:
                self.assertEqual(json.load(fh), {"latest": "v0.10.0", "previous": "v0.6.0",
                                                 "releases": ["v0.10.0", "v0.6.0", "v0.6.0-rc1"]})
            self.assertTrue(os.path.isfile(os.path.join(ghp, "README.md")))
            self.assertTrue(os.path.isdir(os.path.join(ghp, "releases", "v0.10.0", "heltec_v4")))
            with self.assertRaises(ValueError):
                ghpages.add_release(ghp, "../x", src)


if __name__ == "__main__":
    unittest.main()
