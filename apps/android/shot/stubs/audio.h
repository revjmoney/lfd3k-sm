#pragma once

#include <string>

struct Lfd3kSession;

bool audio_start(Lfd3kSession *session, std::string *err);
void audio_stop();
bool audio_mic_open();
const char *audio_note();
