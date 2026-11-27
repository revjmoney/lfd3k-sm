#pragma once

#include <string>

struct android_app;

int jni_usb_prepare(android_app *app, int out[6], std::string *err);
void jni_usb_close(android_app *app);
void jni_multicast(android_app *app, bool acquire);
bool jni_audio_granted(android_app *app);
void jni_request_audio(android_app *app);
std::string jni_files(android_app *app);
std::string jni_extra(android_app *app, const char *key);
void jni_keep_screen(android_app *app, bool on);
