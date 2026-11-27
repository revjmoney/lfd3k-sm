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
    int kind = 1;
};

int usb_prepare(android_app *app, UsbInfo *info, std::string *err);
void usb_setup_line(const UsbInfo &info, std::string *err);
void usb_pump_start(Lfd3kSession *session, const UsbInfo &info);
void usb_pump_stop();
void usb_close(android_app *app);
