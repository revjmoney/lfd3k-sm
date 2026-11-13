"""Voice bench (task 9b): build the stereo call WAV and measure one tier's run.

py -3.14 analyze.py SF7 [out_dir]
Writes <out>/SF7.wav (stereo, left: what DAVE heard of MAYA, right: what MAYA
heard of DAVE), and prints a JSON line with the measurements.
"""
import csv
import json
import os
import re
import sys
import wave

import numpy as np

# Work folder (runs, outputs) and the source conversation (side_a.wav,
# side_b.wav, timeline.csv: decode docs/voice-bench/call/source/*.flac to
# 16 bit WAV first, for example with `flac -d`).
HERE = os.environ.get("LFD3K_CALL_WORK", os.path.join(os.getcwd(), "call-work"))
CONVO = os.environ.get("LFD3K_CALL_SRC", os.path.join(HERE, "source"))
SR = 16000


def rd(p):
    with wave.open(p) as w:
        assert w.getframerate() == SR and w.getnchannels() == 1
        return np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32)


def marks(p):
    with open(p, newline="") as f:
        return list(csv.DictReader(f))


def stats(log):
    lines = [l for l in open(log) if l.startswith("[state] loop")]
    out = []
    for l in lines:
        m = re.search(r"sent (\d+) heard (\d+) dropped (\d+) concealed (\d+)", l)
        p = re.search(r"plan (.*?) words", l)
        out.append({"sent": int(m[1]), "heard": int(m[2]), "dropped": int(m[3]),
                    "concealed": int(m[4]), "plan": p[1].strip() if p else ""})
    return out


def env(x, hop=160):
    n = len(x) // hop
    e = np.sqrt((x[: n * hop].reshape(n, hop) ** 2).mean(axis=1) + 1e-9)
    return np.log10(e + 1.0)


def best_lag(src, rec, max_ms=8000):
    """Lag (ms, 10 ms steps) where the envelope of rec best matches src."""
    a, b = env(src), env(rec)
    n = min(len(a), len(b))
    best = (-2.0, 0)
    for lag in range(0, max_ms // 10):
        if lag >= n - 100:
            break
        x, y = a[: n - lag], b[lag:n]
        if x.std() < 1e-6 or y.std() < 1e-6:
            continue
        c = float(np.corrcoef(x, y)[0, 1])
        if c > best[0]:
            best = (c, lag * 10)
    return best[1], best[0]


def main():
    sf = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, "out")
    os.makedirs(out, exist_ok=True)
    d = os.path.join(HERE, sf)
    side = {"a": rd(os.path.join(CONVO, "side_a.wav")), "b": rd(os.path.join(CONVO, "side_b.wav"))}
    rec, rec0, mic = {}, {}, {}
    for w in "ab":
        m = marks(os.path.join(d, w, "trace", "marks.csv"))
        rec[w] = rd(os.path.join(d, w, "heard.wav"))
        rec0[w] = int(next(r for r in m if r["what"] == "rec")["unix_ms"])
        mic[w] = [(int(r["unix_ms"]), int(r["a"]), int(r["b"])) for r in m if r["what"] == "play" and r["b"] != r["a"]]
    t0 = min(rec0.values())
    t1 = max(rec0[w] + len(rec[w]) * 1000 // SR for w in "ab")
    n = (t1 - t0) * SR // 1000
    # The two recordings on one wall clock (left: DAVE's ear, right: MAYA's).
    st = np.zeros((n, 2), dtype=np.float32)
    for ch, w in enumerate("ab"):
        o = (rec0[w] - t0) * SR // 1000
        k = min(len(rec[w]), n - o)
        st[o : o + k, ch] = rec[w][:k]
    # What each mouth said, on the same clock.
    said = {w: np.zeros(n, dtype=np.float32) for w in "ab"}
    for w in "ab":
        for u, a, b in mic[w]:
            o = (u - t0) * SR // 1000
            seg = side[w][a * 16 : b * 16]
            k = max(0, min(len(seg), n - o))
            said[w][o : o + k] += seg[:k]
    lag_ab, c_ab = best_lag(said["a"], st[:, 1])  # DAVE to MAYA's ear
    lag_ba, c_ba = best_lag(said["b"], st[:, 0])  # MAYA to DAVE's ear
    # Per-turn level in the ear (turn window shifted by the lag) against
    # the same ear while the other side was silent.
    def turn_levels(w, ear, lag):
        x = st[:, ear]
        s = said[w]
        on = np.abs(s) > 300
        lv = []
        # Turns: runs of speech in `said` separated by at least 300 ms.
        idx = np.flatnonzero(on)
        if len(idx) == 0:
            return []
        runs, start, last = [], idx[0], idx[0]
        for i in idx[1:]:
            if i - last > 0.3 * SR:
                runs.append((start, last))
                start = i
            last = i
        runs.append((start, last))
        sh = lag * SR // 1000
        for a, b in runs:
            if b - a < 0.4 * SR:
                continue
            seg = x[a + sh : b + sh]
            lv.append(round(float(np.sqrt((seg ** 2).mean())) if len(seg) else 0.0, 1))
        return lv
    lv_ab = turn_levels("a", 1, lag_ab)
    lv_ba = turn_levels("b", 0, lag_ba)
    sa, sb = stats(os.path.join(d, "a", "log.txt")), stats(os.path.join(d, "b", "log.txt"))
    fa, fb = sa[-1], sb[-1]
    fa["plan"] = sa[0]["plan"]
    pcm = np.clip(st, -32768, 32767).astype(np.int16)
    p = os.path.join(out, f"{sf}.wav")
    with wave.open(p, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(pcm.tobytes())
    go = int(open(os.path.join(d, "go.txt")).read())
    res = {
        "tier": sf, "plan": fa["plan"], "seconds": round(n / SR, 1), "go_s": round((go - t0) / 1000, 2),
        "dave_sent": fa["sent"], "maya_heard": fb["heard"], "maya_sent": fb["sent"], "dave_heard": fa["heard"],
        "lost_d2m": fa["sent"] - fb["heard"], "lost_m2d": fb["sent"] - fa["heard"],
        "concealed_dave": fa["concealed"], "concealed_maya": fb["concealed"],
        "dropped_dave": fa["dropped"], "dropped_maya": fb["dropped"],
        "m2e_dave_to_maya_ms": lag_ab, "match_d2m": round(c_ab, 2),
        "m2e_maya_to_dave_ms": lag_ba, "match_m2d": round(c_ba, 2),
        "turn_rms_in_maya_ear": lv_ab, "turn_rms_in_dave_ear": lv_ba,
        "t0_unix": t0,
    }
    print(json.dumps(res))
    with open(os.path.join(out, f"{sf}.json"), "w") as f:
        json.dump(res, f, indent=1)


if __name__ == "__main__":
    main()
