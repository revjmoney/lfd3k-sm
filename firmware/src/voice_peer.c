#include "voice_peer.h"

static bool same_net(uint32_t peer, uint32_t ip, uint32_t mask)
{
    return ip != 0 && mask != 0 && (peer & mask) == (ip & mask);
}

bool voice_peer_ok(uint32_t peer, uint32_t ap_ip, uint32_t ap_mask, uint32_t sta_ip, uint32_t sta_mask)
{
    if (peer == 0) {
        return false;
    }
    return same_net(peer, ap_ip, ap_mask) || same_net(peer, sta_ip, sta_mask);
}
