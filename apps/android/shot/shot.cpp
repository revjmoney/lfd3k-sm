/* Headless layout check for the phone UI. Draws the same screens as the
 * device, at a chosen pixel size and density, and fails if a row is wider
 * than the window. Run under xvfb. Not part of the APK. */

#include "ui.h"

#include "imgui.h"
#include "imgui_impl_opengl3.h"
#include "lfd3k_abi.h"
#include "usb.h"

#include <GLFW/glfw3.h>

#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_FRAMEBUFFER_COMPLETE 0x8CD5
#define GL_RGBA8 0x8058

typedef void (*PFN_glGenFramebuffers)(int, unsigned *);
typedef void (*PFN_glDeleteFramebuffers)(int, const unsigned *);
typedef void (*PFN_glBindFramebuffer)(unsigned, unsigned);
typedef void (*PFN_glFramebufferTexture2D)(unsigned, unsigned, unsigned, unsigned, int);
typedef unsigned (*PFN_glCheckFramebufferStatus)(unsigned);

PFN_glGenFramebuffers p_glGenFramebuffers;
PFN_glDeleteFramebuffers p_glDeleteFramebuffers;
PFN_glBindFramebuffer p_glBindFramebuffer;
PFN_glFramebufferTexture2D p_glFramebufferTexture2D;
PFN_glCheckFramebufferStatus p_glCheckFramebufferStatus;

void load_gl() {
    p_glGenFramebuffers = (PFN_glGenFramebuffers)glfwGetProcAddress("glGenFramebuffers");
    p_glDeleteFramebuffers = (PFN_glDeleteFramebuffers)glfwGetProcAddress("glDeleteFramebuffers");
    p_glBindFramebuffer = (PFN_glBindFramebuffer)glfwGetProcAddress("glBindFramebuffer");
    p_glFramebufferTexture2D = (PFN_glFramebufferTexture2D)glfwGetProcAddress("glFramebufferTexture2D");
    p_glCheckFramebufferStatus = (PFN_glCheckFramebufferStatus)glfwGetProcAddress("glCheckFramebufferStatus");
}

#include <zlib.h>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

bool audio_start(Lfd3kSession *, std::string *) { return false; }
void audio_stop() {}
bool audio_mic_open() { return false; }
const char *audio_note() { return "preview"; }

int usb_prepare(android_app *, UsbInfo *, std::string *err) {
    if (err) *err = "preview";
    return -1;
}
void usb_setup_line(const UsbInfo &, std::string *) {}
void usb_pump_start(Lfd3kSession *, const UsbInfo &) {}
void usb_pump_stop() {}
void usb_close(android_app *) {}

int jni_usb_prepare(android_app *, int *, std::string *) { return -1; }
void jni_usb_close(android_app *) {}
void jni_multicast(android_app *, bool) {}
bool jni_audio_granted(android_app *) { return true; }
void jni_request_audio(android_app *) {}
std::string jni_files(android_app *) { return "files"; }
std::string jni_extra(android_app *, const char *) { return {}; }
void jni_keep_screen(android_app *, bool) {}

