#include "ui.h"

#include "audio.h"
#include "jni_host.h"
#include "lfd3k_abi.h"
#include "usb.h"

#include "imgui.h"

#include <android_native_app_glue.h>

#include <algorithm>
#include <cstdarg>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <string>

static_assert(sizeof(Lfd3kStatus) == 11436, "Lfd3kStatus layout drifted from the Rust ABI");
static_assert(sizeof(Lfd3kDir) == 96, "Lfd3kDir");
static_assert(sizeof(Lfd3kContact) == 232, "Lfd3kContact");

namespace {

android_app *g_app = nullptr;
Lfd3kSession *g_session = nullptr;
Lfd3kStatus g_st{};
bool g_audio = false;
bool g_keep = false;
bool g_talk = false;
bool g_extras = false;
int g_screen = 0;
int g_tab = 0;
std::string g_err;
std::string g_data;

char *g_focus = nullptr;
int g_focus_cap = 0;
bool g_lower = false;

int g_transport = 0;
char g_name[24] = "REV";
char g_label[16] = "PHONE";
char g_bind[64] = "0.0.0.0:4211";
char g_peer[64] = "127.0.0.1:4210";
char g_station[24] = "";
char g_host[64] = "192.168.4.1";
int g_port = 4220;
char g_find[24] = "";
char g_usb_label[48] = "";
int g_snr = 8;
int g_cap = 0;
bool g_auto = false;

char g_dial[24] = "";
char g_dial_label[16] = "";
char g_join[8] = "";
char g_my_name[24] = "";
char g_my_label[16] = "";
char g_st_name[24] = "";
char g_ap_ssid[40] = "";
char g_ap_pass[68] = "";
char g_sta_ssid[40] = "";
char g_sta_pass[68] = "";
char g_fwd_name[20] = "";
char g_fwd_host[48] = "";
char g_fwd_port[8] = "23";
bool g_fwd_telnet = true;
char g_pin[24] = "";
bool g_pin_voice = true;

char g_atv[8192];
char g_ati[2048];
char g_fwd[2048];
char g_wap[1024];
char g_wsta[1024];
char g_wifi[2048];

const ImVec4 kAccent(0.48f, 0.64f, 0.97f, 1.f);
const ImVec4 kWarn(0.85f, 0.64f, 0.25f, 1.f);
const ImVec4 kOk(0.55f, 0.75f, 0.35f, 1.f);
const ImVec4 kMuted(0.54f, 0.56f, 0.60f, 1.f);

void text_muted(const char *fmt, ...) {
    ImGui::PushStyleColor(ImGuiCol_Text, kMuted);
    va_list args;
    va_start(args, fmt);
    ImGui::TextWrappedV(fmt, args);
    va_end(args);
    ImGui::PopStyleColor();
}

struct Metrics {
    float dpi = 160.f;
    float font = 1.f;
    float left = 0, top = 0, right = 0, bottom = 0;
};
Metrics g_mx;
ImGuiStyle g_style_base;
bool g_style_ready = false;
float g_style_applied = 0;

float dp_scale() {
    float d = g_mx.dpi < 120.f ? 160.f : g_mx.dpi;
    float f = g_mx.font < 0.5f ? 1.f : g_mx.font;
    return (d / 160.f) * f;
}

float touch_h() {
    float dp = g_mx.dpi < 120.f ? 160.f : g_mx.dpi;
    float min_px = 48.f * (dp / 160.f);
    return std::max(ImGui::GetFrameHeight(), min_px);
}

/* A row of equal buttons shares the width. When each would be skinnier
 * than a finger, they stack and each takes the full width. */
int fit_row(const char *const *labels, const bool *accent, const bool *enabled, int n) {
    float avail = ImGui::GetContentRegionAvail().x;
    float gap = ImGui::GetStyle().ItemSpacing.x;
    float h = touch_h();
    float each = n > 0 ? (avail - gap * (float)(n - 1)) / (float)n : avail;
    bool stack = each < h * 2.4f;
    int hit = -1;
    for (int i = 0; i < n; i++) {
        if (!stack && i > 0) ImGui::SameLine();
        bool on = !enabled || enabled[i];
        if (!on) ImGui::BeginDisabled();
        if (accent && accent[i]) ImGui::PushStyleColor(ImGuiCol_Button, kAccent);
        ImVec2 sz = stack ? ImVec2(-1.f, h) : ImVec2(each, h);
        char id[96];
        std::snprintf(id, sizeof id, "%s##row%d", labels[i], i);
        if (ImGui::Button(id, sz) && on) hit = i;
        if (accent && accent[i]) ImGui::PopStyleColor();
        if (!on) ImGui::EndDisabled();
    }
    return hit;
}

void push_wrap() {
    ImGui::PushTextWrapPos(ImGui::GetCursorPos().x + ImGui::GetContentRegionAvail().x);
}

void focus_field(char *buf, int cap) {
    if (ImGui::IsItemActive()) {
        g_focus = buf;
        g_focus_cap = cap;
    }
}

bool field(const char *label, char *buf, int cap, bool secret = false) {
    const char *hash = std::strstr(label, "##");
    if (hash && hash != label) {
        char shown[128];
        size_t n = (size_t)(hash - label);
        if (n >= sizeof shown) n = sizeof shown - 1;
        std::memcpy(shown, label, n);
        shown[n] = 0;
        ImGui::TextWrapped("%s", shown);
    } else if (!hash) {
        ImGui::TextWrapped("%s", label);
    }
    ImGui::SetNextItemWidth(-1.f);
    char id[160];
    std::snprintf(id, sizeof id, "##%s", hash ? hash + 2 : label);
    ImGuiInputTextFlags fl = secret ? ImGuiInputTextFlags_Password : 0;
    bool e = ImGui::InputText(id, buf, (size_t)cap, fl);
    focus_field(buf, cap);
    return e;
}

bool button(const char *label, bool accent = false, bool enabled = true) {
    if (!enabled) ImGui::BeginDisabled();
    if (accent) ImGui::PushStyleColor(ImGuiCol_Button, kAccent);
    bool hit = ImGui::Button(label, ImVec2(-1.f, touch_h()));
    if (accent) ImGui::PopStyleColor();
    if (!enabled) ImGui::EndDisabled();
    return hit && enabled;
}

void set_line(const std::string &line) {
    if (g_session) lfd3k_set(g_session, line.c_str());
}

const char *call_state_name(int s) {
    switch (s) {
    case 1: return "DIALING";
    case 2: return "RINGING";
    case 3: return "TRAINING";
    case 4:
    case 5: return "IN CALL";
    default: return "IDLE";
    }
}

int sreg_of(const char *text, int n) {
    char key[8];
    std::snprintf(key, sizeof key, "S%02d=", n);
    const char *p = std::strstr(text, key);
    if (!p) return -99999;
    return std::atoi(p + std::strlen(key));
}

bool line_value(const char *text, const char *prefix, char *out, int cap) {
    out[0] = 0;
    const char *p = text;
    size_t n = std::strlen(prefix);
    while (*p) {
        const char *nl = std::strchr(p, '\n');
        size_t len = nl ? (size_t)(nl - p) : std::strlen(p);
        if (len >= n && std::strncmp(p, prefix, n) == 0) {
            size_t m = len - n;
            if ((int)m >= cap) m = (size_t)cap - 1;
            std::memcpy(out, p + n, m);
            out[m] = 0;
            return true;
        }
        if (!nl) break;
        p = nl + 1;
    }
    return false;
}

void refresh_text() {
    if (!g_session) return;
    lfd3k_text(g_session, 0, g_atv, sizeof g_atv);
    lfd3k_text(g_session, 1, g_ati, sizeof g_ati);
    lfd3k_text(g_session, 2, g_fwd, sizeof g_fwd);
    lfd3k_text(g_session, 3, g_wap, sizeof g_wap);
    lfd3k_text(g_session, 4, g_wsta, sizeof g_wsta);
    lfd3k_text(g_session, 5, g_wifi, sizeof g_wifi);
}

void stop_session() {
    audio_stop();
    g_audio = false;
    if (g_keep) {
        jni_keep_screen(g_app, false);
        g_keep = false;
    }
    jni_multicast(g_app, false);
    if (g_session) {
        lfd3k_shutdown(g_session);
        usb_pump_stop();
        lfd3k_stop(g_session);
        g_session = nullptr;
    }
    usb_close(g_app);
    g_talk = false;
    g_st = {};
}

bool start_session() {
    stop_session();
    g_err.clear();
    if (g_data.empty()) g_data = jni_files(g_app) + "/voice";
    std::string usb_note;
    UsbInfo usb;
    if (g_transport == 3) {
        int rc = usb_prepare(g_app, &usb, &g_err);
        if (rc == 1) return false;
        if (rc != 0) return false;
        usb_setup_line(usb, &usb_note);
        if (usb.kind == 2) std::snprintf(g_usb_label, sizeof g_usb_label, "CP210x");
        else std::snprintf(g_usb_label, sizeof g_usb_label, "CDC-ACM");
    }
    if (g_transport == 2) jni_multicast(g_app, true);
    char err[400] = {};
    g_session = lfd3k_start(
        g_transport, g_name, g_label, g_data.c_str(), g_bind, g_peer, g_station, g_host, g_port, g_find,
        g_usb_label, g_snr, g_cap, -1, g_auto ? 1 : 0, err, sizeof err);
    if (!g_session) {
        g_err = err[0] ? err : "could not start";
        jni_multicast(g_app, false);
        usb_close(g_app);
        return false;
    }
    if (g_transport == 3) usb_pump_start(g_session, usb);
    if (!usb_note.empty()) g_err = usb_note;
    std::snprintf(g_my_name, sizeof g_my_name, "%s", g_name);
    std::snprintf(g_my_label, sizeof g_my_label, "%s", g_label);
    return true;
}

void maybe_audio() {
    if (!g_session || !g_st.ready || g_st.lost || g_audio) return;
    if (!jni_audio_granted(g_app)) return;
    std::string err;
    g_audio = audio_start(g_session, &err);
    if (!err.empty() && g_err.empty()) g_err = err;
}

void keyboard() {
    if (!g_focus || g_focus_cap <= 1) return;
    ImGui::Separator();
    ImGui::TextWrapped("keyboard");
    auto type = [](char c) {
        if (!g_focus) return;
        size_t n = std::strlen(g_focus);
        if ((int)n + 1 < g_focus_cap) {
            g_focus[n] = c;
            g_focus[n + 1] = 0;
        }
    };
    auto key_row = [&](const char *row, int id_base) {
        int count = (int)std::strlen(row);
        if (count <= 0) return;
        float avail = ImGui::GetContentRegionAvail().x;
        float gap = ImGui::GetStyle().ItemSpacing.x;
        float h = touch_h();
        float min_w = std::max(32.f * (g_mx.dpi < 120.f ? 1.f : g_mx.dpi / 160.f), h * 0.7f);
        int per = count;
        float key_w = (avail - gap * (float)(count - 1)) / (float)count;
        if (key_w < min_w) {
            per = std::max(1, (int)((avail + gap) / (min_w + gap)));
            key_w = (avail - gap * (float)(per - 1)) / (float)per;
        }
        for (int c = 0; c < count; c++) {
            if (c % per != 0) ImGui::SameLine();
            char id[24];
            std::snprintf(id, sizeof id, "%c##k%d", row[c], id_base + c);
            if (ImGui::Button(id, ImVec2(key_w, h))) type(row[c]);
        }
    };
    const char *rows[] = {"1234567890", g_lower ? "qwertyuiop" : "QWERTYUIOP",
                          g_lower ? "asdfghjkl" : "ASDFGHJKL", g_lower ? "zxcvbnm" : "ZXCVBNM"};
    for (int r = 0; r < 4; r++) key_row(rows[r], r * 16);
    const char *extra[] = {g_lower ? "ABC" : "abc", "space", ".", "-", "_", ":", "/"};
    bool accents[7] = {};
    bool enabled[7] = {true, true, true, true, true, true, true};
    int hit = fit_row(extra, accents, enabled, 7);
    if (hit == 0) g_lower = !g_lower;
    else if (hit == 1) type(' ');
    else if (hit == 2) type('.');
    else if (hit == 3) type('-');
    else if (hit == 4) type('_');
    else if (hit == 5) type(':');
    else if (hit == 6) type('/');
    const char *edit[] = {"delete", "done"};
    bool ea[2] = {false, true};
    bool ee[2] = {true, true};
    int eh = fit_row(edit, ea, ee, 2);
    if (eh == 0 && g_focus && g_focus[0]) {
        size_t n = std::strlen(g_focus);
        g_focus[n - 1] = 0;
    } else if (eh == 1) {
        g_focus = nullptr;
    }
}

void draw_scope() {
    ImVec2 p = ImGui::GetCursorScreenPos();
    float w = ImGui::GetContentRegionAvail().x;
    float h = std::max(72.f, 36.f * dp_scale());
    ImDrawList *dl = ImGui::GetWindowDrawList();
    dl->AddRectFilled(p, ImVec2(p.x + w, p.y + h), IM_COL32(11, 12, 14, 255));
    int n = g_st.scope_count;
    if (n < 2) {
        dl->AddText(ImVec2(p.x + 8, p.y + 28), IM_COL32(140, 144, 153, 255), "NO MODEM SIGNAL");
    } else {
        for (int i = 1; i < n; i++) {
            auto x_of = [&](int i) { return p.x + w * (float)i / (float)(n - 1); };
            auto y_of = [&](int dbm) {
                float t = (dbm + 130.f) / 100.f;
                if (t < 0) t = 0;
                if (t > 1) t = 1;
                return p.y + h - t * (h - 8) - 4;
            };
            ImU32 col = g_st.scope[i].tx ? IM_COL32(122, 162, 247, 255) : IM_COL32(180, 186, 194, 255);
            dl->AddLine(ImVec2(x_of(i - 1), y_of(g_st.scope[i - 1].dbm)), ImVec2(x_of(i), y_of(g_st.scope[i].dbm)),
                        col, 1.5f);
        }
    }
    ImGui::Dummy(ImVec2(w, h));
    const char *rssi = g_st.rssi <= -200 ? "NO SIGNAL DATA" : "";
    char line[160];
    if (g_st.rssi > -200) std::snprintf(line, sizeof line, "MODEM SIGNAL  %s  %d dBm  sounds %s", call_state_name(g_st.call_state),
                                        g_st.rssi, g_st.sounds ? "ON" : "OFF");
    else std::snprintf(line, sizeof line, "MODEM SIGNAL  %s  %s  sounds %s", call_state_name(g_st.call_state), rssi,
                       g_st.sounds ? "ON" : "OFF");
    ImGui::TextWrapped("%s", line);
}

void draw_footer() {
    ImGui::Separator();
    ImGui::PushStyleColor(ImGuiCol_Text, kMuted);
    ImGui::TextWrapped("Based on LFD3K-SM by Rev. J. Money / jmscnc.com");
    ImGui::PopStyleColor();
}

void draw_banner() {
    if (g_st.banner[0]) {
        ImGui::PushStyleColor(ImGuiCol_Text, g_st.banner_warn ? kWarn : kOk);
        ImGui::TextWrapped("%s", g_st.banner);
        ImGui::PopStyleColor();
    }
    if (!g_err.empty()) ImGui::TextWrapped("%s", g_err.c_str());
    if (g_st.note[0]) text_muted("%s", g_st.note);
}

void draw_save_bar() {
    if (!g_st.unsaved || g_st.locked) return;
    ImGui::PushStyleColor(ImGuiCol_Text, kWarn);
    ImGui::TextWrapped(g_st.pw_dirty ? "SETTINGS CHANGED, NOT SAVED (a password was set). They are live now."
                                     : "SETTINGS CHANGED, NOT SAVED. They are live now. SAVE keeps them (AT&W).");
    ImGui::PopStyleColor();
    const char *labs[] = {"SAVE", "DISCARD"};
    bool acc[] = {true, false};
    bool en[] = {!g_st.in_call, !g_st.in_call};
    int hit = fit_row(labs, acc, en, 2);
    if (hit == 0) lfd3k_save(g_session);
    if (hit == 1) lfd3k_discard(g_session);
}

void draw_people(bool dir_page) {
    int n = g_st.dir_count;
    if (n > LFD3K_DIR_N) n = LFD3K_DIR_N;
    if (n == 0) text_muted("Nobody nearby yet. SCAN asks at once; a beacon takes about 15 s.");
    for (int i = 0; i < n; i++) {
        const Lfd3kDir &d = g_st.dir[i];
        if (!dir_page && !d.voice) continue;
        ImGui::PushID(i);
        char row[96];
        if (d.label[0]) std::snprintf(row, sizeof row, "%s/%s", d.name, d.label);
        else std::snprintf(row, sizeof row, "%s", d.name);
        ImGui::TextWrapped("%s", row);
        char meta[64];
        if (d.sf) std::snprintf(meta, sizeof meta, "%s  SF%d%s", d.hint, d.sf, d.pinned ? "  PIN" : "");
        else std::snprintf(meta, sizeof meta, "%s%s", d.hint, d.pinned ? "  PIN" : "");
        ImGui::TextWrapped("%s", meta);
        if (button("CALL", true, !g_st.in_call)) {
            lfd3k_call(g_session, d.name, d.voice ? d.label : "");
            g_screen = 0;
        }
        if (dir_page) {
            const char *pin = d.pinned ? "UNPIN" : "PIN";
            if (!d.voice) {
                const char *labs[] = {pin, "DELETE"};
                bool acc[] = {false, false};
                bool en[] = {!g_st.locked, !g_st.locked};
                int hit = fit_row(labs, acc, en, 2);
                if (hit == 0) {
                    char line[64];
                    std::snprintf(line, sizeof line, "AT+DIR=%s%s", d.pinned ? "-" : "+", d.name);
                    set_line(line);
                } else if (hit == 1) {
                    char line[48];
                    std::snprintf(line, sizeof line, "AT+DIR=-%s", d.name);
                    set_line(line);
                }
            } else if (button(pin, false, !g_st.locked)) {
                char line[64];
                std::snprintf(line, sizeof line, "AT+DIR=%sV:%s", d.pinned ? "-" : "+", d.name);
                set_line(line);
            }
        }
        ImGui::PopID();
    }
}

void draw_call_panel() {
    if (g_st.ringing && !g_st.in_call) {
        ImGui::Separator();
        ImGui::TextWrapped("INCOMING CALL");
        ImGui::TextWrapped("%s/%s", g_st.ring_name, g_st.ring_label);
        ImGui::TextWrapped("%s", g_st.ring_station);
        const char *labs[] = {"ANSWER", "DECLINE"};
        bool acc[] = {true, false};
        int hit = fit_row(labs, acc, nullptr, 2);
        if (hit == 0) lfd3k_answer(g_session);
        if (hit == 1) lfd3k_decline(g_session);
        return;
    }
    if (!g_st.in_call && !g_st.who[0]) return;
    ImGui::Separator();
    ImGui::TextWrapped("%s", g_st.who[0] ? g_st.who : "CALL");
    if (g_st.peer_name[0]) ImGui::TextWrapped("%s/%s  via %s", g_st.peer_name, g_st.peer_label, g_st.peer_station);
    if (g_st.plan[0]) ImGui::TextWrapped("%s", g_st.plan);
    if (g_st.trust_line[0]) {
        if (g_st.conflict) ImGui::PushStyleColor(ImGuiCol_Text, kWarn);
        ImGui::TextWrapped("%s", g_st.trust_line);
        if (g_st.conflict) ImGui::PopStyleColor();
    }
    if (g_st.words0[0]) ImGui::TextWrapped("words: %s %s", g_st.words0, g_st.words1);
    if (!g_st.audio_ok && !g_st.conflict && g_st.words0[0]) {
        const char *labs[] = {"WORDS MATCH", "TALK ANYWAY"};
        bool acc[] = {true, false};
        int hit = fit_row(labs, acc, nullptr, 2);
        if (hit == 0) lfd3k_words(g_session);
        if (hit == 1) lfd3k_anyway(g_session);
        ImGui::TextWrapped("Audio is held until the words match, or you talk anyway for this call.");
    } else if (g_st.talk_anyway) {
        text_muted("Talking anyway for this call.");
    }
    if (g_st.ptt) {
        const char *floor = g_st.floor == 1 ? "YOU HAVE THE FLOOR" : g_st.floor == 2 ? "THEY ARE TALKING" : "FLOOR FREE";
        ImGui::TextWrapped("%s", floor);
        if (g_st.floor == 2) ImGui::TextWrapped("WAITING FOR THE FLOOR");
        ImGui::Button("TALK", ImVec2(-1, touch_h() * 1.6f));
        bool down = ImGui::IsItemActive();
        if (down != g_talk) {
            lfd3k_talk(g_session, down ? 1 : 0);
            g_talk = down;
        }
    }
    const char *labs[] = {g_st.muted ? "UNMUTE" : "MUTE", "HANG UP"};
    bool acc[] = {false, true};
    int hit = fit_row(labs, acc, nullptr, 2);
    if (hit == 0) lfd3k_mute(g_session, g_st.muted ? 0 : 1);
    if (hit == 1) {
        if (g_talk) {
            lfd3k_talk(g_session, 0);
            g_talk = false;
        }
        lfd3k_hangup(g_session);
    }
}

void draw_calls() {
    ImGui::TextWrapped("CALL BY NAME");
    field("name##dial", g_dial, sizeof g_dial);
    field("label, optional##dial", g_dial_label, sizeof g_dial_label);
    if (button("CALL", true, g_dial[0] && !g_st.in_call)) {
        lfd3k_call(g_session, g_dial, g_dial_label);
    }
    ImGui::Separator();
    ImGui::TextWrapped("PEOPLE AND STATIONS NEARBY");
    draw_people(false);
    draw_call_panel();
}

void draw_directory() {
    const char *labs[] = {"SCAN", "CLEAR UNPINNED"};
    bool acc[] = {true, false};
    bool en[] = {!g_st.scanning, !g_st.locked};
    int hit = fit_row(labs, acc, en, 2);
    if (hit == 0) lfd3k_scan(g_session);
    if (hit == 1) set_line("AT+DIR=0");
    if (g_st.scanning) text_muted("scanning");
    if (g_st.locked) ImGui::TextWrapped("Settings are locked over Wi-Fi. Plug the modem in by USB to change them. Calls and SCAN still work.");
    draw_people(true);
    ImGui::Separator();
    ImGui::TextWrapped("PIN A NAME");
    field("name##pin", g_pin, sizeof g_pin);
    ImGui::Checkbox("voice name (every device)", &g_pin_voice);
    if (button("PIN", false, g_pin[0] && !g_st.locked)) {
        char line[48];
        if (g_pin_voice) std::snprintf(line, sizeof line, "AT+DIR=+V:%s", g_pin);
        else std::snprintf(line, sizeof line, "AT+DIR=+%s", g_pin);
        set_line(line);
    }
    if (g_st.scan_count > 0) {
        ImGui::Separator();
        ImGui::TextWrapped("SCAN ANSWERS");
        for (int i = 0; i < g_st.scan_count && i < LFD3K_SCAN_N; i++) ImGui::TextWrapped("%s", g_st.scan[i]);
    }
}

void sreg_row(const char *title, int reg, int step, int lo, int hi) {
    int v = sreg_of(g_atv, reg);
    ImGui::PushID(reg);
    ImGui::TextWrapped("%s", title);
    if (v > -9999) ImGui::TextWrapped("%d", v);
    if (reg == 30 && v > 2) ImGui::TextWrapped("ABOVE THE SAFE DEFAULT (2)");
    bool en = !g_st.locked && !g_st.in_call && v > -9999;
    const char *labs[] = {"-", "+"};
    bool acc[] = {false, false};
    bool ena[] = {en && v - step >= lo, en && v + step <= hi};
    int hit = fit_row(labs, acc, ena, 2);
    if (hit == 0) {
        char line[32];
        std::snprintf(line, sizeof line, "ATS%d=%d", reg, v - step);
        set_line(line);
    }
    if (hit == 1) {
        char line[32];
        std::snprintf(line, sizeof line, "ATS%d=%d", reg, v + step);
        set_line(line);
    }
    ImGui::PopID();
}

void draw_radio() {
    if (g_st.locked) ImGui::TextWrapped("Settings are locked over Wi-Fi. Plug the modem in by USB to change them.");
    ImGui::TextWrapped("Station name (AT+NAME)");
    field("name##st", g_st_name, sizeof g_st_name);
    if (button("SET NAME", false, !g_st.locked && !g_st.in_call)) {
        char line[40];
        std::snprintf(line, sizeof line, "AT+NAME=%s", g_st_name);
        set_line(line);
    }
    sreg_row("TX power S30 (dBm, safe default 2)", 30, 1, -9, 22);
    sreg_row("Tier cap S21 (0 SF12 to 7 SF5)", 21, 1, 0, 7);
    if (g_st.cap_active) text_muted("A tier cap is on for this run. S21 is put back when you leave, and SAVE keeps the old value.");
    sreg_row("Auto-answer rings S0 (data calls)", 0, 1, 0, 255);
    sreg_row("Compression S31", 31, 1, 0, 1);
    sreg_row("Boot to menu S32", 32, 1, 0, 1);
    sreg_row("Training margin S20 (quarter dB)", 20, 4, 0, 80);
    sreg_row("Carrier wait S7 (seconds)", 7, 5, 1, 255);
    sreg_row("Resync timeout S22 (seconds)", 22, 1, 1, 255);
}

void draw_privacy() {
    char beacon[32] = {};
    line_value(g_atv, "BEACON ", beacon, sizeof beacon);
    ImGui::TextWrapped("Privacy (AT+BEACON) now: %s", beacon[0] ? beacon : "?");
    const char *modes[] = {"ON", "QUIET", "HIDDEN"};
    const char *notes[] = {
        "Beacons and voice beacons. Your names show up nearby.",
        "No beacons. Still answers scans.",
        "Answers only pinned callers.",
    };
    for (int i = 0; i < 3; i++) {
        bool on = std::strncmp(beacon, modes[i], std::strlen(modes[i])) == 0;
        if (button(modes[i], on, !g_st.locked && !on)) {
            char line[32];
            std::snprintf(line, sizeof line, "AT+BEACON=%s", modes[i]);
            set_line(line);
        }
        ImGui::TextWrapped("%s", notes[i]);
    }
    ImGui::Separator();
    ImGui::TextWrapped("PINNED");
    int n = g_st.dir_count > LFD3K_DIR_N ? LFD3K_DIR_N : g_st.dir_count;
    for (int i = 0; i < n; i++) {
        if (!g_st.dir[i].pinned) continue;
        ImGui::TextWrapped("%s%s%s", g_st.dir[i].name, g_st.dir[i].label[0] ? "/" : "", g_st.dir[i].label);
    }
}

void draw_wifi() {
    char ap[128] = {};
    line_value(g_atv, "WIFI AP ", ap, sizeof ap);
    ImGui::TextWrapped("Hotspot: %s", ap[0] ? ap : "(no readout yet)");
    bool ap_on = std::strncmp(ap, "ON", 2) == 0;
    if (button(ap_on ? "HOTSPOT OFF" : "HOTSPOT ON", false, !g_st.locked)) set_line(ap_on ? "AT+WAP=0" : "AT+WAP=1");
    field("hotspot name##ap", g_ap_ssid, sizeof g_ap_ssid);
    field("password, 8 to 63##ap", g_ap_pass, sizeof g_ap_pass, true);
    if (button("SET HOTSPOT", false, !g_st.locked && g_ap_ssid[0] && std::strlen(g_ap_pass) >= 8)) {
        char line[120];
        std::snprintf(line, sizeof line, "AT+WAP=%s,%s", g_ap_ssid, g_ap_pass);
        set_line(line);
        g_ap_pass[0] = 0;
    }
    ImGui::Separator();
    char sta[128] = {};
    line_value(g_wsta, "WSTA ", sta, sizeof sta);
    if (!sta[0]) line_value(g_atv, "STA ", sta, sizeof sta);
    ImGui::TextWrapped("Client: %s", sta[0] ? sta : "(none)");
    field("network name##sta", g_sta_ssid, sizeof g_sta_ssid);
    field("password##sta", g_sta_pass, sizeof g_sta_pass, true);
    const char *join[] = {"JOIN NETWORK", "FORGET"};
    bool jacc[] = {false, false};
    bool jen[] = {!g_st.locked && g_sta_ssid[0], !g_st.locked};
    int jhit = fit_row(join, jacc, jen, 2);
    if (jhit == 0) {
        char line[120];
        std::snprintf(line, sizeof line, "AT+WSTA=%s,%s", g_sta_ssid, g_sta_pass);
        set_line(line);
        g_sta_pass[0] = 0;
    }
    if (jhit == 1) set_line("AT+WSTA=0");
    ImGui::Separator();
    ImGui::TextWrapped("LIVE");
    ImGui::TextWrapped("%s", g_wifi[0] ? g_wifi : "(waiting)");
    text_muted("The built-in SimNet modem has no Wi-Fi, so these lines do nothing there and no password is written to disk.");
}

void draw_forwards() {
    ImGui::TextWrapped("ATDT name dials a forward over Wi-Fi. Publish bridges radio callers to one of them.");
    ImGui::TextWrapped("%s", g_fwd[0] ? g_fwd : "(no forwards)");
    field("name##fwd", g_fwd_name, sizeof g_fwd_name);
    field("host##fwd", g_fwd_host, sizeof g_fwd_host);
    field("port##fwd", g_fwd_port, sizeof g_fwd_port);
    const char *mode[] = {g_fwd_telnet ? "TELNET" : "RAW", "SAVE FORWARD"};
    bool macc[] = {false, true};
    bool men[] = {true, !g_st.locked && g_fwd_name[0] && g_fwd_host[0]};
    int mhit = fit_row(mode, macc, men, 2);
    if (mhit == 0) g_fwd_telnet = !g_fwd_telnet;
    if (mhit == 1) {
        char line[140];
        std::snprintf(line, sizeof line, "AT+FWD=%s,%s,%s,%s", g_fwd_name, g_fwd_host, g_fwd_port, g_fwd_telnet ? "T" : "R");
        set_line(line);
    }
    const char *act[] = {"DELETE THIS NAME", "PUBLISH", "UNPUBLISH"};
    bool aacc[] = {false, false, false};
    bool aen[] = {!g_st.locked && g_fwd_name[0], !g_st.locked && g_fwd_name[0], !g_st.locked};
    int ahit = fit_row(act, aacc, aen, 3);
    if (ahit == 0) {
        char line[40];
        std::snprintf(line, sizeof line, "AT+FWD=%s", g_fwd_name);
        set_line(line);
    }
    if (ahit == 1) {
        char line[40];
        std::snprintf(line, sizeof line, "AT+PUB=%s", g_fwd_name);
        set_line(line);
    }
    if (ahit == 2) set_line("AT+PUB=0");
}

bool same_key(const uint8_t *a, const uint8_t *b) {
    return std::memcmp(a, b, 32) == 0;
}

void draw_voice() {
    ImGui::TextWrapped("This phone: %s/%s", g_st.name, g_st.label);
    ImGui::TextWrapped("%s", g_st.has_person ? "Holds the person key" : "No person key yet (link to adopt one)");
    field("voice name##me", g_my_name, sizeof g_my_name);
    field("label##me", g_my_label, sizeof g_my_label);
    if (button("SET NAME AND LABEL", false, !g_st.in_call)) lfd3k_rename(g_session, g_my_name, g_my_label);
    ImGui::Separator();
    ImGui::TextWrapped("LINK A DEVICE");
    if (g_st.link_code[0]) {
        ImGui::TextWrapped("CODE %s", g_st.link_code);
        ImGui::TextWrapped("One use, five minutes. Read it on the new phone.");
        if (button("CANCEL")) lfd3k_cancel_link(g_session);
    } else if (button("LINK A DEVICE", true, g_st.has_person && !g_st.linking)) {
        lfd3k_link_offer(g_session);
    }
    field("code##join", g_join, sizeof g_join);
    if (button("JOIN", false, g_join[0])) {
        lfd3k_link_join(g_session, g_join);
        g_join[0] = 0;
    }
    if (g_st.link_note[0]) ImGui::TextWrapped("%s", g_st.link_note);
    text_muted("After JOIN, call the first device by name. The link runs inside that call.");
    ImGui::Separator();
    ImGui::TextWrapped("CHECKED CONTACTS");
    int n = g_st.contact_count > LFD3K_CONTACT_N ? LFD3K_CONTACT_N : g_st.contact_count;
    if (n == 0) text_muted("None yet. WORDS MATCH on a call remembers the device.");
    for (int i = 0; i < n; i++) {
        const Lfd3kContact &c = g_st.contacts[i];
        ImGui::PushID(i);
        ImGui::TextWrapped("%s%s", c.name, c.person ? "  (person key)" : "");
        int nd = c.n_dev;
        if (nd > 4) nd = 4;
        for (int d = 0; d < nd; d++) {
            ImGui::PushID(d);
            bool mine = same_key(c.key[d], g_st.device_key);
            ImGui::TextWrapped("%s %02x%02x%02x%02x%s", c.label[d], c.key[d][0], c.key[d][1], c.key[d][2], c.key[d][3],
                               mine ? "  this phone" : "");
            if (!mine && button("REMOVE")) {
                if (std::strcmp(c.name, g_st.name) == 0) lfd3k_remove_mine(g_session, c.key[d]);
                else lfd3k_remove_contact(g_session, c.name, c.key[d]);
            }
            ImGui::PopID();
        }
        if (c.removed) text_muted("removed devices: %d", c.removed);
        ImGui::PopID();
    }
}

void draw_about() {
    ImGui::TextWrapped("LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM");
    ImGui::TextWrapped("Your own encrypted radio (cell) phone network!!");
    ImGui::TextWrapped("Call anyone with a modem. No carrier. No tower. No one listening.");
    ImGui::TextWrapped("Rev. J. Money");
    ImGui::TextWrapped("Based on LFD3K-SM by Rev. J. Money / jmscnc.com");
    ImGui::Separator();
    ImGui::TextWrapped("lfd3k-voice-android 0.5.0, GPL-3.0-or-later (LICENSE, NOTICE.md, THIRD_PARTY.md)");
    ImGui::TextWrapped("Sound: %s", g_audio ? audio_note() : "not open");
    ImGui::TextWrapped("Data: %s", g_data.c_str());
    ImGui::TextWrapped("Font: IBM Plex Mono (SIL Open Font License 1.1)");
    if (button(g_st.sounds ? "CONNECTION SOUNDS ON" : "CONNECTION SOUNDS OFF", g_st.sounds)) {
        lfd3k_sounds(g_session, g_st.sounds ? 0 : 1);
    }
    text_muted("Chirps while a call is set up, and at NO CARRIER.");
    ImGui::TextWrapped("No ham mode yet, so the FLOWTRON SCRAMBLER is always on. Use a headset for CALL mode: this app has no echo canceller of its own.");
    ImGui::Separator();
    ImGui::TextWrapped("ATI");
    ImGui::TextWrapped("%s", g_ati[0] ? g_ati : "(waiting)");
    ImGui::TextWrapped("AT&V");
    ImGui::TextWrapped("%s", g_atv[0] ? g_atv : "(waiting)");
}

void draw_settings() {
    const char *tabs[] = {"RADIO", "PRIVACY", "WI-FI", "FORWARDS", "VOICE", "ABOUT"};
    bool acc[6];
    for (int i = 0; i < 6; i++) acc[i] = g_tab == i;
    int hit = fit_row(tabs, acc, nullptr, 6);
    if (hit >= 0) g_tab = hit;
    ImGui::Separator();
    if (g_tab == 0) draw_radio();
    else if (g_tab == 1) draw_privacy();
    else if (g_tab == 2) draw_wifi();
    else if (g_tab == 3) draw_forwards();
    else if (g_tab == 4) draw_voice();
    else draw_about();
}

void draw_connected() {
    char head[220];
    if (g_st.link[0]) std::snprintf(head, sizeof head, "%s/%s   %s   %s", g_st.name, g_st.label, g_st.station, g_st.link);
    else std::snprintf(head, sizeof head, "%s/%s   %s   %s to %s", g_st.name, g_st.label, g_st.station, g_st.bind_addr, g_st.peer_addr);
    ImGui::TextWrapped("%s", head);
    if (g_st.connecting) text_muted("Connecting...");
    if (g_st.lost) {
        ImGui::TextColored(kWarn, "MODEM LOST");
        if (button("BACK", true)) {
            stop_session();
            return;
        }
    }
    const char *nav[] = {"CALLS", "DIRECTORY", "SETTINGS", "LEAVE"};
    bool nacc[] = {g_screen == 0, g_screen == 1, g_screen == 2, false};
    int nh = fit_row(nav, nacc, nullptr, 4);
    if (nh == 0) g_screen = 0;
    if (nh == 1) g_screen = 1;
    if (nh == 2) g_screen = 2;
    if (nh == 3) {
        stop_session();
        return;
    }
    if (!jni_audio_granted(g_app)) {
        ImGui::TextWrapped("The microphone needs permission. Calls still connect, with no mic audio.");
        if (button("ALLOW MICROPHONE", true)) jni_request_audio(g_app);
    }
    draw_banner();
    draw_save_bar();
    if (g_screen == 0) draw_calls();
    else if (g_screen == 1) draw_directory();
    else draw_settings();
    if (g_st.ringing && g_screen != 0) {
        g_screen = 0;
    }
    draw_scope();
}

void draw_connect() {
    ImGui::TextWrapped("LRA FLOWTRON DATAMASTER 3000 SERIAL MODEM - LFD3K-SM");
    ImGui::TextWrapped("Your own encrypted radio (cell) phone network!!");
    ImGui::Separator();
    const char *modes[] = {"SIMNET", "TCP", "FIND", "USB"};
    bool macc[] = {g_transport == 0, g_transport == 1, g_transport == 2, g_transport == 3};
    int mh = fit_row(modes, macc, nullptr, 4);
    if (mh >= 0) g_transport = mh;
    field("your name##n", g_name, sizeof g_name);
    field("label##l", g_label, sizeof g_label);
    if (g_transport == 0) {
        field("bind host:port##b", g_bind, sizeof g_bind);
        field("peer host:port##p", g_peer, sizeof g_peer);
        ImGui::TextWrapped("what the other modem hears, dB");
        ImGui::SetNextItemWidth(-1);
        ImGui::InputInt("##snr", &g_snr);
        field("station name, optional##s", g_station, sizeof g_station);
        text_muted("SimNet is the built-in modem over UDP, the same as the PC app. Two phones, or a phone and the PC app, call with no radio.");
    } else if (g_transport == 1) {
        field("modem host##h", g_host, sizeof g_host);
        ImGui::TextWrapped("port (4220)");
        ImGui::SetNextItemWidth(-1);
        ImGui::InputInt("##port", &g_port);
        text_muted("Wi-Fi to a real modem. Settings are locked on this path. Plain TCP, the modem's own voice port.");
    } else if (g_transport == 2) {
        field("station, or empty for the first##f", g_find, sizeof g_find);
        text_muted("Looks for five seconds for a modem advertising voice on the LAN. The phone must be on that Wi-Fi.");
    } else {
        ImGui::TextWrapped("USB host. The V4 speaks CDC-ACM (the Linux ttyACM port). The V3 is a CP210x bridge, not CDC-ACM, and is opened with that chip's line setup. Plug the modem in, then connect, and allow the system USB dialog.");
    }
    ImGui::TextWrapped("tier cap SF, 0 for none, else 5 to 12");
    ImGui::SetNextItemWidth(-1);
    ImGui::InputInt("##cap", &g_cap);
    ImGui::TextWrapped("answer incoming calls (bench)");
    ImGui::Checkbox("##auto", &g_auto);
    if (!g_err.empty()) ImGui::TextWrapped("%s", g_err.c_str());
    if (button("CONNECT", true)) start_session();
    text_muted("Data stays on this phone: %s", g_data.c_str());
}

void apply_extras() {
    if (g_extras || !g_app) return;
    g_extras = true;
    std::string m = jni_extra(g_app, "modem");
    if (m == "tcp") g_transport = 1;
    else if (m == "find") g_transport = 2;
    else if (m == "usb") g_transport = 3;
    else if (m == "sim") g_transport = 0;
    auto take = [](const char *key, char *buf, int cap) {
        std::string v = jni_extra(g_app, key);
        if (!v.empty()) std::snprintf(buf, (size_t)cap, "%s", v.c_str());
    };
    take("name", g_name, sizeof g_name);
    take("label", g_label, sizeof g_label);
    take("bind", g_bind, sizeof g_bind);
    take("peer", g_peer, sizeof g_peer);
    take("host", g_host, sizeof g_host);
    take("station", g_station, sizeof g_station);
    take("find", g_find, sizeof g_find);
    std::string port = jni_extra(g_app, "port");
    if (!port.empty()) g_port = std::atoi(port.c_str());
    std::string snr = jni_extra(g_app, "snr");
    if (!snr.empty()) g_snr = std::atoi(snr.c_str());
    if (jni_extra(g_app, "autoconnect") == "1") start_session();
}

}  // namespace

