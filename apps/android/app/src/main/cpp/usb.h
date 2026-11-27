#pragma once

#include <string>

struct android_app;
struct Lfd3kSession;

struct UsbInfo {
    int fd = -1;
    int ep_in = 0;
    int ep_out = 0;
    int data_iface = 0;
    int ctrl_iface = 0;
    /* 1 CDC-ACM (Heltec V4 USB serial, class 02/0A), 2 CP210x (Heltec V3). */
    int kind = 1;
};

/* 0 the device is open, 1 waiting for the permission dialog, negative on error. */
int usb_prepare(android_app *app, UsbInfo *info, std::string *err);
/* Line coding at 115200 8N1. DTR and RTS stay low so the board is not reset.
 * A failure is reported and the caller may still try the byte stream. */
void usb_setup_line(const UsbInfo &info, std::string *err);
void usb_pump_start(Lfd3kSession *session, const UsbInfo &info);
void usb_pump_stop();
void usb_close(android_app *app);
