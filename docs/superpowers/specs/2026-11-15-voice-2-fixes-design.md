# Voice part 2 fixes: short design

Date 2026-11-15. Approved by Rev in chat ("lock them too, go ahead").

## Intent

Three small fixes after the first real voice call (voice part 2):

1. **Nobody changes modem settings over Wi-Fi** (Rev's ruling, 2026-11-15). The TCP voice port 4220 is reachable by anyone on the modem's hotspot or home network; today an AtLine there can run AT&W, AT+WSTA, AT+FWD, S registers and the rest.
2. The PC app's `--cap-tier` can save the cap or lose track of it when AT&W or ATZ is queued before the S21 read answers, and an S21 edit in Settings while capped vanishes on Quit.
3. A voice session holding a ControlOut never times out, so a crashed PC app can keep USB session 0 framed for good.

## Decisions

**Read-only AtLine on Wi-Fi.** The core keeps a `full` flag per voice session. Session 0 (USB or UART host stream after AT+VOICE) is always full. Sessions 1 to 3 open read-only; `Modem::voice_grant_settings(session)` makes one full. The firmware never calls it (TCP stays read-only); the PC app's built-in SimNet modem and the simulator's `Multi` call it (a local app on the same PC).

On a read-only session these AtLines run (`host::at_read_only`): `AT`, `ATI`, `AT&V`, `ATSn?`, `AT+WAP?`, `AT+WSTA?`, `AT+WIFI?`, `AT+FWD?`, `AT+PUB?`, `AT+FOUND?`, `AT+SIMNET?`, `AT+BEACON?`, `AT+NAME?`, `AT+MODE?`, `AT+DIR?` and `AT+SCAN` (it transmits but changes nothing). Everything else is refused with the new error `err::AT_LOCKED` (7): setters, S register writes, AT&W, ATZ, ATE, ATV, ATH, Wi-Fi, hotspot, forwards, publish, beacon, name, mode, and (Rev: "lock them too") directory pin, unpin, delete and clear. A line that does not parse is refused too (it would only print ERROR). Voice messages (Register, Call, Answer, TalkKey and the rest) are not AtLines and are unchanged.

**PC app.** On `tcp:` or `find` the Settings screen says "Settings are locked over Wi-Fi. Plug the modem in by USB to change them." and its controls are greyed out; the Save / Discard bar does not show; the Directory screen only calls and scans (pin, unpin, delete, add and clear are greyed out). The app decides with the same `at_read_only` the core uses. AT_LOCKED reads "locked over Wi-Fi".

**Cap tier at send time.** `queue_window_line` goes; `at_next` looks at each window line as it is sent: an AT&W with the cap on goes out as `ATS21=<old>`, `AT&W`, `ATS21=<cap>`; an ATZ with the cap on ends the cap when it answers OK. **An S21 edit from Settings while capped ends the cap** (Claude's call): it is sent as typed, the cap is dropped (nothing put back on Quit) and the note says "tier cap ended: S21 set in Settings".

**Hold timeout.** `vhp_tick` no longer skips a session with a held ControlOut. Its clock restarts only when a new frame is taken (not when the same waiting bytes are offered again) or when the link takes held bytes. A live app's heartbeat frames keep counting while it waits; a crashed one goes after 5 s, the call is hung up and session 0's framed mode ends.

## Approaches considered

- Filter AtLines in the firmware C (voice_link.c): rejected, it would mean decoding COBS in C and a second list of commands to keep in step.
- Mark the whole session read-only by a flag in HelloAck so the app knows: not needed, the app knows its own link; kept the protocol at version 1.
- Directory edits allowed over Wi-Fi: rejected by Rev (a voice pin decides who rings a HIDDEN modem).

## Left to the builder

Wording of comments, test names, exact layout of the greyed-out note.
