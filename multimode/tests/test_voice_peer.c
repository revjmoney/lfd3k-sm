/* PC test of the voice peer rule (firmware/src/voice_peer.c): who may open a
 * TCP voice session. Build: see run.ps1. */
#include <stdint.h>
#include "tap.h"
#include "voice_peer.h"

/* Network byte order, as lwIP keeps IPv4 addresses: the first octet is the
 * lowest byte of the word on a little endian host, so build it byte by byte
 * with an explicit layout instead of depending on the host's order. */
static uint32_t ip(unsigned a, unsigned b, unsigned c, unsigned d)
{
    uint8_t bytes[4] = {(uint8_t)a, (uint8_t)b, (uint8_t)c, (uint8_t)d};
    uint32_t v;
    __builtin_memcpy(&v, bytes, 4);
    return v;
}

#define AP ip(192, 168, 4, 1)
#define STA ip(192, 168, 1, 159)
#define M24 ip(255, 255, 255, 0)

static void hotspot_and_client_up(void)
{
    CHECK(voice_peer_ok(ip(192, 168, 4, 2), AP, M24, STA, M24));
    CHECK(voice_peer_ok(ip(192, 168, 1, 20), AP, M24, STA, M24));
    CHECK(!voice_peer_ok(ip(10, 0, 0, 5), AP, M24, STA, M24));
    CHECK(!voice_peer_ok(ip(192, 168, 5, 2), AP, M24, STA, M24));
}

static void client_down(void)
{
    CHECK(voice_peer_ok(ip(192, 168, 4, 2), AP, M24, 0, 0));
    CHECK(!voice_peer_ok(ip(192, 168, 1, 20), AP, M24, 0, M24));
    CHECK(!voice_peer_ok(ip(192, 168, 1, 20), AP, M24, 0, 0));
}

static void both_down(void)
{
    CHECK(!voice_peer_ok(ip(192, 168, 4, 2), 0, 0, 0, 0));
    CHECK(!voice_peer_ok(ip(192, 168, 1, 20), 0, M24, 0, M24));
    CHECK(!voice_peer_ok(ip(10, 0, 0, 5), 0, 0, 0, 0));
}

static void zero_peer_and_zero_mask(void)
{
    CHECK(!voice_peer_ok(0, AP, M24, STA, M24));
    CHECK(!voice_peer_ok(0, 0, 0, 0, 0));
    /* A zero mask would match the whole internet: it matches nothing. */
    CHECK(!voice_peer_ok(ip(8, 8, 8, 8), AP, 0, STA, 0));
}

int main(void)
{
    RUN(hotspot_and_client_up);
    RUN(client_down);
    RUN(both_down);
    RUN(zero_peer_and_zero_mask);
    TAP_DONE();
}
