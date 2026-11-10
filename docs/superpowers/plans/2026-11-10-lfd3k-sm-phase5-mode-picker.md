# LFD3K-SM Phase 5 (Mode Picker and Multi-Mode V4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Heltec V4 that boots into a 5 second animated AXIS BBS logo every time, where a PRG press opens a mode list on the OLED, and that otherwise starts the last used mode: the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM, Simian Radar, RNode, or a patched Meshtastic.

**Architecture:** A new 16 MB partition table with one app slot per mode. Our own bootloader (an ESP-IDF 5.4 `bootloader_components` override, closed) boots the picker (factory app) on every reset except a software reset carrying a one-time RTC token, in which case it follows otadata into the chosen mode. Deep-sleep wakes also follow otadata. The picker (closed, ESP-IDF) draws the logo and menu, remembers the last mode in its own NVS partition, sets otadata, writes the token and restarts. The open images run unmodified (RNode, Simian Radar) or lightly patched (Meshtastic, its own GPL repo) from their slots and find their storage by partition label. LFD3K-SM gets its own NVS partition so no other firmware can erase its settings.

**Tech Stack:** ESP-IDF 5.4.1 via PlatformIO (bootloader, picker, modem), Arduino based images built by their own projects, esptool 4.9 `merge_bin` for the install image, Python 3.12 for layout checks.

