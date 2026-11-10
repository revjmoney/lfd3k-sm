/* Wi-Fi hotspot and client on their own task. The modem task passes
 * configs in and reads status out; this file never calls lfd3k_*. */
#ifndef WIFI_LINK_H
#define WIFI_LINK_H
#include <stdbool.h>
#include <stddef.h>
#include "lfd3k.h"

/* esp_netif, event loop, esp_wifi (RAM storage) and the Wi-Fi task. Call once
 * after NVS init; the radio stays off until the first wifi_link_apply. */
void wifi_link_init(void);
/* Modem task: stop, reconfigure and start (latest config wins, never blocks). */
void wifi_link_apply(const lfd3k_wifi_cfg_t *cfg);
/* Copies the latest status line (NUL terminated) into out. */
void wifi_link_get_status(char *out, size_t cap);
/* True once after a station joined the hotspot. */
bool wifi_link_take_ap_joined(void);

#endif
