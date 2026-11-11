# Phase 4 bench sheet (discovery, compression, the menu)

For Rev. This is the list you follow to try the phase 4 features of the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM on the V4: beacons, `AT+SCAN`, the directory, privacy modes, calling by name, `/LZ` compression, the `AT+MENU` screen, `AT+NAME`, the idle show and the scan radar.

You have one LoRa radio. So the second station is the PC modem over SimNet (see `docs\simnet.md`). The PC runs the same modem core as the V4. Only the radio between them is simulated.

Open PowerShell for every command. Replace `COMn` with the V4 port from Device Manager (VID_303A, PID_1001). Never COM15 or COM9. The controller flashes the V4. You do not.

## 0. Set up SimNet

1. Join your Wi-Fi on the V4 if it is not joined yet: `AT+WSTA=<your network>,<your password>` (you type it yourself), then `AT&W`.
2. Read the V4 address with `AT+WIFI?`. The last item is the address, for example `192.168.1.57`.
3. Find the PC address with `ipconfig`, for example `192.168.1.20`.
4. On the V4 type `AT+SIMNET=192.168.1.20`. It answers `OK`.
5. On the PC, in PowerShell in `F:\lfd3k-sm`, start the PC modem:

   ```
   cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer 192.168.1.57
   ```

   Its name is `PCMODEM` unless you add `--name <NAME>`. Windows may ask about the firewall the first time. Allow it.
6. Type `AT` in the PC window and in your V4 terminal. Both answer `OK`.

Terminal for the V4 (line mode, no log file):

```
python -m serial.tools.miniterm COMn 115200 --dtr 0 --rts 0 --eol CR
```

When you are done with everything, type `AT+SIMNET=0` on the V4.

## 1. Scan from the PC modem

1. In the PC window type `ATV1` (it is the default) and then `AT+SCAN`.
2. Wait about 4 seconds. A modem answers in a random slot of up to 2.5 seconds, so the lines come one after the other.
3. You expect a line like this, then `OK`:

   `SCAN AXISMODEM-16FC SNR 8.0 dB LORA SF5 BW500`

4. `ERROR` instead of `OK` means a call is up, the PC modem is in HIDDEN mode, or a scan is already running.
5. If the V4 had never been heard before, it is in the PC's directory now. Type `AT+DIR?` and look for its name.

## 2. Scan from the V4

1. In your V4 terminal type `AT+SCAN`.
2. Wait about 4 seconds. You expect `SCAN PCMODEM SNR ... dB ...` then `OK`.
3. While the scan runs, the OLED shows the radar: the word SCAN on the left, a sweep line in the circle, a blip for each answer, and the number of answers on the right. It goes back to the idle show when the scan ends.
4. Only one scan every 10 seconds is allowed to transmit. If you type `AT+SCAN` again at once, it waits for its turn.

## 3. Beacons

Beacons are slow on purpose: one every 5 to 6 minutes at SF12 (the first can take 6 minutes and a bit).

1. Leave both modems idle and untouched for 7 minutes. Do not scan.
2. Then type `AT+DIR?` on both. Each should list the other, heard by beacon alone.
3. The OLED idle show has a HEARD item when something was heard in the last 15 minutes. Wait for it in the show.
4. No beacon goes out during a call. The timer restarts after the call ends.

## 4. The directory and pinning

1. Type `AT+DIR?` on the V4. Each line reads:

   `DIR <name> <tier> <age in seconds, or -> [PIN]`

   `-` means the name was loaded or typed in and not heard since boot.
2. Pin a name by hand: `AT+DIR=+KD4REV`. It answers `OK`, and the line now ends with `PIN`. A pinned name is never pushed out when the directory fills (32 names).
3. Remove a name: `AT+DIR=-KD4REV`. Clear every name that is not pinned: `AT+DIR=0`.
4. Type `AT&W` to save. The directory is also saved by itself after changes, but never more than once a minute.
5. The V4 keeps the list in flash. After a reboot `AT+DIR?` shows the same names, with `-` as the age until each is heard again.

## 5. Privacy modes

The mode is `AT+BEACON=ON`, `AT+BEACON=QUIET` or `AT+BEACON=HIDDEN`. `AT+BEACON?` shows it. `AT&W` saves it. ON is the default.

1. ON: sends beacons and answers scans. Try steps 1 to 3.
2. QUIET: `AT+BEACON=QUIET` on the V4. Scan from the PC: the V4 still answers. Wait 7 minutes: the PC does not hear a beacon from the V4.
3. HIDDEN: `AT+BEACON=HIDDEN` on the V4.
   - Scan from the PC: no answer.
   - `AT+SCAN` on the V4 itself gives `ERROR` (a scan would reveal its name).
   - Dial the V4 from the PC with `ATDT <V4 name>`. Nothing answers. The PC prints `NO ANSWER` after about 30 seconds, and the V4 prints no `RING`. The V4 does not even send a reject.
4. HIDDEN still answers pinned names. On the V4 type `AT+DIR=+PCMODEM`, then dial again from the PC. Now the V4 rings, and with `ATS0=1` it answers. Only a name you pinned by hand opens a HIDDEN modem. Hearing a name never does.
5. Put it back with `AT+BEACON=ON`.

## 6. Dial by name

