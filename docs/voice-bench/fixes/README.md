# Voice part 2 fixes: bench (2026-11-15)

Flashed: V4 (COM11, USB) modem slot ota_1, V3 (AXISMODEM-E4B0, on Wi-Fi) modem slot ota_1, both from branch voice-2-fixes.

| Step | Result | Evidence |
|---|---|---|
| Raw voice host protocol on the V3's TCP port 4220: AtLine `ATS30=5`, `AT&W`, `ATZ`, `AT+DIR=+V:EVIL`, `AT+WSTA=X,Y` | each refused with Error 7 (locked), S30 still 022 after | `wifi-atline-locked.txt` |
| Same session: `ATS30?`, `AT+NAME?` | answered | `wifi-atline-locked.txt` |
| PC app on `find:AXISMODEM-E4B0` (Wi-Fi): Settings | greyed out, note "Settings are locked over Wi-Fi. Plug the modem in by USB to change them." | `02-wifi-settings-locked.png`, `04-wifi-tab-locked.png` |
| Script `set ATS30=5` on Wi-Fi | not sent, banner | `03-wifi-change-refused.png` |
| Directory on Wi-Fi | PIN, DELETE, CLEAR greyed out; SCAN NOW and REFRESH work | `05-wifi-directory-call-scan-only.png` |
| PC app on COM11 (USB) with `--cap-tier SF9`, SAVE pressed at once (before the S21 read answered) | Settings live, cap shows SF9; after Quit the V4 has S21=007 and, after ATZ, the saved S21 is still 007 (the cap was not saved) | `06-usb-settings-capped-sf9.png` |
| USB `set ATS30=3`, then DISCARD | Save bar shows, DISCARD puts S30 back to 2 | `07-usb-change-live-save-bar.png`, `08-usb-discarded.png` |

The hold timeout (a crashed app behind a held ControlOut) is covered by the core tests `dead_session_holding_control_times_out` and `live_session_holding_control_stays_open`; not reproduced on the radio.