void ui_init_style(android_app *app, float) {
    g_app = app;
    if (app) g_data = jni_files(app) + "/voice";
    ImGuiStyle &st = ImGui::GetStyle();
    st.WindowRounding = 0;
    st.FrameRounding = 4;
    auto c = [](float r, float g, float b, float a = 1.f) { return ImVec4(r, g, b, a); };
    ImVec4 *col = st.Colors;
    col[ImGuiCol_WindowBg] = c(0.055f, 0.059f, 0.067f);
    col[ImGuiCol_ChildBg] = c(0.055f, 0.059f, 0.067f);
    col[ImGuiCol_FrameBg] = c(0.086f, 0.094f, 0.106f);
    col[ImGuiCol_Button] = c(0.086f, 0.094f, 0.106f);
    col[ImGuiCol_ButtonHovered] = c(0.18f, 0.24f, 0.36f);
    col[ImGuiCol_ButtonActive] = c(0.48f, 0.64f, 0.97f);
    col[ImGuiCol_Header] = c(0.48f, 0.64f, 0.97f, 0.45f);
    col[ImGuiCol_HeaderHovered] = c(0.48f, 0.64f, 0.97f, 0.65f);
    col[ImGuiCol_Text] = c(0.91f, 0.92f, 0.93f);
    g_style_ready = false;
    g_style_applied = 0;
}

