/* TCP voice sessions 1 to 3 (the voice host protocol over Wi-Fi, for the
 * phone and PC voice apps). A listener task owns every socket and only moves
 * bytes between them and two 1536 byte stream buffers per slot; the modem
 * task's pump makes every lfd3k_voice_* call. Session 0 (the USB or UART
 * host stream) is never touched here. */
#ifndef VOICE_LINK_H
#define VOICE_LINK_H
#include <stdbool.h>
#include <stdint.h>
#include "voice_peer.h"

#define VOICE_PORT 4220

/* Modem task, once, after the Wi-Fi config is applied: creates the listener
 * task (priority 6, core 0). The calling task is the one it wakes when bytes
 * arrive or a socket closes. */
void voice_link_start(void);

/* Modem task, every loop turn right after lfd3k_poll: opens, feeds, drains
 * and closes the core's sessions 1 to 3 to match the sockets. */
void voice_link_pump(uint32_t now_ms);

#endif
