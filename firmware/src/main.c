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
#include "usb_link.h"

static const char *TAG = "lfd3k";

#define MODEM_STACK   16384 /* Modem (~5.2 KB) is built on this stack inside lfd3k_init */
#define OUT_CHUNK     128
#define IN_PENDING    256

/* AT+FB dump: "FB:" + 2048 hex digits + CRLF OK CRLF, sent as TX space frees. */
#define FB_BYTES      1024
#define DUMP_HEAD     3
#define DUMP_HEX      (FB_BYTES * 2)
#define DUMP_TAIL     6
#define DUMP_TOTAL    (DUMP_HEAD + DUMP_HEX + DUMP_TAIL)

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

static void modem_task(void *arg)
{
    (void)arg;
    hal_radio_init(xTaskGetCurrentTaskHandle());
    panel_start(); /* after Vext is on */
    usb_link_init();
    usb_link_set_notify(xTaskGetCurrentTaskHandle());

    uint8_t mac[6] = {0};
    esp_efuse_mac_get_default(mac);
    char name[20];
    snprintf(name, sizeof(name), "AXISMODEM-%02X%02X", mac[4], mac[5]);

    uint8_t note[64];
    size_t note_len = hal_radio_take_panic_note(note);
    int rc = lfd3k_init(name, note_len ? note : NULL, note_len);
    ESP_LOGI(TAG, "%s core init rc=%d, last crash note %u bytes", name, rc, (unsigned)note_len);

    esp_task_wdt_add(NULL);

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
        lfd3k_status_t st;
        lfd3k_status(now_ms, &st);
        panel_publish(&st);

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
    xTaskCreatePinnedToCore(modem_task, "modem", MODEM_STACK, NULL, 20, NULL, 1);
}
