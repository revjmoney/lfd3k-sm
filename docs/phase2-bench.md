# Phase 2 bench sheet

For Rev. This is the list you follow to back up the V4, flash the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM onto it, check it on the air, and put Radar back.

Nobody but you runs these commands. The tools in this repo never flash anything.

Open PowerShell for every command. Replace `COMn` with the V4 port you find in step 0.

## 0. Before anything

1. Put the antenna on the V4. Do this first. Nothing transmits until it is on.
2. Plug the V4 into USB.
3. Open Device Manager and look under Ports (COM and LPT).
4. Find the entry with hardware ID `VID_303A` and `PID_1001`. That is the V4. Right-click it, Properties, Details, Hardware Ids to check.
5. Write down its COM number. It can change after a reset or a new plug, so look again if a command says the port is gone.
6. COM15 is the lathe and COM9 is the V3. Never use those.
7. The MAC: esptool prints it in step 1 as a line like `MAC: aa:bb:cc:dd:ee:ff`. Write it down. The bench terminal uses it to make sure it is talking to this V4 and nothing else.
8. If any other board with `VID_303A` and `PID_1001` is plugged in, unplug it now. Before the erase in step 2, check that the `MAC:` line esptool prints matches the V4's known MAC. If it does not, stop.

## 1. Back up the V4

Do this before you flash anything. It saves the whole 16 MB chip, so Radar can come back.

1. Make sure the folder exists:

   ```
   mkdir F:\lfd3k-sm\backups
   ```

2. Read the whole flash. Use today's date in the file name:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --no-stub --after no_reset read_flash 0 0x1000000 F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin
   ```

3. Wait. It takes about 8 minutes on this V4 (the `--no-stub` read is slow). Do not unplug.
4. Check the size. It must be exactly 16,777,216 bytes:

   ```
   (Get-Item F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin).Length
   ```

5. Read the first 64 KB a second time into a new file:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --no-stub --after no_reset read_flash 0 0x10000 F:\lfd3k-sm\backups\check-first64k.bin
   ```

6. Compare it with the start of the backup. Make a 64 KB copy of the backup's start, then compare:

   ```
   $b = [IO.File]::OpenRead("F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin"); $buf = New-Object byte[] 65536; [void]$b.Read($buf,0,65536); $b.Close(); [IO.File]::WriteAllBytes("F:\lfd3k-sm\backups\head-64k.bin", $buf)
   fc.exe /b F:\lfd3k-sm\backups\head-64k.bin F:\lfd3k-sm\backups\check-first64k.bin
   ```

   Use `fc.exe`, not `fc`: in PowerShell plain `fc` is a different command (Format-Custom). Or compare hashes instead:

   ```
   Get-FileHash F:\lfd3k-sm\backups\head-64k.bin, F:\lfd3k-sm\backups\check-first64k.bin
   ```

7. Expected: `fc.exe` says "FC: no differences encountered", or the two hashes are identical. If not, read the backup again.
8. Keep the backup safe. It is in `backups\`, which git ignores.

## 2. Flash the LFD3K-SM

1. Build first (this does not touch the V4). Rebuild the Rust core, then the firmware:

   ```
   powershell -ExecutionPolicy Bypass -File F:\lfd3k-sm\firmware\build_core.ps1
   pio run -d F:\lfd3k-sm\firmware -e heltec_v4_single
   ```

   Then check that the firmware is newer than the Rust library:

   ```
   Get-Item F:\lfd3k-sm\firmware\lib\liblfd3k_ffi.a, F:\lfd3k-sm\firmware\.pio\build\heltec_v4_single\firmware.bin | Select-Object Name, LastWriteTime
   ```

   `firmware.bin` must have the later time. If it does not, run `pio run -d F:\lfd3k-sm\firmware -e heltec_v4_single -t clean`, then the `pio run` again. (The build also stops with "Stale Rust core" if a Rust source is newer than the library: run `build_core.ps1` and build again.)

2. Erase the chip. This clears the old otadata and the Radar settings, so the chip cannot boot the wrong slot:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --after no_reset erase_flash
   ```

3. It stays in download mode for the next step. If the port vanished anyway, unplug the V4, hold the PRG button while you plug it back in (that forces download mode), then check the COM number again in Device Manager.
4. Flash, the easy way:

   ```
   pio run -d F:\lfd3k-sm\firmware -e heltec_v4_single -t upload --upload-port COMn
   ```

