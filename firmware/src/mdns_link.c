#include "mdns_link.h"
#include <ctype.h>
#include <string.h>
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "mdns.h"

static const char *TAG = "mdns";

#define SERVICE       "_lfd3k"
#define BROWSE_TYPE   "_telnet"
#define PROTO         "_tcp"
#define SERVICE_PORT  23
#define BROWSE_MS     3000
#define BROWSE_EVERY_US (60LL * 1000000)
#define FIRST_BROWSE_US (5LL * 1000000)
#define NAME_CAP      17
#define VER_CAP       16

static portMUX_TYPE s_mux = portMUX_INITIALIZER_UNLOCKED;

/* Shared with the modem task (under s_mux). */
static char s_want_name[NAME_CAP];
static char s_want_ver[VER_CAP];
static bool s_idle;
static lfd3k_found_t s_found[LFD3K_FOUND_MAX];
static int s_found_n = -1;               /* -1: nothing new since the last take */

/* Wi-Fi task only. */
static bool s_ok;                        /* mdns_init succeeded */
static bool s_service;                   /* _lfd3k._tcp added */
static char s_name[NAME_CAP];            /* name the service carries now */
static mdns_search_once_t *s_search;
static int64_t s_next_browse_us;
static bool s_was_up;
static char s_net[16];                   /* the network the list belongs to (client address) */
static int64_t s_add_retry_us;           /* a failed service add waits until then */
static uint32_t s_add_backoff_s = 1;     /* 1, 2, 4 ... 60 s */

void mdns_link_init(void)
{
    esp_err_t e = mdns_init();
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "mdns_init failed: %s", esp_err_to_name(e));
        return;
    }
    s_ok = true;
}

/* Station name to a host name: letters and digits lower case, anything else a dash. */
static void host_name(const char *name, char *out, size_t cap)
{
    size_t n = 0;
    for (; name[n] && n + 1 < cap; n++) {
        unsigned char c = (unsigned char)name[n];
        out[n] = isalnum(c) ? (char)tolower(c) : '-';
    }
    out[n] = 0;
}

/* Advertise _lfd3k._tcp under the modem's name, or rename it. False when the
 * service could not be added (the caller retries with a back off). */
static bool apply_name(const char *name, const char *ver)
{
    char host[NAME_CAP];
    host_name(name, host, sizeof(host));
    mdns_hostname_set(host);
    mdns_instance_name_set(name);
    if (!s_service) {
        mdns_txt_item_t txt[2] = {
            {"name", name},
            {"ver", ver},
        };
        esp_err_t e = mdns_service_add(name, SERVICE, PROTO, SERVICE_PORT, txt, 2);
        if (e != ESP_OK) {
            /* One warning per back off step, never per loop turn. */
            ESP_LOGW(TAG, "service add failed: %s, retry in %u s", esp_err_to_name(e),
                     (unsigned)s_add_backoff_s);
            return false;
        }
        s_service = true;
    } else {
        mdns_service_instance_name_set(SERVICE, PROTO, name);
        mdns_service_txt_item_set(SERVICE, PROTO, "name", name);
        mdns_service_txt_item_set(SERVICE, PROTO, "ver", ver);
    }
    strlcpy(s_name, name, sizeof(s_name));
    ESP_LOGI(TAG, "advertising %s.%s.%s", name, SERVICE, PROTO);
    return true;
}

/* The list is for a network the modem is no longer on: hand the core an empty one. */
static void clear_found(void)
{
    portENTER_CRITICAL(&s_mux);
    memset(s_found, 0, sizeof(s_found));
    s_found_n = 0;
    portEXIT_CRITICAL(&s_mux);
}

