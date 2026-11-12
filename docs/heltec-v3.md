# LFD3K-SM on the Heltec V3

The LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM runs on the **Heltec WiFi LoRa 32 V3** as well
as the V4. Same modem, same AT commands, same radio protocol: a V3 and a V4 call each other like any
two LFD3K-SM modems. On the V3 the modem is the only firmware on the board (no mode picker), and the
AXIS boot logo plays for 5 seconds at power on before the modem's own splash.

## What is different on the V3

| | Heltec V4 | Heltec V3 |
|---|---|---|
| Chip | ESP32-S3R2, 2 MB PSRAM | ESP32-S3, no PSRAM |
| Flash | 16 MB | 8 MB |
| Radio | SX1262 with the GC1109 front end (about +12 dB) | SX1262 straight to the antenna |
| TX power | S30 is the chip setting, about 12 dB more comes out | S30 is the power at the antenna, 22 dBm at most |
| USB | native USB (VID 303A, PID 1001) | CP2102 USB to serial bridge (VID 10C4, PID EA60) |
| Firmware build | `heltec_v4` (multimode) or `heltec_v4_single` | `heltec_v3_single` |
| Multimode picker | yes (four modes) | no, the modem alone |

The radio wiring is the same on both boards (SX1262 on SPI pins 8 to 14, TCXO at 1.8 V on DIO3, DIO2
switching the antenna), and so are the OLED, the PRG button, the LED and the Vext power pin.

`AT&V` tells you which power figure applies: on a V3 it prints `TX POWER S30=2 dBm at the antenna`, on a
V4 `TX POWER S30=2 dBm chip, about 14 dBm out`. The setup menu (`AT+MENU`) says the same.

## Build and flash

You need the same tools as for the V4 (see the README: `espup` for the Rust toolchain, PlatformIO).

1. Put the antenna on. Nothing should transmit without it.
2. Plug the V3 into USB. Device Manager shows it under Ports as **Silicon Labs CP210x USB to UART
   Bridge (COMn)**. If nothing appears, try another USB-C cable: many are charge only.
3. Optional but wise: back up what is on the board first (about 2 minutes):

   ```
   python -m esptool --port COMn --baud 921600 read-flash 0 0x800000 v3-backup.bin
   ```

4. Build the Rust core, then build and upload the V3 firmware:

   ```
   powershell -ExecutionPolicy Bypass -File firmware\build_core.ps1
   pio run -d firmware -e heltec_v3_single -t upload --upload-port COMn
   ```

   The V3 resets itself into download mode and back over the CP2102, so no buttons are needed. If the
   upload cannot connect, hold **PRG**, tap **RST**, let go of PRG, and run the upload again.

5. The board restarts: the AXIS logo builds up on the OLED, then the modem's splash and idle screen.

To put the old firmware back: `python -m esptool --port COMn --baud 921600 write-flash 0 v3-backup.bin`.

## Talking to it

The modem is on the V3's COM port at **115200 baud, 8 data bits, no parity, 1 stop bit, no flow
control**.

* **Open the port with DTR and RTS off.** On the V3 those two lines are wired to the reset and boot pins,
  so a terminal that raises them when it opens the port resets the board. In miniterm:

  ```
  python -m serial.tools.miniterm COMn 115200 --dtr 0 --rts 0 --eol CR
  ```

  In SyncTERM pick the serial connection, 115200, and turn hardware flow control off.
* After any reset the ESP32-S3 itself prints a few lines of boot text (`ESP-ROM:esp32s3...`) on the port
  before the modem starts. That comes from the chip's ROM and cannot be turned off without burning
  eFuses, which this project never does. Once the modem runs, the port carries nothing but modem bytes.
* There is no flow control on this port, so very long pastes at full speed can outrun the modem's
  buffer. Normal typing, menus and BBS calls are fine.

Then everything works as on the V4: `ATI`, `AT&V`, `AT+MENU`, `ATDT <name>`, `AT+SCAN`, `AT+WSTA`,
`AT+SIMNET` (see [SimNet](simnet.md)), and the rest in [Features, settings and AT commands](FEATURES.md).

## TX power and the rules

The default S30 is 2, which on a V3 is 2 dBm at the antenna (on a V4 the same setting puts out about
14 dBm, so a V3 at the default is the quieter of the two). `ATS30=<n>` sets -9 to 22 dBm; `AT&W` saves
it. Check the rules for your region
before you raise it.

## Not on the V3

* **The multimode picker** (SIMIAN RADAR, RNODE, MESH beside the modem) is built for the V4's 16 MB and
  its layout. `AT+MODE` answers `ERROR` on a V3, as on any board with the modem alone.