5. Or flash with esptool directly, run from `F:\lfd3k-sm\firmware\.pio\build\heltec_v4_single`. These are the real offsets from `firmware\partitions.csv` and the build's `flasher_args.json`:

   | What | Offset | File |
   | --- | --- | --- |
   | Bootloader | 0x0 | `bootloader.bin` |
   | Partition table | 0x8000 | `partitions.bin` |
   | otadata | 0xe000 | `ota_data_initial.bin` |
   | App (ota_0) | 0x10000 | `firmware.bin` |

   ```
   cd F:\lfd3k-sm\firmware\.pio\build\heltec_v4_single
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --before default_reset --after watchdog_reset write_flash --flash_mode dio --flash_freq 80m --flash_size 16MB 0x0 bootloader.bin 0x8000 partitions.bin 0xe000 ota_data_initial.bin 0x10000 firmware.bin
   ```

6. The chip resets by itself at the end (watchdog reset). The antenna stays on.
7. A new COM number may show up. Check Device Manager.
8. To look at it, use the bench terminal (see step 5), or any terminal at 115200 with DTR and RTS off.

## 3. Put Radar back

1. Antenna on.
2. Write the backup over the whole chip:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --after watchdog_reset write_flash 0 F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin
   ```

3. Wait for it to finish. The V4 restarts into Radar.

## 4. Check it on the air with the SDR

1. Antenna on the V4. Antenna on the NESDR SMArt v5 too. Keep them a few feet apart.
2. Open SDRangel with the NESDR SMArt v5.
3. Set the center frequency to 918.5 MHz.
4. Set the sample rate to 2.4 MS/s.
5. Add a ChirpChat demodulator channel.
6. Set it to bandwidth 500 kHz, spreading factor SF12, explicit header, CRC on.
7. Start the device. You should see the noise floor and the demod waiting.
8. On the V4 (with the bench terminal or any terminal), type:

   ```
   ATDT TEST
   ```

   Expected: the terminal prints `DIALING`, then `NO ANSWER` about 30 seconds later (nobody answers). See step 5 for how to type it.

9. Watch the ChirpChat window. A frame should appear.
10. The CallReq frame starts with byte `D1`. The next byte starts with `1` (type 1 in the high nibble), so the first two bytes look like `D1 1x`.
11. If you see `D1 1x`, the air frame is right. If you see nothing, check the frequency, the antenna, and that the demod settings match step 6.

## 5. Bench terminal and screenshots (optional)

The bench terminal only opens the port once it has checked the USB identity. It will refuse COM15 and COM9 always.

1. Install the one package it needs, once: `pip install pyserial`
2. Talk to the V4:

   ```
   python F:\lfd3k-sm\tools\lfd3k_term.py --port COMn --expect-mac AA:BB:CC:DD:EE:FF --send "ATI" --log F:\lfd3k-sm\backups\session.log
   ```

   Expected for `ATI`: three banner lines (the modem name, the version with its release name, the release tagline).

   For `AT&V`, send `--send "AT&V"`. Expected: the settings list, including `S30` and the line `RADIO OK` (and `TX POWER S30=2 dBm chip, about 14 dBm out`).

   For the dial, the terminal must stay open about 35 seconds:

   ```
   python F:\lfd3k-sm\tools\lfd3k_term.py --port COMn --expect-mac AA:BB:CC:DD:EE:FF --send "ATDT TEST" --wait-ms 35000
   ```

   Expected: `DIALING`, then `NO ANSWER` after about 30 seconds.

   To type by hand, use miniterm (same pyserial, it holds DTR and RTS off):

   ```
   python -m serial.tools.miniterm COMn 115200 --dtr 0 --rts 0 --eol CR
   ```

   Confirm the COM port first in Device Manager (VID_303A PID_1001). Never COM15 or COM9: miniterm has no guard.

3. Take a picture of the OLED:

   ```
   python F:\lfd3k-sm\tools\lfd3k_term.py --port COMn --expect-mac AA:BB:CC:DD:EE:FF --shot-safe --shot F:\lfd3k-sm\backups\oled.png
   ```

   Expected: a 128 by 64 picture of the front panel (the same layout as the files in `docs\phase2-bench\mockups`). It can be taken during a dial too.

4. To turn a saved `FB:` line into a picture later:

   ```
   python F:\lfd3k-sm\tools\fb2png.py saved.txt oled.png
   ```

## Bench results, 2026-11-10 (first flash)

- Board: Heltec V4, ESP32-S3 (QFN56) rev v0.2, 2 MB PSRAM, MAC 88:56:A6:96:16:FC, on COM11 (VID 303A PID 1001). Modem name AXISMODEM-16FC.
- Backup: `backups\v4-simian-radar-2026-11-10.bin`, 16,777,216 bytes, SHA256 8DF2531CB7ABDA3F630F0199B68080FDD59944408A89CEFE4874AD96B86A0B55. First 64 KB re-read matched (`fc.exe`: no differences). The `--no-stub` read took 1540 s (about 26 minutes, not 8).
- esptool 4.9 spells the flag `--after no_reset` (underscore); this sheet now uses it.
- Erase 14.8 s, flash via `pio run -t upload`: bootloader, partition table, otadata and app (332,896 B) all hash-verified, watchdog reset into the app.
- Terminal (`lfd3k_term.py`, DTR and RTS held off): opening and closing the port does not reset the board, and no ROM banner shows on USB.
- `ATI`: the three banner lines. `AT&V`: S30=2, `TX POWER S30=2 dBm chip, about 14 dBm out`, `RADIO OK`, `TIER NONE`.
- `ATDT TEST` (antenna on, 918.5 MHz, SF12): `DIALING TEST`, `RINGING` every few seconds, `NO ANSWER` after the 30 s S7 timeout. Proves TX, TX done, return to receive and the call timers on real hardware. `RADIO OK` afterwards.
- OLED screenshots: `docs/phase2-bench/oled-idle.png`, `oled-dialing.png`, `oled-after.png`. Orientation correct, font and lights as in the mockups.
- Not done: SDR decode (NESDR availability not confirmed). Two-radio call waits for the second V4.
- Noticed: `AT` typed while dialing answered `OK` and the dial carried on; a classic Hayes modem aborts a dial on any key. Decide later.

## Bench rerun, 2026-11-10 (startup hardening, commit 2566ee5)

- Reflashed on COM11 (MAC 88:56:A6:96:16:FC matched), all parts hash-verified, no erase.
- ATI banner, AT&V RADIO OK with the new reset order and 1 ms settle; safe mode did not trip.
- ATDT TEST: DIALING, RINGING, NO ANSWER after the 30 s S7 timeout; RADIO OK afterwards.
- Log: docs/phase2-bench/bench-2026-11-10b.log, screenshot oled-idle-b.png.
- Safe mode itself is not exercised on hardware (it needs two quick panics); its logic is in hal_radio.c boot_check().

## SDR radio test, 2026-11-10 (NESDR SMArt v5, serial 54066557, SDRangel 8.0.0)

Setup: RTL-SDR 2.048 MS/s, decimation 2 (1.024 MS/s), gain 10 dB, about a few feet from the V4. Waterfall reads from SDRangel's MCP server.

- On air: every `ATDT TEST` makes bursts at 918.5 MHz, one per `RINGING`, each about 0.3 s long (matches the SF12 CallReq airtime). Screenshot `docs/phase2-bench/sdr-waterfall-bursts.png`.
- Frequency and bandwidth: with the SDR centred on 918.5 MHz the bursts span about 918.245 to 918.755 MHz (2 kHz bins), so about 510 kHz wide and centred on 918.50 MHz with no offset visible at 2 kHz resolution. Screenshot `docs/phase2-bench/sdr-waterfall-918500.png`.
- Power steps (`ATS30`, peak level at the SDR, same bins and gain):

  | S30 (chip dBm) | SDR peak | Step |
  |---|---|---|
  | -9 | -55.1 dB | |
  | 2 | -43.3 dB | +11.8 dB for +11 |
  | 10 | -32.8 dB | +10.5 dB for +8 |

  The power setting works and tracks within about 2 dB per step (the SDR is not a calibrated meter). S30 put back to 2 after the test.
- LoRa decode: SDRangel's ChirpChat decoder (SF12, BW 500 kHz, CR 4/5, explicit header, CRC, DE 0) did not lock on the preamble. Its own readme says it only decodes reliably with distance enhancement on (DE 2), which our BW500 tiers do not use, so this is a decoder limit, not proof of a bad frame. A frame level check needs a second SX1262 (second V4, or the V3 as a plain receiver).
- SDRangel restored afterwards: test channel removed, RTL-SDR back to 144.36 MHz, 1.024 MS/s, decimation 8, gain 49.6 dB (the APRS setup).
