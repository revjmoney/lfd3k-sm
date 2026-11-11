# Multimode: four firmwares on one Heltec V4

For Rev. One board, four firmwares, one small program in front that lets you pick. This is the
plain language guide. The engineering details are in `multimode/README.md`, and the install steps
(backup, erase, write) are in `multimode/INSTALL.md`.

## The four modes

| Menu name | What it is | Lives in |
|---|---|---|
| LFD3K-SM | The LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM, the Hayes style LoRa and Wi-Fi modem. The default. | slot ota_1 |
| SIMIAN RADAR | Your listen only pocket scanner: license plate cameras, trackers, smart glasses. | slot ota_2 |
| RNODE | The RNode firmware (official 1.86 Heltec V4PA build) so the board works as a Reticulum LoRa interface. | slot ota_3 |
| MESH | Meshtastic 2.7.26 for the Heltec V4, patched so it lives politely next to the others. | slot ota_0 |

Only one runs at a time. Each one has its own space in the flash, so installing or updating one never
touches the others. The picker itself is a fifth, tiny program (it is not a mode) that starts first on
every boot.

## What you see on every boot

1. The OLED shows the AXIS BBS logo for 5 seconds. The letters build up from scan lines, a sweep
   runs along the rule under them, and five dots count down at the bottom.
2. If you do nothing, the picker starts the mode you used last. The very first time (nothing stored
   yet) it starts LFD3K-SM.
3. If you press PRG during the logo, the logo stops and the mode list opens.

## The menu keys

The only key is PRG (the button on the board, also called GPIO0).

* **Short tap:** move the highlight to the next mode. It wraps around: LFD3K-SM, SIMIAN RADAR, RNODE,
  MESH, then back to LFD3K-SM.
* **Hold for about 0.8 seconds, then let go:** start the highlighted mode. It starts when you release, not
  while you hold, so the board never boots with the button held down.
* **Do nothing for 15 seconds:** the picker starts the highlighted mode on its own.
* An arrow marks the mode you used last.
* A slot with no usable firmware shows NO IMAGE. You cannot start it, and holding PRG on it just shows
  the message again. If a start fails at the last moment (the firmware does not verify), you land back
  in the list with NO IMAGE on that line. The board cannot get stuck here: the list is always reachable.
* If PRG is already held down when the board powers up, the list opens and that first press is ignored,
  so holding the button while plugging in never starts anything by accident. (This is different from
  the recovery trick below, which needs PRG held at the very moment power comes on.)

## Switching from inside LFD3K-SM: AT+MODE

From any terminal talking to the modem:

* `AT+MODE?` lists the modes. The one you are in has a star after it, and a slot with nothing in it
  says NO IMAGE:

  ```
  MODE LFD3K-SM *
  MODE SIMIAN RADAR
  MODE RNODE NO IMAGE
  MODE MESH
  OK
  ```

* `AT+MODE=RADAR`, `AT+MODE=RNODE`, `AT+MODE=MESH` or `AT+MODE=MODEM` switches straight to that mode.
  There is no logo and no waiting: the modem answers `OK`, the board restarts, and the new mode is up in
  a couple of seconds. Upper or lower case both work.
* `AT+MODE=PICKER` restarts into the picker, so you see the logo and the list.

What to know:

* The switch only happens when the modem is idle. During a call (dialing, ringing, connected, a Wi-Fi
  call) and while an `AT+SCAN` is running it answers `ERROR` and nothing changes. Hang up first.
* An empty slot answers `ERROR` too. A firmware that does not verify also answers `ERROR`, and the
  board stays in LFD3K-SM.
* The mode you switch to becomes the "last used" mode. So after a power cycle the board starts that
  mode again, the same as if you had chosen it in the picker. (`AT+MODE=PICKER` leaves the stored last
  mode as it was.)
* If a setting or the station directory had not been written to flash yet, the modem writes it before
  restarting. If that write fails, it answers `ERROR` and does not switch.
* Anything typed after `AT+MODE=...` in the same burst of characters is thrown away, so nothing can
  start behind the restart.
* With `ATV0` (numeric answers) you get `0` for OK and `4` for ERROR, as with every other command.
* This only exists on a board with the multimode layout. A modem flashed alone on a single app board
  answers `ERROR` to every `AT+MODE`.

## What happens after a crash

If a mode crashes (a panic, a watchdog reset, or a brownout), the board restarts through the picker,
as it always does. The picker notices why it restarted. This time it does not start anything on its own:
the logo ends in the mode list with the line LAST MODE CRASHED, and the 15 second automatic start is
switched off for that boot. You choose a mode by hand (or pick another one). A mode that crashes at
startup can never trap the board in a reboot loop, because the picker is in front of every start.

The next normal power on or RST press behaves as usual again.

A mode that restarts itself on purpose (after a settings change, say) also comes back through the
picker, so you see the logo, and then it starts again after 5 seconds. The one exception is Meshtastic
power saving: waking from deep sleep goes straight back into the running mode with no logo, so battery
use is not hurt.