1. On the V4 type `ATS0=1` so it answers by itself.
2. On the PC, scan first so the V4 is in the directory. Then type `ATDT <V4 name>`.
3. Training still picks the speed. The directory only keeps a note of the best tier it saw. You expect `DIALING`, `RINGING`, `TRAINING SNR ...`, then `CONNECT LORA SF5 BW500 62500/ARQ/LZ` (the speed differs with your signal).
4. In the menu you can dial a directory name with Enter (see step 9).
5. To leave the call: wait a second, type `+++` alone, wait for `OK`, then `ATH`. You see `NO CARRIER`.

## 7. Compression (`/ARQ/LZ`)

1. In step 6 the CONNECT line ends in `/ARQ/LZ` when both ends compress. `/ARQ` alone means one end has it off.
2. The DC light on the OLED lights while the call compresses.
3. During the call, send some ANSI screen text. For example call the AXIS BBS through the V4 bridge (phase 3 sheet, step f), or type lines on one side.
4. Leave the call (`+++`, then `ATH`) and type `AT&V`. Look for the line:

   `LZ ON RATIO 2.9`

   The ratio is host bytes over air bytes. 2.9 means the screen took about a third of the air time. Plain typed text gives a ratio close to 1.0. ANSI art gives the high numbers.
5. Turn it off on one side with `ATS31=0` (before dialing). The next CONNECT line has no `/LZ`. The setting `S31=1` is the default and is saved by `AT&W`.
6. Time it: load the same AXIS screen with `ATS31=1` and with `ATS31=0`, and note the seconds.

## 8. Station name (`AT+NAME`)

1. `AT+NAME?` shows `NAME <name>`. The default is the board name, for example `AXISMODEM-16FC`.
2. `AT+NAME=REVJ` sets it. It answers `OK` and takes effect at once, so scans and beacons now carry `REVJ`. The hotspot name does not change.
3. A name is 1 to 16 printable characters and no colon. `AT+NAME=REV:J` gives `ERROR`.
4. You cannot change the name during a call. It gives `ERROR`.
5. `AT+NAME=` with nothing after it goes back to the default name.
6. `AT&W` saves it. Settings saved by older firmware still load, with the default name.

## 9. The menu in SyncTERM

1. Close miniterm. One program at a time may use the port.
2. Open SyncTERM and make a new entry:
   - Connection type: Serial
   - Device: COM11 (check Device Manager first, it must be the V4)
   - Baud rate: 115200
   - Emulation: ANSI-BBS, with the CP437 font
3. Connect. Type `AT` and Enter. You see `OK`.
4. Type `AT+MENU` and Enter. The blue title bar appears and the cursor hides.
5. Keys on every screen:

   - Up, Down, Tab: move
   - Enter: open or edit
   - a letter: jump to the item that starts with it
   - Esc: back, or leave from the main menu

6. Main menu: Directory and Scan, Link Status, Settings, Forwards, Wi-Fi, About, Save settings, Exit menu. Press Enter on Save settings to do `AT&W`.
7. Directory and Scan: Enter dials the selected name (the menu closes and `ATDT` runs), `S` scans, `P` pins, `A` adds a name, `D` deletes (it asks Y or N).
8. Settings: Left and Right step a value, Enter edits it. The fields are station name, privacy mode, TX power (S30), tier cap (S21), auto-answer (S0), compression (S31) and boot to menu (S32). An edit is checked the same way the AT command checks it, and a bad value shows a red line.
9. Wi-Fi: passwords always show as `********`. When you type a new one, each character shows as `*`.
10. The bottom line shows the name, the call state, the tier, the Wi-Fi address, and RING, SCAN and UNSAVED when they apply.
11. Esc on the main menu leaves. If you changed something and did not save, it asks: Y discards and leaves, S saves and leaves, N stays. A second Esc at that question discards and leaves too.
12. A call that rings while the menu is open shows `RING` in the bottom line. Leave the menu with Esc, then type `ATA`.
13. Mid call: type `+++` (guard time before and after), then `AT+MENU`. Link Status shows the tier, SNR and compression live. Leave and type `ATO` to go back to the call.
14. Boot to menu: `ATS32=1`, `AT&W`, reboot. The first key you press opens the menu.

If the screen looks wrong, check the font is CP437 and the size is 80 columns by 24 rows or more.

## 10. The PC modem in raw mode

Line mode (the default) sends a line at a time, so it cannot do the menu. Raw mode sends every key as you press it.

1. Start it with `--raw`:

   ```
   cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer 192.168.1.57 --raw
   ```

2. Type `AT+MENU` and Enter. The same menu draws in the PowerShell window. Arrows, Esc, Enter, Tab and Backspace work.
3. To quit, press Ctrl+] (hold Ctrl and press the right square bracket). The console is put back. Ctrl+C does not quit: it goes to the modem as a key.
4. Raw mode was written without a real console to try it on. If a key does nothing or the console looks odd afterwards, tell the controller which key.
5. `--raw` does not work together with `--script`.

## 11. The idle show and the scan radar on the OLED

1. With nothing happening, the OLED runs the idle show: words in many languages, small facts (time, uptime, temperature), and 12 kinds of change between them. A scope of the channel runs underneath.
2. When something was heard in the last 15 minutes, a HEARD item with the count appears. After a radio call, the name of the station that called shows as an item too.
3. Start `AT+SCAN` and watch the radar (see step 2). It replaces the idle show only while the scan runs.
4. During a call the OLED shows the call screen as before. The DC light shows when the call compresses.

## What to tell the controller

- Which steps worked and which did not.
- The AXIS screen load times with `ATS31=1` and `ATS31=0` (step 7).
- Whether `--raw` and the SyncTERM menu looked right.
