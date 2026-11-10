#!/usr/bin/env python3
"""Tests for tools/lfd3k_term.py: password redaction, the --shot-safe and
PWSHOW gate, and the refusal to write into the repo's docs folder.

Fakes only. No serial port is ever opened: a fake `serial` package replaces
pyserial for the whole run (its Serial raises if anything builds one), the
tool's open_port is replaced by a scripted fake, and time is a fake clock so
no test waits.

  python -m unittest tools/test_lfd3k_term.py
  python tools/test_lfd3k_term.py
"""
import contextlib
import io
import os
import sys
import tempfile
import types
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

# A fake pyserial, installed before the tool can import the real one.
_fake_serial = types.ModuleType("serial")
_fake_tools = types.ModuleType("serial.tools")
_fake_lp = types.ModuleType("serial.tools.list_ports")
_PORTS = []


def _comports():
    return list(_PORTS)


def _no_serial(*a, **k):
    raise AssertionError("a real serial.Serial was constructed in a test")


_fake_lp.comports = _comports
_fake_serial.Serial = _no_serial
_fake_serial.tools = _fake_tools
_fake_tools.list_ports = _fake_lp
sys.modules["serial"] = _fake_serial
sys.modules["serial.tools"] = _fake_tools
sys.modules["serial.tools.list_ports"] = _fake_lp

import lfd3k_term as T  # noqa: E402

FAKE_PW = "SeCrEt-pw-42"
MAC = "AA:BB:CC:DD:EE:FF"
FB_REPLY = "FB:" + "00" * 1024 + "\r\nOK\r\n"


class FakePort:
    """A V4 that is not there. `replies` maps a written line (bytes) to the
    bytes it answers with."""

    def __init__(self, replies):
        self.replies = replies
        self.writes = []
        self.rx = b""
        self.closed = False

    def write(self, data):
        self.writes.append(data)
        self.rx += self.replies.get(data, b"")

    def read(self, n):
        out, self.rx = self.rx[:n], self.rx[n:]
        return out

    def close(self):
        self.closed = True


class Clock:
    """Time that moves 1 ms per look, so drain loops end at once (they read the\n    port on every look, so every scripted reply is read before time is up)."""

    def __init__(self):
        self.t = 1000.0

    def time(self):
        self.t += 0.001
        return self.t

    def sleep(self, s):
        self.t += s


def atv(pwshow):
    body = b"\r\nE1 V1 NAME AXISMODEM-1234\r\nWIFI AP ON AXISMODEM-1234  STA OFF\r\n"
    if pwshow is not None:
        body += b"PWSHOW " + str(pwshow).encode() + b"\r\n"
    return body + b"PUB NONE\r\nFWD 0\r\nOK\r\n"


class Run:
    """One main() run against a fake port. Records what was opened."""

    def __init__(self, replies, ports=None):
        self.port = FakePort(replies)
        self.opened = []
        _PORTS[:] = ports if ports is not None else [T._P("COM11", T.VID, T.PID, MAC)]

    def main(self, argv):
        def fake_open(name):
            self.opened.append(name)
            return self.port

        out, err = io.StringIO(), io.StringIO()
        with mock.patch.object(T, "open_port", fake_open), \
                mock.patch.object(T, "time", Clock()), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                code = T.main(argv)
            except SystemExit as e:  # argparse errors
                code = e.code
        self.out, self.err = out.getvalue(), err.getvalue()
        return code

    @property
    def written(self):
        return b"".join(self.port.writes)


BASE = ["--port", "COM11", "--expect-mac", MAC, "--wait-ms", "10"]


