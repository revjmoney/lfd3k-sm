#include "wire_pump.h"
#include <stdbool.h>
#include <string.h>
#include "lfd3k.h"
#include "tcp_link.h"

#define CHUNK 256

static struct {
    uint32_t gen_ctr;
    uint32_t cur_gen;      /* socket the core knows about, 0 = none */
    uint32_t closing_gen;  /* CLOSE sent for this gen, waiting for its ack */
    bool closing;
    bool want_close;       /* core asked to close; draining wire_out first */
    bool connected;        /* CONNECTED reported to the core */
    bool sock_dead;        /* FAILED or CLOSED reported: nothing more to send */
    bool pend_open;
    char host[64];
    uint16_t port;
    uint8_t end_ev;        /* held FAILED/CLOSED until every received byte is delivered */
    uint8_t rx[CHUNK];     /* received bytes the core has not taken yet */
    size_t rx_len;
} w;

/* Pulls what the core wants to send into the tcp tx buffer, as space allows.
 * Returns true when the core has no more to give right now. */
static bool pull_out(void)
{
    uint8_t chunk[CHUNK];
    for (;;) {
        size_t space = tcp_link_tx_space();
        if (space == 0) {
            return false;
        }
        size_t n = lfd3k_wire_out(chunk, space < sizeof(chunk) ? space : sizeof(chunk));
        if (n == 0) {
            return true;
        }
        tcp_link_tx_write(chunk, n); /* n <= space and only this task writes */
    }
}

void wire_pump(uint32_t now_ms)
{
    /* 1. Close before request: a close always refers to the socket that exists. */
    if (lfd3k_take_wire_close()) {
        if (w.pend_open) {
            w.pend_open = false;        /* cancels a connect that never started */
        } else if (w.cur_gen != 0) {
            w.want_close = true;
            w.end_ev = 0;
        }
    }

    /* 2. A request opens a new socket, but only after the old one is acked. */
    if (!w.pend_open) {
        char host[64];
        uint16_t port = 0;
        if (lfd3k_take_wire_request(host, sizeof(host), &port)) {
            memcpy(w.host, host, sizeof(w.host));
            w.port = port;
            w.pend_open = true;
        }
    }
    if (w.pend_open && w.cur_gen != 0 && !w.want_close) {
        w.want_close = true; /* the core did not close the old socket: do it for it */
        w.end_ev = 0;
    }

    /* 3. Drain wire_out, then close the socket (and abort a pending connect). */
    if (w.want_close) {
        if (pull_out() && tcp_link_close(w.cur_gen)) {
            w.closing = true;
            w.closing_gen = w.cur_gen;
            w.cur_gen = 0;
            w.want_close = false;
            w.connected = false;
            w.sock_dead = false;
            w.rx_len = 0;
        }
    }

    /* 4. Events. Anything for a socket we closed (or are closing) is dropped. */
    tcp_event_t ev;
    while (!w.end_ev && tcp_link_event(&ev)) {
        if (ev.type == TCP_EV_CLOSE_ACK) {
            if (w.closing && ev.gen == w.closing_gen) {
                w.closing = false;
                tcp_link_rx_discard(); /* the old socket's undelivered bytes */
            }
            continue;
        }
        if (w.cur_gen == 0 || ev.gen != w.cur_gen || w.want_close) {
            continue;
        }
        if (ev.type == TCP_EV_CONNECTED) {
            w.connected = true;
            lfd3k_wire_event(1, now_ms);
        } else if (ev.type == TCP_EV_FAILED || ev.type == TCP_EV_CLOSED) {
            w.end_ev = ev.type;
        }
    }

    /* 5. Received bytes to the core; the core may take only part. */
    if (w.cur_gen != 0 && !w.want_close && w.connected) {
        for (int i = 0; i < 2 * (2048 / CHUNK) + 2; i++) {
            if (w.rx_len == 0) {
                w.rx_len = tcp_link_rx_read(w.rx, sizeof(w.rx));
                if (w.rx_len == 0) {
                    break;
                }
            }
            size_t taken = lfd3k_wire_in(w.rx, w.rx_len, now_ms);
            if (taken > w.rx_len) taken = w.rx_len;
            memmove(w.rx, w.rx + taken, w.rx_len - taken);
            w.rx_len -= taken;
            if (taken == 0) {
                break;
            }
        }
    }

    /* 6. Report closed or failed only after every received byte was delivered. */
    if (w.end_ev && w.rx_len == 0 && tcp_link_rx_avail() == 0) {
        lfd3k_wire_event(w.end_ev, now_ms);
        w.end_ev = 0;
        w.sock_dead = true;
    }

    /* 7. Core output into the socket's tx buffer while the socket is up. */
    if (w.cur_gen != 0 && w.connected && !w.want_close && !w.end_ev && !w.sock_dead) {
        pull_out();
    }

    /* 8. Open a new socket once the old one is fully gone. */
    if (w.pend_open && w.cur_gen == 0 && !w.closing && !w.want_close) {
        uint32_t gen = ++w.gen_ctr;
        if (gen == 0) gen = ++w.gen_ctr;
        if (tcp_link_open(gen, w.host, w.port)) {
            w.cur_gen = gen;
            w.connected = false;
            w.sock_dead = false;
            w.end_ev = 0;
            w.rx_len = 0;
            w.pend_open = false;
        }
    }
}
