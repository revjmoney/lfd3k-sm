# Voice bench: the PC app's screens

Voice part 1, Task 9b (2026-11-13). `lfd3k-voice-pc` (release build) on this PC, two copies on SimNet (127.0.0.1), driven by `--script` (`tools/voice_call_bench/run_shots.py`; the calls by `run_call.py`). Each PNG is the whole window at 1280 x 720 (taken at 1600 x 900, Windows at 125 %, scaled down and stored as 256 colour PNGs to keep the repo small). Every one was looked at: nothing is cut off and there is no scroll bar. While checking, the DIRECTORY screen's scan answer was cut (`... BW500 VOIC.`); it now shows on two lines (03).

| PNG | What |
|---|---|
| 01 to 08 | DAVE/PC calls MAYA/PC (an `--auto-answer --play` copy) at SF5: idle, people nearby after a scan, the DIRECTORY screen with the scan answer, calling, NOT CHECKED with the two words, connected after WORDS MATCH (CALL mode, Opus), muted, CALL ENDED |
| 09, 10 | From the recorded SF7 call: MAYA's ring screen (a real ring), DAVE in the SF7 call (Codec2 2400, CALL mode) |
| 11, 12 | From the recorded SF9 call: push to talk, DAVE holding TALK, MAYA hearing him (THEY ARE TALKING) |
| 13, 14 | From the recorded SF12 call: PTT DELAYED, DAVE talking, MAYA hearing him about 5.8 s late |
| 15 to 19 | Screen only states (the script's `fake`, nothing sent): NOT THE MAYA YOU CHECKED (a conflict), ANSWERED ELSEWHERE, DECLINED, a full people list and directory paged |
| 20 to 30 | SETTINGS: Radio, then S30 set to 5 (the warning above the safe default, SETTINGS CHANGED NOT SAVED), Privacy set to QUIET, Wi-Fi (the built-in modem has no Wi-Fi; a client name typed, nothing sent), Forwards with AXIS added, Voice and keys, About with CONNECTION SOUNDS OFF, then SAVE (AT&W): saved, About reading AT&V back (S30=005, BEACON QUIET, FWD 1), Radio and Forwards after the save |
| 31 to 38 | Linking: DAVE/PC shows a link code, a new device NEW/PC2 joins with it and calls DAVE/PC, both show LINKED, and the new device is now DAVE/PC2 with the person key |
| 39 | From the recorded SF12 call: MAYA pressed TALK and is talking, WAITING FOR THE FLOOR (her words are kept and sent once the floor is hers) |

Known limit of SimNet: ANSWERED ELSEWHERE with a third copy cannot be shown. A SimNet modem has one UDP peer (`--peer`), so a third copy cannot hear the call; the two pickup race is covered by the sim tests (`two_ringing_one_pickup_other_answered_elsewhere`, `two_pickups_one_winner`) and the banner is shown here as a screen only state (16).