class Redaction(unittest.TestCase):
    def test_wap_line_masks_everything_after_on_or_off(self):
        self.assertEqual(T.redact_line("WAP ON AXISMODEM-1 " + FAKE_PW), "WAP ON ********")
        self.assertEqual(T.redact_line("WAP OFF My Net " + FAKE_PW), "WAP OFF ********")
        # A password that looks like part of the ssid is still hidden.
        out = T.redact_line("WAP ON a b c d " + FAKE_PW)
        self.assertNotIn(FAKE_PW, out)
        self.assertNotIn("b c d", out)

    def test_pw_marker_masks_the_rest(self):
        self.assertEqual(T.redact_line("PW " + FAKE_PW), "PW ********")
        self.assertEqual(T.redact_line("AP AXIS PW " + FAKE_PW), "AP AXIS PW ********")

    def test_echoed_wap_and_wsta_lines_keep_only_the_name(self):
        self.assertEqual(T.redact_line("AT+WAP=Net," + FAKE_PW), "AT+WAP=Net,********")
        self.assertEqual(T.redact_line("at+wsta=Home," + FAKE_PW), "at+wsta=Home,********")
        self.assertEqual(T.redact_line("ATE0 AT+WSTA=Home," + FAKE_PW), "ATE0 AT+WSTA=Home,********")

    def test_spaced_and_mixed_case_command_forms_are_masked(self):
        for cmd in ("AT +WSTA=Home,", "AT+ WSTA=Home,", "AT+WSTA =Home,", "at + wap =Home,",
                    "At+WsTa=Home,", "AT  +  WSTA  =Home,"):
            got = T.redact_line(cmd + FAKE_PW)
            self.assertNotIn(FAKE_PW, got, cmd)
            self.assertTrue(got.endswith(",********"), got)

    def test_harmless_lines_are_untouched(self):
        for line in ("OK", "PWSHOW 1", "PWSHOW 0", "WSTA Home ********", "WSTA OFF",
                     "PUB NONE", "FWD AXIS 192.168.1.164:23 T", "AT+WAP?", "AT+WSTA?",
                     "CONNECT WIFI TCP", ""):
            self.assertEqual(T.redact_line(line), line)

    def test_redactor_catches_a_password_split_across_reads(self):
        r = T.Redactor()
        out = r.feed("AT+WAP=AXIS,EXAMPLE") + r.feed("PASS1\r\nOK\r\nWAP ON A " + FAKE_PW[:4])
        out += r.feed(FAKE_PW[4:]) + r.flush()
        self.assertNotIn("EXAMPLE", out)
        self.assertNotIn(FAKE_PW, out)
        self.assertNotIn(FAKE_PW[:4], out)
        self.assertIn("\r\nOK\r\n", out)

    def test_redactor_keeps_line_endings_and_holds_partial_lines(self):
        r = T.Redactor()
        self.assertEqual(r.feed("OK\r\nWAP ON x "), "OK\r\n")
        self.assertEqual(r.feed(FAKE_PW + "\r\n"), "WAP ON ********\r\n")
        self.assertEqual(r.feed("a\nb\rc"), "a\nb\r")
        self.assertEqual(r.flush(), "c")

    def test_parse_pwshow(self):
        self.assertEqual(T.parse_pwshow("OK\r\nPWSHOW 0\r\nPUB NONE\r\n"), 0)
        self.assertEqual(T.parse_pwshow("PWSHOW 1\r\n"), 1)
        self.assertIsNone(T.parse_pwshow("OK\r\n"))
        self.assertIsNone(T.parse_pwshow(""))
        self.assertIsNone(T.parse_pwshow(None))
        # Only a real line counts, not a mention inside another one.
        self.assertIsNone(T.parse_pwshow("see PWSHOW 0 here\r\n"))
        self.assertIsNone(T.parse_pwshow("PWSHOW 2\r\n"))
        self.assertIsNone(T.parse_pwshow("XPWSHOW 0\r\n"))


class PortChoice(unittest.TestCase):
    def test_com15_and_com9_are_refused_before_any_open(self):
        for name in ("COM15", "com15", "COM9", " com9 "):
            r = Run({}, ports=[T._P(name.strip().upper(), T.VID, T.PID, MAC)])
            code = r.main(["--port", name, "--expect-mac", MAC, "--send", "ATI"])
            self.assertEqual(code, 3, name)
            self.assertEqual(r.opened, [], name)
            self.assertEqual(r.port.writes, [])
            self.assertIn("refused", r.err.lower())

    def test_wrong_vid_pid_or_mac_is_refused(self):
        for p in (T._P("COM11", 0x1A86, 0x7523, MAC),
                  T._P("COM11", T.VID, T.PID, "11:22:33:44:55:66"),
                  T._P("COM11", T.VID, T.PID, None)):
            r = Run({}, ports=[p])
            self.assertEqual(r.main(BASE + ["--send", "ATI"]), 3)
            self.assertEqual(r.opened, [])
        r = Run({}, ports=[])
        self.assertEqual(r.main(BASE + ["--send", "ATI"]), 3)
        self.assertEqual(r.opened, [])

    def test_good_port_opens_once_and_closes(self):
        r = Run({b"ATI\r": b"LFD3K-SM\r\nOK\r\n"})
        self.assertEqual(r.main(BASE + ["--send", "ATI"]), 0)
        self.assertEqual(r.opened, ["COM11"])
        self.assertTrue(r.port.closed)
        self.assertIn(b"ATI\r", r.written)

    def test_selftest_opens_nothing(self):
        r = Run({})
        self.assertEqual(r.main(["--selftest"]), 0)
        self.assertEqual(r.opened, [])

    def test_the_real_serial_class_is_never_built(self):
        # open_port is the only place that builds one; with the fake package
        # in place it raises, so a test that forgot to patch it would fail.
        with self.assertRaises(AssertionError):
            T.open_port("COM11")


