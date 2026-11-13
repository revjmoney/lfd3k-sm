# Voice bench: the recorded test call

Voice part 1, Task 9b (2026-11-13). A real call between two copies of the PC voice app `lfd3k-voice-pc` (release build) on this PC, each with its own built-in LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM core on SimNet (the simulated radio over UDP on 127.0.0.1, two ports per tier). DAVE/PC (modem DAVEMODEM) calls MAYA/PC (modem MAYAMODEM) by name; both press WORDS MATCH; the FLOWTRON SCRAMBLER is on, as always. One call per tier, both modems capped at the tier with `--cap-tier` (S21), SimNet SNR 8 dB, fresh data folders each time (new keys, so every call starts NOT CHECKED).

The two mics are the two sides of a two speaker conversation about the AXIS BBS (`source/`, synthetic speech, see below): DAVE's app plays `side_a` as its mic (`--play`), MAYA's plays `side_b`, and each app records what it hears (`--record`). The radio version of this recording comes in part 2 with the boards.

## Files

| File | What |
|---|---|
| `SF5.opus`, `SF7.opus`, `SF9.opus`, `SF12.opus` | The call, stereo 48 kHz Ogg Opus streams at 32 kbps (from 16 kHz recordings) (WAV would be 8 to 16 MB each). Left: what DAVE heard of MAYA. Right: what MAYA heard of DAVE. Both recordings sit on one wall clock, from the first sample either app recorded (the call setup and its connection sounds) to the end of the NO CARRIER sounds |
| `source/mix.opus` | The original conversation, both speakers in one mono track (117.5 s) |
| `source/side_a.flac`, `source/side_b.flac` | Dave's and Maya's sides on one timeline (the other side silent while one speaks), 16 kHz mono |
| `source/script.md`, `source/timeline.csv` | The lines, and when each turn starts and ends |

## Results

| Tier | Codec | Mode | DAVE to MAYA: sent / heard / lost | MAYA to DAVE: sent / heard / lost | Concealed (MAYA / DAVE) | Mouth to ear DAVE to MAYA (match) | Mouth to ear MAYA to DAVE (match) | Recording |
|---|---|---|---|---|---|---|---|---|
| SF5 | Opus 10 kbps | Call (duplex) | 1060 / 1059 / 1 | 1060 / 1059 / 1 | 0 / 0 | 280 ms (0.98) | 300 ms (0.97) | 133.0 s |
| SF7 | Codec2 2400 | Call (duplex) | 636 / 635 / 1 | 636 / 635 / 1 | 1 / 0 | 460 ms (0.93) | 500 ms (0.97) | 132.8 s |
| SF9 | Codec2 1200 | Push to talk | 185 / 185 / 0 | 120 / 120 / 0 | 0 / 0 | 900 ms (0.95) | 970 ms (0.95) | 168.9 s |
| SF12 | Codec2 700C | Push to talk, delayed | 40 / 40 / 0 | 27 / 27 / 0 | 0 / 0 | 5,780 ms (0.90) | 5,770 ms (0.83) | 242.3 s |

- Frames: voice frames of the plan's length counted by the two apps (sent by one, heard by the other). Lost is sent minus heard; a frame sent as the call hangs up counts as lost. Concealed: frames the far app filled in.
- Mouth to ear: the lag at which the loudness envelope of what the far ear recorded best matches what this mouth said (both on the wall clock, 10 ms steps); match is that correlation (-1 to 1). Every turn of both speakers was found in the far ear on every tier (`tools/voice_call_bench/analyze.py` gives the level of each turn, `turn_gaps.py` which tenths of a second came through, with the lag of each turn); see below for one SF12 turn.
- On the duplex tiers (SF5, SF7) both mics start the whole conversation at one go mark, so the call runs on the conversation's own timing. On the push to talk tiers (SF9, SF12) the conversation runs turn by turn: each speaker waits until the screen shows the floor free, presses TALK and starts that turn of their side at once (before the floor is theirs), and lets go just after the last word. The recording is therefore stretched on these tiers by the floor hand-overs (each turn waits for the floor to come free and then to come to the new speaker).

