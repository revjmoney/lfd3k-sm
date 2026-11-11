/* LFD3K-SM firmware: the modem task glues the USB stream, the Rust core and
 * the radio together. Only this task ever calls lfd3k_*. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "driver/temperature_sensor.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_netif_sntp.h"
#include "esp_system.h"
#include "esp_task_wdt.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "hal_radio.h"
#include "lfd3k.h"
#include "mode_switch.h"
#include "panel.h"
#include "settings_store.h"
#include "tcp_link.h"
#include "udp_link.h"
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

/* A blob waiting for NVS: held until no call is active, then written. The settings and
 * the directory (key "dir") each have their own, so one never overwrites the other. */
typedef struct {
    uint8_t blob[LFD3K_BLOB_LEN]; /* LFD3K_DIR_BLOB_LEN is the same 1024 */
    size_t len;
    bool held;
    bool failed;                  /* last write failed; retry 5 s after failed_ms */
    uint32_t failed_ms;
} pending_save_t;

static pending_save_t s_set_save; /* settings; its blob is also the boot load buffer */
static pending_save_t s_dir_save; /* directory; its blob is also the boot load buffer */
static bool s_nvs_ok;
static char s_boot_pass[13];     /* drawn in app_main before the Wi-Fi driver exists */

/* SimNet socket switch the UDP task has not accepted yet (queue full). */
static int s_udp_want = -1; /* -1 none, 0 close, 1 open */
static uint8_t s_udp_ip[4];

/* AT+MODE: the mode to restart into once the OK has left (-1 none), and when
 * it was accepted (the restart goes ahead after 2 s even if the OK cannot
 * leave, so a host that stopped reading never parks the modem). */
static int s_mode_go = -1;
static uint32_t s_mode_ms;
#define MODE_OUT_WAIT_MS 2000

/* AT+SIMNET: open or close the UDP socket as the core switched radios. */
static void simnet_step(void)
{
    uint8_t ip[4] = {0};
    int sw = lfd3k_take_simnet(ip);
    if (sw >= 0) {
        s_udp_want = sw;
        if (sw == 1) {
            memcpy(s_udp_ip, ip, sizeof(ip));
        }
    }
    lfd3k_set_simnet_net_fail(udp_link_net_failed());
    if (s_udp_want == 1 && udp_link_open(s_udp_ip)) {
        s_udp_want = -1;
    } else if (s_udp_want == 0 && udp_link_close()) {
        s_udp_want = -1;
    }
}

/* Datagrams from the SimNet peer into the core (a bounded batch per loop). */
static void simnet_in(uint32_t now_ms)
{
    static udp_dgram_t d; /* modem task only */
    for (int i = 0; i < 8 && udp_link_recv(&d); i++) {
        lfd3k_udp_in(d.data, d.len, now_ms);
    }
}

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
    if (s_nvs_ok && settings_store_load(s_set_save.blob, sizeof(s_set_save.blob), &len) &&
        lfd3k_load_settings(s_set_save.blob, len) == 0) {
        ESP_LOGI(TAG, "settings loaded (%u bytes)", (unsigned)len);
        return;
    }
    lfd3k_default_settings(s_boot_pass); /* only valid after a rejected load; asks for a save */
    ESP_LOGI(TAG, "first boot: factory settings");
}

/* Boot, after the settings (so an AT+NAME name is in force): the saved directory, parsed
 * in place by the core. None yet (first boot) or a refused blob leaves it empty. */
static void load_directory(void)
{
    size_t len = 0;
    if (!s_nvs_ok || !settings_store_load_dir(s_dir_save.blob, sizeof(s_dir_save.blob), &len)) {
        ESP_LOGI(TAG, "no saved directory");
        return;
    }
    int rc = lfd3k_load_directory(s_dir_save.blob, len);
    ESP_LOGI(TAG, "directory %s (%u bytes)", rc == 0 ? "loaded" : "refused", (unsigned)len);
}