void ui_frame(android_app *app) {
    g_app = app;
    apply_extras();
    if (g_data.empty()) g_data = jni_files(app) + "/voice";
    if (g_session) {
        lfd3k_status(g_session, &g_st);
        refresh_text();
        maybe_audio();
        bool want = g_st.in_call != 0;
        if (want != g_keep) {
            jni_keep_screen(app, want);
            g_keep = want;
        }
        if (g_st.name[0] && std::strcmp(g_st.name, g_my_name) != 0) {
            std::snprintf(g_my_name, sizeof g_my_name, "%s", g_st.name);
            std::snprintf(g_my_label, sizeof g_my_label, "%s", g_st.label);
        }
    }
    ImGuiIO &io = ImGui::GetIO();
    float x = g_mx.left;
    float y = g_mx.top;
    float w = io.DisplaySize.x - g_mx.left - g_mx.right;
    float h = io.DisplaySize.y - g_mx.top - g_mx.bottom;
    if (w < 1.f) {
        x = 0;
        w = io.DisplaySize.x;
    }
    if (h < 1.f) {
        y = 0;
        h = io.DisplaySize.y;
    }
    ImGui::SetNextWindowPos(ImVec2(x, y), ImGuiCond_Always);
    ImGui::SetNextWindowSize(ImVec2(w, h), ImGuiCond_Always);
    ImGui::Begin("lfd3k", nullptr,
                 ImGuiWindowFlags_NoDecoration | ImGuiWindowFlags_NoMove | ImGuiWindowFlags_NoSavedSettings |
                     ImGuiWindowFlags_NoBringToFrontOnFocus | ImGuiWindowFlags_NoResize);
    push_wrap();
    if (g_session) draw_connected();
    else draw_connect();
    keyboard();
    draw_footer();
    ImGui::PopTextWrapPos();
    ImGui::End();
}

