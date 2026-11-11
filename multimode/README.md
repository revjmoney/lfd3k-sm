# LFD3K-SM multimode (picker and bootloader)

One Heltec V4 (ESP32-S3R2, 16 MB flash QIO 80 MHz, quad PSRAM 2 MB) carries
four firmware modes, each in its own app slot. A small custom bootloader starts
the **picker** on every boot; the picker shows the AXIS BBS logo for 5 seconds,
opens the mode list on a PRG press, and otherwise starts the last used mode.

Closed source. Private. Never pushed.

## Partition layout (`partitions-multimode.csv`)

| Label | Type / subtype | Offset | Size | For |
|---|---|---|---|---|
| nvs | data/nvs | 0x9000 | 0xF000 | shared NVS (RNode, Simian, Meshtastic) |
| otadata | data/ota | 0x18000 | 0x2000 | boot selection |
| picker_nvs | data/nvs | 0x1A000 | 0x6000 | picker only (last mode) |
| factory | app/factory | 0x20000 | 0x80000 | picker (512 KB) |
| ota_0 | app/ota_0 | 0xA0000 | 0x400000 | MESH (Meshtastic, 4 MB) |
| ota_1 | app/ota_1 | 0x4A0000 | 0x180000 | LFD3K-SM (1.5 MB) |
| ota_2 | app/ota_2 | 0x620000 | 0x180000 | SIMIAN RADAR (1.5 MB) |
| ota_3 | app/ota_3 | 0x7A0000 | 0x200000 | RNODE (2 MB) |
| lfd3k_nvs | data/nvs | 0x9A0000 | 0x10000 | LFD3K-SM settings, its own |
| spiffs | data/spiffs | 0x9B0000 | 0x200000 | Meshtastic LittleFS |
| littlefs | data/spiffs | 0xBB0000 | 0x100000 | Simian Radar marks |
| camdb | data/0x40 | 0xCB0000 | 0x320000 | Simian camera DB |
| coredump | data/coredump | 0xFD0000 | 0x10000 | crash dumps |
| (free) | | 0xFE0000 | 0x20000 | spare |

The bootloader sits at 0x0 (limit 0x8000), the partition table at 0x8000.

Menu order and slots: LFD3K-SM = ota_1, SIMIAN RADAR = ota_2, RNODE = ota_3,
MESH = ota_0.

## The launch token

12 bytes in RTC FAST memory at **0x600FFFC0** (`LFMP_TOKEN_ADDR` in
`bootloader/choose.h`):

| Offset | Field | Value |
|---|---|---|
| 0 | magic (u32) | 0x504D464C, the bytes `L F M P` |
| 4 | slot (u8) | ota index the picker chose (informational) |
| 5 | pad (3 bytes) | zero |
| 8 | crc32 (u32) | CRC-32 (IEEE, reflected) of bytes 0..7 |

Why this address:

* RTC FAST RAM (0x600FE000 to 0x60100000 on the data bus, 8 KB) is reachable
  from the second stage bootloader and from apps on the PRO CPU.
* It **keeps its contents across a software reset** (`esp_restart()`, a panic
  reset): only the digital core and CPU are reset, the RTC domain is not, and
  neither the ROM nor the bootloader clears it.
* After **power on** its contents are random, which the magic plus CRC rejects.
* 0x600FFFC0 is 64 bytes below the top. In the **picker** nothing owns it:
  its RTC FAST code and data end at 0x600FE11C, its RTC timer data is the top
  24 bytes (0x600FFFE8), and RTC FAST heap is off (`sdkconfig.defaults`, and
  `main.c` refuses to build if it or an IDF RTC reserve option is turned on).
* The **other modes** do use that memory: Arduino 2.x (Simian, RNode), the
  LFD3K-SM modem and Meshtastic all keep RTC FAST in their heap, and Arduino
  reserves only the top 16 bytes. So the address is not free in those modes, and
  it does not need to be: the token is only trusted on the boot right after the
  picker wrote it, the bootloader consumes it there, and stray heap data never
  forms a valid token (magic, zero pad, slot 0..15 and a matching CRC-32).
* We do **not** use IDF's `BOOTLOADER_CUSTOM_RESERVE_RTC`: with it the
  bootloader rewrites the top of RTC FAST on every boot, deep sleep wakes
  included, which would overwrite whatever the running mode keeps there across
  a deep sleep. This bootloader writes RTC FAST only to clear a valid token,
  which exists only on the boot right after the picker.

Other modes may scribble anything over that address; they never write a valid
token (magic, zero pad, CRC), and the token is consumed on the boot that sees it.

