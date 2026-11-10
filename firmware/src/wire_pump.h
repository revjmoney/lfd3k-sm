/* Modem task side of the wire: joins lfd3k_wire_* to tcp_link. Modem task only. */
#ifndef WIRE_PUMP_H
#define WIRE_PUMP_H
#include <stdint.h>

/* Call every modem loop. Never blocks. Follows the platform contract at the
 * top of core/src/wire.rs, with a generation number per socket. */
void wire_pump(uint32_t now_ms);

#endif