void ui_shutdown(android_app *app) {
    g_app = app;
    stop_session();
}

void ui_set_metrics(float dpi, float font_scale, float left, float top, float right, float bottom) {
    if (dpi >= 120.f && dpi < 2000.f) g_mx.dpi = dpi;
    if (font_scale >= 0.5f && font_scale <= 3.f) g_mx.font = font_scale;
    g_mx.left = std::max(0.f, left);
    g_mx.top = std::max(0.f, top);
    g_mx.right = std::max(0.f, right);
    g_mx.bottom = std::max(0.f, bottom);
}

float ui_style_scale() { return dp_scale(); }

float ui_font_px() { return 16.f * dp_scale(); }

void ui_apply_style_scale() {
    if (!g_style_ready) {
        g_style_base = ImGui::GetStyle();
        g_style_ready = true;
        g_style_applied = 0;
    }
    float s = dp_scale();
    if (std::fabs(g_style_applied - s) < 0.02f) return;
    ImGui::GetStyle() = g_style_base;
    ImGui::GetStyle().ScaleAllSizes(s);
    g_style_applied = s;
}

float ui_preview(int screen) {
    g_session = nullptr;
    g_st = {};
    g_err.clear();
    if (g_data.empty()) g_data = "files/voice";
    ImGuiIO &io = ImGui::GetIO();
    float x = g_mx.left;
    float y = g_mx.top;
    float w = io.DisplaySize.x - g_mx.left - g_mx.right;
    float h = io.DisplaySize.y - g_mx.top - g_mx.bottom;
    if (w < 1.f) {
        x = 0;
        w = io.DisplaySize.x;
    }
    if (h < 1.f) {
        y = 0;
        h = io.DisplaySize.y;
    }
    ImGui::SetNextWindowPos(ImVec2(x, y), ImGuiCond_Always);
    ImGui::SetNextWindowSize(ImVec2(w, h), ImGuiCond_Always);
    ImGui::Begin("lfd3k", nullptr,
                 ImGuiWindowFlags_NoDecoration | ImGuiWindowFlags_NoMove | ImGuiWindowFlags_NoSavedSettings |
                     ImGuiWindowFlags_NoBringToFrontOnFocus | ImGuiWindowFlags_NoResize);
    push_wrap();
    if (screen <= 0) {
        draw_connect();
        draw_footer();
        ImGui::PopTextWrapPos();
        float over = ImGui::GetScrollMaxX();
        ImGui::End();
        return over;
    }
    g_session = reinterpret_cast<Lfd3kSession *>(1);
    std::snprintf(g_st.name, sizeof g_st.name, "REV");
    std::snprintf(g_st.label, sizeof g_st.label, "PHONE");
    std::snprintf(g_st.station, sizeof g_st.station, "PHONEMODEM");
    std::snprintf(g_st.link, sizeof g_st.link, "SIM 127.0.0.1:4211");
    std::snprintf(g_st.bind_addr, sizeof g_st.bind_addr, "0.0.0.0:4211");
    std::snprintf(g_st.peer_addr, sizeof g_st.peer_addr, "127.0.0.1:4210");
    g_st.ready = 1;
    g_st.sounds = 1;
    g_st.dir_count = 2;
    std::snprintf(g_st.dir[0].name, sizeof g_st.dir[0].name, "ANN");
    std::snprintf(g_st.dir[0].label, sizeof g_st.dir[0].label, "PC");
    std::snprintf(g_st.dir[0].hint, sizeof g_st.dir[0].hint, "CALL");
    g_st.dir[0].sf = 7;
    g_st.dir[0].voice = 1;
    std::snprintf(g_st.dir[1].name, sizeof g_st.dir[1].name, "AXISMODEM-E4B0-LONG");
    std::snprintf(g_st.dir[1].hint, sizeof g_st.dir[1].hint, "PTT");
    g_st.dir[1].sf = 12;
    g_st.dir[1].voice = 1;
    g_st.scope_count = 8;
    for (int i = 0; i < 8; i++) {
        g_st.scope[i].dbm = (int8_t)(-90 + i * 4);
        g_st.scope[i].tx = i % 3 == 0;
    }
    g_st.rssi = -74;
    if (screen == 2) g_screen = 1;
    else if (screen >= 3) g_screen = 2;
    else g_screen = 0;
    g_tab = 0;
    draw_connected();
    keyboard();
    draw_footer();
    g_session = nullptr;
    ImGui::PopTextWrapPos();
    float over = ImGui::GetScrollMaxX();
    ImGui::End();
    return over;
}
