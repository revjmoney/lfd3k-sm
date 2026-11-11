/* mDNS on the LAN (ESP-IDF mdns component): the modem advertises _lfd3k._tcp
 * (TXT name=<modem name> ver=<version>) and, once a minute while no call is up,
 * browses _telnet._tcp for an AXIS board. Runs on the Wi-Fi task; the modem
 * task only hands names in and takes results out. This file never calls lfd3k_*. */
#ifndef MDNS_LINK_H
#define MDNS_LINK_H
#include <stdbool.h>
#include <stddef.h>
#include "lfd3k.h"

/* Startup, once, from wifi_link_init (after esp_netif and the default event loop exist). */
void mdns_link_init(void);
/* Wi-Fi task, every turn of its loop: applies a new name (a failed service add
 * is retried after 1, 2, 4 ... 60 s), starts the browse when due (net_up and the
 * modem said idle), collects its results. net names the network (the client
 * address, "" on the hotspot alone): when it goes down or changes the found list
 * is cleared. Never blocks. */
void mdns_link_step(bool net_up, const char *net);

/* Modem task: the live station name and the core version (NUL terminated). */
void mdns_link_set_name(const char *name, const char *version);
/* Modem task: true while no call of any kind is up (radio or wire). */
void mdns_link_set_idle(bool idle);
/* Modem task: after a browse finished, copies up to cap hosts into out and
 * returns the count (0 when it found none); -1 when no new browse finished. */
int  mdns_link_take_found(lfd3k_found_t *out, size_t cap);

#endif