/* A finished browse: the first LFD3K_FOUND_MAX IPv4 hosts with a port. */
static void take_results(mdns_result_t *r)
{
    lfd3k_found_t f[LFD3K_FOUND_MAX];
    int n = 0;
    memset(f, 0, sizeof(f));
    for (; r && n < LFD3K_FOUND_MAX; r = r->next) {
        if (r->port == 0) {
            continue;
        }
        const mdns_ip_addr_t *a = r->addr;
        while (a && a->addr.type != ESP_IPADDR_TYPE_V4) {
            a = a->next;
        }
        if (!a || a->addr.u_addr.ip4.addr == 0) {
            continue;
        }
        const char *nm = r->instance_name ? r->instance_name : r->hostname;
        if (!nm || !nm[0]) {
            continue;
        }
        strlcpy(f[n].name, nm, sizeof(f[n].name));
        memcpy(f[n].ip, &a->addr.u_addr.ip4.addr, 4); /* network order: a.b.c.d */
        f[n].port = r->port;
        n++;
    }
    portENTER_CRITICAL(&s_mux);
    memcpy(s_found, f, sizeof(s_found));
    s_found_n = n;
    portEXIT_CRITICAL(&s_mux);
    ESP_LOGI(TAG, "browse found %d telnet host(s)", n);
}

void mdns_link_step(bool net_up, const char *net)
{
    if (!s_ok) {
        return;
    }
    char name[NAME_CAP];
    char ver[VER_CAP];
    bool idle;
    portENTER_CRITICAL(&s_mux);
    memcpy(name, s_want_name, sizeof(name));
    memcpy(ver, s_want_ver, sizeof(ver));
    idle = s_idle;
    portEXIT_CRITICAL(&s_mux);
    int64_t now = esp_timer_get_time();
    if (name[0] && strcmp(name, s_name) != 0 && now >= s_add_retry_us) {
        if (apply_name(name, ver)) {
            s_add_backoff_s = 1;
            s_add_retry_us = 0;
        } else {
            s_add_retry_us = now + (int64_t)s_add_backoff_s * 1000000;
            s_add_backoff_s = s_add_backoff_s * 2 > 60 ? 60 : s_add_backoff_s * 2;
        }
    }

    /* The network went down or changed: the hosts found are not reachable as
     * they were, so the list is cleared (and a running browse dropped). */
    bool changed = net_up && s_was_up && strcmp(net, s_net) != 0;
    if ((s_was_up && !net_up) || changed) {
        if (s_search) {
            mdns_query_async_delete(s_search);
            s_search = NULL;
        }
        clear_found();
    }
    if (net_up && (!s_was_up || changed)) {
        s_next_browse_us = now + FIRST_BROWSE_US; /* a first look soon after the network comes up */
    }
    s_was_up = net_up;
    strlcpy(s_net, net_up ? net : "", sizeof(s_net));

    if (s_search) {
        mdns_result_t *res = NULL;
        uint8_t num = 0;
        if (mdns_query_async_get_results(s_search, 0, &res, &num)) {
            take_results(res);
            mdns_query_results_free(res);
            mdns_query_async_delete(s_search);
            s_search = NULL;
        }
        return;
    }
    /* Once a minute, only while no call is up (never during a call). */
    if (net_up && idle && now >= s_next_browse_us) {
        s_next_browse_us = now + BROWSE_EVERY_US;
        s_search = mdns_query_async_new(NULL, BROWSE_TYPE, PROTO, MDNS_TYPE_PTR, BROWSE_MS,
                                        LFD3K_FOUND_MAX * 2, NULL);
        if (!s_search) {
            ESP_LOGW(TAG, "browse did not start");
        }
    }
}

void mdns_link_set_name(const char *name, const char *version)
{
    portENTER_CRITICAL(&s_mux);
    strlcpy(s_want_name, name, sizeof(s_want_name));
    strlcpy(s_want_ver, version, sizeof(s_want_ver));
    portEXIT_CRITICAL(&s_mux);
}

void mdns_link_set_idle(bool idle)
{
    portENTER_CRITICAL(&s_mux);
    s_idle = idle;
    portEXIT_CRITICAL(&s_mux);
}

int mdns_link_take_found(lfd3k_found_t *out, size_t cap)
{
    int n;
    portENTER_CRITICAL(&s_mux);
    n = s_found_n;
    if (n >= 0) {
        if ((size_t)n > cap) {
            n = (int)cap;
        }
        memcpy(out, s_found, (size_t)n * sizeof(*out));
        s_found_n = -1;
    }
    portEXIT_CRITICAL(&s_mux);
    return n;
}
