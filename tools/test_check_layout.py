#!/usr/bin/env python3
"""Tests of the multimode layout checker (tools/check_layout.py), the install
image builder's offset table (tools/build_multimode.ps1 and, when it has been
built, multimode/out), and the RAM segment parser of tools/check_images.py.

    python -m unittest tools/test_check_layout.py

Stdlib only. Nothing here opens a port or runs a flasher.
"""
import contextlib
import hashlib
import io
import os
import re
import struct
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)

import check_images  # noqa: E402
import check_layout  # noqa: E402

CSV = os.path.join(ROOT, "multimode", "partitions-multimode.csv")
BUILDER = os.path.join(HERE, "build_multimode.ps1")
OUT = os.path.join(ROOT, "multimode", "out")

HEADER = "# Name, Type, SubType, Offset, Size, Flags\n"
GOOD_ROWS = [
    "nvs, data, nvs, 0x9000, 0xF000,",
    "otadata, data, ota, 0x18000, 0x2000,",
    "factory, app, factory, 0x20000, 0x80000,",
    "ota_0, app, ota_0, 0xA0000, 0x400000,",
    "ota_1, app, ota_1, 0x4A0000, 0x180000,",
    "lfd3k_nvs, data, nvs, 0x9A0000, 0x10000,",
    "spiffs, data, spiffs, 0x9B0000, 0x200000,",
]


def app_image(size=0x1000, chip=9, desc=0xABCD5432, magic=0xE9):
    """A minimal fake app image: header, first segment header, descriptor magic."""
    b = bytearray(size)
    b[0] = magic
    b[1] = 1
    struct.pack_into("<H", b, 12, chip)
    struct.pack_into("<I", b, 0x20, desc)
    return bytes(b)


def run_check(csv_text=None, images=(), flash=None, csv_path=None):
    """Runs check_layout.main; returns (exit code, output)."""
    tmp = None
    if csv_path is None:
        tmp = tempfile.NamedTemporaryFile("w", suffix=".csv", delete=False, encoding="utf-8")
        tmp.write(csv_text)
        tmp.close()
        csv_path = tmp.name
    argv = ["--csv", csv_path]
    if flash is not None:
        argv += ["--flash-size", str(flash)]
    argv += list(images)
    buf = io.StringIO()
    try:
        with contextlib.redirect_stdout(buf):
            rc = check_layout.main(argv)
    finally:
        if tmp:
            os.unlink(tmp.name)
    return rc, buf.getvalue()


def csv_of(rows):
    return HEADER + "\n".join(rows) + "\n"