extern "C" {

size_t lfd3k_status_bytes(void) { return sizeof(Lfd3kStatus); }
Lfd3kSession *lfd3k_start(int, const char *, const char *, const char *, const char *, const char *, const char *,
                          const char *, int, const char *, const char *, int, int, int, int, char *, size_t) {
    return nullptr;
}
void lfd3k_shutdown(Lfd3kSession *) {}
void lfd3k_stop(Lfd3kSession *) {}
void lfd3k_status(Lfd3kSession *, Lfd3kStatus *) {}
int lfd3k_text(Lfd3kSession *, int, char *buf, size_t n) {
    if (buf && n) buf[0] = 0;
    return 0;
}
void lfd3k_mic(Lfd3kSession *, const int16_t *, size_t) {}
size_t lfd3k_speaker(Lfd3kSession *, int16_t *, size_t) { return 0; }
void lfd3k_usb_rx(Lfd3kSession *, const uint8_t *, size_t) {}
size_t lfd3k_usb_tx(Lfd3kSession *, uint8_t *, size_t) { return 0; }
void lfd3k_usb_tx_unget(Lfd3kSession *, const uint8_t *, size_t) {}
void lfd3k_call(Lfd3kSession *, const char *, const char *) {}
void lfd3k_answer(Lfd3kSession *) {}
void lfd3k_decline(Lfd3kSession *) {}
void lfd3k_hangup(Lfd3kSession *) {}
void lfd3k_talk(Lfd3kSession *, int) {}
void lfd3k_words(Lfd3kSession *) {}
void lfd3k_anyway(Lfd3kSession *) {}
void lfd3k_mute(Lfd3kSession *, int) {}
void lfd3k_link_offer(Lfd3kSession *) {}
void lfd3k_link_join(Lfd3kSession *, const char *) {}
void lfd3k_cancel_link(Lfd3kSession *) {}
void lfd3k_set(Lfd3kSession *, const char *) {}
void lfd3k_save(Lfd3kSession *) {}
void lfd3k_discard(Lfd3kSession *) {}
void lfd3k_scan(Lfd3kSession *) {}
void lfd3k_rename(Lfd3kSession *, const char *, const char *) {}
void lfd3k_sounds(Lfd3kSession *, int) {}
void lfd3k_remove_mine(Lfd3kSession *, const uint8_t *) {}
void lfd3k_remove_contact(Lfd3kSession *, const char *, const uint8_t *) {}

}  // extern "C"

