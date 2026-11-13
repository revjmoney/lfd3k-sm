"""Voice bench (task 9b): one recorded AXIS call per tier between two lfd3k-voice-pc copies.

py -3.14 run_call.py SF7 [shot_every_s]

Environment: LFD3K_CALL_WORK (work folder), LFD3K_CALL_SRC (side_a.wav,
side_b.wav, timeline.csv), LEAD_MS (push to talk: wait this long after TALK
goes down before speaking, default 0), RUN_SUFFIX (run folder name
suffix), PORT_ADD (added to the UDP ports).
"""
import csv
import os
import shutil
import subprocess
import sys
import time

# Work folder (runs, outputs) and the source conversation (side_a.wav,
# side_b.wav, timeline.csv: decode docs/voice-bench/call/source/*.flac to
# 16 bit WAV first, for example with `flac -d`).
HERE = os.environ.get("LFD3K_CALL_WORK", os.path.join(os.getcwd(), "call-work"))
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
EXE = os.path.join(REPO, "target", "release", "lfd3k-voice-pc.exe")
CONVO = os.environ.get("LFD3K_CALL_SRC", os.path.join(HERE, "source"))
PTT = {"SF9": 400, "SF10": 400, "SF11": 1000, "SF12": 2000}
PORTS = {"SF5": 4310, "SF7": 4320, "SF9": 4330, "SF12": 4340}
LEN_MS = 117465
LEAD = int(os.environ.get("LEAD_MS", "0"))


def turns():
    with open(os.path.join(CONVO, "timeline.csv"), newline="") as f:
        return [
            (int(r["turn"]), r["speaker"], int(float(r["start_s"]) * 1000), int(float(r["end_s"]) * 1000))
            for r in csv.DictReader(f)
        ]


def head(me, tag):
    return [
        "size 1280 720",
        "play 0 0",
        "wait 1500",
        "until at_idle 15000",
        f"shot {tag}_000_start",
        "at ATI",
        "at AT&V",
        "until at_idle 15000",
    ]


def dave_script(sf, d, every):
    s = head("DAVE", "a")
    s += [
        "scan",
        "until dir:MAYA 60000",
        "until at_idle 30000",
        "at AT+DIR?",
        "until at_idle 15000",
        "screen dir",
        "wait 500",
        "shot a_010_directory",
        "screen calls",
        "wait 500",
        "shot a_011_people",
        "call MAYA",
        "wait 600",
        "shot a_020_calling",
        "until connected 180000",
        "wait 300",
        "shot a_030_words",
        "words_match",
        "until slots 120000",
        "wait 300",
        "shot a_040_connected",
    ]
    s += body(sf, d, "dave", "a", every)
    s += [
        "print",
        "shot a_900_before_hangup",
        "hangup",
        "until idle 60000",
        "wait 400",
        "shot a_910_ended",
        "wait 3500",
        "at AT+DIR?",
        "until at_idle 15000",
        "print",
        "quit",
    ]
    return s


def maya_script(sf, d, every):
    s = head("MAYA", "b")
    s += [
        "until ringing 240000",
        "wait 300",
        "shot b_020_ringing",
        "answer",
        "until connected 180000",
        "wait 300",
        "shot b_030_words",
        "words_match",
        "until slots 120000",
        "wait 300",
        "shot b_040_connected",
    ]
    s += body(sf, d, "maya", "b", every)
    s += [
        "print",
        "until idle 120000",
        "wait 400",
        "shot b_910_ended",
        "wait 3500",
        "at AT+DIR?",
        "until at_idle 15000",
        "print",
        "quit",
    ]
    return s


def body(sf, d, me, tag, every):
    sync = os.path.join(d, "go.txt")
    s = [f"sync {sync} 4000"]
    if sf not in PTT:
        # Duplex: both play their whole side from the same go mark.
        s.append(f"play 0 {LEN_MS}")
        t = every * 1000
        while t <= LEN_MS + 4000:
            s += [f"at_t {t}", f"shot {tag}_{100 + t // 1000:03d}_t{t // 1000:03d}"]
            t += every * 1000
        s.append(f"at_t {LEN_MS + 5000}")
        return s
    frame = PTT[sf]
    for n, who, a, b in turns():
        if who == me:
            # Talk at once when TALK goes down, like a person: the floor
            # comes a moment later (the app keeps what was said meanwhile);
            # let go right after the last word.
            d = b - a + 200
            s += [
                "until floor_free 120000",
                "talk down",
                f"wait {LEAD}",
                f"play {max(0, a - 100)} {b + 100}",
                "wait 300",
                f"shot {tag}_{100 + n:03d}_turn{n:02d}_waiting",
                f"wait {max(0, d // 2 - 300)}",
                f"shot {tag}_{100 + n:03d}_turn{n:02d}_talking",
                f"wait {d - d // 2}",
                "talk up",
            ]
        else:
            s += [
                "until floor_theirs 180000",
                "wait 1500",
                f"shot {tag}_{100 + n:03d}_turn{n:02d}_listening",
            ]
    # Let the last turn arrive (SF12 hears about 5.8 s late).
    s += ["until floor_free 60000", f"wait {3 * frame + 3000}"]
    return s


def main():
    sf = sys.argv[1]
    every = int(sys.argv[2]) if len(sys.argv) > 2 else 2
    d = os.path.join(HERE, sf + os.environ.get("RUN_SUFFIX", ""))
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    pa = PORTS[sf] + int(os.environ.get("PORT_ADD", "0"))
    pb = pa + 1
    procs = []
    for who, name, me, peer, wav, script in (
        ("a", "DAVE", pa, pb, "side_a.wav", dave_script(sf, d, every)),
        ("b", "MAYA", pb, pa, "side_b.wav", maya_script(sf, d, every)),
    ):
        sd = os.path.join(d, who)
        os.makedirs(sd)
        with open(os.path.join(sd, "script.txt"), "w", newline="\n") as f:
            f.write("\n".join(script) + "\n")
        args = [
            EXE, "--name", name, "--label", "PC",
            "--bind", f"127.0.0.1:{me}", "--peer", f"127.0.0.1:{peer}",
            "--station", f"{name}MODEM",
            "--cap-tier", sf,
            "--data", os.path.join(sd, "data"),
            "--play", os.path.join(CONVO, wav),
            "--record", os.path.join(sd, "heard.wav"),
            "--script", os.path.join(sd, "script.txt"),
            "--shots", os.path.join(sd, "shots"),
            "--trace", os.path.join(sd, "trace"),
        ]
        out = open(os.path.join(sd, "log.txt"), "w")
        procs.append(subprocess.Popen(args, stdout=out, stderr=subprocess.STDOUT))
        time.sleep(0.5)
    t0 = time.time()
    for p in procs:
        p.wait(timeout=1500)
    print(sf, "done in", round(time.time() - t0), "s")


if __name__ == "__main__":
    main()
