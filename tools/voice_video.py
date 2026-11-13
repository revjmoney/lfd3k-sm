"""Footage for the voice testing video from one recorded call per tier.

Each tier's run folder holds a/ (DAVE) and b/ (MAYA), each with shots/
(the app's `shot` PNGs) and trace/ (`lfd3k-voice-pc --trace`: status.csv,
host.log, marks.csv). This writes into <out>/<tier>/:

  app-a/, app-b/    the app windows, JPEG, scaled to --app-width
  term-a/, term-b/  the modem's host side as an 80x25 CP437 terminal (the
                    AT lines the app sent, the modem's replies, and the
                    result lines of the core's formatter on each call state
                    change), drawn with tools/menu_preview.py's renderer
  panel-a/, panel-b/ the front panel: status.csv replayed through the
                    firmware's own panel_render.c (tools/panel_replay.c)
  timeline.csv      time_s (from the start of this tier's footage, the
                    first status line; file names carry the same time),
                    source, file, what (the call audio row says where
                    call/<tier>.opus starts)

and <out>/timeline.csv with every tier one after the other (time_s from
the start of the first tier's footage; offsets in the README).

  py -3.14 tools/voice_video.py --runs <dir> --out <footage dir, outside the repo>
      --tiers SF5,SF7,SF9,SF12 --t0 <tier>=<unix_ms>,... [--gcc <gcc.exe>]
"""
import argparse
import csv
import os
import re
import subprocess
import sys
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
import fb2png  # noqa: E402
import menu_preview as mp  # noqa: E402

SIDES = {"a": "DAVE", "b": "MAYA"}
STATIONS = {"a": "DAVEMODEM", "b": "MAYAMODEM"}
STATE = {0: "idle", 1: "dialing", 2: "ringing", 3: "training", 4: "online", 5: "online"}
TIERS = ["SF5", "SF6", "SF7", "SF8", "SF9", "SF10", "SF11", "SF12"]


def build_replay(gcc):
    exe = ROOT / "target" / "panel_check" / "panel_replay.exe"
    exe.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env["PATH"] = str(Path(gcc).parent) + os.pathsep + env.get("PATH", "")
    subprocess.run(
        [gcc, "-std=c11", "-O1", "-Wall", "-Wextra", "-Werror",
         "-I", str(ROOT / "firmware" / "src"), "-I", str(ROOT / "firmware" / "include"),
         "-o", str(exe), str(HERE / "panel_replay.c"),
         str(ROOT / "firmware" / "src" / "panel_render.c"),
         str(ROOT / "firmware" / "src" / "idle_show.c")],
        check=True, env=env)
    return exe, env


def panels(exe, env, run, side, out, t0, every_ms):
    d = out / f"panel-{side}"
    d.mkdir(parents=True, exist_ok=True)
    txt = out / f"panel-{side}.fb.txt"
    subprocess.run([str(exe), str(run / side / "trace" / "status.csv"), STATIONS[side], str(txt),
                    str(every_ms)], check=True, env=env, stdout=subprocess.DEVNULL)
    status = {}
    with open(run / side / "trace" / "status.csv", newline="") as f:
        for r in csv.DictReader(f):
            status[int(r["unix_ms"])] = r
    rows = []
    for line in txt.read_text().splitlines():
        u, fb = line.split(" ", 1)
        u = int(u)
        name = f"{(u - t0) / 1000:07.1f}.png"
        (d / name).write_bytes(fb2png.fb_to_png(fb2png.parse_fb(fb)))
        r = status.get(u, {})
        st = STATE.get(int(r.get("state", 0)), "?")
        tier = int(r.get("tier", -1))
        what = st + (f" {TIERS[tier]}" if 0 <= tier < 8 and st != "idle" else "")
        rows.append((u, f"panel {SIDES[side]}", f"panel-{side}/{name}", f"front panel: {what}"))
    txt.unlink()
    return rows


def term_png(lines, status, out):
    mp.ROWS = 25
    t = mp.Term()
    t.strict = False
    t.cells = [[(0x20, 7, 0)] * 80 for _ in range(25)]
    for r, (text, fg) in enumerate(lines[-24:]):
        for c, ch in enumerate(text.encode("cp437", "replace")[:80]):
            t.cells[r][c] = (ch, fg, 0)
    bar = status.encode("cp437", "replace")[:80].ljust(80)
    t.cells[24] = [(ch, 15, 4) for ch in bar]
    mp.render(t, out)


