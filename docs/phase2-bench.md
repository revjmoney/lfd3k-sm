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
7. Write down the MAC of the V4 if you want to use the bench terminal. In Device Manager, Details, pick "Device instance path" or "Bus reported device description". The USB serial number is the MAC, like `AA:BB:CC:DD:EE:FF`.

## 1. Back up the V4

Do this before you flash anything. It saves the whole 16 MB chip, so Radar can come back.

1. Make sure the folder exists:

   ```
   mkdir F:\lfd3k-sm\backups
   ```

2. Read the whole flash. Use today's date in the file name:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --no-stub --after no-reset read_flash 0 0x1000000 F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin
   ```

3. Wait. It takes a few minutes. Do not unplug.
4. Check the size. It must be exactly 16,777,216 bytes:

   ```
   (Get-Item F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin).Length
   ```

5. Read the first 64 KB a second time into a new file:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 --no-stub --after no-reset read_flash 0 0x10000 F:\lfd3k-sm\backups\check-first64k.bin
   ```

6. Compare it with the start of the backup. Make a 64 KB copy of the backup's start, then compare:

   ```
   $b = [IO.File]::OpenRead("F:\lfd3k-sm\backups\v4-simian-radar-YYYY-MM-DD.bin"); $buf = New-Object byte[] 65536; [void]$b.Read($buf,0,65536); $b.Close(); [IO.File]::WriteAllBytes("F:\lfd3k-sm\backups\head-64k.bin", $buf)
   fc /b F:\lfd3k-sm\backups\head-64k.bin F:\lfd3k-sm\backups\check-first64k.bin
   ```

7. `fc` must say "no differences encountered". If it does not, read the backup again.
8. Keep the backup safe. It is in `backups\`, which git ignores.

## 2. Flash the LFD3K-SM

1. Build first (this does not touch the V4):

   ```
   pio run -d F:\lfd3k-sm\firmware -e heltec_v4
   ```

2. Erase the chip. This clears the old otadata and the Radar settings, so the chip cannot boot the wrong slot:

   ```
   python "$env:USERPROFILE\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 --port COMn --baud 921600 erase_flash
   ```

3. Unplug the V4 and plug it back in if the port vanished. Check the COM number again in Device Manager.
4. Flash, the easy way:

   ```
   pio run -d F:\lfd3k-sm\firmware -e heltec_v4 -t upload --upload-port COMn
   ```

5. Or flash with esptool directly, run from `F:\lfd3k-sm\firmware\.pio\build\heltec_v4`. These are the real offsets from `firmware\partitions.csv` and the build's `flasher_args.json`:

   | What | Offset | File |
   | --- | --- | --- |
   | Bootloader | 0x0 | `bootloader.bin` |
   | Partition table | 0x8000 | `partitions.bin` |
   | otadata | 0xe000 | `ota_data_initial.bin` |
   | App (ota_0) | 0x10000 | `firmware.bin` |

   ```
   cd F:\lfd3k-sm\firmware\.pio\build\heltec_v4
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

3. Take a picture of the OLED:

   ```
   python F:\lfd3k-sm\tools\lfd3k_term.py --port COMn --expect-mac AA:BB:CC:DD:EE:FF --shot F:\lfd3k-sm\backups\oled.png
   ```

4. To turn a saved `FB:` line into a picture later:

   ```
   python F:\lfd3k-sm\tools\fb2png.py saved.txt oled.png
   ```