namespace {

uint32_t png_crc(const uint8_t *p, size_t n) { return (uint32_t)crc32(0, p, (uInt)n); }

void be32(std::vector<uint8_t> &o, uint32_t v) {
    o.push_back((uint8_t)(v >> 24));
    o.push_back((uint8_t)(v >> 16));
    o.push_back((uint8_t)(v >> 8));
    o.push_back((uint8_t)v);
}

void chunk(std::vector<uint8_t> &o, const char *tag, const uint8_t *data, size_t n) {
    be32(o, (uint32_t)n);
    size_t at = o.size();
    o.insert(o.end(), tag, tag + 4);
    o.insert(o.end(), data, data + n);
    be32(o, png_crc(o.data() + at, 4 + n));
}

bool write_png(const char *path, const uint8_t *rgba, int w, int h) {
    std::vector<uint8_t> raw;
    raw.reserve((size_t)(w * 4 + 1) * (size_t)h);
    for (int y = 0; y < h; y++) {
        raw.push_back(0);
        const uint8_t *row = rgba + (size_t)y * (size_t)w * 4;
        raw.insert(raw.end(), row, row + (size_t)w * 4);
    }
    uLongf bound = compressBound((uLong)raw.size());
    std::vector<uint8_t> comp(bound);
    if (compress2(comp.data(), &bound, raw.data(), (uLong)raw.size(), 6) != Z_OK) return false;
    std::vector<uint8_t> out;
    const uint8_t sig[8] = {137, 80, 78, 71, 13, 10, 26, 10};
    out.insert(out.end(), sig, sig + 8);
    uint8_t ihdr[13];
    ihdr[0] = (uint8_t)(w >> 24);
    ihdr[1] = (uint8_t)(w >> 16);
    ihdr[2] = (uint8_t)(w >> 8);
    ihdr[3] = (uint8_t)w;
    ihdr[4] = (uint8_t)(h >> 24);
    ihdr[5] = (uint8_t)(h >> 16);
    ihdr[6] = (uint8_t)(h >> 8);
    ihdr[7] = (uint8_t)h;
    ihdr[8] = 8;
    ihdr[9] = 6;
    ihdr[10] = 0;
    ihdr[11] = 0;
    ihdr[12] = 0;
    chunk(out, "IHDR", ihdr, 13);
    chunk(out, "IDAT", comp.data(), bound);
    chunk(out, "IEND", nullptr, 0);
    FILE *f = std::fopen(path, "wb");
    if (!f) return false;
    size_t n = std::fwrite(out.data(), 1, out.size(), f);
    std::fclose(f);
    return n == out.size();
}

bool load_font(const char *path, float px) {
    ImGuiIO &io = ImGui::GetIO();
    io.FontGlobalScale = 1.f;
    FILE *f = std::fopen(path, "rb");
    if (!f) {
        std::fprintf(stderr, "font missing: %s\n", path);
        return false;
    }
    std::fseek(f, 0, SEEK_END);
    long n = std::ftell(f);
    std::fseek(f, 0, SEEK_SET);
    if (n <= 0) {
        std::fclose(f);
        return false;
    }
    void *copy = IM_ALLOC((size_t)n);
    if (std::fread(copy, 1, (size_t)n, f) != (size_t)n) {
        std::fclose(f);
        IM_FREE(copy);
        return false;
    }
    std::fclose(f);
    ImFontConfig cfg;
    cfg.FontDataOwnedByAtlas = true;
    io.Fonts->AddFontFromMemoryTTF(copy, (int)n, px, &cfg);
    return true;
}

int visible_overflow(float margin) {
    ImDrawData *data = ImGui::GetDrawData();
    if (!data) return 0;
    ImVec2 disp = data->DisplaySize;
    int bad = 0;
    float worst = 0;
    for (int i = 0; i < data->CmdListsCount; i++) {
        const ImDrawList *list = data->CmdLists[i];
        for (int c = 0; c < list->CmdBuffer.Size; c++) {
            const ImDrawCmd &cmd = list->CmdBuffer[c];
            if (cmd.UserCallback) continue;
            ImVec4 clip = cmd.ClipRect;
            unsigned start = cmd.IdxOffset;
            unsigned end = start + cmd.ElemCount;
            if (end > (unsigned)list->IdxBuffer.Size) end = (unsigned)list->IdxBuffer.Size;
            for (unsigned k = start; k < end; k++) {
                ImDrawIdx idx = list->IdxBuffer[k];
                if (idx >= list->VtxBuffer.Size) continue;
                ImVec2 p = list->VtxBuffer[idx].pos;
                if (p.x < clip.x || p.y < clip.y || p.x > clip.z || p.y > clip.w) continue;
                float dx = 0;
                if (p.x < -margin) dx = -margin - p.x;
                if (p.x > disp.x + margin) dx = p.x - (disp.x + margin);
                float dy = 0;
                if (p.y < -margin) dy = -margin - p.y;
                if (p.y > disp.y + margin) dy = p.y - (disp.y + margin);
                if (dx > 0 || dy > 0) {
                    bad++;
                    if (dx > worst) worst = dx;
                    if (dy > worst) worst = dy;
                }
            }
        }
    }
    if (bad) std::fprintf(stderr, "visible vertices outside the display: %d, worst %.1f px\n", bad, worst);
    return bad;
}

struct Shot {
    const char *name;
    int w;
    int h;
    float dpi;
    float font;
    int screen;
    float left, top, right, bottom;
};

int render_one(GLFWwindow *win, const Shot &s, const char *font, const char *dir) {
    glfwSetWindowSize(win, s.w, s.h);
    glfwMakeContextCurrent(win);
    load_gl();
    GLuint fbo = 0, color = 0;
    p_glGenFramebuffers(1, &fbo);
    glGenTextures(1, &color);
    glBindTexture(GL_TEXTURE_2D, color);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, s.w, s.h, 0, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    p_glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    p_glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, color, 0);
    if (p_glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        std::fprintf(stderr, "%s framebuffer incomplete\n", s.name);
        p_glBindFramebuffer(GL_FRAMEBUFFER, 0);
        glDeleteTextures(1, &color);
        p_glDeleteFramebuffers(1, &fbo);
        return 1;
    }
    IMGUI_CHECKVERSION();
    ImGui::CreateContext();
    ImGui::StyleColorsDark();
    ui_init_style(nullptr, 1.f);
    ui_set_metrics(s.dpi, s.font, s.left, s.top, s.right, s.bottom);
    ui_apply_style_scale();
    if (!load_font(font, ui_font_px())) {
        ImGui::DestroyContext();
        return 1;
    }
    ImGui_ImplOpenGL3_Init("#version 330");
    float over = 0;
    for (int frame = 0; frame < 4; frame++) {
        glfwPollEvents();
        ImGuiIO &io = ImGui::GetIO();
        io.DisplaySize = ImVec2((float)s.w, (float)s.h);
        io.DisplayFramebufferScale = ImVec2(1.f, 1.f);
        io.DeltaTime = 1.f / 60.f;
        ImGui_ImplOpenGL3_NewFrame();
        ImGui::NewFrame();
        over = ui_preview(s.screen);
        ImGui::Render();
        glViewport(0, 0, s.w, s.h);
        glClearColor(0.055f, 0.059f, 0.067f, 1.f);
        glClear(GL_COLOR_BUFFER_BIT);
        ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());
    }
    int bad = visible_overflow(8.f);
    std::vector<uint8_t> rgba((size_t)s.w * (size_t)s.h * 4);
    std::vector<uint8_t> flip((size_t)s.w * (size_t)s.h * 4);
    glReadPixels(0, 0, s.w, s.h, GL_RGBA, GL_UNSIGNED_BYTE, rgba.data());
    for (int y = 0; y < s.h; y++) {
        std::memcpy(flip.data() + (size_t)(s.h - 1 - y) * (size_t)s.w * 4,
                    rgba.data() + (size_t)y * (size_t)s.w * 4, (size_t)s.w * 4);
    }
    std::string path = std::string(dir) + "/" + s.name;
    bool wrote = write_png(path.c_str(), flip.data(), s.w, s.h);
    std::fprintf(stderr, "%s  scroll-x %.2f  outside %d  %s\n", s.name, over, bad, wrote ? "wrote" : "write failed");
    p_glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDeleteTextures(1, &color);
    p_glDeleteFramebuffers(1, &fbo);
    ImGui_ImplOpenGL3_Shutdown();
    ImGui::DestroyContext();
    if (!wrote) return 1;
    if (over > 4.f || bad > 0) return 1;
    return 0;
}

}  // namespace