class TempFiles(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def write(self, name, data):
        p = os.path.join(self.dir.name, name)
        with open(p, "wb") as f:
            f.write(data)
        return p


class LayoutCheckPasses(TempFiles):
    def test_layout_check_passes(self):
        rc, out = run_check(csv_path=CSV)
        self.assertEqual(rc, 0, out)
        self.assertIn("RESULT: PASS", out)
        self.assertIn("overlap", out)

    def test_real_csv_values(self):
        parts = {p.name: p for p in check_layout.parse_csv(CSV)}
        # The slots the modem, picker and bootloader rely on.
        self.assertEqual(parts["factory"].off, 0x20000)
        self.assertEqual(parts["ota_0"].off, 0xA0000)
        self.assertEqual(parts["ota_1"].off, 0x4A0000)
        self.assertEqual(parts["ota_2"].off, 0x620000)
        self.assertEqual(parts["ota_3"].off, 0x7A0000)
        self.assertEqual(parts["lfd3k_nvs"].off, 0x9A0000)
        self.assertEqual(parts["picker_nvs"].off, 0x1A000)
        self.assertEqual(parts["camdb"].off, 0xCB0000)
        # Apps on 64 KB, data on 4 KB, all inside 16 MB, below nothing but 0x9000.
        for p in parts.values():
            self.assertEqual(p.off % (0x10000 if p.typ == "app" else 0x1000), 0, p.name)
            self.assertLessEqual(p.end, 16 * 1024 * 1024, p.name)
            self.assertGreaterEqual(p.off, 0x9000, p.name)
        # Subtypes: ota_N slots and nvs partitions are what the code looks for by subtype.
        self.assertEqual(parts["ota_1"].sub_num(), 0x11)
        self.assertEqual(parts["lfd3k_nvs"].sub_num(), 2)
        self.assertEqual(parts["picker_nvs"].sub_num(), 2)
        self.assertEqual(parts["factory"].sub_num(), 0)
        # The picker slot is 512 KB and holds the 5.4 picker with room to spare.
        self.assertEqual(parts["factory"].size, 0x80000)

    def test_good_synthetic_table_passes(self):
        rc, out = run_check(csv_of(GOOD_ROWS))
        self.assertEqual(rc, 0, out)


class LayoutCheckFails(TempFiles):
    def assert_fails(self, rows, needle, **kw):
        rc, out = run_check(csv_of(rows), **kw)
        self.assertEqual(rc, 1, out)
        self.assertIn("RESULT: FAIL", out)
        self.assertIn(needle, out)
        return out

    def test_overlap_fails(self):
        rows = list(GOOD_ROWS)
        rows[3] = "ota_0, app, ota_0, 0xA0000, 0x410000,"  # runs 0x10000 into ota_1
        out = self.assert_fails(rows, "overlap")
        self.assertIn("ota_0", out)
        self.assertIn("ota_1", out)

    def test_overlap_of_identical_offsets_fails(self):
        rows = GOOD_ROWS + ["dup, data, nvs, 0x9A0000, 0x1000,"]
        self.assert_fails(rows, "overlap")

    def test_app_misaligned_fails(self):
        rows = list(GOOD_ROWS)
        rows[4] = "ota_1, app, ota_1, 0x4A8000, 0x100000,"  # 32 KB aligned only
        out = self.assert_fails(rows, "align ota_1")
        self.assertIn("0x10000", out)

    def test_data_misaligned_fails(self):
        rows = list(GOOD_ROWS)
        rows[5] = "lfd3k_nvs, data, nvs, 0x9A0800, 0x10000,"
        self.assert_fails(rows, "align lfd3k_nvs")

    def test_size_not_sector_multiple_fails(self):
        rows = list(GOOD_ROWS)
        rows[5] = "lfd3k_nvs, data, nvs, 0x9A0000, 0x10800,"
        self.assert_fails(rows, "size lfd3k_nvs")

    def test_out_of_range_fails(self):
        rows = list(GOOD_ROWS) + ["late, data, nvs, 0xFF0000, 0x20000,"]  # ends 0x1010000
        out = self.assert_fails(rows, "range late")
        self.assertIn("past flash", out)

    def test_smaller_flash_than_table_fails(self):
        rc, out = run_check(csv_path=CSV, flash=8 * 1024 * 1024)
        self.assertEqual(rc, 1, out)
        self.assertIn("range", out)

    def test_below_first_partition_fails(self):
        rows = list(GOOD_ROWS)
        rows[0] = "nvs, data, nvs, 0x8000, 0xF000,"  # on top of the partition table
        self.assert_fails(rows, "low nvs")

    def test_duplicate_label_fails(self):
        rows = GOOD_ROWS + ["ota_1, app, ota_2, 0x620000, 0x180000,"]
        self.assert_fails(rows, "unique ota_1")

    def test_unreadable_csv_fails(self):
        rc, out = run_check(csv_path=os.path.join(self.dir.name, "missing.csv"))
        self.assertEqual(rc, 1)
        self.assertIn("cannot read table", out)

    def test_short_row_and_bad_type_fail(self):
        rc, out = run_check("nvs, data, nvs\n")
        self.assertEqual(rc, 1)
        rc, out = run_check("x, widget, nvs, 0x9000, 0x1000,\n")
        self.assertEqual(rc, 1)
        self.assertIn("unknown type", out)


class ImageChecks(TempFiles):
    def test_good_images_pass(self):
        p = self.write("ota1.bin", app_image(0x1000))
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 0, out)
        self.assertIn("fit ota_1", out)
        self.assertIn("magic ota_1", out)

    def test_image_exactly_slot_size_passes(self):
        p = self.write("ota1.bin", app_image(0x180000))
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 0, out)

    def test_oversize_image_fails(self):
        p = self.write("ota1.bin", app_image(0x180000 + 1))
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 1, out)
        self.assertIn("fit ota_1", out)
        self.assertIn("slot size", out)

    def test_oversize_picker_fails(self):
        p = self.write("picker.bin", app_image(0x80001))
        rc, out = run_check(csv_path=CSV, images=["factory=" + p])
        self.assertEqual(rc, 1, out)

    def test_bad_magic_fails(self):
        p = self.write("ota1.bin", app_image(magic=0xE8))
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 1, out)
        self.assertIn("first byte", out)

    def test_bad_descriptor_magic_fails(self):
        p = self.write("ota1.bin", app_image(desc=0x12345678))
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 1, out)
        self.assertIn("descriptor", out)

    def test_wrong_chip_fails(self):
        p = self.write("ota1.bin", app_image(chip=0))  # ESP32, not an S3
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 1, out)
        self.assertIn("chip id", out)

    def test_too_short_image_fails(self):
        p = self.write("ota1.bin", b"\xe9" * 8)
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + p])
        self.assertEqual(rc, 1, out)

    def test_missing_image_file_fails(self):
        rc, out = run_check(csv_path=CSV, images=["ota_1=" + os.path.join(self.dir.name, "nope.bin")])
        self.assertEqual(rc, 1, out)

    def test_unknown_label_and_bad_arg_fail(self):
        p = self.write("x.bin", app_image())
        rc, out = run_check(csv_path=CSV, images=["ota_9=" + p])
        self.assertEqual(rc, 1, out)
        rc, out = run_check(csv_path=CSV, images=["justapath"])
        self.assertEqual(rc, 1, out)

    def test_data_partition_image_needs_no_app_header(self):
        p = self.write("camdb.bin", b"\x00" * 0x1000)
        rc, out = run_check(csv_path=CSV, images=["camdb=" + p])
        self.assertEqual(rc, 0, out)
        p = self.write("big.bin", b"\x00" * (0x320000 + 1))
        rc, out = run_check(csv_path=CSV, images=["camdb=" + p])
        self.assertEqual(rc, 1, out)

    def test_bootloader_limits(self):
        p = self.write("bl.bin", app_image(0x5C30))  # 23600 bytes, like the real one
        rc, out = run_check(csv_path=CSV, images=["bootloader=" + p])
        self.assertEqual(rc, 0, out)
        p = self.write("bl.bin", app_image(0x8001))  # would run onto the partition table
        rc, out = run_check(csv_path=CSV, images=["bootloader=" + p])
        self.assertEqual(rc, 1, out)
        self.assertIn("fit bootloader", out)
        p = self.write("bl.bin", b"\x00" * 100)
        rc, out = run_check(csv_path=CSV, images=["bootloader=" + p])
        self.assertEqual(rc, 1, out)

    def test_partition_table_binary(self):
        parts = check_layout.parse_csv(CSV)
        blob = b""
        for p in parts:
            name = p.name.encode().ljust(16, b"\0")
            blob += b"\xaa\x50" + struct.pack("<BBII", p.type_num(), p.sub_num(), p.off, p.size) + name + b"\0\0\0\0"
        blob += b"\xeb\xeb" + b"\xff" * 14 + b"\x00" * 16  # the MD5 entry ends the table
        good = self.write("pt.bin", blob)
        rc, out = run_check(csv_path=CSV, images=["partitions=" + good])
        self.assertEqual(rc, 0, out)
        self.assertIn("table = csv", out)
        # One entry moved: must differ from the CSV.
        bad_entries = bytearray(blob)
        off_field = 32 * 3 + 4  # ota_0's... any entry offset field
        struct.pack_into("<I", bad_entries, off_field, 0x30000)
        bad = self.write("pt2.bin", bytes(bad_entries))
        rc, out = run_check(csv_path=CSV, images=["partitions=" + bad])
        self.assertEqual(rc, 1, out)
        self.assertIn("differs", out)
        # A table that is too big for its 4 KB sector.
        big = self.write("pt3.bin", blob + b"\x00" * 0x1000)
        rc, out = run_check(csv_path=CSV, images=["partitions=" + big])
        self.assertEqual(rc, 1, out)

    def test_num_parser(self):
        self.assertEqual(check_layout.num("0x10"), 16)
        self.assertEqual(check_layout.num("4K"), 4096)
        self.assertEqual(check_layout.num("2M"), 2 * 1024 * 1024)
        self.assertEqual(check_layout.num(" 16 "), 16)


