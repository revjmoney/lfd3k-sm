#!/usr/bin/env python3
"""Bench terminal for the LFD3K-SM on Rev's Heltec V4.

Safety: before anything is opened, the port is looked up passively with
serial.tools.list_ports. It is only opened when it is VID 303A PID 1001 and its
USB serial number (the ESP32-S3 MAC) matches --expect-mac. COM15 and COM9 are
refused by name, always. DTR and RTS are set False before open() and never
touched again.

  python lfd3k_term.py --port COM11 --expect-mac AA:BB:CC:DD:EE:FF --send ATI
  python lfd3k_term.py --port COM11 --expect-mac AA:BB:CC:DD:EE:FF --shot-safe --shot oled.png

Secrets: the hotspot password shows on the OLED while AT&V says PWSHOW 1, and AT+WAP?
always prints it. This tool therefore
  * redacts what it prints and logs: the rest of a "WAP " line after ON or OFF,
    everything after "PW " on any line, and everything after the first comma of an
    echoed AT+WAP= or AT+WSTA= line (spaces around + and =, and any case, included), all
    shown as ********. (--allow-secret turns redaction off for the screen only; the log
    is always redacted, and an AT+FB dump is never written to the log.)
  * refuses --shot unless --shot-safe is given (it reads AT&V first and only sends
    AT+FB when it sees PWSHOW 0) or --allow-secret is given.
  * refuses to write a --shot or --log file inside the repo's docs folder unless
    AT&V says PWSHOW 0 (checked quietly before anything is written).
"""
import argparse
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

REFUSED = {"COM15", "COM9"}
VID, PID = 0x303A, 0x1001
MASK = "********"
SECRET_CMD = re.compile(r"at\s*\+\s*(?:wap|wsta)\s*=", re.I)
DOCS_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs")


def redact_line(line):
    """Hide passwords in one line of modem output (no line ending in it)."""
    # The core ignores spaces around "+" and "=" and the case, so this does too.
    m = SECRET_CMD.search(line)
    if m:
        j = line.find(",", m.end())
        if j >= 0:
            line = line[: j + 1] + MASK
    if line.startswith("WAP "):
        # WAP <ON|OFF> <ssid> <password>: an ssid may hold spaces, so everything after
        # ON or OFF is masked (a leaked half password is worse than a hidden name).
        parts = line.split(" ", 2)
        if len(parts) > 1:
            line = parts[0] + " " + parts[1] + " " + MASK
    i = line.find("PW ")
    if i >= 0:
        line = line[: i + 3] + MASK
    return line


class Redactor:
    """Line buffered: a partial line is held until its end arrives, so a password
    split across two reads is still caught."""

    def __init__(self):
        self.buf = ""

    def feed(self, text):
        self.buf += text
        out = []
        while True:
            m = re.search(r"\r\n|\r|\n", self.buf)
            if not m:
                break
            out.append(redact_line(self.buf[: m.start()]) + m.group(0))
            self.buf = self.buf[m.end():]
        return "".join(out)

    def flush(self):
        out, self.buf = redact_line(self.buf), ""
        return out


def parse_pwshow(text):
    """1 or 0 from a PWSHOW line of AT&V output, None when there is none."""
    m = re.search(r"^PWSHOW ([01])\s*$", text or "", re.M)
    return int(m.group(1)) if m else None


def in_docs(path):
    """True when path lies inside the repo's docs folder."""
    try:
        p = os.path.realpath(os.path.abspath(path))
        d = os.path.realpath(DOCS_DIR)
        return os.path.commonpath([p, d]) == d
    except ValueError:
        return False


def norm_mac(s):
    return re.sub(r"[^0-9A-Fa-f]", "", s or "").upper()