def terminals(run, side, out, t0):
    d = out / f"term-{side}"
    d.mkdir(parents=True, exist_ok=True)
    events = []
    for line in (run / side / "trace" / "host.log").read_text(encoding="utf-8").splitlines():
        m = re.match(r"(\d+) ([<>!]) (.*)", line)
        if m:
            events.append((int(m[1]), m[2], m[3]))
    # Everything the log holds: the app's own settings read at start (ATI,
    # AT&V, AT+FWD?, AT+WAP? with the password masked, AT+WSTA?), then the
    # script's lines and the call's result lines.
    lines = []
    rows = []
    i = 0
    while i < len(events):
        u = events[i][0]
        j = i
        while j < len(events) and events[j][0] - u <= 300:
            u2, k, text = events[j]
            if k == ">":
                lines.append((text, 15))
            elif k == "!":
                lines.append(("", 7))
                lines.append((text, 14))
            else:
                lines.append((text, 11 if text in ("OK", "ERROR") else 7))
            j += 1
        name = f"{(u - t0) / 1000:07.1f}.png"
        left = f" {STATIONS[side]}  SimNet"
        right = "LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM "
        bar = f"{left:<{80 - len(right)}}{right}"
        term_png(lines, bar, d / name)
        last = [x for x in lines if x[0]][-1][0]
        first = events[i][2] if events[i][1] == ">" else last
        rows.append((u, f"terminal {STATIONS[side]}", f"term-{side}/{name}", f"host side: {first}"))
        i = j
    return rows


def apps(run, side, out, t0, width):
    d = out / f"app-{side}"
    d.mkdir(parents=True, exist_ok=True)
    rows = []
    for p in sorted((run / side / "shots").glob("*.png")):
        u = int(p.stat().st_mtime * 1000)
        im = Image.open(p).convert("RGB")
        h = round(im.height * width / im.width)
        im = im.resize((width, h), Image.LANCZOS)
        name = f"{(u - t0) / 1000:07.1f}_{p.stem}.jpg"
        im.save(d / name, quality=78, optimize=True)
        what = re.sub(r"^[ab]_\d+_", "", p.stem).replace("_", " ")
        rows.append((u, f"app {SIDES[side]}", f"app-{side}/{name}", f"app window: {what}"))
    return rows


def audio_rows(run, t0, tier):
    rows = [(t0, "audio", f"call/{tier}.opus", "call audio starts (left: what DAVE hears, right: what MAYA hears)")]
    go = int((run / "go.txt").read_text())
    rows.append((go, "audio", f"call/{tier}.opus", "both mics start the AXIS conversation (go mark)"))
    for side in "ab":
        with open(run / side / "trace" / "marks.csv", newline="") as f:
            for r in csv.DictReader(f):
                if r["what"] == "play" and r["a"] != r["b"]:
                    rows.append((int(r["unix_ms"]), "audio", f"call/{tier}.opus",
                                 f"{SIDES[side]} speaks ({r['a']} to {r['b']} ms of side_{side})"))
    return rows


def rel(fl, call_prefix, prefix):
    """A file name as seen from a timeline: call audio lives in docs/voice-bench/call."""
    return call_prefix + fl if fl.startswith("call/") else prefix + fl


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--tiers", default="SF5,SF7,SF9,SF12")
    ap.add_argument("--t0", required=True, help="tier=unix_ms of the first audio sample, comma separated")
    ap.add_argument("--gcc", default=r"D:\onerous\msys64\mingw64\bin\gcc.exe")
    ap.add_argument("--app-width", type=int, default=960)
    ap.add_argument("--panel-ms", default="SF7=1000", help="tier=ms between panel frames (default 2000)")
    a = ap.parse_args()
    exe, env = build_replay(a.gcc)
    t0s = dict((k, int(v)) for k, v in (x.split("=") for x in a.t0.split(",")))
    pms = dict((k, int(v)) for k, v in (x.split("=") for x in a.panel_ms.split(",") if x))
    out_all = []
    offset = 0.0
    for tier in a.tiers.split(","):
        run = Path(a.runs) / tier
        out = Path(a.out) / tier
        out.mkdir(parents=True, exist_ok=True)
        audio0 = t0s[tier]
        # Times count from the first status line of either copy (the start
        # of this tier's footage).
        t0 = min(int(next(csv.DictReader(open(run / s / "trace" / "status.csv")))["unix_ms"]) for s in "ab")
        rows = audio_rows(run, audio0, tier)
        for side in "ab":
            rows += apps(run, side, out, t0, a.app_width)
            rows += terminals(run, side, out, t0)
            rows += panels(exe, env, run, side, out, t0, pms.get(tier, 2000))
        rows.sort(key=lambda r: (r[0], r[1]))
        first = rows[0][0]
        with open(out / "timeline.csv", "w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow(["time_s", "source", "file", "what"])
            for u, s, fl, what in rows:
                w.writerow([f"{(u - t0) / 1000:.2f}", s, rel(fl, "../../", ""), what])
        last = rows[-1][0]
        for u, s, fl, what in rows:
            out_all.append((offset + (u - first) / 1000, f"{tier} {s}", rel(fl, "../", f"{tier}/"), what))
        print(f"{tier}: {len(rows)} rows, footage offset {offset:.1f} s (call audio at {offset + (audio0 - first) / 1000:.1f} s)")
        offset += (last - first) / 1000 + 2.0
    with open(Path(a.out) / "timeline.csv", "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["time_s", "source", "file", "what"])
        for t, s, fl, what in out_all:
            w.writerow([f"{t:.2f}", s, fl, what])


if __name__ == "__main__":
    main()
