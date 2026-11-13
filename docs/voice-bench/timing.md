# Voice bench: timing table

Voice part 1, Task 9a (2026-11-13). Two voice apps on two simulated modems (`lfd3k-voicesim`, release build): ANN/PC on modem ALPHA calls BEN/PC on modem BRAVO, both modems capped at the tier with S21 (training margin S20 = 0), channel SNR 10 dB, seed 1. ANN's mic plays the LibriVox clip `sim/testdata/speech-16k.wav` (20.0 s, public domain); BEN's speaker is recorded into `wav/SF<n>.wav` (16 kHz mono, 16 bit). On push to talk tiers ANN holds TALK for the whole clip.

```
cargo run --release -p lfd3k-sim --bin lfd3k-voicesim -- --tier all --in sim/testdata/speech-16k.wav --out docs/voice-bench/wav
```

| Tier | Codec | Mode | Frames sent | Heard | Lost | Concealed | Mouth to ear | Match | CONNECT to slots | CONNECT to first audio |
|---|---|---|---|---|---|---|---|---|---|---|
| SF5 | Opus 10 kbps | Call (duplex) | 195 | 195 | 0 | 1 | 280 ms | 0.98 | 460 ms | 740 ms |
| SF6 | Opus 8 kbps | Call (duplex) | 119 | 119 | 0 | 1 | 470 ms | 0.98 | 520 ms | 990 ms |
| SF7 | Codec2 2400 | Call (duplex) | 119 | 119 | 0 | 1 | 460 ms | 0.97 | 670 ms | 1,120 ms |
| SF8 | Codec2 1300 | Call (duplex) | 119 | 119 | 0 | 1 | 480 ms | 0.95 | 900 ms | 1,370 ms |
| SF9 | Codec2 1200 | Push to talk | 61 | 61 | 0 | 0 | 550 ms | 0.94 | 1,370 ms | 2,310 ms |
| SF10 | Codec2 1200 | Push to talk | 61 | 61 | 0 | 0 | 650 ms | 0.94 | 2,320 ms | 3,360 ms |
| SF11 | Codec2 700C | Push to talk | 27 | 27 | 0 | 0 | 1,610 ms | 0.91 | 4,090 ms | 6,680 ms |
| SF12 | Codec2 700C | Push to talk, delayed | 15 | 15 | 0 | 0 | 3,860 ms | 0.86 | 7,290 ms | 13,070 ms |

- Mouth to ear: the lag at which the loudness envelope of the recording best matches the clip's (10 ms steps); match is that correlation (-1 to 1).
- Targets (controller rulings): SF5 under 400 ms, SF6 to SF8 under 600 ms, checked by the test `duplex_delay_under_target` (`sim/tests/voice_calls.rs`); SF12 CONNECT to first audio under 60 s, checked by `connect_to_first_audio_each_tier`.
- The counts are voice frames of the plan's length (SF5 120 ms, SF6 to SF8 200 ms, SF9 and SF10 400 ms, SF11 1 s, SF12 2 s). No frame was lost; the one concealed frame on each duplex tier is an underrun of the jitter buffer (the engine conceals one frame on an underrun on tiers with frames of 200 ms or less), most likely when ANN's mic stopped.

## The recorded test call (Task 9b)

Two copies of `lfd3k-voice-pc` on SimNet (SNR 8 dB, both capped with `--cap-tier`), DAVE/PC calls MAYA/PC, each app plays its side of the two speaker AXIS BBS conversation as its mic and records what it hears. The stereo recordings and how they were made are in `call/` (README.md). On push to talk tiers the conversation runs turn by turn (each speaker waits for the floor), so it is longer than the 117.5 s source.

| Tier | Codec | Mode | DAVE to MAYA: sent / heard / lost | MAYA to DAVE: sent / heard / lost | Concealed (MAYA / DAVE) | Mouth to ear DAVE to MAYA (match) | Mouth to ear MAYA to DAVE (match) | Recording |
|---|---|---|---|---|---|---|---|---|
| SF5 | Opus 10 kbps | Call (duplex) | 1060 / 1059 / 1 | 1060 / 1059 / 1 | 0 / 0 | 280 ms (0.98) | 300 ms (0.97) | 133.0 s |
| SF7 | Codec2 2400 | Call (duplex) | 636 / 635 / 1 | 636 / 635 / 1 | 1 / 0 | 460 ms (0.93) | 500 ms (0.97) | 132.8 s |
| SF9 | Codec2 1200 | Push to talk | 191 / 183 / 8 | 124 / 123 / 1 | 0 / 0 | 540 ms (0.98) | 600 ms (0.97) | 157.5 s |
| SF12 | Codec2 700C | Push to talk, delayed | 48 / 36 / 12 | 34 / 31 / 3 | 0 / 0 | 3,850 ms (0.78) | 5,820 ms (0.73) | 252.7 s |
| SF12, speaker waits one frame | Codec2 700C | Push to talk, delayed | 56 / 46 / 10 | 42 / 36 / 6 | 0 / 0 | 3,860 ms (0.87) | 5,820 ms (0.85) | 285.1 s |

- Lost: sent by one app, never heard by the other (the counters of both apps after the call). On SF9 the lost frames are silent ones at the edges of DAVE's holds; on SF12 the first frame after the floor comes back is often lost (about 2 s of speech when the speaker talks at once; see `call/README.md`).
