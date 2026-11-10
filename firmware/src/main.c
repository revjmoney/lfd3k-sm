/* LFD3K-SM firmware: the modem task glues the USB stream, the Rust core and
 * the radio together. Only this task ever calls lfd3k_*. */
#include <stdio.h>
#include <string.h>
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_task_wdt.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "hal_radio.h"
#include "lfd3k.h"
#include "panel.h"
#include "settings_store.h"
#include "tcp_link.h"
#include "usb_link.h"
#include "wifi_link.h"
#include "wire_pump.h"

static const char *TAG = "lfd3k";

#define MODEM_STACK   16384 /* the Modem is built in place in the core's static slot; STACK FREE in AT&V shows the headroom */
#define OUT_CHUNK     128
#define IN_PENDING    256

/* AT+FB dump: "FB:" + 2048 hex digits + CRLF OK CRLF, sent as TX space frees. */
#define FB_BYTES      1024
#define DUMP_HEAD     3
#define DUMP_HEX      (FB_BYTES * 2)
#define DUMP_TAIL     6
#define DUMP_TOTAL    (DUMP_HEAD + DUMP_HEX + DUMP_TAIL)

/* Settings save: held until no call is active, then written to NVS. */
static uint8_t s_blob[LFD3K_BLOB_LEN];
static size_t s_blob_len;
static bool s_save_held;
static bool s_save_failed;       /* last write failed; retry 5 s after s_save_failed_ms */
static uint32_t s_save_failed_ms;
static bool s_nvs_ok;
static char s_boot_pass[13];     /* drawn in app_main before the Wi-Fi driver exists */

static uint8_t s_fb[FB_BYTES];
static bool s_fb_wanted;   /* AT+FB seen, waiting for earlier output to drain */
static bool s_dump_active;
static size_t s_dump_pos;

static uint8_t dump_byte(size_t pos)
{
    static const char hex[] = "0123456789ABCDEF";
    static const char head[] = "FB:";
    static const char tail[] = "\r\nOK\r\n";
    if (pos < DUMP_HEAD) {
        return (uint8_t)head[pos];
    }
    pos -= DUMP_HEAD;
    if (pos < DUMP_HEX) {
        uint8_t b = s_fb[pos / 2];
        return (uint8_t)hex[(pos & 1) ? (b & 0x0F) : (b >> 4)];
    }
    return (uint8_t)tail[pos - DUMP_HEX];
}

/* Pushes as much of the dump as the TX buffer takes. Never blocks. */
static void dump_step(void)
{
    uint8_t chunk[OUT_CHUNK];
    while (s_dump_pos < DUMP_TOTAL) {
        size_t space = usb_link_tx_space();
        size_t n = DUMP_TOTAL - s_dump_pos;
        if (n > sizeof(chunk)) n = sizeof(chunk);
        if (n > space) n = space;
        if (n == 0) {
            return;
        }
        for (size_t i = 0; i < n; i++) {
            chunk[i] = dump_byte(s_dump_pos + i);
        }
        s_dump_pos += usb_link_write(chunk, n);
    }
    s_dump_active = false;
}

/* Moves core output into the TX buffer. Returns true when the core has no more. */
static bool drain_host_out(void)
{
    uint8_t chunk[OUT_CHUNK];
    for (;;) {
        size_t space = usb_link_tx_space();
        if (space == 0) {
            return false;
        }
        size_t want = space < sizeof(chunk) ? space : sizeof(chunk);
        size_t n = lfd3k_host_out(chunk, want);
        if (n == 0) {
            return true;
        }
        /* Only this task fills the buffer, so n <= space always fits. */
        usb_link_write(chunk, n);
    }
}

/* Boot: load the saved settings, or make factory ones with a fresh hotspot password. */
static void load_or_default_settings(void)
{
    size_t len = 0;
    if (s_nvs_ok && settings_store_load(s_blob, sizeof(s_blob), &len) &&
        lfd3k_load_settings(s_blob, len) == 0) {
        ESP_LOGI(TAG, "settings loaded (%u bytes)", (unsigned)len);
        return;
    }
    lfd3k_default_settings(s_boot_pass); /* only valid after a rejected load; asks for a save */
    ESP_LOGI(TAG, "first boot: factory settings");
}

/* Saves wait for the call to end (a flash write must not stall a live call). */
static void save_step(uint32_t now_ms, const lfd3k_status_t *st)
{
    if (lfd3k_take_save(s_blob, sizeof(s_blob), &s_blob_len)) {
        s_save_held = s_nvs_ok; /* a newer blob replaces a held one; no NVS, no saves */
        s_save_failed = false;
    }
    bool wait = s_save_failed && (uint32_t)(now_ms - s_save_failed_ms) < 5000;
    if (s_save_held && st->state == 0 && st->wire == 0 && !wait) {
        if (settings_store_save(s_blob, s_blob_len)) {
            s_save_held = false;
            s_save_failed = false;
        } else {
            s_save_failed = true;
            s_save_failed_ms = now_ms;
        }
    }
}