def read(path):
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        return f.read()


class InstallImageOffsets(unittest.TestCase):
    """The offsets the builder writes at must be the CSV's, so a drifting table
    can never put an image over the wrong partition."""

    @classmethod
    def setUpClass(cls):
        cls.parts = {p.name: p for p in check_layout.parse_csv(CSV)}
        cls.builder = read(BUILDER)

    def test_builder_expect_table_equals_csv(self):
        m = re.search(r"\$expect\s*=\s*@\{([^}]*)\}", self.builder)
        self.assertIsNotNone(m, "no $expect table in the builder")
        pairs = re.findall(r"(\w+)\s*=\s*(0x[0-9A-Fa-f]+)", m.group(1))
        self.assertGreaterEqual(len(pairs), 6)
        for name, off in pairs:
            self.assertIn(name, self.parts, name)
            self.assertEqual(int(off, 16), self.parts[name].off, name)

    def test_builder_places_each_mode_by_csv_label(self):
        # Each piece is placed at $off['<label>'], read from the CSV at run time.
        want = {"factory": "factory", "ota_0": "ota_0", "ota_1": "ota_1", "ota_2": "ota_2",
                "ota_3": "ota_3", "camdb": "camdb"}
        for label, csv_name in want.items():
            self.assertIn("Offset = $off['%s']" % csv_name, self.builder, label)
        self.assertIn("Offset = 0x0;", self.builder)       # bootloader
        self.assertIn("Offset = 0x8000;", self.builder)    # partition table
        self.assertIn("[Convert]::ToInt64($c[3], 16)", self.builder)  # offsets read from the CSV
        # Modes in their slots: MESH ota_0, MODEM ota_1, SIMIAN ota_2, RNODE ota_3, picker factory.
        pairs = dict(re.findall(r"Label = '(\w+)';\s*Offset = [^;]*;\s*Path = \$(\w+);", self.builder))
        self.assertEqual(pairs["factory"], "Picker")
        self.assertEqual(pairs["ota_0"], "Mesh")
        self.assertEqual(pairs["ota_1"], "Modem")
        self.assertEqual(pairs["ota_2"], "Simian")
        self.assertEqual(pairs["ota_3"], "Rnode")

    def test_builder_never_flashes(self):
        code = [l for l in self.builder.splitlines() if not l.lstrip().startswith("#")]
        text = chr(10).join(code)
        for bad in ("erase_flash", "read_flash", "'upload'", "'monitor'"):
            self.assertNotIn(bad, text)
        # write_flash appears once, inside the sample command written to flash_args.txt
        # (a string for Rev to run later), never as a call.
        hits = [l.strip() for l in code if "write_flash" in l]
        self.assertEqual(len(hits), 1, hits)
        self.assertTrue(hits[0].startswith("$line = ("), hits[0])
        self.assertIn("merge_bin", text)
        self.assertIn("check_layout.py", text)
        # No call opens a serial port: the only esptool invocation is merge_bin.
        self.assertEqual(len(re.findall(r"@\(\$Esptool", text)), 1)

    @unittest.skipUnless(os.path.exists(os.path.join(OUT, "manifest.txt")), "multimode/out not built")
    def test_install_image_offsets_match_table(self):
        manifest = read(os.path.join(OUT, "manifest.txt"))
        rows = re.findall(r"^([0-9a-f]{64})\s+0x([0-9A-Fa-f]+)\s+(\d+) bytes\s+(.+?)\s{2,}(.+)$", manifest, re.M)
        by_name = {r[3].strip(): (int(r[1], 16), int(r[2]), r[0], r[4]) for r in rows}
        self.assertTrue({"bootloader", "partition table", "picker", "MESH", "MODEM", "SIMIAN", "RNODE"} <= set(by_name),
                        sorted(by_name))
        slot_of = {"picker": "factory", "MESH": "ota_0", "MODEM": "ota_1", "SIMIAN": "ota_2",
                   "RNODE": "ota_3", "camdb": "camdb"}
        for name, label in slot_of.items():
            if name not in by_name:
                continue
            off, size, _, _ = by_name[name]
            part = self.parts[label]
            self.assertEqual(off, part.off, "%s at 0x%X, CSV %s is 0x%X" % (name, off, label, part.off))
            self.assertLessEqual(size, part.size, "%s does not fit %s" % (name, label))
        self.assertEqual(by_name["bootloader"][0], 0)
        self.assertLessEqual(by_name["bootloader"][1], 0x8000)
        self.assertEqual(by_name["partition table"][0], 0x8000)

    @unittest.skipUnless(os.path.exists(os.path.join(OUT, "full-16mb.bin")), "multimode/out not built")
    def test_full_image_bytes_sit_at_the_csv_offsets(self):
        manifest = read(os.path.join(OUT, "manifest.txt"))
        rows = re.findall(r"^([0-9a-f]{64})\s+0x([0-9A-Fa-f]+)\s+(\d+) bytes\s+(.+?)\s{2,}(.+)$", manifest, re.M)
        with open(os.path.join(OUT, "full-16mb.bin"), "rb") as f:
            full = f.read()
        self.assertEqual(len(full), 16 * 1024 * 1024)
        covered = []
        for digest, off, size, name, src in rows:
            off, size = int(off, 16), int(size)
            if name.strip() == "bootloader":
                # merge_bin patches the flash parameters and the image digest, so compare
                # the middle of the image with the bootloader file it came from.
                self.assertEqual(full[off], 0xE9)
                if os.path.isfile(src.strip()):
                    with open(src.strip(), "rb") as f:
                        orig = f.read()
                    self.assertEqual(len(orig), size)
                    self.assertEqual(full[off + 4:off + size - 64], orig[4:size - 64])
            else:
                self.assertEqual(hashlib.sha256(full[off:off + size]).hexdigest(), digest, name)
            covered.append((off, off + size, name.strip()))
        # No piece overlaps another, and nothing outside the pieces was written (erased flash is 0xFF).
        covered.sort()
        for a, b in zip(covered, covered[1:]):
            self.assertLessEqual(a[1], b[0], "%s overlaps %s" % (a[2], b[2]))
        pos = 0
        for start, end, name in covered:
            gap = full[pos:start]
            self.assertEqual(gap.count(0xFF), len(gap), "stray bytes before %s (0x%X..0x%X)" % (name, pos, start))
            pos = max(pos, end)
        tail = full[pos:]
        self.assertEqual(tail.count(0xFF), len(tail), "stray bytes after the last piece")
        # The image never reaches the data areas the modes own, except the camdb partition.
        for label in ("nvs", "otadata", "picker_nvs", "lfd3k_nvs", "spiffs", "littlefs", "coredump"):
            p = self.parts[label]
            region = full[p.off:p.end]
            self.assertEqual(region.count(0xFF), len(region), label + " is not blank in the install image")

    @unittest.skipUnless(os.path.exists(os.path.join(OUT, "flash_args.txt")), "multimode/out not built")
    def test_flash_args_write_one_image_at_zero(self):
        text = read(os.path.join(OUT, "flash_args.txt"))
        cmds = [l for l in text.splitlines() if "write_flash" in l and not l.lstrip().startswith("#")]
        self.assertEqual(len(cmds), 1)
        self.assertRegex(cmds[0], r"\b0x0 full-16mb\.bin\s*$")
        self.assertIn("--flash_size 16MB", cmds[0])
        self.assertIn("--flash_mode qio", cmds[0])


