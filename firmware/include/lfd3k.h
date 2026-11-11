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
/* AT+SIMNET: queue one datagram (8 byte header plus frame, at most
 * LFD3K_SIMNET_DGRAM_MAX bytes) for the UDP task to send to peer:4210.
 * Never blocks; a full queue drops it. Called only while SimNet is on. */
void     lfd3k_hal_udp_send(const uint8_t *d, size_t len);

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

/* lights bits: OH 0, CD 1, SD 2, RD 3, HS 4, EC 5, DC 6 (radio call compressing, /ARQ/LZ), AA 7.
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
    uint8_t wire;      /* 0 none, 1 dialing, 2 online, 3 bridging a radio call */
    uint8_t simnet;    /* 1 while AT+SIMNET has the simulated radio on (OLED: SI for HS) */
    uint8_t scanning;  /* 1 while an AT+SCAN (or the menu's scan) runs (OLED: radar sweep) */
    uint8_t scan_hits; /* answers the running scan has counted; 0 when none */
    uint16_t heard;    /* directory entries heard in the last 15 minutes (the beacon's count) */
} lfd3k_status_t;
_Static_assert(sizeof(lfd3k_status_t) == 16, "lfd3k_status_t must match ffi/src/api.rs");
void     lfd3k_status(uint32_t now_ms, lfd3k_status_t *out);

/* Channel trace for the idle scope: up to 64 samples taken every 50 ms in
 * lfd3k_poll (dbm: -128..0, oldest first; tx: 1 for samples taken while
 * transmitting). Copies at most cap samples (the newest), returns the count.
 * Either pointer may be NULL. Returns 0 before init. */
size_t   lfd3k_rf_trace(int8_t *dbm, uint8_t *tx, size_t cap);

/* ---- AT+MODE: firmware modes on the multimode board. ---- */

/* Multimode board only, once at boot after lfd3k_init: bit i set when mode i's
 * slot holds an app image (0 LFD3K-SM, 1 SIMIAN RADAR, 2 RNODE, 3 MESH). Without
 * this call (the single app layout) AT+MODE answers ERROR. */
void     lfd3k_set_mode_slots(uint8_t mask);
/* Taken once after an accepted AT+MODE= (no call, slot has an image): -1 none,
 * else 0 LFD3K-SM, 1 SIMIAN RADAR, 2 RNODE, 3 MESH, 4 the picker. The modem
 * prints nothing for it until lfd3k_mode_result. */
int      lfd3k_take_mode_request(void);
/* Answer to a taken request: true prints OK (then send it out and restart),
 * false prints ERROR. */
void     lfd3k_mode_result(bool ok);

/* ---- SimNet: the simulated radio over UDP (AT+SIMNET, test mode). ---- */

#define LFD3K_SIMNET_PORT      4210
#define LFD3K_SIMNET_HDR_LEN   8
#define LFD3K_SIMNET_DGRAM_MAX (LFD3K_SIMNET_HDR_LEN + 255)

/* Taken once after AT+SIMNET switched radios: -1 no change, 0 back on the
 * SX1262 (close the UDP socket), 1 simulated radio on (ip gets the peer;
 * open the socket). Every boot starts on the SX1262. */
int      lfd3k_take_simnet(uint8_t ip[4]);
/* A datagram from the peer (header and frame). Returns len (always all of
 * it), 0 before init. Call lfd3k_poll after a batch. now_ms is unused. */
size_t   lfd3k_udp_in(const uint8_t *d, size_t len, uint32_t now_ms);
/* The UDP socket could not be opened (socket or bind failed): AT&V shows
 * RADIO SIMNET <ip> NET FAIL. Call each loop with the UDP task's state. */
void     lfd3k_set_simnet_net_fail(bool fail);
/* Milliseconds until the simulated radio needs lfd3k_poll again (a frame's
 * airtime ends), UINT32_MAX when nothing is pending or SimNet is off. */
uint32_t lfd3k_simnet_due_ms(void);

/* ---- Settings (NVS blob) and Wi-Fi. ---- */