class ShotGate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.png = os.path.join(self.tmp.name, "oled.png")

    def test_shot_without_shot_safe_or_allow_secret_is_refused(self):
        r = Run({b"AT+FB\r": FB_REPLY.encode()})
        code = r.main(BASE + ["--shot", self.png])
        self.assertEqual(code, 4)
        self.assertEqual(r.opened, [], "refused before the port is even looked at")
        self.assertFalse(os.path.exists(self.png))
        self.assertIn("REFUSED", r.err)

    def test_shot_safe_needs_a_shot_file(self):
        r = Run({})
        self.assertNotEqual(r.main(BASE + ["--shot-safe"]), 0)
        self.assertEqual(r.opened, [])

    def test_shot_safe_with_pwshow_1_sends_no_fb_and_writes_nothing(self):
        r = Run({b"AT&V\r": atv(1), b"AT+FB\r": FB_REPLY.encode()})
        code = r.main(BASE + ["--shot-safe", "--shot", self.png])
        self.assertEqual(code, 5)
        self.assertNotIn(b"AT+FB\r", r.written)
        self.assertIn(b"AT&V\r", r.written)
        self.assertFalse(os.path.exists(self.png))
        self.assertIn("PWSHOW 1", r.err)
        self.assertTrue(r.port.closed)

    def test_shot_safe_with_no_pwshow_line_is_also_refused(self):
        r = Run({b"AT&V\r": atv(None), b"AT+FB\r": FB_REPLY.encode()})
        code = r.main(BASE + ["--shot-safe", "--shot", self.png])
        self.assertEqual(code, 5)
        self.assertNotIn(b"AT+FB\r", r.written)
        self.assertFalse(os.path.exists(self.png))
        self.assertIn("missing", r.err)

    def test_shot_safe_with_pwshow_0_takes_the_shot(self):
        r = Run({b"AT&V\r": atv(0), b"AT+FB\r": FB_REPLY.encode()})
        code = r.main(BASE + ["--shot-safe", "--shot", self.png])
        self.assertEqual(code, 0, r.err)
        self.assertLess(r.written.index(b"AT&V\r"), r.written.index(b"AT+FB\r"))
        with open(self.png, "rb") as f:
            self.assertEqual(f.read(8), b"\x89PNG\r\n\x1a\n")

    def test_allow_secret_lifts_the_gate_without_asking_atv(self):
        r = Run({b"AT&V\r": atv(1), b"AT+FB\r": FB_REPLY.encode()})
        code = r.main(BASE + ["--allow-secret", "--shot", self.png])
        self.assertEqual(code, 0, r.err)
        self.assertNotIn(b"AT&V\r", r.written)
        self.assertTrue(os.path.exists(self.png))


