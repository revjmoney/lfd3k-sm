/* Who may open a TCP voice session: a pure rule with no ESP-IDF or lwIP
 * types, so a PC C test can compile voice_peer.c on its own. */
#ifndef VOICE_PEER_H
#define VOICE_PEER_H
#include <stdbool.h>
#include <stdint.h>

/* True when peer is on the hotspot's subnet or the client network's subnet.
 * Every address and mask is IPv4 in network byte order (as lwIP keeps them).
 * A zero ip means that interface is down and matches nothing; a zero mask
 * also matches nothing (it would let in the whole internet), and a zero
 * peer is never accepted. Known limit: anyone on the same LAN may connect. */
bool voice_peer_ok(uint32_t peer, uint32_t ap_ip, uint32_t ap_mask, uint32_t sta_ip, uint32_t sta_mask);

#endif