## Recovery: PRG at power on

If anything ever goes wrong with what is flashed, even the picker or the bootloader themselves:

1. Unplug USB.
2. Hold **PRG**.
3. Plug USB in (or switch the board on) while still holding PRG, then let go after a second.

The ESP32-S3 then starts its built in download mode, which lives in the chip itself and cannot be
overwritten. A flashing tool can write the whole board again from there: either the install image
(`multimode/out/full-16mb.bin`) or your full 16 MB backup. The exact commands are in
`multimode/INSTALL.md`. Nothing in this project can remove that safety net.

Two cautions that follow from the layout:

* Never use a plain upload or erase from PlatformIO on this board (for example `pio run -t upload`
  or `-t erase`). Either one would replace or wipe every mode. The modem project refuses both.
* Never flash a `firmware.factory.bin` (Meshtastic and others build one). It carries its own
  bootloader and partition table and would replace the picker. Only the plain app image of each mode
  is written, into its own slot.

## Setting up RNode, and one button to avoid

* **RNode setup is EEPROM bootstrap and firmware hash only.** While RNODE mode is running, use
  `rnodeconf` over its serial port for those commands. NEVER run `rnodeconf --autoinstall`, `--update`
  or anything that writes firmware: it would overwrite the bootloader, the partition table, the shared
  nvs, the picker and the start of MESH. The RNode image's file SHA-256 is
  `df590b2d1aa4f4414cbf02314550a70d0636d7c5a689b9389189c028c86a0ea1`; the firmware hash the device
  should report (appended SHA-256 in the image) is
  `f312de7d8ec431fe1b80a84067cb01932cdeeaa7cb24f69e1b499f11c0e61d12`. Compare before setting it.
* **In RNODE mode, do not long-press PRG.** That may start RNode's console feature, which may mount and
  format the first SPIFFS partition, which is Meshtastic's data (unverified against the 1.86 source).

## Your settings stay yours

* LFD3K-SM keeps its settings and station directory in its own storage area (`lfd3k_nvs`). The other
  modes only use the shared area, so nothing they do to it can touch the modem's Wi-Fi name, password,
  or directory. The first time the modem starts on this layout it copies any old settings from the
  shared area, once, and removes just those two items there. The modem never erases or reformats the
  shared area.
* The picker's own "last mode" lives in a separate small area of its own.
* After a full install the settings area is empty, so the modem comes up with defaults. Retype
  `AT+WSTA` (Wi-Fi name and password) and your other settings. Your old ones are in the backup.

## Meshtastic and the shared Bluetooth bonds

Meshtastic's "factory reset" has been changed so it no longer wipes the whole shared storage area. It
now clears only Meshtastic's own entries, so RNode's stored configuration survives.

One thing it still clears: the Bluetooth pairing store (`nimble_bond`) is shared by every firmware here
that uses the NimBLE Bluetooth stack. So **a Meshtastic factory reset also deletes Simian Radar's saved
Bluetooth bonds**. Simian Radar then just needs to be paired again. Nothing else is lost.

Meshtastic's built in firmware updater is switched off in this build (it would try to take over a slot
that belongs to another mode). Meshtastic is updated by installing a new image from the PC, like the
other modes.

## Source code and licences

* **LFD3K-SM, the picker and the bootloader override, and the install tools** are yours: closed
  source, private, never pushed.
* **Simian Radar** is your own project, licensed GPL v3, in `F:\simian-radar` (branch `multimode`). Its
  source goes with its binary.
* **MESH** is Meshtastic, GPL v3, and the image is patched, so the patched source is kept in its own
  repository, `F:\meshtastic-axis` (branch `axis-multimode`, based on tag v2.7.26.54e0d8d). `PATCHES.md`
  there lists every change. Anyone who is given that binary must also be given that source.
* **RNODE** is the official RNode firmware 1.86 for the Heltec V4PA, GPL v3, installed exactly as
  released (the application image only, not changed). Its source is the upstream RNode_Firmware
  project at the 1.86 release. If the binary is ever handed to someone, hand them the matching source
  or an offer for it too.

The closed parts and the open parts are separate programs in separate slots. They never link together:
the picker only starts an image, it does not contain one. That is what keeps the GPL images GPL and the
closed parts closed.

## Where things are

| What | Where |
|---|---|
| This guide | `docs/multimode.md` |
| Layout, token, boot rules (engineering) | `multimode/README.md` |
| Install steps: backup, erase, write, recovery | `multimode/INSTALL.md` |
| Install image builder | `tools/build_multimode.ps1` (never flashes) |
| Layout and RAM checks | `tools/check_layout.py`, `tools/check_images.py` |
| PC tests | `multimode/tests/run.ps1`, `tools/test_check_layout.py`, `core/tests/mode.rs` |
