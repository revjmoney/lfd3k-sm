/* C interface to the LFD3K-SM Rust core (ffi crate). Mirrors ffi/src/api.rs
 * and ffi/src/hal.rs exactly. Everything marked "Rust" is called only from
 * the one modem task, never from an ISR or another task. */
#ifndef LFD3K_H
#define LFD3K_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- Provided by the firmware (C HAL), called by Rust. ---- */

/* Full duplex SPI transfer, NSS low for the whole call; tx and rx are len bytes. */
void     lfd3k_hal_spi_xfer(const uint8_t *tx, uint8_t *rx, size_t len);
/* SX1262 BUSY pin level. */
bool     lfd3k_hal_busy(void);
/* NRST low 1 ms, then high. */
void     lfd3k_hal_reset(void);
/* GC1109 PA mode (GPIO46): true while transmitting. */
void     lfd3k_hal_fem_tx(bool on);
/* Monotonic microseconds. */
uint64_t lfd3k_hal_now_us(void);
/* Store note (up to 64 bytes, "file:line") in RTC RAM, then esp_restart. */
void     lfd3k_hal_panic(const uint8_t *note, size_t len);

/* ---- Provided by Rust. ---- */

/* Copies the core version (no NUL) into out, at most len bytes.
 * Returns the bytes written; 0 when out is NULL. */
size_t lfd3k_core_version(uint8_t *out, size_t len);

/* Create the modem (station name, NUL terminated, 1 to 16 printable ASCII),
 * show last_panic (the previous boot's note, may be NULL/0) on ATI, and bring
 * up the radio at the S30 power.
 * Returns 0, or <0: -1 radio BUSY timeout, -2 no chip, -3 radio config
 * (the modem still runs and AT&V shows RADIO FAIL <code> ERR 0x<errors>),
 * -4 bad name (nothing created, every other call is then a no-op). */
int      lfd3k_init(const char *name, const uint8_t *last_panic, size_t len);

/* Service the radio, then tick the modem. Call on every DIO1 edge and every 10 ms. */
void     lfd3k_poll(uint32_t now_ms);

/* Host bytes into the modem; returns the bytes taken. */
size_t   lfd3k_host_in(const uint8_t *buf, size_t len, uint32_t now_ms);
/* Bytes for the host (echo, result codes, call data); returns the bytes written. */
size_t   lfd3k_host_out(uint8_t *buf, size_t len);

/* True once after AT+FB was entered in command mode (the modem prints
 * nothing for it; the firmware writes the dump and OK). */
bool     lfd3k_take_fb_request(void);

/* lights bits: OH 0, CD 1, SD 2, RD 3, HS 4, EC 5, DC 6 (never set), AA 7.
 * state: 0 idle, 1 dialing, 2 ringing, 3 training, 4 online, 5 command-online.
 * tier: -1 when none, else 0 (SF12) to 7 (SF5).
 * rssi dBm and snr_x4 (quarter dB) are from the last frame heard, including
 * frames the link then ignored (wrong call, foreign); 0 when none.
 * radio_ok: 1 usable, 0 failed (or not initialised). */
typedef struct {
    uint8_t lights;
    uint8_t state;
    int8_t  tier;
    int16_t rssi;
    int16_t snr_x4;
    uint8_t radio_ok;
} lfd3k_status_t;
void     lfd3k_status(uint32_t now_ms, lfd3k_status_t *out);

#ifdef __cplusplus
}
#endif

#endif