What it sounds like, and what was found:

- SF5 and SF7: the whole conversation comes through on both sides; the one frame lost each way is the frame sent as DAVE hung up. These two were recorded before the push to talk fix below; it does not touch the duplex path.
- SF9 and SF12 (recorded after the fix): every frame sent is heard, both ways, and every turn comes through on both sides. Each speaker presses TALK and starts talking at once, as a person would, before the floor is theirs.
- The fix (Task 9b review): the first recordings lost words at the edges of push to talk turns (SF12: 12 of 48 and 3 of 34 frames). Two causes, both fixed. (1) The modem gave the floor away as soon as the holder's key went up and dropped the voice frames still queued, which were the holder's last words (a frame finished just after the holder's send time waits a whole slot, so one or two frames were often still queued). The holder now keeps the floor until its queue is empty, and the push to talk queue holds 6 frames (duplex keeps 2). (2) The app threw away what was said between pressing TALK and getting the floor (up to about 6 s for the answerer at SF12: its key reaches the caller in its listener slot, every second slot, and the caller announces the change in the next slot). The app now keeps up to 3 frames of it and sends them first when the floor comes; if TALK goes up before the floor came, the modem keeps the key down until those words are out. Regression tests: `regression_ptt_alternating_turns_lose_no_words_sf9` and `_sf12` in `sim/tests/voice_calls.rs`.
- Mouth to ear on push to talk now includes the wait for the floor (the far end hears the turn from its first word, that much later): about 0.9 s at SF9 and 5.8 s at SF12, the same both ways. Before the fix SF12 read 3.9 s one way and 5.8 s the other: a frame waits one whole slot (2 s at SF12) when it is finished just after the holder's send time in the slot; the answerer gets the floor one caller frame into a slot, so its frames always waited, while the caller's went out at once. With the words kept from TALK both sides start their turn with frames queued, so both see the longer delay. One SF12 turn of MAYA's (turn 4) waited longer than 6 s for the floor and lost the part of its first words said before the last 6 s of the wait.
- Known limit of SimNet: ANSWERED ELSEWHERE with a third copy cannot be recorded (a SimNet modem has one UDP peer); see `../pc/README.md`.

## How it was made

1. `cargo build --release -p lfd3k-voice-pc`.
2. Decode `source/side_a.flac`, `side_b.flac` to 16 bit WAV (`flac -d`) and put them with `source/timeline.csv` in one folder (`LFD3K_CALL_SRC`).
3. `py -3.14 tools/voice_call_bench/run_call.py SF7 1` (and SF5, SF9, SF12) writes the two scripts, starts both copies with `--name DAVE`/`MAYA --label PC --cap-tier SF7 --play side_x.wav --record heard.wav --script ... --shots ... --trace ...` and waits for both. The scripts use the app's test helpers: `play 0 0` (mic silent before the go mark), `scan`, `call MAYA`, `answer`, `words_match`, `sync <file>` (both copies meet at one wall clock go mark through a file), `play <from_ms> <to_ms>`, `talk down`/`up` with `until floor_free`/`floor_mine`/`floor_theirs` on the push to talk tiers, `at_t` and `shot` for the footage, `print` (the frame counters), `hangup`.
4. `py -3.14 tools/voice_call_bench/analyze.py SF7 <out>` puts the two `--record` WAVs on the wall clock (from the `rec` and `play` marks in each copy's `--trace` `marks.csv`), writes the stereo WAV and measures the table above; `ffmpeg -i SF7.wav -c:a libopus -b:a 32k SF7.opus` makes the file here. `turn_gaps.py SF7` shows which tenths of a second of each turn reached the far ear.

## The source conversation

`source/` is synthetic speech generated on this PC with Bark (suno/bark, MIT license) from a script written for this test, using Bark's stock speaker presets `v2/en_speaker_6` (Dave) and `v2/en_speaker_9` (Maya). Dave and Maya are invented; no real person's voice was recorded or cloned. See THIRD_PARTY.md.