/* Writes a held blob once no call is up (a flash write must not stall a live call). */
static void save_pending(pending_save_t *p, bool (*store)(const uint8_t *, size_t), uint32_t now_ms,
                         const lfd3k_status_t *st)
{
    bool wait = p->failed && (uint32_t)(now_ms - p->failed_ms) < 5000;
    if (p->held && st->state == 0 && st->wire == 0 && st->scanning == 0 && !wait) {
        if (store(p->blob, p->len)) {
            p->held = false;
            p->failed = false;
        } else {
            p->failed = true;
            p->failed_ms = now_ms;
        }
    }
}

/* Settings and directory saves; a newer blob replaces a held one; no NVS, no saves. */
static void save_step(uint32_t now_ms, const lfd3k_status_t *st)
{
    if (lfd3k_take_save(s_set_save.blob, sizeof(s_set_save.blob), &s_set_save.len)) {
        s_set_save.held = s_nvs_ok;
        s_set_save.failed = false;
    }
    if (lfd3k_take_dir_save(s_dir_save.blob, sizeof(s_dir_save.blob), &s_dir_save.len)) {
        s_dir_save.held = s_nvs_ok;
        s_dir_save.failed = false;
    }
    save_pending(&s_set_save, settings_store_save, now_ms, st);
    save_pending(&s_dir_save, settings_store_save_dir, now_ms, st);
}

/* Writes a held blob now (AT+MODE). True when nothing was held or it was written. */
static bool save_now(pending_save_t *p, bool (*store)(const uint8_t *, size_t), uint32_t now_ms)
{
    if (!p->held) {
        return true;
    }
    if (store(p->blob, p->len)) {
        p->held = false;
        p->failed = false;
        return true;
    }
    p->failed = true;
    p->failed_ms = now_ms;
    return false;
}

/* AT+MODE: still no call (the status is checked again here), any held save
 * written (ruling P5-3: a save that fails answers ERROR, nothing switches),
 * then otadata points at the slot (the hash of the image runs under the task
 * watchdog, so it is fed around it), then the core answers. */
static void mode_step(uint32_t now_ms, const lfd3k_status_t *st)
{
    if (s_mode_go >= 0) {
        return;
    }
    int m = lfd3k_take_mode_request();
    if (m < 0) {
        return;
    }
    bool ok = st->state == 0 && st->wire == 0 && st->scanning == 0;
    if (ok) {
        bool a = save_now(&s_set_save, settings_store_save, now_ms);
        bool b = save_now(&s_dir_save, settings_store_save_dir, now_ms);
        ok = a && b;
        if (!ok) {
            ESP_LOGW(TAG, "AT+MODE refused: a pending save failed");
        }
    }
    if (ok) {
        esp_task_wdt_reset();
        ok = mode_switch_prepare(m);
        esp_task_wdt_reset();
    }
    lfd3k_mode_result(ok);
    if (ok) {
        s_mode_go = m;
        s_mode_ms = now_ms;
    }
}

/* Last step of AT+MODE, once the core's output (the OK) is in the TX buffer
 * or 2 s have passed: let the OK leave the USB port, restart. */
static void mode_restart(void)
{
    esp_task_wdt_reset();
    usb_link_flush(500);
    mode_switch_restart(s_mode_go);
}

/* Wi-Fi strings for the OLED rows, once a second. The hotspot password goes along only
 * while the core allows it on screen; every copy is wiped after use. */
static void publish_net(void)
{
    lfd3k_wifi_cfg_t wc;
    panel_net_t n;
    memset(&n, 0, sizeof(n));
    lfd3k_get_wifi_config(&wc);
    n.show_pass = lfd3k_show_ap_pass() ? 1 : 0;
    n.ap_on = (wc.ap_on && wc.ap_ssid[0]) ? 1 : 0;
    memcpy(n.ap_ssid, wc.ap_ssid, sizeof(n.ap_ssid) - 1);
    if (n.show_pass) {
        memcpy(n.ap_pass, wc.ap_pass, sizeof(n.ap_pass) - 1);
    }
    memset(&wc, 0, sizeof(wc));
    bool up = false;
    wifi_link_get_ip(n.ip, &up);
    n.sta_up = up ? 1 : 0;
    panel_publish_net(&n);
    memset(&n, 0, sizeof(n));
}