SAMPLE_INFO = """\
Segments Information
====================
Segment   Length   Load addr   File offs  Memory types
-------  -------  ----------  ----------  ------------
      0  0x24ca4  0x3c0b0020  0x00000018  DROM
      1  0x086c0  0x3fc9d300  0x00024cc4  BYTE_ACCESSIBLE, MEM_INTERNAL, DRAM
      2  0x02c84  0x40374000  0x0002d38c  MEM_INTERNAL, IRAM
      3  0xaf0b0  0x42000020  0x00030018  IROM
      4  0x16608  0x40376c84  0x000df0d0  MEM_INTERNAL, IRAM
      5  0x0001c  0x600fe100  0x000f56e0  RTC_DRAM, RTC_IRAM

ESP32-S3 Image Footer
=====================
Checksum: 0x0f (valid)
Validation hash: 7e7c (valid)
"""


class ImageRamSegments(unittest.TestCase):
    def info(self, rows="", chip=True, extra=""):
        text = "Chip ID: 9 (ESP32-S3)\n" if chip else "Chip ID: 0 (ESP32)\n"
        body = SAMPLE_INFO.replace("\n      5  0x0001c", "\n" + rows + "\n      5  0x0001c") if rows else SAMPLE_INFO
        return text + body + extra

    def test_parse_rows(self):
        segs = check_images.parse_segments(SAMPLE_INFO)
        self.assertEqual(len(segs), 6)
        self.assertEqual(segs[1], (1, 0x86C0, 0x3FC9D300, "BYTE_ACCESSIBLE, MEM_INTERNAL, DRAM"))

    def test_loader_range_derives_from_the_linker_script(self):
        # bootloader.ld: dram_seg_end = 0x3fce9700 - 0x2000, dram_seg_start = end - 0x5000,
        # iram_loader_seg_start = dram_seg_start - 0x7000 + 0x6f0000 = 0x403CB700.
        dram_end = 0x3FCE9700 - 0x2000
        dram_start = dram_end - 0x5000
        loader_start = dram_start - 0x7000 + 0x6F0000
        self.assertEqual(loader_start, 0x403CB700)
        self.assertEqual(check_images.LOADER_START_IRAM, loader_start)
        self.assertEqual(check_images.LOADER_START_DRAM, loader_start - 0x6F0000)
        self.assertEqual(dram_start, 0x3FCE2700)

    def test_sample_passes(self):
        problems, segs, top = check_images.image_problems(self.info())
        self.assertEqual(problems, [])
        self.assertEqual(top["IRAM"], 0x40376C84 + 0x16608)
        self.assertEqual(top["DRAM"], 0x3FC9D300 + 0x86C0)

    def test_ends_exactly_at_the_limit_passes(self):
        # A segment may end at 0x403CB700 / 0x3FCDB700 exactly.
        rows = "      6  0x%05x  0x40376c84  0x00000000  IRAM" % (0x403CB700 - 0x40376C84)
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertEqual(problems, [])
        rows = "      6  0x%05x  0x3fc9d300  0x00000000  DRAM" % (0x3FCDB700 - 0x3FC9D300)
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertEqual(problems, [])

    def test_one_byte_over_in_iram_fails(self):
        rows = "      6  0x%05x  0x40376c84  0x00000000  IRAM" % (0x403CB700 - 0x40376C84 + 1)
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("overlaps the bootloader", problems[0])

    def test_one_byte_over_in_dram_fails(self):
        rows = "      6  0x%05x  0x3fc9d300  0x00000000  DRAM" % (0x3FCDB700 - 0x3FC9D300 + 1)
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertEqual(len(problems), 1, problems)

    def test_idf44_style_image_fails(self):
        # IDF 4.4 apps may reach 0x403CC700: 4 KB into the 5.4 bootloader's text.
        rows = "      6  0x01000  0x403cb700  0x00000000  IRAM"
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertTrue(problems)

    def test_segment_inside_loader_data_fails(self):
        rows = "      6  0x00100  0x3fce2700  0x00000000  DRAM"
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertTrue(problems)

    def test_flash_and_rtc_segments_are_ignored(self):
        rows = "      6  0x100000  0x42100000  0x00000000  IROM\n      7  0x00010  0x600fffc0  0x00000000  RTC_DRAM"
        problems, _, _ = check_images.image_problems(self.info(rows))
        self.assertEqual(problems, [])

    def test_other_faults(self):
        problems, _, _ = check_images.image_problems(self.info(chip=False))
        self.assertTrue(any("ESP32-S3" in p for p in problems))
        problems, _, _ = check_images.image_problems(self.info(extra="Checksum: 0x00 (invalid, expected 0x0f)\n"))
        self.assertTrue(problems)
        problems, _, _ = check_images.image_problems("Chip ID: 9 (ESP32-S3)\nnothing\n")
        self.assertTrue(any("segment table" in p for p in problems))


if __name__ == "__main__":
    unittest.main()
