#include "audio.h"

#include "lfd3k_abi.h"

#include <aaudio/AAudio.h>
#include <android/log.h>

#include <cmath>
#include <vector>

#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, "lfd3k", __VA_ARGS__)

namespace {

constexpr int kEngineRate = 16000;

struct Stream {
    AAudioStream *s = nullptr;
    int32_t rate = kEngineRate;
};

struct State {
    Lfd3kSession *session = nullptr;
    Stream in{};
    Stream out{};
    double mic_pos = 0;
    std::vector<int16_t> spk_hold;
    double spk_pos = 0;
    std::string note;
    bool mic_open = false;
};

State g;

/* Linear resample from `src` (rate from_hz) into `dst` (to_hz). `pos` is
 * the fractional source index carried across callbacks. */
void resample(const int16_t *src, int src_n, int from_hz, int16_t *dst, int dst_n, int to_hz, double *pos) {
    if (dst_n <= 0) return;
    if (src_n <= 0 || from_hz <= 0 || to_hz <= 0) {
        for (int i = 0; i < dst_n; i++) dst[i] = 0;
        return;
    }
    if (from_hz == to_hz) {
        int n = src_n < dst_n ? src_n : dst_n;
        for (int i = 0; i < n; i++) dst[i] = src[i];
        for (int i = n; i < dst_n; i++) dst[i] = 0;
        *pos = 0;
        return;
    }
    double step = (double)from_hz / (double)to_hz;
    const int16_t *p = src;
    int n = src_n;
    double cursor = *pos;
    if (cursor < 0) cursor = 0;
    for (int i = 0; i < dst_n; i++) {
        int i0 = (int)cursor;
        double f = cursor - i0;
        int16_t a = i0 < n ? p[i0] : 0;
        int16_t b = (i0 + 1) < n ? p[i0 + 1] : a;
        dst[i] = (int16_t)((1.0 - f) * a + f * b);
        cursor += step;
    }
    *pos = cursor - n;
    if (*pos < 0) *pos = 0;
}

aaudio_data_callback_result_t in_cb(AAudioStream *, void *, void *data, int32_t frames) {
    auto *dst = static_cast<int16_t *>(data);
    if (!g.session || frames <= 0) return AAUDIO_CALLBACK_RESULT_CONTINUE;
    int rate = g.in.rate > 0 ? g.in.rate : kEngineRate;
    int need = (int)std::ceil((double)frames * kEngineRate / rate) + 2;
    if (need < 1) need = 1;
    std::vector<int16_t> eng((size_t)need);
    resample(dst, frames, rate, eng.data(), need, kEngineRate, &g.mic_pos);
    lfd3k_mic(g.session, eng.data(), (size_t)need);
    return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

aaudio_data_callback_result_t out_cb(AAudioStream *, void *, void *data, int32_t frames) {
    auto *dst = static_cast<int16_t *>(data);
    if (!g.session || frames <= 0) {
        if (frames > 0) {
            for (int i = 0; i < frames; i++) dst[i] = 0;
        }
        return AAUDIO_CALLBACK_RESULT_CONTINUE;
    }
    int rate = g.out.rate > 0 ? g.out.rate : kEngineRate;
    int need = (int)std::ceil((double)frames * kEngineRate / rate) + 8;
    if (need < frames) need = frames;
    std::vector<int16_t> eng((size_t)need);
    size_t got = lfd3k_speaker(g.session, eng.data(), (size_t)need);
    for (size_t i = got; i < eng.size(); i++) eng[i] = 0;
    resample(eng.data(), (int)eng.size(), kEngineRate, dst, frames, rate, &g.spk_pos);
    return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

bool open_one(Stream *st, aaudio_direction_t dir, AAudioStream_dataCallback cb, std::string *err, const char *what) {
    AAudioStreamBuilder *b = nullptr;
    aaudio_result_t r = AAudio_createStreamBuilder(&b);
    if (r != AAUDIO_OK || !b) {
        if (err) *err = std::string(what) + ": no audio builder";
        return false;
    }
    AAudioStreamBuilder_setDirection(b, dir);
    AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setChannelCount(b, 1);
    AAudioStreamBuilder_setSampleRate(b, kEngineRate);
    AAudioStreamBuilder_setSharingMode(b, AAUDIO_SHARING_MODE_SHARED);
    AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
    /* Usage, content type, and input preset are API 28. minSdk is 26, so
     * the streams stay plain PCM and this binary still loads on Android 8. */
    AAudioStreamBuilder_setDataCallback(b, cb, nullptr);
    AAudioStream *s = nullptr;
    r = AAudioStreamBuilder_openStream(b, &s);
    AAudioStreamBuilder_delete(b);
    if (r != AAUDIO_OK || !s) {
        if (err) *err = std::string(what) + ": " + AAudio_convertResultToText(r);
        return false;
    }
    st->s = s;
    st->rate = AAudioStream_getSampleRate(s);
    r = AAudioStream_requestStart(s);
    if (r != AAUDIO_OK) {
        if (err) *err = std::string(what) + " start: " + AAudio_convertResultToText(r);
        AAudioStream_close(s);
        st->s = nullptr;
        return false;
    }
    LOGI("%s at %d Hz", what, st->rate);
    return true;
}

void close_stream(Stream *st) {
    if (!st->s) return;
    AAudioStream_requestStop(st->s);
    AAudioStream_close(st->s);
    st->s = nullptr;
}

}  // namespace

bool audio_start(Lfd3kSession *session, std::string *err) {
    audio_stop();
    g.session = session;
    g.mic_pos = 0;
    g.spk_pos = 0;
    g.note.clear();
    g.mic_open = false;
    std::string mic_err;
    bool spk = open_one(&g.out, AAUDIO_DIRECTION_OUTPUT, out_cb, err, "speaker");
    bool mic = open_one(&g.in, AAUDIO_DIRECTION_INPUT, in_cb, &mic_err, "microphone");
    g.mic_open = mic;
    if (!mic) {
        g.note = mic_err.empty() ? "microphone not open" : mic_err;
        if (err && err->empty()) *err = g.note;
    } else if (g.in.rate != kEngineRate || g.out.rate != kEngineRate) {
        g.note = "audio resampled to 16000 Hz";
    } else {
        g.note = "AAudio 16000 Hz mono";
    }
    return spk;
}

void audio_stop() {
    close_stream(&g.in);
    close_stream(&g.out);
    g.session = nullptr;
    g.mic_open = false;
}

bool audio_mic_open() { return g.mic_open; }

const char *audio_note() { return g.note.c_str(); }