def check_port(port, expect_mac, ports):
    """Return (ok, reason). `ports` is an iterable of list_ports entries."""
    name = (port or "").strip().upper()
    if name in REFUSED:
        return False, "%s is refused by name" % name
    want = norm_mac(expect_mac)
    if len(want) != 12:
        return False, "--expect-mac must be 6 bytes (XX:XX:XX:XX:XX:XX)"
    for p in ports:
        if (p.device or "").upper() != name:
            continue
        if p.vid != VID or p.pid != PID:
            return False, "%s is not VID 303A PID 1001" % name
        if norm_mac(p.serial_number) != want:
            return False, "%s has USB serial %s, not the expected MAC" % (name, p.serial_number)
        return True, "ok"
    return False, "%s not found in the port list" % name


def open_port(port):
    import serial
    s = serial.Serial()
    s.port = port
    s.baudrate = 115200
    s.dtr = False
    s.rts = False
    s.timeout = 0.1
    s.open()
    return s


def drain(s, wait_ms, log, stop=None, red=None, show=True, allow_secret=False):
    end = time.time() + wait_ms / 1000.0
    buf = b""
    while time.time() < end:
        d = s.read(4096)
        if d:
            buf += d
            end = max(end, time.time() + 0.15)
            if stop and stop(buf):
                break
    text = buf.decode("ascii", errors="replace")
    if show:
        sys.stdout.write(text if allow_secret or red is None else red[0].feed(text))
        sys.stdout.flush()
    if log:
        log.write(red[1].feed(text) if red else text)
        log.flush()
    return text


def fb_done(buf):
    i = buf.find(b"FB:")
    return i >= 0 and b"OK" in buf[i:]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", help="COM port, for example COM11")
    ap.add_argument("--expect-mac", help="V4 MAC, the USB serial number, XX:XX:XX:XX:XX:XX")
    ap.add_argument("--send", action="append", default=[], help="line to send (CR added); repeatable")
    ap.add_argument("--log", help="append everything to this file")
    ap.add_argument("--shot", help="send AT+FB and save the OLED as a 4x PNG (needs --shot-safe or --allow-secret)")
    ap.add_argument("--shot-safe", action="store_true",
                    help="with --shot: read AT&V first and only send AT+FB when it says PWSHOW 0")
    ap.add_argument("--allow-secret", action="store_true",
                    help="show the hotspot password on screen and allow --shot without --shot-safe")
    ap.add_argument("--wait-ms", type=int, default=1000, help="wait after each send (default 1000)")
    ap.add_argument("--selftest", action="store_true", help="check the port filter against fake ports, open nothing")
    a = ap.parse_args(argv)

    if a.selftest:
        return selftest()
    if not a.port or not a.expect_mac:
        ap.error("--port and --expect-mac are required")

    if a.shot and not (a.shot_safe or a.allow_secret):
        print("REFUSED: --shot may capture the hotspot password; use --shot-safe "
              "(checks AT&V for PWSHOW 0) or --allow-secret", file=sys.stderr)
        return 4
    if a.shot_safe and not a.shot:
        ap.error("--shot-safe needs --shot <file>")

    from serial.tools import list_ports
    ok, why = check_port(a.port, a.expect_mac, list_ports.comports())
    if not ok:
        print("REFUSED:", why, file=sys.stderr)
        return 3

    import fb2png
    log = None
    s = open_port(a.port)
    try:
        drain(s, 300, None, show=False)
        need_check = a.shot_safe or any(in_docs(p) for p in (a.shot, a.log) if p)
        if need_check:
            s.write(b"AT&V\r")
            pw = parse_pwshow(drain(s, max(a.wait_ms, 1500), None, show=False))
            if pw != 0:
                print("REFUSED: AT&V says PWSHOW %s; the hotspot password is on screen or "
                      "unknown. Nothing written." % ("1" if pw == 1 else "missing"), file=sys.stderr)
                return 5
        log = open(a.log, "a", encoding="utf-8") if a.log else None
        red = (Redactor(), Redactor())
        for line in a.send:
            if line == "+++":
                # The escape: guard time of silence on both sides, no CR.
                drain(s, 1500, log, red=red, allow_secret=a.allow_secret)
                s.write(b"+++")
            else:
                s.write(line.encode("ascii") + b"\r")
            drain(s, a.wait_ms, log, red=red, allow_secret=a.allow_secret)
        if a.shot:
            s.write(b"AT+FB\r")
            # The framebuffer hex holds the PW row as pixels: it never goes to the log.
            text = drain(s, max(a.wait_ms, 2000), None, stop=fb_done, red=red, show=False)
            if log:
                log.write("AT+FB dump omitted from the log\n")
                log.flush()
            try:
                png = fb2png.fb_to_png(fb2png.parse_fb(text))
            except ValueError as e:
                print("\nshot failed:", e, file=sys.stderr)
                return 1
            with open(a.shot, "wb") as f:
                f.write(png)
            print("\nwrote", a.shot)
    finally:
        s.close()
        if log:
            log.write(red[1].flush())
            log.close()
    return 0