static void modem_task(void *arg)
{
    (void)arg;
    hal_radio_init(xTaskGetCurrentTaskHandle());
    usb_link_init();
    usb_link_set_notify(xTaskGetCurrentTaskHandle());

    uint8_t mac[6] = {0};
    esp_efuse_mac_get_default(mac);
    char name[20];
    snprintf(name, sizeof(name), "AXISMODEM-%02X%02X", mac[4], mac[5]);

    panel_set_name(name);
    panel_start(); /* after Vext is on (hal_radio_init) */

    uint8_t note[64];
    size_t note_len = hal_radio_take_panic_note(note);
    int rc = lfd3k_init(name, note_len ? note : NULL, note_len);
    ESP_LOGI(TAG, "%s core init rc=%d, last crash note %u bytes", name, rc, (unsigned)note_len);

    load_or_default_settings();
    memset(s_boot_pass, 0, sizeof(s_boot_pass));
    tcp_link_init(xTaskGetCurrentTaskHandle());
    {
        lfd3k_wifi_cfg_t wc;
        (void)lfd3k_take_wifi_changed(); /* the load just reported; the config below is current */
        lfd3k_get_wifi_config(&wc);
        wifi_link_apply(&wc);
        memset(&wc, 0, sizeof(wc));
    }

    esp_task_wdt_add(NULL);
    uint32_t last_second = (uint32_t)(esp_timer_get_time() / 1000);

    uint8_t pend[IN_PENDING];
    size_t pend_len = 0;

    for (;;) {
        esp_task_wdt_reset();
        ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(10));
        uint32_t now_ms = (uint32_t)(esp_timer_get_time() / 1000);

        /* Host bytes in; whatever the core does not take stays for next time. */
        if (pend_len < sizeof(pend)) {
            pend_len += usb_link_read(pend + pend_len, sizeof(pend) - pend_len, 0);
        }
        if (pend_len > 0) {
            size_t taken = lfd3k_host_in(pend, pend_len, now_ms);
            if (taken > pend_len) taken = pend_len;
            memmove(pend, pend + taken, pend_len - taken);
            pend_len -= taken;
        }

        lfd3k_poll(now_ms);
        wire_pump(now_ms);

        if (lfd3k_take_wifi_changed()) {
            lfd3k_wifi_cfg_t wc;
            lfd3k_get_wifi_config(&wc);
            wifi_link_apply(&wc);
            memset(&wc, 0, sizeof(wc));
        }
        if (wifi_link_take_ap_joined()) {
            lfd3k_note_ap_client_joined();
        }

        lfd3k_status_t st;
        lfd3k_status(now_ms, &st);
        panel_publish(&st);
        {
            rf_trace_t tr;
            size_t n = lfd3k_rf_trace(tr.dbm, tr.tx, RF_TRACE_N);
            tr.count = (uint8_t)n;
            panel_publish_trace(&tr);
        }
        save_step(now_ms, &st);

        if ((uint32_t)(now_ms - last_second) >= 1000) {
            last_second = now_ms;
            char line[128]; /* the core keeps 96 */
            if (s_nvs_ok) {
                wifi_link_get_status(line, sizeof(line));
            } else {
                char ws[96];
                wifi_link_get_status(ws, sizeof(ws));
                snprintf(line, sizeof(line), "NVS FAIL %s", ws);
            }
            lfd3k_set_wifi_status(line);
            lfd3k_set_stack_free((uint32_t)uxTaskGetStackHighWaterMark(NULL) * sizeof(StackType_t));
        }

        /* Out to the host, never waiting. During a dump the core's own ring holds output. */
        if (s_dump_active) {
            dump_step();
        } else {
            if (!s_fb_wanted && lfd3k_take_fb_request()) {
                s_fb_wanted = true;
            }
            bool empty = drain_host_out();
            if (s_fb_wanted && empty && usb_link_tx_space() > 0) {
                panel_snapshot(s_fb);
                s_fb_wanted = false;
                s_dump_active = true;
                s_dump_pos = 0;
                dump_step();
            }
        }
    }
}

void app_main(void)
{
    s_nvs_ok = settings_store_init();
    settings_store_gen_ap_pass(s_boot_pass); /* before the Wi-Fi driver exists */
    wifi_link_init(); /* driver only; the radio starts when the modem task applies the config */
    xTaskCreatePinnedToCore(modem_task, "modem", MODEM_STACK, NULL, 20, NULL, 1);
}
