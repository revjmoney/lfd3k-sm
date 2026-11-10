#!/usr/bin/env python3
"""Bench terminal for the LFD3K-SM on Rev's Heltec V4.

Safety: before anything is opened, the port is looked up passively with
serial.tools.list_ports. It is only opened when it is VID 303A PID 1001 and its
USB serial number (the ESP32-S3 MAC) matches --expect-mac. COM15 and COM9 are
refused by name, always. DTR and RTS are set False before open() and never
touched again.

  python lfd3k_term.py --port COM11 --expect-mac AA:BB:CC:DD:EE:FF --send ATI
  python lfd3k_term.py --port COM11 --expect-mac AA:BB:CC:DD:EE:FF --shot oled.png
"""
import argparse
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

REFUSED = {"COM15", "COM9"}
VID, PID = 0x303A, 0x1001


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


def drain(s, wait_ms, log, stop=None):
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
    sys.stdout.write(text)
    sys.stdout.flush()
    if log:
        log.write(text)
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
    ap.add_argument("--shot", help="send AT+FB and save the OLED as a 4x PNG")
    ap.add_argument("--wait-ms", type=int, default=1000, help="wait after each send (default 1000)")
    ap.add_argument("--selftest", action="store_true", help="check the port filter against fake ports, open nothing")
    a = ap.parse_args(argv)

    if a.selftest:
        return selftest()
    if not a.port or not a.expect_mac:
        ap.error("--port and --expect-mac are required")

    from serial.tools import list_ports
    ok, why = check_port(a.port, a.expect_mac, list_ports.comports())
    if not ok:
        print("REFUSED:", why, file=sys.stderr)
        return 3

    import fb2png
    log = open(a.log, "a", encoding="utf-8") if a.log else None
    s = open_port(a.port)
    try:
        drain(s, 300, log)
        for line in a.send:
            s.write(line.encode("ascii") + b"\r")
            drain(s, a.wait_ms, log)
        if a.shot:
            s.write(b"AT+FB\r")
            text = drain(s, max(a.wait_ms, 2000), log, stop=fb_done)
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
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