class _P:
    def __init__(self, device, vid, pid, sn):
        self.device, self.vid, self.pid, self.serial_number = device, vid, pid, sn


def selftest():
    """Port filter against fake ports. Opens nothing."""
    mac = "AA:BB:CC:DD:EE:FF"
    ports = [_P("COM11", VID, PID, "aa:bb:cc:dd:ee:ff"), _P("COM15", VID, PID, mac),
             _P("COM9", 0x1A86, 0x7523, None), _P("COM4", VID, PID, "11:22:33:44:55:66"),
             _P("COM5", 0x10C4, 0xEA60, mac)]
    cases = [("COM11", mac, True), ("com11", "AABBCCDDEEFF", True), ("COM15", mac, False),
             ("COM9", mac, False), ("COM4", mac, False), ("COM5", mac, False),
             ("COM77", mac, False), ("COM11", "AA:BB", False)]
    bad = 0
    for port, m, want in cases:
        ok, why = check_port(port, m, ports)
        bad += ok != want
        print("%s %-6s %-17s -> %s (%s)" % ("pass" if ok == want else "FAIL", port, m, ok, why))
    # redaction and the PWSHOW parser (pure text, no port)
    fake = "EXAMPLEPASS1"
    red_cases = [("WAP ON AXISMODEM-1234 " + fake, "WAP ON ********"),
                 ("WAP OFF my net " + fake, "WAP OFF ********"),
                 ("AT+WAP=AXIS," + fake, "AT+WAP=AXIS,********"),
                 ("at+wsta=Home," + fake, "at+wsta=Home,********"),
                 ("PW " + fake, "PW ********"),
                 ("PWSHOW 1", "PWSHOW 1"), ("WSTA Home ********", "WSTA Home ********"),
                 ("OK", "OK")]
    for src, want in red_cases:
        got = redact_line(src)
        bad += got != want or fake in got
        print("%s redact %r -> %r" % ("pass" if got == want else "FAIL", src, got))
    r = Redactor()
    out = r.feed("AT+WAP=AXIS,EXAMPLE") + r.feed("PASS1\r\nOK\r\nWAP ON A " + fake) + r.flush()
    ok = fake not in out and "EXAMPLE" not in out
    bad += not ok
    print("%s redact across reads %r" % ("pass" if ok else "FAIL", out))
    for text, want in (("OK\r\nPWSHOW 0\r\nPUB NONE\r\n", 0), ("PWSHOW 1\r\n", 1), ("OK\r\n", None)):
        got = parse_pwshow(text)
        bad += got != want
        print("%s PWSHOW %r -> %r" % ("pass" if got == want else "FAIL", text, got))
    for path, want in ((os.path.join(DOCS_DIR, "x.png"), True),
                       (os.path.join(DOCS_DIR, "a", "..", "..", "x.png"), False),
                       (os.path.join(DOCS_DIR, "..", "target", "x.png"), False)):
        got = in_docs(path)
        bad += got != want
        print("%s in_docs %s -> %s" % ("pass" if got == want else "FAIL", path, got))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