/* Hotspot and client network. All strings NUL terminated. */
typedef struct {
    uint8_t ap_on;
    uint8_t sta_set;          /* 1 when a client network is set */
    char    ap_ssid[33];
    char    ap_pass[64];
    char    sta_ssid[33];
    char    sta_pass[64];
} lfd3k_wifi_cfg_t;

/* Load the saved blob. 0 accepted, -1 rejected or not initialised (defaults
 * remain; call lfd3k_default_settings and save). */
int      lfd3k_load_settings(const uint8_t *blob, size_t len);
/* True once when AT&W (or a hotspot join) asked for a save: fills blob and
 * *len. cap must be at least 1024 (LFD3K_BLOB_LEN), else false and the
 * request is kept. */
#define LFD3K_BLOB_LEN 1024
bool     lfd3k_take_save(uint8_t *blob, size_t cap, size_t *len);
/* First boot only: call it just after lfd3k_load_settings returned -1. It
 * replaces ALL settings with factory ones: S registers, E and V go back to
 * their power-on values and the hotspot reuses the current AP ssid, with
 * password ap_pass (8 to 63 printable characters, no comma); asks for a
 * save. A bad password changes nothing. */
void     lfd3k_default_settings(const char *ap_pass);
void     lfd3k_get_wifi_config(lfd3k_wifi_cfg_t *out);

/* ---- Directory (NVS key "dir") and names. ---- */

/* Load the saved directory blob, parsed in place in the core. Call after
 * lfd3k_load_settings. 0 accepted, -1 rejected or not initialised (the
 * directory stays empty; nothing else to do). */
int      lfd3k_load_directory(const uint8_t *blob, size_t len);
/* True once when the directory should be saved (AT&W, or a change once 60 s
 * passed since the last save): fills blob and *len. cap must be at least
 * 1024 (LFD3K_DIR_BLOB_LEN), else false and the request is kept. */
#define LFD3K_DIR_BLOB_LEN 1024
bool     lfd3k_take_dir_save(uint8_t *blob, size_t cap, size_t *len);
/* The live station name (AT+NAME can change it), NUL terminated, cut to cap;
 * 17 holds any name. Returns the length; "" before init. */
size_t   lfd3k_station_name(char *out, size_t cap);
/* The station that last called us over the radio, NUL terminated ("" until
 * one has since boot); 17 holds any name. Returns the length. */
size_t   lfd3k_last_caller(char *out, size_t cap);
/* True once after AT+WAP, AT+WSTA, ATZ or a load changed Wi-Fi settings. */
bool     lfd3k_take_wifi_changed(void);
/* Live status line for AT+WIFI? (up to 96 bytes kept). */
void     lfd3k_set_wifi_status(const char *line);
/* True while the generated hotspot password may be shown on the OLED. */
bool     lfd3k_show_ap_pass(void);
/* A device joined the hotspot: hide the password (asks for a save). */
void     lfd3k_note_ap_client_joined(void);
/* Modem task stack high-water mark in bytes free; AT&V prints STACK FREE <n>. */
void     lfd3k_set_stack_free(uint32_t bytes);
/* Lowest free heap since boot in bytes; AT&V prints HEAP MIN <n>. */
void     lfd3k_set_heap_min(uint32_t bytes);

/* ---- The wire (a TCP connection for Wi-Fi calls and bridges). ---- */

/* True once when a call wants a connection: host (NUL terminated) and port.
 * cap must be at least 64, else false and the request is kept. Poll
 * lfd3k_take_wire_close first. */
bool     lfd3k_take_wire_request(char *host, size_t cap, uint16_t *port);
/* ev: 1 connected, 2 failed, 3 closed by the far end; others ignored. */
void     lfd3k_wire_event(int ev, uint32_t now_ms);
/* Bytes received from the socket; returns the bytes taken (offer the rest later). */
size_t   lfd3k_wire_in(const uint8_t *buf, size_t len, uint32_t now_ms);
/* Bytes to send on the socket (telnet encoded); returns the bytes written. */
size_t   lfd3k_wire_out(uint8_t *buf, size_t len);
/* True once when the socket should be closed (drain lfd3k_wire_out first). */
bool     lfd3k_take_wire_close(void);

#ifdef __cplusplus
}
#endif

#endif
