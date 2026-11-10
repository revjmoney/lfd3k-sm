# Phase 3 bench sheet (Wi-Fi)

For Rev. This is the list you follow to try the Wi-Fi side of the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM on the V4.

The radio caller path is simulated. You have one LoRa radio, so a "radio call" in steps e and f comes from the PC over Wi-Fi (SimNet, see `docs\simnet.md`). Everything after the radio (the modem, the bridge, TCP, the AXIS board) is real. A call from a second real LoRa radio waits until you have one.

Open PowerShell for every command. Replace `COMn` with the V4 port from Device Manager (VID_303A, PID_1001). Never COM15 or COM9.

## 0. Flashing

1. Tell the controller in chat when you want the firmware flashed. It does the flash. You do not.
2. After the flash the V4 reboots. The OLED shows the splash, then the idle screen.

## 0b. Open a terminal

1. Type commands by hand in miniterm. It writes no log file:

   ```
   python -m serial.tools.miniterm COMn 115200 --dtr 0 --rts 0 --eol CR
   ```

2. Type `AT` and press Enter. You see `OK`.
3. Never paste a password into a chat or a file. Never put one in a `--send` argument.

## a. Join your Wi-Fi

1. Type this yourself, with your own network name and password:

   `AT+WSTA=<your network>,<your password>`

   It answers `OK`.
2. Type `AT&W`. This saves it. The V4 keeps it across power cycles.
3. Check what it saved: `AT+WSTA?`. It shows your network name and `********`. It never shows the password.

## b. Read the address

1. Type `AT+WIFI?`
2. You see one line, for example:

   `WIFI AP AXISMODEM-16FC 0 CLIENTS STA MyNetwork 192.168.1.57`

3. The last item is the V4's address. `NO IP` means it has not joined yet. Wait ten seconds and try again.
4. The same address shows on the OLED. The bottom row of the idle screen changes every 3 seconds: the board name, `AP <hotspot name>`, `PW <password>` (see step d), and `IP <address>` once you are on your network.

## c. Call your AXIS board from the V4

1. Add your board as a forward. The address is the board's, for example 192.168.1.164:

   `AT+FWD=AXIS,192.168.1.164,23`

2. Type `AT&W` to save it.
3. Type `ATDT AXIS`.
4. You expect `DIALING AXIS`, then `CONNECT WIFI TCP`, then the AXIS welcome screen.
5. The OLED shows `WIFI TCP` and `ONLINE`.
6. To leave: wait a second, type `+++` alone (no Enter), wait for `OK`, then type `ATH`. You see `NO CARRIER`.

If you see `NO CARRIER` right away, check the address with `AT+WIFI?` and that the board is on the same network.

## d. The hotspot

1. The V4 makes its own Wi-Fi network. The name is the modem name, for example `AXISMODEM-16FC`. It is on the idle screen as `AP AXISMODEM-16FC`.
2. The password is 12 letters and digits it made for itself on first boot. Two places show it:
   - The OLED row `PW <password>`. It shows only until the first device joins the hotspot, or until you saved with `AT&W` while a client network is set.
   - `AT+WAP?`, which prints `WAP ON <name> <password>`. The line shows the password on purpose, so do this on your own screen only.
3. On a phone, open Wi-Fi settings, pick the hotspot name, and type the password.
4. Once a device joins, the OLED stops showing `PW`. `AT&V` then says `PWSHOW 0`.
5. The hotspot reaches only the V4 itself. It does not give the phone internet and does not reach your home network.

## e. Call the V4 from the PC with a simulated radio

Follow `docs\simnet.md`, section "Plain call: PC dials the V4". In short:

1. On the V4: `AT+SIMNET=<PC address>`.
2. On the PC: `cargo run -p lfd3k-sim --bin lfd3k-pcmodem -- --peer <V4 address>`
3. On the PC window: `ATDT <V4 name>`. On the V4: `ATA`.
4. Both sides train and print `CONNECT`. The OLED shows the tier and the ladder.
5. End with `+++` then `ATH`. Then on the V4: `AT+SIMNET=0`.

## f. Publish the AXIS board to radio callers

1. On the V4 (you did `AT+FWD=AXIS,...` in step c):

   `AT+PUB=AXIS`

   `ATS0=1`

   `AT&W`

2. Turn SimNet on as in step e (`AT+SIMNET=<PC address>`).
3. On the PC, start the PC modem as in step e and type `ATDT <V4 name>`.
4. The V4 answers by itself and joins the call to AXIS. The PC prints `CONNECT`, then the AXIS welcome screen appears in the PC window.
5. The V4 OLED shows `BRIDGE` and the radio tier. The V4 terminal shows `BRIDGE AXIS`.
6. End with `+++` then `ATH` on the PC. Both sides print `NO CARRIER`. Then on the V4: `AT+SIMNET=0`.
7. To stop bridging: `AT+PUB=0`.

## Screenshots without leaking the password

The bench terminal can take a picture of the OLED. It refuses when the password may be on screen.

1. Use `--shot-safe`. It reads `AT&V` first and sends `AT+FB` only when it sees `PWSHOW 0`:

   ```
   python F:\lfd3k-sm\tools\lfd3k_term.py --port COMn --expect-mac AA:BB:CC:DD:EE:FF --shot-safe --shot F:\lfd3k-sm\backups\oled.png
   ```

2. Plain `--shot` is refused. `--allow-secret` lifts the refusal. Do not use it near anything you will share.
3. A `--shot` or `--log` file inside the repo `docs` folder is refused unless `AT&V` says `PWSHOW 0`.
4. The tool prints `********` for the password in `WAP` lines, `PW` lines and echoed `AT+WAP=` or `AT+WSTA=` commands, and always in its log file.
5. Do not send any file or screenshot that shows the hotspot password to anyone.

## If something looks wrong

- `AT&V` shows `WIFI`, `PWSHOW`, `PUB`, `FWD` and `STACK FREE` lines. Copy those lines (they hold no password) to the controller.
- A `NO CARRIER` after `ATDT` means the connect failed or timed out (S7 seconds).
- No `IP` row on the OLED means the client side has no address yet.