int main(int argc, char **argv) {
    const char *font = "apps/pc/assets/fonts/IBMPlexMono-Regular.ttf";
    const char *dir = "docs/voice-android";
    if (argc > 1) font = argv[1];
    if (argc > 2) dir = argv[2];
    if (!glfwInit()) {
        std::fprintf(stderr, "glfwInit failed\n");
        return 1;
    }
    glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
    glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 3);
    glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);
    glfwWindowHint(GLFW_VISIBLE, GLFW_FALSE);
    glfwWindowHint(GLFW_SCALE_TO_MONITOR, GLFW_FALSE);
    GLFWwindow *win = glfwCreateWindow(360, 800, "lfd3k-shot", nullptr, nullptr);
    if (!win) {
        std::fprintf(stderr, "no GL context\n");
        glfwTerminate();
        return 1;
    }
    const Shot shots[] = {
        {"connect-360x800.png", 360, 800, 160.f, 1.f, 0, 0, 0, 0, 0},
        {"calls-360x800.png", 360, 800, 160.f, 1.f, 1, 0, 0, 0, 0},
        {"directory-360x800.png", 360, 800, 160.f, 1.f, 2, 0, 0, 0, 0},
        {"settings-360x800.png", 360, 800, 160.f, 1.f, 3, 0, 0, 0, 0},
        {"connect-412x915.png", 412, 915, 160.f, 1.f, 0, 0, 0, 0, 0},
        {"calls-412x915.png", 412, 915, 160.f, 1.f, 1, 0, 0, 0, 0},
        {"calls-1080x2400-3x.png", 1080, 2400, 480.f, 1.f, 1, 0, 0, 0, 0},
        {"calls-landscape-800x360.png", 800, 360, 160.f, 1.f, 1, 0, 0, 0, 0},
        {"calls-landscape-2400x1080-3x.png", 2400, 1080, 480.f, 1.f, 1, 0, 0, 0, 0},
        {"calls-1080x2400-3x-insets.png", 1080, 2400, 480.f, 1.f, 1, 0, 96, 0, 72},
    };
    int rc = 0;
    for (const Shot &s : shots) {
        rc |= render_one(win, s, font, dir);
    }
    glfwDestroyWindow(win);
    glfwTerminate();
    return rc ? 1 : 0;
}
