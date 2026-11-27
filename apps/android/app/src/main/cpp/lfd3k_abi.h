/* C ABI of lfd3k-voice-android. Field order and sizes match
 * apps/android/engine/src/abi.rs. sizeof(Lfd3kStatus) is 11436. */
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct Lfd3kSession Lfd3kSession;

typedef struct Lfd3kDir {
    char name[24];
    char label[16];
    char station[24];
    char hint[16];
    int32_t sf;
    int32_t pinned;
    int32_t trust;
    int32_t voice;
} Lfd3kDir;

typedef struct Lfd3kContact {
    char name[24];
    char label[4][16];
    uint8_t key[4][32];
    int32_t n_dev;
    int32_t person;
    int32_t removed;
    int32_t _pad;
} Lfd3kContact;

typedef struct Lfd3kScope {
    int8_t dbm;
    uint8_t tx;
    uint8_t _pad[2];
} Lfd3kScope;

typedef struct Lfd3kAir {
    int32_t rx;
    int32_t sf;
    int32_t start_ms;
    int32_t air_ms;
} Lfd3kAir;

enum { LFD3K_DIR_N = 48, LFD3K_CONTACT_N = 16, LFD3K_SCOPE_N = 160, LFD3K_AIR_N = 24, LFD3K_SCAN_N = 8 };

typedef struct Lfd3kStatus {
    char name[24];
    char label[16];
    char station[24];
    char link[96];
    char bind_addr[64];
    char peer_addr[64];
    char banner[160];
    char note[160];
    char words0[24];
    char words1[24];
    char plan[96];
    char who[64];
    char peer_name[24];
    char peer_label[16];
    char peer_station[24];
    char trust_line[64];
    char link_code[8];
    char link_note[160];
    uint8_t device_key[32];
    int32_t banner_warn;
    int32_t has_person;
    int32_t in_call;
    int32_t ringing;
    char ring_name[24];
    char ring_label[16];
    char ring_station[24];
    int32_t audio_ok;
    int32_t talk_anyway;
    int32_t conflict;
    int32_t muted;
    int32_t ptt;
    int32_t floor;
    int32_t call_state;
    int32_t sf;
    int32_t locked;
    int32_t sounds;
    int32_t linking;
    int32_t cap_active;
    int32_t unsaved;
    int32_t pw_dirty;
    int32_t lost;
    int32_t ready;
    int32_t connecting;
    int32_t scanning;
    int32_t rssi;
    int32_t scope_now;
    int32_t dir_count;
    int32_t contact_count;
    int32_t scope_count;
    int32_t air_count;
    int32_t scan_count;
    Lfd3kDir dir[LFD3K_DIR_N];
    Lfd3kContact contacts[LFD3K_CONTACT_N];
    Lfd3kScope scope[LFD3K_SCOPE_N];
    Lfd3kAir air[LFD3K_AIR_N];
    char scan[LFD3K_SCAN_N][96];
} Lfd3kStatus;

size_t lfd3k_status_bytes(void);

/* transport: 0 sim, 1 tcp, 2 find, 3 usb. sounds: -1 prefs, 0 off, 1 on.
 * cap_sf: 0 none, or 5 to 12. */
Lfd3kSession *lfd3k_start(
    int transport,
    const char *name,
    const char *label,
    const char *data_dir,
    const char *bind_addr,
    const char *peer_addr,
    const char *station,
    const char *tcp_host,
    int tcp_port,
    const char *find_station,
    const char *usb_label,
    int snr_db,
    int cap_sf,
    int sounds,
    int auto_answer,
    char *err,
    size_t err_len);

/* End the modem thread and keep the session. The USB pump must still be
 * running so the Leave frame is written. */
void lfd3k_shutdown(Lfd3kSession *s);
/* Shutdown if needed, then free. Stop the USB pump before this. */
void lfd3k_stop(Lfd3kSession *s);

void lfd3k_status(Lfd3kSession *s, Lfd3kStatus *out);
/* which: 0 AT&V, 1 ATI, 2 forwards, 3 hotspot, 4 client, 5 live Wi-Fi. */
int lfd3k_text(Lfd3kSession *s, int which, char *buf, size_t n);

void lfd3k_mic(Lfd3kSession *s, const int16_t *pcm, size_t n);
size_t lfd3k_speaker(Lfd3kSession *s, int16_t *pcm, size_t n);

void lfd3k_usb_rx(Lfd3kSession *s, const uint8_t *data, size_t n);
size_t lfd3k_usb_tx(Lfd3kSession *s, uint8_t *data, size_t n);
void lfd3k_usb_tx_unget(Lfd3kSession *s, const uint8_t *data, size_t n);

void lfd3k_call(Lfd3kSession *s, const char *name, const char *label);
void lfd3k_answer(Lfd3kSession *s);
void lfd3k_decline(Lfd3kSession *s);
void lfd3k_hangup(Lfd3kSession *s);
void lfd3k_talk(Lfd3kSession *s, int down);
void lfd3k_words(Lfd3kSession *s);
void lfd3k_anyway(Lfd3kSession *s);
void lfd3k_mute(Lfd3kSession *s, int on);
void lfd3k_link_offer(Lfd3kSession *s);
void lfd3k_link_join(Lfd3kSession *s, const char *code);
void lfd3k_cancel_link(Lfd3kSession *s);
void lfd3k_set(Lfd3kSession *s, const char *line);
void lfd3k_save(Lfd3kSession *s);
void lfd3k_discard(Lfd3kSession *s);
void lfd3k_scan(Lfd3kSession *s);
void lfd3k_rename(Lfd3kSession *s, const char *name, const char *label);
void lfd3k_sounds(Lfd3kSession *s, int on);
void lfd3k_remove_mine(Lfd3kSession *s, const uint8_t *key);
void lfd3k_remove_contact(Lfd3kSession *s, const char *name, const uint8_t *key);

#ifdef __cplusplus
}
#endif