**Spec:** `docs/2026-11-09-lfd3k-sm-design.md` section 12 and section 19 (Rev's picker decisions); research `docs/research/2026-11-10-multimode-research.md`.

## Global Constraints

- Closed parts: bootloader override, picker, LFD3K-SM, install tooling, all in `F:\lfd3k-sm`, private, never pushed. Open parts (Simian Radar, RNode, Meshtastic) are separate images in their own slots, never linked into closed code. The patched Meshtastic lives in its own GPL repo `F:\meshtastic-axis` with its source kept beside every binary.
- No AI attribution in any commit in any repo (Rev's rule overrides any harness reminder). No em dashes or double hyphens in user-facing text (OLED text, docs).
- **Hardware rules:** no subagent opens a serial port or flashes. Only the controller flashes, only when Rev says so, after matching VID 303A PID 1001 and MAC 88:56:A6:96:16:FC; a full 16 MB backup comes first (`--no-stub --after no_reset read_flash`, about 26 minutes, verified with a 64 KB re-read and `fc.exe`). The one-time full wipe for the new layout is approved by Rev (2026-11-10) but Claude asks again right before it.
- **Downloads:** official GitHub release assets only (RNode 1.86 "Heltec V4PA", Meshtastic v2.7.26 heltec-v4); the controller shows file names and sizes to Rev before fetching. Cloning the Meshtastic source for the patched build is asked separately. Downloaded files go in their own empty folder under `F:\lfd3k-sm\vendor\` (git-ignored) and are never executed; only their `.bin` images are written to flash.
- **Board:** plain V4, ESP32-S3R2, 16 MB flash, QIO 80 MHz, quad PSRAM 2 MB. Bootloader built for exactly that.
- **Recovery:** holding PRG at power-on enters the ROM download mode regardless of anything we flash; every doc says so. A bad bootloader is fixed by re-flashing it from download mode.
- Rev's rule: tests come in the final task; earlier tasks keep the existing tests passing.

### Partition layout (16 MB)

| Label | Type / subtype | Offset | Size | For |
|---|---|---|---|---|
| nvs | data/nvs | 0x9000 | 0xF000 | shared NVS (RNode EEPROM emulation, Simian, Meshtastic) |
| otadata | data/ota | 0x18000 | 0x2000 | boot selection |
| picker_nvs | data/nvs | 0x1A000 | 0x6000 | picker only (last mode) |
| factory | app/factory | 0x20000 | 0x80000 | picker (512 KB) |
| ota_0 | app/ota_0 | 0xA0000 | 0x400000 | Meshtastic (4 MB) |
| ota_1 | app/ota_1 | 0x4A0000 | 0x180000 | LFD3K-SM (1.5 MB) |
| ota_2 | app/ota_2 | 0x620000 | 0x180000 | Simian Radar (1.5 MB) |
| ota_3 | app/ota_3 | 0x7A0000 | 0x200000 | RNode (2 MB) |
| lfd3k_nvs | data/nvs | 0x9A0000 | 0x10000 | LFD3K-SM settings and directory, its own |
| spiffs | data/spiffs | 0x9B0000 | 0x200000 | Meshtastic LittleFS (label "spiffs") |
| littlefs | data/spiffs | 0xBB0000 | 0x100000 | Simian Radar marks |
| camdb | data/0x40 | 0xCB0000 | 0x320000 | Simian camera DB (3.1 MB; file is 1.99 MB) |
| coredump | data/coredump | 0xFD0000 | 0x10000 | crash dumps |
| (free) | | 0xFE0000 | 0x20000 | spare |

The patched Meshtastic has no OTA loader, so it does not need a second slot. Builder may adjust sizes only if an image does not fit; the layout checker (Task 6) must pass. Mode order in the menu: LFD3K-SM, SIMIAN RADAR, RNODE, MESH (the Meshtastic name is fine on Rev's own board; nothing sold gets the name without asking).

## Review Focus

1. The board must never brick: a missing or invalid image in a slot shows `NO IMAGE` in the picker and stays in the picker; a mode that crashes at boot returns to the picker on the next reset, not into a reboot loop. Tests `picker_skips_empty_slot`, `bootloader_without_token_boots_picker`.
2. The token is single use: a stale token in RTC memory after a power cut (RTC memory is garbage after power-on) must never pick a mode; it carries a magic, the slot, and a CRC, and is cleared by the bootloader before it boots anything. Tests `token_needs_magic_and_crc`, `token_cleared_before_boot`.
3. Deep sleep wake (Meshtastic power saving) goes straight back into the running mode without the logo. Test on the decision function: `deep_sleep_wake_follows_otadata`.
4. LFD3K-SM settings survive anything another mode does to the shared `nvs`: our modem reads and writes only `lfd3k_nvs`, and migrates its old settings from `nvs` once if present. Test `modem_uses_own_nvs_partition`.
5. The install image must never put anything over the bootloader or partition table by accident and every image must fit its slot. Test `layout_check_passes` and `install_image_offsets_match_table`.

---

### Task 1: Partition table and the modem's own NVS

**Files:** Create `multimode/partitions-multimode.csv`, `multimode/README.md`. Modify `firmware/partitions.csv` (becomes the multimode table, or the env points at the new one), `firmware/src/settings_store.c` (open `lfd3k_nvs` with `nvs_flash_init_partition` and `nvs_open_from_partition`; on first boot copy an old `lfd3k/settings` and `lfd3k/dir` blob from `nvs` if present, then erase only those keys), `docs/FEATURES.md`.
- [ ] The CSV exactly as the layout table. The modem env builds an app for slot `ota_1` (any slot works; nothing in the app depends on its offset).
- [ ] Build `pio run`, cargo test. Commit `feat(firmware): multimode partition table, modem settings in their own NVS`.

### Task 2: The bootloader override

**Files:** Create `multimode/bootloader/` as an ESP-IDF `bootloader_components/main` override used by a dedicated PlatformIO env `picker` (Task 3 project), with a pure C decision function in `multimode/bootloader/choose.c` and `choose.h`:
```c
typedef enum { BOOT_PICKER, BOOT_OTADATA } boot_choice_t;
/* reset: the ROM reset reason; token: the RTC token as read (may be garbage); returns the choice
 * and whether the token must be cleared. Pure: no hardware access, PC-testable. */
boot_choice_t choose_boot(uint32_t reset_reason, const boot_token_t *token, bool *clear_token);
```
- Token in RTC FAST memory at a fixed address both bootloader and picker agree on (document it): `{ uint32 magic 'LFMP'; uint8 slot; uint8 pad[3]; uint32 crc32 }`.
- Rule: deep-sleep wake: otadata. Software reset (or core panic reset) with a valid token: clear it, otadata. Everything else (power on, RST button, brownout, watchdog without token, software reset without token): picker.
- The override reuses the stock bootloader steps (`bootloader_init`, `bootloader_utility_load_partition_table`, `bootloader_utility_get_selected_boot_partition`, `bootloader_utility_load_boot_image`) and only changes which index it loads; the factory index is the picker.
- [ ] Build with the picker env. Commit `feat(multimode): bootloader that shows the picker on every boot`.

### Task 3: The picker app

**Files:** Create `multimode/picker/` (ESP-IDF app, PlatformIO env `picker`, `board_build.partitions` the multimode CSV, `board_build.bootloader` override from Task 2). Reuse the SSD1306 driver and the 5x7 font from `firmware/src` by path (shared closed code, not copied).
- Boot: Vext on, OLED on, 5 second animated AXIS BBS logo (the AXIS letters build up from scan lines, a sweep runs under them, a 5 dot countdown at the bottom). During it, a PRG press (GPIO0, debounced, read as a normal input after boot) opens the mode list.
- Mode list: the four modes with a marker on the last used one; short press moves, long press (about 0.8 s) starts it. Slots without a valid app image (check `esp_image_verify` or the app descriptor magic) show `NO IMAGE` and cannot be started. 15 s with no press starts the highlighted mode.
- No press during the logo: start the last mode (from `picker_nvs`); first ever boot with no last mode: LFD3K-SM.
- Start a mode: `esp_ota_set_boot_partition(slot)`, write the token, `esp_restart()`.
- Also exposes nothing on USB (no console on USB) so a host never sees noise; logs on UART0.
- Size target under 512 KB.
- [ ] Build `pio run -d multimode/picker`. PC check of the menu logic (pure function: input events to state). Commit `feat(multimode): picker app with the AXIS logo and mode list`.

### Task 4: Switch modes from inside LFD3K-SM

- [ ] `AT+MODE?` lists modes with `*` on the running one and `NO IMAGE` where empty; `AT+MODE=RADAR|RNODE|MESH|MODEM` sets otadata, writes the same token and restarts into that mode directly (no logo); `AT+MODE=PICKER` restarts into the picker. Core parses, firmware does it (C ABI `lfd3k_take_mode_request`). Commit `feat: AT+MODE switches firmware modes`.

### Task 5: The other images and the install image

- [ ] Simian Radar: in `F:\simian-radar` (Rev's GPL repo), add a PlatformIO env `heltec_v4_multimode` that uses the multimode partition CSV (copied there with a comment), and update `tools/flash_camdb.ps1` to take the camdb offset from the CSV (0xCB0000). Commit there without AI attribution.
- [ ] Meshtastic: after Rev OKs the clone, `F:\meshtastic-axis` from the v2.7.26 tag; patch: no OTA loader partition use and no slot switching from its updater, factory reset erases only Meshtastic's own NVS namespaces and its filesystem instead of `nvs_flash_erase()`, partition table = multimode CSV; build `heltec-v4`. Keep a `PATCHES.md` listing every change. Commit there.
- [ ] RNode: the official 1.86 Heltec V4PA release `.bin` (application image only, no console image), fetched after Rev OKs the exact files; verify its SHA-256 against the release page.
- [ ] `tools/build_multimode.ps1` in `F:\lfd3k-sm`: builds or collects every piece, runs the layout checker, and writes `multimode/out/full-16mb.bin` with `esptool merge_bin` plus a `flash_args.txt`; never flashes.
- [ ] Commit `feat(multimode): install image builder`.

### Task 6: Tests, docs and the bench

- [ ] PC tests: `choose_boot` table (every reset reason, valid, stale, corrupt tokens), picker menu logic, layout checker (`tools/check_layout.py`: alignment 0x10000 for apps, 0x1000 for data, no overlap, inside 16 MB, every image fits), install offsets match the CSV, `modem_uses_own_nvs_partition` (host test with a fake NVS layer in settings_store or by code inspection plus a build assertion), and the Review Focus items.
- [ ] `docs/multimode.md` for Rev: what each mode is, how to pick, `AT+MODE`, recovery with PRG at power-on, the GPL source note for the open images.
- [ ] **Bench (controller, with Rev, after his go on the wipe):** full backup, verify, erase, write `full-16mb.bin`, logo and menu screenshots (picker gets an `AT+FB` equivalent? no USB: photograph by Rev or add a UART-free screenshot later), boot each mode once, switch modes with `AT+MODE` from LFD3K-SM, RNode provisioned with `rnodeconf` over its serial port (no firmware writes from rnodeconf), Meshtastic set up fresh, Rev retypes `AT+WSTA`.