/* Idle show facts. The clock: SNTP from pool.ntp.org, started once the client side has
 * an address, shown in Rev's local time (South Carolina, US Eastern: EST, daylight time
 * from the second Sunday of March to the first Sunday of November). */
#define LOCAL_TZ   "EST5EDT,M3.2.0,M11.1.0"
#define NTP_SERVER "pool.ntp.org"

static bool s_sntp_started;
static temperature_sensor_handle_t s_tsens; /* NULL when the sensor did not start */

/* SNTP's callback (lwIP task): only sets a flag under the panel lock. */
static void sntp_synced(struct timeval *tv)
{
    (void)tv;
    panel_note_time_synced();
}

static void sntp_start_once(void)
{
    if (s_sntp_started) {
        return;
    }
    char ip[16];
    bool up = false;
    wifi_link_get_ip(ip, &up);
    if (!up) {
        return;
    }
    s_sntp_started = true; /* one try per boot; SNTP keeps polling by itself */
    esp_sntp_config_t cfg = ESP_NETIF_SNTP_DEFAULT_CONFIG(NTP_SERVER);
    cfg.sync_cb = sntp_synced;
    esp_err_t err = esp_netif_sntp_init(&cfg);
    ESP_LOGI(TAG, "SNTP %s (%s)", NTP_SERVER, esp_err_to_name(err));
}

static void tsens_start(void)
{
    temperature_sensor_config_t tc = TEMPERATURE_SENSOR_CONFIG_DEFAULT(-10, 80);
    temperature_sensor_handle_t h = NULL;
    if (temperature_sensor_install(&tc, &h) != ESP_OK) {
        ESP_LOGW(TAG, "temperature sensor did not start");
        return;
    }
    if (temperature_sensor_enable(h) != ESP_OK) {
        temperature_sensor_uninstall(h);
        ESP_LOGW(TAG, "temperature sensor did not start");
        return;
    }
    s_tsens = h;
}

/* Clock, uptime, chip temperature, the heard count (directory entries heard in the last
 * 15 minutes, from the status) and the last caller for the idle show, once a second.
 * GPS stays empty for now (later tasks); the location stays hidden. */
static void publish_info(const lfd3k_status_t *st)
{
    panel_info_t in = PANEL_INFO_INIT;
    in.uptime_s = (uint32_t)(esp_timer_get_time() / 1000000);
    in.heard = st->heard;
    lfd3k_last_caller(in.last_caller, sizeof(in.last_caller));
    if (panel_time_synced()) {
        time_t now = time(NULL);
        struct tm lt;
        localtime_r(&now, &lt);
        in.time_valid = 1;
        in.hour = (uint8_t)lt.tm_hour;
        in.minute = (uint8_t)lt.tm_min;
    }
    float c;
    if (s_tsens && temperature_sensor_get_celsius(s_tsens, &c) == ESP_OK) {
        int v = (int)(c < 0 ? c - 0.5f : c + 0.5f);
        in.temp_valid = 1;
        in.temp_c = (int8_t)(v < -99 ? -99 : (v > 127 ? 127 : v));
    }
    panel_publish_info(&in);
}

