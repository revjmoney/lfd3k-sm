"""Voice bench (task 9b, Step 6): the app screenshots (DAVE/PC scripted, MAYA/PC auto-answer;
then DAVE/PC links a second device DAVE's NEW/PC2).

py -3.14 run_shots.py tour|link
"""
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
D = os.path.join(HERE, "pc")
# The data folders show on the About tab: keep them somewhere neutral.
DATA = os.environ.get("LFD3K_SHOTS_DATA", os.path.join(D, "data"))

TOUR = """
size 1280 720
play 0 0
wait 2000
until at_idle 15000
shot 01-calls-idle
scan
until dir:MAYA 90000
until at_idle 30000
wait 500
shot 02-calls-people
screen dir
wait 300
shot 03-directory
screen calls
call MAYA
wait 400
shot 04-calling
until connected 90000
wait 300
shot 05-not-checked-words
words_match
until slots 30000
play 0 117465
until heard:40 60000
wait 2000
shot 06-call-duplex-checked
mute on
wait 1500
shot 07-muted
mute off
wait 2000
print
fake conflict
wait 400
shot 30-conflict-screen-only
hangup
until idle 20000
wait 800
shot 08-call-ended
fake full_dir
wait 300
shot 10-calls-people-paged
screen dir
wait 300
shot 11-directory-paged
fake real_dir
wait 300
tab radio
until at_idle 10000
wait 300
shot 13-settings-radio
set ATS30=5
until at_idle 10000
wait 300
shot 14-settings-radio-s30-warning-unsaved
tab privacy
set AT+BEACON=QUIET
until at_idle 10000
wait 300
shot 15-settings-privacy-quiet-unsaved
tab wifi
fill sta_ssid SHOPWIFI
wait 2500
shot 16-settings-wifi
tab forwards
set AT+FWD=AXIS,192.168.4.20,23,T
until at_idle 10000
wait 300
shot 17-settings-forwards-added-unsaved
tab voice
until at_idle 10000
wait 300
shot 18-settings-voice-keys
tab about
sounds off
wait 300
shot 20-settings-about-sounds-off
sounds on
save
until saved 15000
wait 800
shot 21-settings-saved
at AT&V
until at_idle 10000
tab about
wait 800
shot 22-settings-about-atv-after-save
tab radio
wait 300
shot 23-settings-radio-saved-s30
tab forwards
wait 300
shot 24-settings-forwards-saved
set ATS30=2
set AT+BEACON=ON
set AT+FWD=AXIS
until at_idle 10000
save
until saved 15000
screen calls
fake answered_elsewhere
wait 300
shot 31-fake-answered-elsewhere
fake declined
wait 300
shot 32-fake-declined
print
quit
"""

LINK_A = """
size 1280 720
play 0 0
wait 2000
until at_idle 15000
tab voice
wait 300
link_offer
until link_code 10000
wait 300
shot 40-link-code-shown
link_code_file {code}
until ringing 120000
wait 300
shot 41-link-ringing-from-new-device
answer
until connected 60000
until linked 60000
wait 800
shot 42-linked-first-device
tab voice
wait 500
shot 43-voice-keys-after-link
until idle 120000
quit
"""

LINK_C = """
size 1280 720
play 0 0
wait 2000
until at_idle 15000
tab voice
wait 6000
link_join_file {code}
wait 500
shot 50-joiner-joining
screen calls
scan
until dir:DAVE 90000
call DAVE/PC
until connected 60000
wait 300
shot 51-joiner-in-call
talk_anyway
until linked 60000
wait 800
shot 52-joiner-linked
tab voice
wait 500
shot 53-joiner-voice-keys-linked
hangup
until idle 30000
wait 1000
quit
"""


def launch(name, label, me, peer, script, extra, sub):
    sd = os.path.join(D, sub)
    os.makedirs(sd, exist_ok=True)
    sp = os.path.join(sd, "script.txt")
    with open(sp, "w", newline="\n") as f:
        f.write(script.strip() + "\n")
    args = [EXE, "--name", name, "--label", label, "--bind", f"127.0.0.1:{me}",
            "--peer", f"127.0.0.1:{peer}", "--station", f"{name}{label}MODEM"[:16],
            "--data", os.path.join(DATA, sub), "--script", sp, "--script-unsafe",
            "--shots", os.path.join(D, "shots"), "--trace", os.path.join(sd, "trace")] + extra
    return subprocess.Popen(args, stdout=open(os.path.join(sd, "log.txt"), "w"), stderr=subprocess.STDOUT)


def main():
    what = sys.argv[1]
    os.makedirs(D, exist_ok=True)
    if what == "tour":
        for s in ("tour-a", "tour-b"):
            shutil.rmtree(os.path.join(D, s), ignore_errors=True)
            shutil.rmtree(os.path.join(DATA, s), ignore_errors=True)
        b = launch("MAYA", "PC", 4351, 4350, "size 1280 720\nwait 600000\n",
                   ["--auto-answer", "--play", os.path.join(CONVO, "side_b.wav")], "tour-b")
        time.sleep(0.5)
        a = launch("DAVE", "PC", 4350, 4351, TOUR,
                   ["--play", os.path.join(CONVO, "side_a.wav")], "tour-a")
        a.wait(timeout=900)
        b.kill()
    else:
        for s in ("link-a", "link-c"):
            shutil.rmtree(os.path.join(D, s), ignore_errors=True)
            shutil.rmtree(os.path.join(DATA, s), ignore_errors=True)
        code = os.path.join(D, "code.txt")
        if os.path.exists(code):
            os.remove(code)
        a = launch("DAVE", "PC", 4360, 4361, LINK_A.format(code=code), [], "link-a")
        time.sleep(0.5)
        c = launch("NEW", "PC2", 4361, 4360, LINK_C.format(code=code), [], "link-c")
        a.wait(timeout=900)
        c.wait(timeout=300)


if __name__ == "__main__":
    main()
