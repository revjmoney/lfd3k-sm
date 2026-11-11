# Installing the multimode image

For the controller and Rev. The image `multimode/out/full-16mb.bin` holds the bootloader,
the partition table, the picker, and the four modes (MESH, LFD3K-SM, SIMIAN RADAR, RNODE),
plus Simian's camera database. It covers all 16 MB of the flash, so writing it replaces
everything on the board, including the old LFD3K-SM settings, RNode and Meshtastic data.

Nothing here runs by itself. The build script never opens a port; only these steps do,
and only the controller runs them, after Rev says go on the wipe.

## 0. Before anything

* Build the image: `powershell -ExecutionPolicy Bypass -File tools\build_multimode.ps1`
  (add `-Build` to rebuild every piece first). It stops if a piece is missing or a
  size or layout check fails. Result: `multimode\out\full-16mb.bin`, `flash_args.txt`, `manifest.txt`.
* Read `manifest.txt` and keep it with the backup. It lists the SHA-256 of every piece and of the image.
* Identify the board: USB VID 303A, PID 1001, MAC 88:56:A6:96:16:FC. If any of these differ, stop.
* Esptool is the PlatformIO copy: `%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py` (version 4.11.0, PlatformIO `tool-esptoolpy`).
  Below, `COMn` is the board's port.

## 1. Full backup (about 26 minutes)

```
python "%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 -p COMn -b 460800 --no-stub --after no_reset read_flash 0 0x1000000 backup-16mb.bin
```

Keep the file somewhere safe, outside `multimode\out`. Record its SHA-256 in the bench log:
`powershell -Command "(Get-FileHash -Algorithm SHA256 backup-16mb.bin).Hash"`.

## 2. Verify the backup

* The file must be exactly 16,777,216 bytes.
* Read the first 64 KB again and compare:
  `... --no-stub --after no_reset read_flash 0 0x10000 check-64k.bin`, then
  compare it with the first 64 KB of the backup (must print `True`):
  `python -I -c "a=open('backup-16mb.bin','rb').read(65536);b=open('check-64k.bin','rb').read();print(len(b)==65536 and a==b)"`
* If the sizes or bytes differ, redo the backup. Do not continue without a good one.

## 3. Erase

Only after Rev confirms the wipe again, right before this step. Use only this esptool line,
never `pio run -d firmware -e heltec_v4 -t erase` (or `-t upload`) later: on a board that already
runs the multimode layout that would wipe every mode (the env refuses both, see
`firmware/platformio.ini`).

```
python "%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 -p COMn --after no_reset erase_flash
```

`--after no_reset` keeps the chip from resetting into blank flash (it would boot loop on the native
USB port and the COM port could renumber before step 4). The erase is optional before a full 16 MB
write, because the write erases every sector it covers. It is kept for a clean `nvs`.

## 4. Write the image

Run it from `multimode\out`. The exact line is in `flash_args.txt` (replace `COMn`):

```
python "%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 -p COMn -b 460800 --before default_reset --after no_reset write_flash --flash_mode keep --flash_freq keep --flash_size keep 0x0 full-16mb.bin
```

It writes one file at 0x0. Esptool verifies the hash of what it wrote. The bootloader header in the
image is DIO (like every image this board has run; the bootloader switches the flash to QIO itself),
and `keep` leaves it untouched.

Then **unplug and replug USB** for a clean power on. Do not use `--after watchdog_reset`: that reset
counts as a crash for the picker, so the first boot would show LAST MODE CRASHED and not start anything.

## 5. First boot

* The OLED shows the AXIS BBS logo for 5 seconds. Press PRG during it to open the mode list.
* With no press, the picker starts its last mode. On the first ever boot there is none, so it starts
  LFD3K-SM, the default.
* The settings partition is empty: LFD3K-SM comes up with defaults, so retype `AT+WSTA` (Wi-Fi name
  and password) and any other settings. The old settings are in the backup only.
* The other modes also start fresh. Set up Meshtastic as new, and Simian Radar starts with no marks.
* **RNode setup, with care.** While RNODE mode is running, use `rnodeconf` over its serial port for the
  EEPROM bootstrap and the firmware hash commands ONLY. NEVER run `rnodeconf --autoinstall`, `--update`,
  `-u`, or any option that writes firmware: it flashes at RNode's own offsets (0x0, 0x8000, 0xE000,
  0x10000) and overwrites our bootloader, partition table, nvs, picker and the start of MESH. Check the
  flag names with `rnodeconf --help` first. The RNode image (1.86, heltec32v4pa) has file SHA-256
  `df590b2d1aa4f4414cbf02314550a70d0636d7c5a689b9389189c028c86a0ea1`; the firmware hash it reports (its
  appended SHA-256, last 32 bytes of the .bin) is
  `f312de7d8ec431fe1b80a84067cb01932cdeeaa7cb24f69e1b499f11c0e61d12`. Read what the device reports
  first, compare, and only then set the hash.
* **Do not long-press PRG in RNODE mode.** It may start RNode's console, which may mount and format the
  first SPIFFS partition, Meshtastic's data (unverified against the 1.86 source).
* **First launch check.** The first start from the picker must reach LFD3K-SM (the modem screen, not the
  logo again). If the logo loops, the RTC token did not survive `esp_restart` on this chip: power off,
  hold PRG at power on, restore the backup (step 7), and report.
* A slot with no usable image shows NO IMAGE in the list and cannot be started.
* From LFD3K-SM, `AT+MODE?` lists the modes and `AT+MODE=RADAR`, `RNODE`, `MESH`, `MODEM` or
  `PICKER` switches.

## 6. Recovery

Hold PRG while powering the board on (or while plugging in USB). The ESP32-S3 ROM then enters download
mode no matter what is flashed. From there, anything can be written again with the commands above, so a
bad bootloader, picker or table is fixed by writing `full-16mb.bin` again, or the backup.

If a mode crashes, the next reset returns to the picker, which opens the list and says LAST MODE CRASHED.

## 7. Restore the backup

With the board in download mode (or running), write the backup back as one file at 0x0:

```
python "%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py" --chip esp32s3 -p COMn -b 460800 --before default_reset --after hard_reset write_flash --flash_mode keep --flash_freq keep --flash_size keep 0x0 backup-16mb.bin
```

The board then boots exactly as before the install, old firmware and settings included. Camera database
alone: `tools\flash_camdb.ps1` in the Simian repo with `-Multimode`.
