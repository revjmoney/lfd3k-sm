# LFD3K-SM Phase 2 (Real Radio) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM on Rev's Heltec V4: our own SX1262 driver, the phase 1 core running on the device, AT commands over USB, the OLED front panel with screenshots, and an on-air check with an SDR, ready for the first two-radio call when a second V4 arrives.

**Architecture:** A new `no_std` Rust crate `sx1262` drives the chip through a tiny `Bus` trait (SPI transfer, BUSY pin, reset, front-end pins, clock) and implements `lfd3k_core::Radio`; on the PC it is tested against a mock bus. The `ffi` crate owns the one static `Modem` plus the device `Radio`, exposes a C ABI and calls a small C HAL. The ESP-IDF firmware supplies that HAL (SPI, GPIO, DIO1 interrupt, ms clock), runs every core call on one modem task, bridges USB-Serial/JTAG to the core, and draws the SSD1306 front panel on a low-priority task.

**Tech Stack:** Rust (stable for PC, `esp` toolchain for `xtensa-esp32s3-none-elf`), ESP-IDF 5.4.1 via PlatformIO `espressif32@6.11.0`, C for the platform, Python 3.12 + pyserial 3.5 for bench tools.

**Spec:** `docs/2026-11-09-lfd3k-sm-design.md` (sections 3, 4, 5, 8.4, 13, 15 step 2, 18).

## Global Constraints

