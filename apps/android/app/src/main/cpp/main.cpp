#include "ui.h"

#include "imgui.h"
#include "imgui_impl_android.h"
#include "imgui_impl_opengl3.h"
#include "jni_host.h"

#include <EGL/egl.h>
#include <GLES3/gl3.h>
#include <android/asset_manager.h>
#include <android/configuration.h>
#include <android/log.h>
#include <android_native_app_glue.h>

#include <cmath>
#include <cstring>
#include <vector>

#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, "lfd3k", __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "lfd3k", __VA_ARGS__)

namespace {

struct Egl {
    EGLDisplay display = EGL_NO_DISPLAY;
    EGLSurface surface = EGL_NO_SURFACE;
    EGLContext context = EGL_NO_CONTEXT;
    EGLConfig config = nullptr;
    bool imgui = false;
    float font_px = 0;
};

Egl g;
android_app *g_app = nullptr;

void destroy_surface() {
    if (g.display == EGL_NO_DISPLAY) return;
    eglMakeCurrent(g.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    if (g.surface != EGL_NO_SURFACE) eglDestroySurface(g.display, g.surface);
    g.surface = EGL_NO_SURFACE;
}

bool init_display(ANativeWindow *window) {
    if (g.display == EGL_NO_DISPLAY) {
        g.display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
        eglInitialize(g.display, nullptr, nullptr);
        const EGLint attribs[] = {
            EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
            EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
            EGL_BLUE_SIZE, 8,
            EGL_GREEN_SIZE, 8,
            EGL_RED_SIZE, 8,
            EGL_NONE};
        EGLint n = 0;
        eglChooseConfig(g.display, attribs, &g.config, 1, &n);
        if (n < 1) {
            LOGE("no EGL config");
            return false;
        }
        const EGLint ctx[] = {EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE};
        g.context = eglCreateContext(g.display, g.config, EGL_NO_CONTEXT, ctx);
    }
    if (g.surface == EGL_NO_SURFACE) {
        g.surface = eglCreateWindowSurface(g.display, g.config, window, nullptr);
    }
    if (g.surface == EGL_NO_SURFACE || !eglMakeCurrent(g.display, g.surface, g.surface, g.context)) {
        LOGE("EGL surface failed");
        return false;
    }
    return true;
}

void load_font(android_app *app, float px) {
    ImGuiIO &io = ImGui::GetIO();
    io.FontGlobalScale = 1.f;
    AAsset *asset = AAssetManager_open(app->activity->assetManager, "IBMPlexMono-Regular.ttf", AASSET_MODE_BUFFER);
    if (!asset) {
        io.Fonts->AddFontDefault();
        io.FontGlobalScale = px / 13.f;
        return;
    }
    size_t n = (size_t)AAsset_getLength(asset);
    std::vector<unsigned char> bytes(n);
    AAsset_read(asset, bytes.data(), n);
    AAsset_close(asset);
    ImFontConfig cfg;
    cfg.FontDataOwnedByAtlas = true;
    void *copy = IM_ALLOC(n);
    memcpy(copy, bytes.data(), n);
    io.Fonts->AddFontFromMemoryTTF(copy, (int)n, px, &cfg);
}

void refresh_metrics(android_app *app) {
    float dpi = 0;
    if (app->config) {
        int d = AConfiguration_getDensity(app->config);
        if (d >= 120 && d < 0xfffe) dpi = (float)d;
    }
    if (dpi < 120.f) dpi = (float)jni_density_dpi(app);
    float font = jni_font_scale(app);
    int insets[4] = {};
    jni_insets(app, insets);
    ui_set_metrics(dpi, font, (float)insets[0], (float)insets[1], (float)insets[2], (float)insets[3]);
}

void apply_font(android_app *app) {
    float px = ui_font_px();
    if (std::fabs(g.font_px - px) < 0.5f && ImGui::GetIO().Fonts->Fonts.Size > 0) return;
    if (g.imgui) ImGui_ImplOpenGL3_DestroyFontsTexture();
    ImGui::GetIO().Fonts->Clear();
    load_font(app, px);
    g.font_px = px;
}

void init_imgui(android_app *app) {
    if (g.imgui) return;
    IMGUI_CHECKVERSION();
    ImGui::CreateContext();
    ImGui::StyleColorsDark();
    ui_init_style(app, 1.f);
    refresh_metrics(app);
    ui_apply_style_scale();
    apply_font(app);
    ImGui_ImplAndroid_Init(app->window);
    ImGui_ImplOpenGL3_Init("#version 300 es");
    g.imgui = true;
    LOGI("imgui ready, dpi scale %.2f, font %.1f px", ui_style_scale(), ui_font_px());
}

void apply_surface_size() {
    if (g.display == EGL_NO_DISPLAY || g.surface == EGL_NO_SURFACE) return;
    EGLint sw = 0, sh = 0;
    eglQuerySurface(g.display, g.surface, EGL_WIDTH, &sw);
    eglQuerySurface(g.display, g.surface, EGL_HEIGHT, &sh);
    ImGuiIO &io = ImGui::GetIO();
    if (sw > 0 && sh > 0) io.DisplaySize = ImVec2((float)sw, (float)sh);
    io.DisplayFramebufferScale = ImVec2(1.f, 1.f);
}

void shutdown_all() {
    ui_shutdown(g_app);
    if (g.imgui) {
        ImGui_ImplOpenGL3_Shutdown();
        ImGui_ImplAndroid_Shutdown();
        ImGui::DestroyContext();
        g.imgui = false;
    }
    destroy_surface();
    if (g.display != EGL_NO_DISPLAY) {
        if (g.context != EGL_NO_CONTEXT) eglDestroyContext(g.display, g.context);
        eglTerminate(g.display);
    }
    g = {};
}

int32_t on_input(android_app *, AInputEvent *event) {
    return ImGui_ImplAndroid_HandleInputEvent(event);
}

void on_cmd(android_app *app, int32_t cmd) {
    switch (cmd) {
    case APP_CMD_INIT_WINDOW:
        if (app->window && init_display(app->window)) {
            if (g.imgui) {
                ImGui_ImplAndroid_Shutdown();
                ImGui_ImplAndroid_Init(app->window);
            } else {
                init_imgui(app);
            }
        }
        break;
    case APP_CMD_TERM_WINDOW:
        destroy_surface();
        break;
    case APP_CMD_CONFIG_CHANGED:
    case APP_CMD_WINDOW_RESIZED:
    case APP_CMD_CONTENT_RECT_CHANGED:
        jni_request_insets(app);
        break;
    case APP_CMD_DESTROY:
        shutdown_all();
        break;
    default:
        break;
    }
}

}  // namespace

extern "C" void android_main(android_app *app) {
    g_app = app;
    app->onAppCmd = on_cmd;
    app->onInputEvent = on_input;
    while (true) {
        int events = 0;
        android_poll_source *source = nullptr;
        while (ALooper_pollOnce(g.imgui && g.surface != EGL_NO_SURFACE ? 0 : -1, nullptr, &events,
                                reinterpret_cast<void **>(&source)) >= 0) {
            if (source) source->process(app, source);
            if (app->destroyRequested) {
                shutdown_all();
                return;
            }
        }
        if (!g.imgui || g.surface == EGL_NO_SURFACE) continue;
        refresh_metrics(app);
        ui_apply_style_scale();
        apply_font(app);
        ImGui_ImplOpenGL3_NewFrame();
        ImGui_ImplAndroid_NewFrame();
        apply_surface_size();
        ImGui::NewFrame();
        ui_frame(app);
        ImGui::Render();
        glViewport(0, 0, (int)ImGui::GetIO().DisplaySize.x, (int)ImGui::GetIO().DisplaySize.y);
        glClearColor(0.055f, 0.059f, 0.067f, 1.f);
        glClear(GL_COLOR_BUFFER_BIT);
        ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());
        eglSwapBuffers(g.display, g.surface);
    }
}