/* The live station name for the panel's name row (the boot name until settings load). */
static void publish_name(void)
{
    char nm[17];
    if (lfd3k_station_name(nm, sizeof(nm)) > 0) {
        panel_set_name(nm);
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
    if (mode_switch_available()) {
        lfd3k_set_mode_slots(mode_switch_slots()); /* AT+MODE; the single app layout leaves it ERROR */
    }

    load_or_default_settings();
    memset(s_boot_pass, 0, sizeof(s_boot_pass));
    load_directory();
    publish_name(); /* a saved AT+NAME name shows from the first idle frame */
    tcp_link_init(xTaskGetCurrentTaskHandle());
    udp_link_init(xTaskGetCurrentTaskHandle());
    {
        lfd3k_wifi_cfg_t wc;
        (void)lfd3k_take_wifi_changed(); /* the load just reported; the config below is current */
        lfd3k_get_wifi_config(&wc);
        wifi_link_apply(&wc);
        memset(&wc, 0, sizeof(wc));
    }

    setenv("TZ", LOCAL_TZ, 1);
    tzset();
    tsens_start();

    esp_task_wdt_add(NULL);
    uint32_t last_second = (uint32_t)(esp_timer_get_time() / 1000);

    uint8_t pend[IN_PENDING];
    size_t pend_len = 0;
    bool wifi_apply_pending = false;

    for (;;) {
        esp_task_wdt_reset();
        /* 10 ms tick, or sooner when a SimNet frame's airtime ends then. */
        uint32_t wait_ms = lfd3k_simnet_due_ms();
        if (wait_ms > 10) wait_ms = 10;
        if (wait_ms < 1) wait_ms = 1;
        ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(wait_ms));
        uint32_t now_ms = (uint32_t)(esp_timer_get_time() / 1000);

        /* Host bytes in; whatever the core does not take stays for next time.
         * Nothing more once an AT+MODE restart is on its way. */
        if (s_mode_go < 0 && pend_len < sizeof(pend)) {
            pend_len += usb_link_read(pend + pend_len, sizeof(pend) - pend_len, 0);
        }
        if (s_mode_go < 0 && pend_len > 0) {
            size_t taken = lfd3k_host_in(pend, pend_len, now_ms);
            if (taken > pend_len) taken = pend_len;
            memmove(pend, pend + taken, pend_len - taken);
            pend_len -= taken;
        }

        simnet_step();
        simnet_in(now_ms);
        lfd3k_poll(now_ms);
        simnet_step();
        wire_pump(now_ms);

        lfd3k_status_t st;
        lfd3k_status(now_ms, &st);

        /* A Wi-Fi change is a full stop and start, which would kill a live socket or SimNet
         * link: hold it while any call is up (any wire state or radio call), apply it after. */
        if (lfd3k_take_wifi_changed()) {
            wifi_apply_pending = true;
        }
        if (wifi_apply_pending && st.state == 0 && st.wire == 0) {
            wifi_apply_pending = false;
            lfd3k_wifi_cfg_t wc;
            lfd3k_get_wifi_config(&wc);
            wifi_link_apply(&wc);
            memset(&wc, 0, sizeof(wc));
        }
        if (wifi_link_take_ap_joined()) {
            lfd3k_note_ap_client_joined();
        }
        panel_publish(&st);
        {
            rf_trace_t tr;
            size_t n = lfd3k_rf_trace(tr.dbm, tr.tx, RF_TRACE_N);
            tr.count = (uint8_t)n;
            panel_publish_trace(&tr);
        }
        save_step(now_ms, &st);
        mode_step(now_ms, &st);

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
            publish_net();
            sntp_start_once();
            publish_info(&st);
            publish_name(); /* AT+NAME can change it */
            lfd3k_set_stack_free((uint32_t)uxTaskGetStackHighWaterMark(NULL) * sizeof(StackType_t));
            lfd3k_set_heap_min(esp_get_minimum_free_heap_size());
        }

        /* AT+MODE accepted but the OK could not leave in 2 s (host not reading,
         * or a dump still running): restart anyway. */
        if (s_mode_go >= 0 && (uint32_t)(now_ms - s_mode_ms) >= MODE_OUT_WAIT_MS) {
            mode_restart(); /* never returns */
        }

        /* Out to the host, never waiting. During a dump the core's own ring holds output. */
        if (s_dump_active) {
            dump_step();
        } else {
            if (!s_fb_wanted && lfd3k_take_fb_request()) {
                s_fb_wanted = true;
            }
            bool empty = drain_host_out();
            if (s_mode_go >= 0 && empty) {
                mode_restart(); /* never returns */
            }
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
