#include "usb.h"

#include "jni_host.h"
#include "lfd3k_abi.h"

#include <android/log.h>
#include <errno.h>
#include <linux/usbdevice_fs.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include <atomic>
#include <thread>

#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "lfd3k", __VA_ARGS__)

namespace {

std::atomic<Lfd3kSession *> g_session{nullptr};
std::atomic<bool> g_run{false};
std::thread g_pump;
UsbInfo g_info;

int ctrl(int fd, uint8_t type, uint8_t req, uint16_t value, uint16_t index, void *data, uint16_t len) {
    usbdevfs_ctrltransfer c{};
    c.bRequestType = type;
    c.bRequest = req;
    c.wValue = value;
    c.wIndex = index;
    c.wLength = len;
    c.timeout = 1000;
    c.data = data;
    return ioctl(fd, USBDEVFS_CONTROL, &c);
}

int bulk(int fd, unsigned ep, void *data, unsigned len, unsigned timeout) {
    usbdevfs_bulktransfer b{};
    b.ep = ep;
    b.len = len;
    b.timeout = timeout;
    b.data = data;
    int n = ioctl(fd, USBDEVFS_BULK, &b);
    if (n < 0) {
        if (errno == ETIMEDOUT || errno == EAGAIN || errno == EINTR) return 0;
        return -1;
    }
    return n;
}

void pump_main() {
    uint8_t in[512];
    uint8_t out[512];
    while (g_run.load()) {
        Lfd3kSession *s = g_session.load();
        if (!s) break;
        int n = bulk(g_info.fd, (unsigned)g_info.ep_in, in, sizeof in, 20);
        if (n < 0) {
            LOGE("USB read failed: %s", strerror(errno));
            break;
        }
        s = g_session.load();
        if (!s) break;
        if (n > 0) lfd3k_usb_rx(s, in, (size_t)n);
        s = g_session.load();
        if (!s) break;
        size_t m = lfd3k_usb_tx(s, out, sizeof out);
        if (m == 0) continue;
        int w = bulk(g_info.fd, (unsigned)g_info.ep_out, out, (unsigned)m, 100);
        if (w < 0) {
            LOGE("USB write failed: %s", strerror(errno));
            s = g_session.load();
            if (s) lfd3k_usb_tx_unget(s, out, m);
            break;
        }
        if ((size_t)w < m) {
            s = g_session.load();
            if (s) lfd3k_usb_tx_unget(s, out + w, m - (size_t)w);
        }
    }
    g_run.store(false);
}

}  // namespace

int usb_prepare(android_app *app, UsbInfo *info, std::string *err) {
    int out[6] = {};
    int rc = jni_usb_prepare(app, out, err);
    if (rc != 0) return rc;
    info->fd = out[0];
    info->ep_in = out[1];
    info->ep_out = out[2];
    info->data_iface = out[3];
    info->ctrl_iface = out[4];
    info->kind = out[5];
    return 0;
}

void usb_setup_line(const UsbInfo &info, std::string *err) {
    if (info.fd < 0) return;
    if (info.kind == 2) {
        /* CP210x, the V3's bridge. Values match the Linux cp210x driver:
         * enable the UART, 8N1, 115200, and write DTR and RTS low. */
        int rc = ctrl(info.fd, 0x41, 0x00, 0x0001, (uint16_t)info.ctrl_iface, nullptr, 0);
        uint8_t baud[4] = {0x00, 0xC2, 0x01, 0x00}; /* 115200 little endian */
        rc |= ctrl(info.fd, 0x41, 0x1E, 0, (uint16_t)info.ctrl_iface, baud, 4);
        rc |= ctrl(info.fd, 0x41, 0x03, 0x0800, (uint16_t)info.ctrl_iface, nullptr, 0);
        rc |= ctrl(info.fd, 0x41, 0x07, 0x0300, (uint16_t)info.ctrl_iface, nullptr, 0);
        if (rc != 0 && err) *err = "CP210x line setup failed; the byte stream is still opened";
        return;
    }
    /* CDC-ACM SET_LINE_CODING 115200 8N1, then SET_CONTROL_LINE_STATE with
     * DTR and RTS off (the PC app holds them low on Windows for the same
     * reason: a toggle resets the board). */
    uint8_t line[7] = {0x00, 0xC2, 0x01, 0x00, 0x00, 0x00, 0x08};
    int rc = ctrl(info.fd, 0x21, 0x20, 0, (uint16_t)info.ctrl_iface, line, 7);
    rc |= ctrl(info.fd, 0x21, 0x22, 0, (uint16_t)info.ctrl_iface, nullptr, 0);
    if (rc != 0 && err) {
        *err = "CDC-ACM line coding was refused; the byte stream is still opened";
    }
}

void usb_pump_start(Lfd3kSession *session, const UsbInfo &info) {
    usb_pump_stop();
    g_info = info;
    g_session.store(session);
    g_run.store(true);
    g_pump = std::thread(pump_main);
}

void usb_pump_stop() {
    g_run.store(false);
    g_session.store(nullptr);
    if (g_pump.joinable()) g_pump.join();
}

void usb_close(android_app *app) { jni_usb_close(app); }
