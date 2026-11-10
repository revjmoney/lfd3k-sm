#include "wifi_link.h"
#include <stdio.h>
#include <string.h>
#include "esp_event.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

static const char *TAG = "wifi";

#define AP_CHANNEL   6
#define AP_MAX_CONN  4
#define BACKOFF_MAX  30

static QueueHandle_t s_cfgq;                 /* length 1, xQueueOverwrite */
static esp_netif_t *s_sta_netif;
static portMUX_TYPE s_mux = portMUX_INITIALIZER_UNLOCKED;
static char s_status[96] = "WIFI AP OFF 0 CLIENTS STA OFF NO IP";
static volatile bool s_ap_joined;
static volatile bool s_sta_wanted;
static volatile int64_t s_retry_at_us;       /* 0 = none */
static volatile uint32_t s_backoff_s = 1;
static lfd3k_wifi_cfg_t s_cfg;               /* the Wi-Fi task's copy, normalised */
static bool s_started;

static void on_event(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    (void)arg;
    (void)data;
    if (base == WIFI_EVENT) {
        if (id == WIFI_EVENT_AP_STACONNECTED) {
            s_ap_joined = true;
        } else if (id == WIFI_EVENT_STA_DISCONNECTED && s_sta_wanted) {
            uint32_t b = s_backoff_s;
            s_retry_at_us = esp_timer_get_time() + (int64_t)b * 1000000;
            s_backoff_s = (b * 2 > BACKOFF_MAX) ? BACKOFF_MAX : b * 2;
        }
    } else if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        s_backoff_s = 1;
        s_retry_at_us = 0;
    }
}

static void apply(const lfd3k_wifi_cfg_t *c)
{
    s_sta_wanted = false;
    s_retry_at_us = 0;
    s_backoff_s = 1;
    if (s_started) {
        esp_wifi_stop();
        s_started = false;
    }
    s_cfg = *c;
    /* A hotspot needs a name and a WPA2 password (8 or more); never run it open. */
    bool ap = c->ap_on && c->ap_ssid[0] && strlen(c->ap_pass) >= 8;
    bool sta = c->sta_set && c->sta_ssid[0];
    s_cfg.ap_on = ap;
    s_cfg.sta_set = sta;
    wifi_mode_t mode = (ap && sta) ? WIFI_MODE_APSTA : ap ? WIFI_MODE_AP : sta ? WIFI_MODE_STA : WIFI_MODE_NULL;
    if (mode == WIFI_MODE_NULL) {
        ESP_LOGI(TAG, "Wi-Fi off");
        return;
    }
    wifi_config_t wc;
    esp_err_t e = esp_wifi_set_mode(mode);
    if (e == ESP_OK && ap) {
        memset(&wc, 0, sizeof(wc));
        size_t n = strlen(c->ap_ssid);
        if (n > 32) n = 32;
        memcpy(wc.ap.ssid, c->ap_ssid, n);
        wc.ap.ssid_len = (uint8_t)n;
        strlcpy((char *)wc.ap.password, c->ap_pass, sizeof(wc.ap.password));
        wc.ap.channel = AP_CHANNEL;
        wc.ap.authmode = WIFI_AUTH_WPA2_PSK;
        wc.ap.max_connection = AP_MAX_CONN;
        e = esp_wifi_set_config(WIFI_IF_AP, &wc);
    }
    if (e == ESP_OK && sta) {
        memset(&wc, 0, sizeof(wc));
        strlcpy((char *)wc.sta.ssid, c->sta_ssid, sizeof(wc.sta.ssid));
        strlcpy((char *)wc.sta.password, c->sta_pass, sizeof(wc.sta.password));
        wc.sta.threshold.authmode = c->sta_pass[0] ? WIFI_AUTH_WPA_PSK : WIFI_AUTH_OPEN;
        e = esp_wifi_set_config(WIFI_IF_STA, &wc);
    }
    memset(&wc, 0, sizeof(wc));
    if (e == ESP_OK) {
        e = esp_wifi_start();
    }
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "configure failed: %s", esp_err_to_name(e));
        return;
    }
    s_started = true;
    if (sta) {
        s_sta_wanted = true;
        esp_wifi_connect();
    }
    ESP_LOGI(TAG, "Wi-Fi up (ap=%d sta=%d)", ap, sta);
}

static void build_status(void)
{
    int clients = 0;
    if (s_cfg.ap_on && s_started) {
        wifi_sta_list_t l;
        if (esp_wifi_ap_get_sta_list(&l) == ESP_OK) {
            clients = l.num;
        }
    }
    char ip[16] = "NO IP";
    if (s_cfg.sta_set && s_started) {
        esp_netif_ip_info_t ii;
        if (esp_netif_get_ip_info(s_sta_netif, &ii) == ESP_OK && ii.ip.addr != 0) {
            snprintf(ip, sizeof(ip), IPSTR, IP2STR(&ii.ip));
        }
    }
    char line[128]; /* the core keeps 96 */
    snprintf(line, sizeof(line), "WIFI AP %s %d CLIENTS STA %s %s",
             s_cfg.ap_on ? s_cfg.ap_ssid : "OFF", clients,
             s_cfg.sta_set ? s_cfg.sta_ssid : "OFF", ip);
    portENTER_CRITICAL(&s_mux);
    strlcpy(s_status, line, sizeof(s_status));
    portEXIT_CRITICAL(&s_mux);
}

static void wifi_task(void *arg)
{
    (void)arg;
    int64_t last_status = 0;
    for (;;) {
        lfd3k_wifi_cfg_t c;
        if (xQueueReceive(s_cfgq, &c, pdMS_TO_TICKS(100)) == pdTRUE) {
            apply(&c);
            memset(&c, 0, sizeof(c));
            last_status = 0;
        }
        int64_t now = esp_timer_get_time();
        if (s_sta_wanted && s_retry_at_us != 0 && now >= s_retry_at_us) {
            s_retry_at_us = 0;
            esp_wifi_connect();
        }
        if (now - last_status >= 1000000) {
            last_status = now;
            build_status();
        }
    }
}

void wifi_link_init(void)
{
    s_cfgq = xQueueCreate(1, sizeof(lfd3k_wifi_cfg_t));
    configASSERT(s_cfgq);
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    esp_netif_create_default_wifi_ap();
    s_sta_netif = esp_netif_create_default_wifi_sta();
    wifi_init_config_t ic = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&ic));
    /* The driver never keeps its own NVS copy of the client password. */
    ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
    ESP_ERROR_CHECK(esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, on_event, NULL));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, on_event, NULL));
    xTaskCreatePinnedToCore(wifi_task, "wifi", 5120, NULL, 5, NULL, 0);
}

void wifi_link_apply(const lfd3k_wifi_cfg_t *cfg)
{
    xQueueOverwrite(s_cfgq, cfg);
}

void wifi_link_get_status(char *out, size_t cap)
{
    if (cap == 0) return;
    portENTER_CRITICAL(&s_mux);
    strlcpy(out, s_status, cap);
    portEXIT_CRITICAL(&s_mux);
}

bool wifi_link_take_ap_joined(void)
{
    bool j = s_ap_joined;
    s_ap_joined = false;
    return j;
}