## Boot rules (bootloader, `bootloader/choose.c`)

| Reset reason | Token | Boots | Token |
|---|---|---|---|
| deep-sleep wake | any | otadata (the mode resumes itself) | left alone |
| software reset (system or CPU0, which includes panic) | valid | otadata (the mode the picker set) | cleared |
| anything else: power on, RST button, brownout, watchdog, USB reset, software reset without token | | picker (factory) | cleared if valid |

`esp_restart()` on the S3 ends in a CPU0 software reset (reason 0x0C), so both
0x03 and 0x0C count as software resets.

On the token path the bootloader also checks that otadata selects the slot the
token names and that its image verifies; otherwise it boots the picker, so the
stock loader never falls back to a neighbouring mode.

Consequences: a mode that reboots itself comes back through the picker. After a
normal restart the picker starts it again after 5 s unless PRG is pressed; after
a crash (panic, watchdog, brownout) the picker opens the list instead (see below). The override keeps the stock steps (`bootloader_init`, partition table,
`bootloader_utility_get_selected_boot_partition`, `bootloader_utility_load_boot_image`)
and only changes the index. If the chosen image fails verification the stock
loader tries the other slots.

## The picker

* Vext on, OLED on, 5 s AXIS BBS logo: a CRT line opens, the AXIS letters build
  up from interlaced scan lines, a glint crosses them, a sweep runs along the
  B B S rule, a 5 dot countdown runs at the bottom, "PRESS PRG FOR MODES".
* PRG during the logo: mode list (LFD3K-SM, SIMIAN RADAR, RNODE, MESH). The arrow
  marks the last used mode. Tap: next. Hold about 0.8 s, then release: start
  (the mode starts only once PRG is up, so it never boots with GPIO0 low).
  15 s with no press: the highlighted mode starts.
* After a crash (the picker sees a panic, task or interrupt watchdog, or
  brownout reset reason) nothing starts by itself: the logo ends in the list with
  the line LAST MODE CRASHED, and the 15 s idle start is off for that boot. The
  next normal boot (power on, RST button) behaves as usual.
* No press during the logo: the last mode starts (stored in `picker_nvs`,
  namespace `picker`, key `last`); on the first ever boot: LFD3K-SM.
* Slots without an app image (image magic 0xE9, S3 chip id, app descriptor magic
  0xABCD5432 at offset 0x20, read with `esp_partition_read`) show NO IMAGE and
  cannot be started. If a start fails anyway, NO IMAGE shows and the list stays.
* Starting a mode: a full `esp_image_verify` of the slot every time (a failure
  shows NO IMAGE and stays), `esp_ota_set_boot_partition` (skipped when otadata
  already points there), save the last mode, write the token, `esp_restart()`.
* Console and logs on UART0 only; nothing on the native USB port. The picker
  only ever opens its own `picker_nvs`, never the shared `nvs`.

Pure logic for PC tests: `bootloader/choose.c` (boot decision and token) and
`picker/src/picker_menu.c` (debounced level and time in, state out).
`picker/src/picker_draw.c` is pure drawing.

## Recovery

Holding **PRG at power on** enters the ESP32-S3 ROM download mode regardless of
anything flashed. A bad bootloader, picker or partition table is fixed by
reflashing it from download mode.

## Build

```
$env:PATH = "$env:USERPROFILE\.platformio\penv\Scripts;$env:PATH"
pio run -d multimode/picker
```

Outputs in `multimode/picker/.pio/build/picker/`: `bootloader.bin` (0x0),
`partitions.bin` (0x8000), `firmware.bin` (the picker, factory at 0x20000).
The bootloader override is `picker/bootloader_components/main` (ESP-IDF's
bootloader_override mechanism); the sdkconfig of this project also builds the
bootloader, so it fixes QIO, 80 MHz, 16 MB for every mode.

Nothing here uploads. Flashing is done by hand, after a full backup.

## Previews

`preview/preview.c` runs the same drawing and menu code on the PC:

```
gcc -O2 -I multimode/picker/src -I firmware/src multimode/preview/preview.c ^
    multimode/picker/src/picker_draw.c multimode/picker/src/picker_menu.c -o preview.exe
preview.exe logo.raw menu.raw
py -3.14 multimode/preview/make_preview.py logo.raw menu.raw
```

Writes `preview/logo.gif`, `preview/logo_sheet.png` and `preview/menu.png`.
The AXIS letters come from `preview/gen_logo.py`, which writes
`picker/src/logo_bitmaps.h`.