- Closed source, private repo, never pushed. No GPL or LGPL code. MeshCore (MIT), RadioLib (MIT) and Semtech's driver (BSD) may be read for facts; write the code fresh. Record any new crate in `THIRD_PARTY.md`.
- `lfd3k-core` and `sx1262` are `no_std`, no heap, `#![forbid(unsafe_code)]`, never panic on any input (no `unwrap`, `expect`, or out-of-range indexing in non-test code). Only `lfd3k-ffi` may use `unsafe`.
- **Hardware rules:** no subagent ever opens a serial port, runs `-t upload`, `esptool`, or `pio device monitor`. Nobody but Rev flashes, resets or probes COM11 (Rev's V4) or COM15 (lathe), and COM9 (the V3, stays RNode) is not touched. Rev flashes the V4 after a full backup. Only the controller (not a subagent) may open the V4 **as a serial terminal only**, in Task 7 Step 5, after Rev says LFD3K-SM is on it, with DTR and RTS set False before open and never toggled, and only after the tool has matched the port to the V4 by USB identity.
- **Board:** Rev's V4 reports `Heltec V4 OLED` (plain V4, GC1109 front end, not the V4.3 KCT8103L), native USB `VID_303A PID_1001`. Uploads use esptool 4.9 (`platform_packages = platformio/tool-esptoolpy@2.40900.250804`) with `board_upload.after_reset = watchdog_reset`, as in `F:\simian-radar\platformio.ini`.
- User-facing text never uses em dashes or double hyphens. Chat with Rev uses the full name `LORA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM`.
- No AI attribution in commits.
- Per Rev's rule, Tasks 1 to 6 write no new tests; the existing 40 tests must keep passing after every task (update their expected values where a planned change moves them). All new tests are written in Task 7.
- Keep `docs/FEATURES.md` updated in the task that adds a feature or setting.
- **V4 pins:** SX1262 NSS 8, SCK 9, MOSI 10, MISO 11, RST 12, BUSY 13, DIO1 14; TCXO 1.8 V on DIO3; DIO2 is the RF switch. GC1109 front end: GPIO7 power LDO HIGH, GPIO2 enable HIGH, GPIO46 PA mode (HIGH while transmitting, LOW otherwise). OLED SSD1306 I2C SDA 17, SCL 18, RST 21, address 0x3C. Vext GPIO36 **LOW = on**. LED GPIO35. PRG GPIO0.
- **Radio settings (all tiers):** calling channel 918.5 MHz (frequency word `0x3968_0000`), BW 500 kHz, CR 4/5, LDRO off, explicit header, CRC on, IQ standard, private sync word (registers 0x0740 = 0x14, 0x0741 = 0x24), preamble 8 symbols (12 at SF5 and SF6).
- **Power:** new S-register `S30` = SX1262 chip output in dBm, range -9 to 22, default 2. The GC1109 adds about 12 dB (chip 10 gives about 22 dBm at the antenna), so the default is about 14 dBm out. `AT&V` shows `TX POWER S30=<n> dBm chip, about <n+12> dBm out`.
- **Air id:** every frame starts with one byte `AIR_ID = 0xD1` (0xD = LFD3K-SM, 1 = air protocol version 1). Anything else decodes as `FrameError::Foreign` and is ignored.

## Review Focus

1. A false preamble (noise or a foreign LoRa packet that never produces a valid header) must not leave `busy()` stuck true: the driver times it out and re-arms receive. Test `false_preamble_times_out` (Task 7, sx1262 mock).
2. Foreign LoRa traffic on 918.5 MHz (Meshtastic, RNode, LoRaWAN at a matching SF) must never crash or confuse a modem: wrong air id, wrong length, CRC errors are dropped silently. Tests `foreign_air_id_rejected`, `crc_error_frame_dropped`.
3. Typing into the USB terminal while the radio is mid-transmit must never lose or reorder host bytes: the USB task only queues; the modem task alone calls the core. Test `host_bytes_cross_tasks_in_order` (Task 7, ffi host test with a simulated queue).
4. The chip not answering (BUSY stuck high, or GetStatus wrong after reset) must give a clear `RADIO FAIL` on the OLED and `ATI` instead of a hang. Test `busy_stuck_reports_fail`.
5. A Rust panic on the device must restart the board and leave a note that `ATI` shows after the reboot, never a silent spin. Test: host build of the note format `panic_note_roundtrip`; on device, checked by reading `ATI` after boot.

## Parked for phase 3 (known, not in this plan)

- call_id is 7 bits with no addressing after setup, so two pairs in range can cross-talk. Phase 3 adds addressing with discovery and the directory.
- CAD (listen before talk) is not in the `Radio` trait yet; phase 3 adds it with the rotary and traffic channels.
- SX1262 SNR saturates near +10 dB, so retrain thresholds at the top of the ladder (S20) are re-tuned from the two-radio measurements in Task 7 Step 6.
- The 5 s AXIS BBS boot logo and PRG mode picker are build step 6.
- TinyUSB with two CDC ports and real modem control lines (design 4 and builder check) replaces USB-Serial/JTAG later; phase 2 uses USB-Serial/JTAG because it needs no extra stack and keeps the flash path Rev already uses.

---

### Task 1: Core prep for the air

**Files:**
- Modify: `core/src/link.rs` (split), new `core/src/link/` modules
- Modify: `core/src/frame.rs`, `core/src/tier.rs`, `core/src/at.rs`, `core/src/modem.rs`, `core/src/lib.rs`, `docs/FEATURES.md`
- Modify (expected values only): `sim/tests/*.rs`, `sim/src/lib.rs`

**Interfaces:**
- Produces: `frame::AIR_ID: u8 = 0xD1`, `frame::HEADER_LEN = 5`, `frame::MAX_PAYLOAD = 250`, `FrameError::Foreign`; `lfd3k_core::CHANNEL_HZ: u32 = 918_500_000`; `Tier::preamble_symbols(self) -> u16` (12 for SF5/SF6, else 8); `SRegs` register 30 (`get(30)`, `set(30, v)` with v as two's complement i16 in -9..=22, default 2) and `Modem::tx_power_dbm(&self) -> i8`.

- [ ] **Step 1:** Split `link.rs` (1219 lines) into `link/mod.rs` (state, public API), `link/setup.rs` (dial, ring, answer, training), `link/data.rs` (ARQ glue, queue, turn and jitter), `link/retrain.rs` (EWMA, step up and down, resync). Pure move: no behavior change, no test file touched, 40 passing, demo output byte-identical (`--out` before and after, compare). **Commit this alone:** `refactor(core): split link.rs, no behavior change`.
- [ ] **Step 1b (rule for Steps 2 to 4):** the air id adds 1 byte to every frame and the SF5/SF6 airtime grows, so seeded sim timings, exact simulated end times, retrain counts and trace frame lengths may shift; update those expected values. What must not change: every scenario still connects, data arrives intact both ways, NO CARRIER appears where it did, the fuzzers still pass, and the default demo still connects at SF5 and ends in NO CARRIER on both screens. If any of those breaks, stop and report it as a finding.
- [ ] **Step 2:** Add the air id byte in front of the existing 4 header bytes in `encode`/`decode`; `decode` returns `Foreign` when byte 0 is not `AIR_ID`. Update any test or sim constant that assumes a 4-byte header.
- [ ] **Step 3:** Fix `Tier::airtime_us` for SF5/SF6 to the SX1261/2 datasheet form: payload numerator `8*PL + 16 - 4*SF + 20` (explicit header, CRC on); SF7 to SF12 unchanged (`8*PL + 16 - 4*SF + 28`). SF5, 1-byte payload must come out 2320 us. Update test expectations that move. Add `preamble_symbols()` and use it in `airtime_us`.
- [ ] **Step 4:** Add S30 (TX power) to `SRegs` and `NUMBERS`, the `AT&V` line from Global Constraints, `CHANNEL_HZ`, and FEATURES.md entries (air id, S30, channel).
- [ ] **Step 5:** `cargo test --workspace` (40 pass), `cargo clippy --workspace --all-targets -- -D warnings`, demo `cargo run -p lfd3k-sim` still ends in NO CARRIER on both sides. Commit `feat(core): air id byte, SF5/SF6 airtime, S30 power` (the split is already its own commit from Step 1).

### Task 2: `sx1262` driver crate

**Files:**
- Create: `sx1262/Cargo.toml`, `sx1262/src/lib.rs`, `sx1262/src/cmd.rs` (opcodes, encoders), `sx1262/src/driver.rs`
- Modify: `Cargo.toml` (workspace member)

**Interfaces:**
- Consumes: `lfd3k_core::{Radio, RxInfo, Tier, CHANNEL_HZ}`, `Tier::sf()`, `Tier::preamble_symbols()`, `Tier::airtime_us()`.
- Produces:
```rust
pub trait Bus {
    /// Full-duplex SPI transfer with NSS low for the whole call.
    fn xfer(&mut self, tx: &[u8], rx: &mut [u8]);
    fn busy(&self) -> bool;                 // BUSY pin level
    fn reset(&mut self);                    // NRST low 1 ms, then high
    fn fem(&mut self, tx: bool);            // GPIO46: true while transmitting
    fn now_us(&self) -> u64;
}
pub enum Error { BusyTimeout, NoChip, Config }
pub struct Sx1262<B: Bus> { .. }
impl<B: Bus> Sx1262<B> {
    pub fn new(bus: B) -> Self;
    pub fn init(&mut self, power_dbm: i8) -> Result<(), Error>;
    pub fn set_power(&mut self, power_dbm: i8) -> Result<(), Error>; // clamps to -9..=22
    /// Call on every DIO1 edge and before every core tick. Reads and clears
    /// IRQs, applies the false-preamble and max-packet timeouts, and returns
    /// a received frame when one completed.
    pub fn service(&mut self, buf: &mut [u8; 255]) -> Option<(usize, RxInfo)>;
    pub fn failed(&self) -> bool;
}
impl<B: Bus> lfd3k_core::Radio for Sx1262<B> { .. }
```

- [ ] **Step 1:** `cmd.rs`: opcodes (SetStandby 0x80, SetPacketType 0x8A, SetRfFrequency 0x86, SetPaConfig 0x95, SetTxParams 0x8E, SetModulationParams 0x8B, SetPacketParams 0x8C, SetDioIrqParams 0x08, GetIrqStatus 0x12, ClearIrqStatus 0x02, SetDIO2AsRfSwitchCtrl 0x9D, SetDIO3AsTCXOCtrl 0x97, Calibrate 0x89, CalibrateImage 0x98, SetRegulatorMode 0x96, SetBufferBaseAddress 0x8F, WriteBuffer 0x0E, ReadBuffer 0x1E, WriteRegister 0x0D, ReadRegister 0x1D, SetTx 0x83, SetRx 0x82, GetRxBufferStatus 0x13, GetPacketStatus 0x14, GetStatus 0xC0, ClearDeviceErrors 0x07, SetRxTxFallbackMode 0x93) and IRQ bits (TxDone 0, RxDone 1, PreambleDetected 2, HeaderValid 4, HeaderErr 5, CrcErr 6, Timeout 9). Encoders write into fixed arrays.
- [ ] **Step 2:** `init`: reset; wait BUSY low (timeout 10 ms gives `BusyTimeout`); SetStandby RC; SetDIO3AsTCXOCtrl 1.8 V (0x02) with 5 ms delay; Calibrate 0x7F; SetRegulatorMode DC-DC; CalibrateImage 0xE1 0xE9 (902 to 928); SetDIO2AsRfSwitchCtrl 1; SetPacketType LoRa; SetRfFrequency from `CHANNEL_HZ` (integer `hz * 2^25 / 32_000_000`); SetPaConfig 0x04 0x07 0x00 0x01; OCP register 0x08E7 = 0x38; SetTxParams(power, ramp 40 us); sync word registers; SetBufferBaseAddress 0, 0; SetRxTxFallbackMode STDBY_XOSC (0x30) so the TCXO stays on across every TX to RX turn (STDBY_RC would add the 5 ms TCXO start to each turn); SetDioIrqParams with TxDone, RxDone, PreambleDetected, HeaderValid, HeaderErr, CrcErr, Timeout all on DIO1; errata 15.2 (register 0x08D8 |= 0x1E). GetStatus must report a sane chip mode or `NoChip`; set `failed()`.
- [ ] **Step 3:** Per-tier setup (only when the tier changes): SetModulationParams(SF, BW 0x06, CR 0x01, LDRO 0); errata 15.1 for BW 500 (register 0x0889 bit 2 cleared); SetPacketParams(preamble, explicit, length, CRC on, IQ standard).
- [ ] **Step 4:** `send`: standby, tier setup, WriteBuffer, packet length, `fem(true)`, SetTx with timeout 0; state Tx. `listen`: `fem(false)`, tier setup, SetRx continuous (0xFFFFFF); state Rx. `busy()`: true in Tx, and in Rx after PreambleDetected until RxDone, HeaderErr, CrcErr, or a timeout.
- [ ] **Step 5:** `service`: GetIrqStatus, ClearIrqStatus. TxDone ends Tx (`fem(false)`, state Idle; the core calls `listen`). PreambleDetected starts a guard: no HeaderValid within `(preamble + 12) symbols` means false preamble, re-arm Rx. HeaderValid moves the guard to `airtime_us(255) + 2 ms`. RxDone without CrcErr: GetRxBufferStatus, ReadBuffer, GetPacketStatus (rssi = -RssiPkt/2, snr_x4 = SnrPkt as i8), return the frame. CrcErr or HeaderErr: drop, re-arm. Any SPI wait over 10 ms sets `failed()`.
- [ ] **Step 6:** `cargo build --workspace`, `cargo clippy ...`, and `cargo +esp build -p sx1262 --release --target xtensa-esp32s3-none-elf -Zbuild-std=core` (from PowerShell with `C:\Users\rev\export-esp.ps1` sourced) all clean. Commit `feat(sx1262): interrupt-driven LoRa driver`.

### Task 3: FFI device glue

**Files:**
- Modify: `ffi/src/lib.rs` (split into `ffi/src/hal.rs`, `ffi/src/api.rs`, `ffi/src/panic.rs`), `ffi/Cargo.toml`, `firmware/include/lfd3k.h`

**Interfaces:**
- Consumes: `Sx1262`, `Bus`, `Modem`, `Config::new`, `Modem::lights`, `Modem::tx_power_dbm`.
- Produces (C ABI, mirrored exactly in `lfd3k.h`):
```c
/* Provided by the firmware (C HAL), called by Rust. */
void     lfd3k_hal_spi_xfer(const uint8_t *tx, uint8_t *rx, size_t len);
bool     lfd3k_hal_busy(void);
void     lfd3k_hal_reset(void);
void     lfd3k_hal_fem_tx(bool on);
uint64_t lfd3k_hal_now_us(void);
void     lfd3k_hal_panic(const uint8_t *note, size_t len); /* store note in RTC RAM, esp_restart */
/* Provided by Rust, called only from the modem task. */
int      lfd3k_init(const char *name, const uint8_t *last_panic, size_t len); /* 0 ok, <0 radio fail */
void     lfd3k_poll(uint32_t now_ms);            /* service radio, then tick modem */
size_t   lfd3k_host_in(const uint8_t *buf, size_t len, uint32_t now_ms); /* returns bytes taken */
size_t   lfd3k_host_out(uint8_t *buf, size_t len);
bool     lfd3k_take_fb_request(void);            /* AT+FB seen in command mode */
typedef struct { uint8_t lights; uint8_t state; int8_t tier; int16_t rssi; int16_t snr_x4; uint8_t radio_ok; } lfd3k_status_t;
void     lfd3k_status(uint32_t now_ms, lfd3k_status_t *out);
```
  `lights` bit order: OH 0, CD 1, SD 2, RD 3, HS 4, EC 5, DC 6, AA 7. `state`: 0 idle, 1 dialing, 2 ringing, 3 training, 4 online, 5 command-online. `tier`: -1 when none, else `Tier::index()`.

- [ ] **Step 1:** Add `AtCmd::FrameBuffer` (`AT+FB`) to the core parser; `Modem` sets a flag, prints nothing, and does not print OK (the firmware writes the dump and `OK`). Add the last-panic note to `ATI` (fourth line `LAST CRASH: <note>` only when a note exists). FEATURES.md entries.
- [ ] **Step 2:** One static holder for `Modem` + `Sx1262<HalBus>` in a single `UnsafeCell` wrapper, initialised once by `lfd3k_init`; every other entry point returns early before init. SAFETY comments state the single-task contract.
- [ ] **Step 3:** `lfd3k_poll`: `service` until no frame, feed each frame to `Modem::radio_rx`, then `Modem::tick`. Firmware calls it on every DIO1 edge and every 10 ms.
- [ ] **Step 4:** Device panic handler formats `file:line` into a 64-byte buffer and calls `lfd3k_hal_panic`; never spins. Host build keeps std.
- [ ] **Step 5:** `cargo build --workspace`, clippy, `firmware\build_core.ps1` all clean. Commit `feat(ffi): modem and radio C ABI, resetting panic`.

### Task 4: Firmware platform

**Files:**
- Modify: `firmware/src/main.c`, `firmware/src/CMakeLists.txt`, `firmware/sdkconfig.defaults`, `firmware/platformio.ini`
- Create: `firmware/src/hal_radio.c`, `firmware/src/usb_link.c`, `firmware/src/usb_link.h`, `firmware/src/board.h`

**Interfaces:**
- Consumes: the C ABI from Task 3.
- Produces: `board.h` pin defines from Global Constraints; `usb_link_init()`, `usb_link_read(uint8_t*, size_t, TickType_t)`, `usb_link_write(const uint8_t*, size_t)`; the modem task.

- [ ] **Step 0:** `sdkconfig.defaults`: `CONFIG_FREERTOS_HZ=1000` (the 10 ms poll must be real, not 0 to 10 ms). `platformio.ini`: the esptool 4.9 package and `board_upload.after_reset = watchdog_reset` from Global Constraints; no `upload_port` line (Rev passes it).
- [ ] **Step 1:** `board.h` pins; boot order: Vext GPIO36 LOW, FEM GPIO7 HIGH and GPIO2 HIGH, GPIO46 LOW.
- [ ] **Step 2:** `hal_radio.c`: SPI2 master at 8 MHz mode 0 on the radio pins with NSS by the driver (whole transaction), BUSY input, RST output, DIO1 rising-edge ISR that only does `vTaskNotifyGiveFromISR` to the modem task; `lfd3k_hal_now_us` from `esp_timer_get_time`; `lfd3k_hal_panic` stores the note in `RTC_NOINIT_ATTR` memory with a magic word and calls `esp_restart`.
- [ ] **Step 3:** `usb_link.c`: USB-Serial/JTAG driver for the modem stream. Console and ESP_LOG go to UART0 only (`CONFIG_ESP_CONSOLE_UART_DEFAULT=y`, secondary console none) so nothing but modem bytes reaches USB. A USB reader task (priority 5) fills a 2 KB RX stream buffer; a USB writer task (priority 5) drains a 4 KB TX stream buffer to the driver. Neither calls Rust. The modem task only ever does non-blocking sends into the TX buffer (timeout 0); when it is full the bytes stay in the core's own output ring (call `lfd3k_host_out` only for what fits). With no terminal open or a host that stops reading, nothing blocks.
- [ ] **Step 4:** Modem task on core 1, priority 20, stack 8 KB: `lfd3k_init` with name `AXISMODEM-XXXX` (last two MAC bytes, hex) and the RTC panic note (then clear it); loop: wait for notify or 10 ms, drain the stream buffer into `lfd3k_host_in` (keep what it does not take for next time), `lfd3k_poll(now_ms)`, write `lfd3k_host_out` to USB; when `lfd3k_take_fb_request()` is true, ask the display task for a snapshot and write `FB:` + 2048 hex digits (1024 framebuffer bytes, page order as the SSD1306 stores them) + `\r\nOK\r\n`. The `FB:` dump goes through the same TX buffer in pieces as space frees, never a blocking write. Task watchdog subscribed. Radio fail is not fatal: AT still works and reports it. UART0 has no USB bridge on this board, so AT is the only window: `lfd3k_init`'s negative code names the failure (-1 BusyTimeout, -2 NoChip, -3 Config) and the core shows it plus the chip's GetDeviceErrors word in `AT&V` as `RADIO <OK|FAIL code> ERR 0x<hex>` (add a `lfd3k_set_radio_diag(int code, uint16_t errs)` entry if Task 3 did not).
- [ ] **Step 5:** `pio run -d firmware -e heltec_v4` succeeds; record RAM and flash. Commit `feat(firmware): radio HAL, USB modem stream, modem task`.

### Task 5: OLED front panel

**Files:**
- Create: `firmware/src/ssd1306.c`, `firmware/src/ssd1306.h`, `firmware/src/panel.c`, `firmware/src/panel.h`, `firmware/src/font5x7.h` (our own glyphs)
- Modify: `firmware/src/main.c`, `docs/FEATURES.md`

**Interfaces:**
- Consumes: `lfd3k_status`.
- Produces: `ssd1306_init()`, `ssd1306_flush(const uint8_t fb[1024])`; `panel_task` (priority 2, core 0); `panel_snapshot(uint8_t out[1024])` (mutex-protected copy of the last drawn frame).

- [ ] **Step 1:** SSD1306 driver over the new IDF I2C master API, 400 kHz, RST 21 pulse, landscape 128x64, full-frame flush.
- [ ] **Step 2:** Panel layout: top row the 8 lights as boxes labelled OH CD SD RD HS EC DC AA (filled when lit, SD and RD flicker with traffic); middle the tier name and rate (`SF7 BW500 21875`) or the state word (`IDLE`, `DIALING`, `RING`, `TRAINING`, `ONLINE`); bottom the modem name, RSSI and SNR bars; `RADIO FAIL` replaces the middle when `radio_ok` is 0.
- [ ] **Step 3:** Animations: dialing blink on OH, training sweep across the eight tiers that snaps to the chosen tier, idle carrier pulse. 20 fps cap, never blocks the modem task. LED GPIO35 follows SD or RD.
- [ ] **Step 4:** Boot splash for 1 s: `LFD3K-SM`, `r.420`, `BL4Z3B4ND`. (The 5 s AXIS BBS logo with the PRG mode picker belongs to the picker in build step 6, not here.)
- [ ] **Step 5:** `pio run` succeeds; FEATURES.md updated. Commit `feat(firmware): OLED front panel and snapshots`.

### Task 6: Bench tools and Rev's flashing sheet

**Files:**
- Create: `tools/lfd3k_term.py`, `tools/fb2png.py`, `docs/phase2-bench.md`

**Interfaces:**
- Produces: `python tools/lfd3k_term.py --port COMn --expect-mac XX:XX:XX:XX:XX:XX [--send "ATI"]... [--log file] [--shot file.png] [--wait-ms n]`: first a passive `serial.tools.list_ports` lookup; refuses to open unless the port is `VID 303A PID 1001` and its USB serial number matches `--expect-mac` (the ESP32-S3 USB-Serial/JTAG serial is the MAC); refuses COM15 and COM9 by name always. Then opens with `dtr=False, rts=False` set before `open()` and never changed, sends each line with CR, waits, prints and logs everything; `--shot` sends `AT+FB`, parses the hex and writes a 4x scaled PNG (black and cyan) using only the standard library (`zlib`, `struct`). `fb2png.py` converts a saved `FB:` line.

- [ ] **Step 1:** The two tools as above; no third-party package beyond pyserial. The implementer checks them only offline: `fb2png.py` on a canned `FB:` line (checkerboard plus text) and `lfd3k_term.py --help` plus its port filter against a faked port list. Never open a real port.
- [ ] **Step 2:** `docs/phase2-bench.md` for Rev, plain language, no em dashes:
  0. Antenna on before anything transmits. Confirm the V4 port in Device Manager (`VID_303A PID_1001`), since it may change number.
  1. Back up the V4 (Rev runs): esptool 4.9 from `%USERPROFILE%\.platformio\packages\tool-esptoolpy\esptool.py` (the same one `F:\simian-radar\tools\flash_camdb.ps1` uses), `--chip esp32s3 --port <V4 port> --baud 921600 --no-stub --after no-reset read_flash 0 0x1000000 F:\lfd3k-sm\backups\v4-simian-radar-<date>.bin`. Verify: the file is exactly 16,777,216 bytes, and a second read of the first 64 KB matches (`read_flash 0 0x10000` to a second file, compare with `fc /b`).
  2. Flash (Rev runs): `erase_flash` first (so stale otadata and the Radar NVS cannot pick the wrong slot), then `pio run -d firmware -e heltec_v4 -t upload --upload-port <V4 port>`. Also give the exact esptool `write_flash` line with the bootloader, partition table, otadata and app offsets the build prints, ending `--after watchdog_reset`.
  3. Restore Radar: `write_flash 0 <backup> --after watchdog_reset`.
  4. SDR check: SDRangel with the NESDR SMArt v5, center 918.5 MHz, 2.4 MS/s, ChirpChat demod at BW 500 kHz, SF12, explicit header, CRC on; then `ATDT TEST` on the V4 and look for the CallReq frame starting `D1 1`.
- [ ] **Step 3:** `backups/` added to `.gitignore`. Commit `feat(tools): bench terminal, screenshots, flashing sheet`.

### Task 7: Tests, build, and the bench run

**Files:**
- Create: `sx1262/tests/mock_bus.rs`, `ffi/tests/host_api.rs`
- Modify: `sim/tests/calls.rs`, `core` unit tests, `docs/phase2-bench.md` (results)

- [ ] **Step 1: Driver tests on a mock bus** (records every SPI transaction, scripts BUSY, IRQ status and buffer reads): `init_sequence_matches_datasheet` (exact opcode bytes in order, frequency word `39 68 00 00`, sync word registers, TCXO 1.8 V, PA config `04 07 00 01`); `tier_change_sets_sf_and_preamble` (SF5 preamble 12, SF12 preamble 8, BW byte 0x06, errata 0x0889 write); `send_sets_fem_and_busy` (fem true on send, false on TxDone; busy true until TxDone); `rx_frame_reports_snr_and_rssi` (RssiPkt 0x50 gives -40 dBm, SnrPkt 0x1C gives snr_x4 28); `false_preamble_times_out`; `crc_error_frame_dropped`; `busy_stuck_reports_fail`; `power_clamped` (30 gives 22, -20 gives -9).
- [ ] **Step 2: Core tests:** `foreign_air_id_rejected` (byte 0 = 0x94 or 0x00 gives `Foreign`); `airtime_sf5_one_byte_is_2320us`; `s30_default_and_range`; `at_fb_sets_request_without_ok`; `ati_shows_last_crash`.
- [ ] **Step 3: FFI host tests** (host build with a fake HAL and `#[cfg(test)]` hooks): `host_bytes_cross_tasks_in_order` (interleave `lfd3k_host_in` partial takes with `lfd3k_poll`, all bytes reach the core in order); `calls_before_init_are_safe`; `panic_note_roundtrip`.
- [ ] **Step 4:** Whole suite: `cargo test --workspace` (all pass, none ignored), clippy clean, sim demo unchanged in outcome, `build_core.ps1` and `pio run` succeed. Commit `test: phase 2 driver, core and ffi tests`.
- [ ] **Step 5 (needs Rev): bench run on the V4.** Before it: ask Rev whether the NESDR SMArt v5 is free (the brain puts it in the car and on Airwatch), and get the V4's MAC from the backup log for `--expect-mac`. Rev backs up and flashes per `docs/phase2-bench.md`. Then the controller (never a subagent), terminal only on the matched port: `ATI` (banner, three lines), `AT&V` (S30 line, tier none), `--shot` the idle panel, `ATS30=2`, `ATDT TEST` and expect `NO ANSWER` after S7 (proves TX, TX done, timeouts on hardware), `--shot` during dialing. SDR check with Rev: SDRangel decodes the SF12 CallReq at 918.5 MHz. Save transcript and PNGs in `docs/phase2-bench/`, send each PNG to Rev, record results in the bench doc and the brain.
- [ ] **Step 6 (when the second V4 arrives, Rev flashes it):** two-radio call: `ATDT AXISMODEM-xxxx`, CONNECT, 4 KB each way, `+++`, `ATH`, NO CARRIER; measure turnaround time and real throughput per tier; feed results back to the turn gap and the SF5/SF6 step-down margins.