class DocsFolder(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.docs = os.path.join(self.tmp.name, "repo", "docs")
        os.makedirs(self.docs)
        # Point the tool's idea of the docs folder at a throwaway one, so a
        # test that gets through can never write into the real repo.
        p = mock.patch.object(T, "DOCS_DIR", self.docs)
        p.start()
        self.addCleanup(p.stop)

    def test_in_docs(self):
        self.assertTrue(T.in_docs(os.path.join(self.docs, "x.png")))
        self.assertTrue(T.in_docs(os.path.join(self.docs, "sub", "x.log")))
        self.assertTrue(T.in_docs(self.docs))
        self.assertFalse(T.in_docs(os.path.join(self.docs, "..", "x.png")))
        self.assertFalse(T.in_docs(os.path.join(self.docs, "sub", "..", "..", "x.png")))
        self.assertFalse(T.in_docs(os.path.join(self.tmp.name, "repo", "docs2", "x.png")))
        self.assertFalse(T.in_docs(os.path.join(self.tmp.name, "other.png")))
        self.assertTrue(T.in_docs(os.path.join(self.docs, "a", "..", "x.png")), "dot dot that stays inside")

    def shot_in_docs(self, pwshow):
        png = os.path.join(self.docs, "phase3", "oled.png")
        os.makedirs(os.path.dirname(png), exist_ok=True)
        r = Run({b"AT&V\r": atv(pwshow), b"AT+FB\r": FB_REPLY.encode()})
        # --allow-secret would lift the AT&V gate for the shot, so the docs
        # check is what is being tested: use --shot-safe, which asks anyway,
        # and a --log path in docs as the second way in.
        return r, png

    def test_shot_into_docs_refused_unless_pwshow_0(self):
        for pw in (1, None):
            r, png = self.shot_in_docs(pw)
            self.assertEqual(r.main(BASE + ["--shot-safe", "--shot", png]), 5)
            self.assertFalse(os.path.exists(png))
            self.assertNotIn(b"AT+FB\r", r.written)
        r, png = self.shot_in_docs(0)
        self.assertEqual(r.main(BASE + ["--shot-safe", "--shot", png]), 0, r.err)
        self.assertTrue(os.path.exists(png))

    def test_allow_secret_does_not_open_the_docs_folder(self):
        # --allow-secret skips the shot-safe question, but a file in docs is
        # still checked against AT&V (the shot would be committed).
        png = os.path.join(self.docs, "oled.png")
        r = Run({b"AT&V\r": atv(1), b"AT+FB\r": FB_REPLY.encode()})
        self.assertEqual(r.main(BASE + ["--allow-secret", "--shot", png]), 5)
        self.assertFalse(os.path.exists(png))
        self.assertNotIn(b"AT+FB\r", r.written)

    def test_log_into_docs_refused_unless_pwshow_0(self):
        log = os.path.join(self.docs, "bench.log")
        for pw in (1, None):
            r = Run({b"AT&V\r": atv(pw), b"ATI\r": b"LFD3K-SM\r\nOK\r\n"})
            self.assertEqual(r.main(BASE + ["--log", log, "--send", "ATI"]), 5)
            self.assertFalse(os.path.exists(log), "nothing written")
            self.assertNotIn(b"ATI\r", r.written, "no command was sent either")
        r = Run({b"AT&V\r": atv(0), b"ATI\r": b"LFD3K-SM\r\nOK\r\n"})
        self.assertEqual(r.main(BASE + ["--log", log, "--send", "ATI"]), 0, r.err)
        self.assertTrue(os.path.exists(log))

    def test_paths_outside_docs_need_no_pwshow_check(self):
        log = os.path.join(self.tmp.name, "bench.log")
        r = Run({b"ATI\r": b"LFD3K-SM\r\nOK\r\n"})
        self.assertEqual(r.main(BASE + ["--log", log, "--send", "ATI"]), 0, r.err)
        self.assertNotIn(b"AT&V\r", r.written)
        self.assertTrue(os.path.exists(log))


class LiveRedaction(unittest.TestCase):
    """The tool's output and log never carry the password."""

    def run_wap(self, extra):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        log = os.path.join(tmp.name, "t.log")
        reply = b"AT+WAP?\r\r\nWAP ON AXISMODEM-1234 " + FAKE_PW.encode() + b"\r\nOK\r\n"
        r = Run({b"AT+WAP?\r": reply, b"AT+WSTA=Home," + FAKE_PW.encode() + b"\r": b"AT+WSTA=Home," + FAKE_PW.encode() + b"\r\r\nOK\r\n"})
        code = r.main(BASE + ["--log", log, "--send", "AT+WAP?",
                              "--send", "AT+WSTA=Home," + FAKE_PW] + extra)
        with open(log, encoding="utf-8") as f:
            logged = f.read()
        return code, r, logged

    def test_screen_and_log_are_redacted_by_default(self):
        code, r, logged = self.run_wap([])
        self.assertEqual(code, 0, r.err)
        self.assertNotIn(FAKE_PW, r.out)
        self.assertNotIn(FAKE_PW, logged)
        self.assertIn("WAP ON ********", r.out)
        self.assertIn("WAP ON ********", logged)
        self.assertIn("AT+WSTA=Home,********", logged)
        # The password was still sent to the modem (it is the user's command).
        self.assertIn(FAKE_PW.encode(), r.written)

    def test_allow_secret_shows_the_screen_but_never_the_log(self):
        code, r, logged = self.run_wap(["--allow-secret"])
        self.assertEqual(code, 0, r.err)
        self.assertIn(FAKE_PW, r.out)
        self.assertNotIn(FAKE_PW, logged)
        self.assertIn("WAP ON ********", logged)

    def test_fb_hex_is_never_logged_even_with_allow_secret(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        log = os.path.join(tmp.name, "t.log")
        png = os.path.join(tmp.name, "oled.png")
        r = Run({b"AT+FB\r": FB_REPLY.encode()})
        code = r.main(BASE + ["--log", log, "--allow-secret", "--shot", png])
        self.assertEqual(code, 0, r.err)
        with open(log, encoding="utf-8") as f:
            logged = f.read()
        self.assertNotIn("FB:", logged)
        self.assertNotIn("0000", logged)
        self.assertTrue(os.path.exists(png))


if __name__ == "__main__":
    unittest.main()
