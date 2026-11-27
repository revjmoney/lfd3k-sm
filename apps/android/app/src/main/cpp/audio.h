#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

struct Lfd3kSession;

/* Open AAudio in and out at 16 kHz mono when the device allows it, and
 * resample otherwise. RECORD_AUDIO must already be granted or the mic
 * stream fails and `err` says so. The speaker still opens. */
bool audio_start(Lfd3kSession *session, std::string *err);
void audio_stop();
bool audio_mic_open();
const char *audio_note();
