/* Front panel task. Draws panel_render() output to the OLED at up to 20 fps. */
#include <string.h>
#include "board.h"
#include "driver/gpio.h"
#include "esp_log.h"
#include "esp_partition.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "panel.h"
#include "panel_render.h"
#include "picker_draw.h"
#include "ssd1306.h"

static const char *TAG = "panel";

#define FRAME_MS    50   /* 20 fps cap */
#define SPLASH_MS   1000
#define HOLD_MS     80   /* SD and RD stay lit this long so short bursts are visible */
#define BOOT_WAIT   50   /* ms after Vext before touching the OLED */
#define RETRY_MS    5000 /* OLED init retry period */
#define SNAP_WAIT   5    /* ms the modem task will wait for the frame mutex */

static portMUX_TYPE s_pub_lock = portMUX_INITIALIZER_UNLOCKED;
static rf_trace_t s_pub_trace; /* zero count: plain breathing line */
static panel_net_t s_pub_net;  /* all zero: no Wi-Fi rows */
static panel_info_t s_pub_info = PANEL_INFO_INIT; /* idle show facts: none yet */
static bool s_time_synced;
static lfd3k_status_t s_pub = {.lights = 0, .state = 0, .tier = -1, .rssi = 0, .snr_x4 = 0, .radio_ok = 1};

static SemaphoreHandle_t s_frame_lock;
static uint8_t s_done[PANEL_FB_BYTES];      /* last fully drawn frame, guarded by s_frame_lock */
static uint8_t s_last_snap[PANEL_FB_BYTES]; /* modem task only: last good snapshot */

static char s_name[17] = "LFD3K-SM"; /* guarded by s_pub_lock; the panel task copies it */

void panel_set_name(const char *name)
{
    if (name && name[0]) {
        char nm[17];
        strncpy(nm, name, 16);
        nm[16] = 0;
        taskENTER_CRITICAL(&s_pub_lock);
        memcpy(s_name, nm, sizeof(s_name));
        taskEXIT_CRITICAL(&s_pub_lock);
    }
}

void panel_publish(const lfd3k_status_t *s)
{
    taskENTER_CRITICAL(&s_pub_lock);
    s_pub = *s;
    taskEXIT_CRITICAL(&s_pub_lock);
}

void panel_publish_trace(const rf_trace_t *t)
{
    taskENTER_CRITICAL(&s_pub_lock);
    s_pub_trace = *t;
    taskEXIT_CRITICAL(&s_pub_lock);
}

void panel_publish_net(const panel_net_t *n)
{
    taskENTER_CRITICAL(&s_pub_lock);
    s_pub_net = *n;
    taskEXIT_CRITICAL(&s_pub_lock);
}

void panel_publish_info(const panel_info_t *info)
{
    taskENTER_CRITICAL(&s_pub_lock);
    s_pub_info = *info;
    taskEXIT_CRITICAL(&s_pub_lock);
}

void panel_note_time_synced(void)
{
    taskENTER_CRITICAL(&s_pub_lock);
    s_time_synced = true;
    taskEXIT_CRITICAL(&s_pub_lock);
}

bool panel_time_synced(void)
{
    taskENTER_CRITICAL(&s_pub_lock);
    bool v = s_time_synced;
    taskEXIT_CRITICAL(&s_pub_lock);
    return v;
}

void panel_snapshot(uint8_t out[1024])
{
    if (s_frame_lock && xSemaphoreTake(s_frame_lock, pdMS_TO_TICKS(SNAP_WAIT)) == pdTRUE) {
        memcpy(s_last_snap, s_done, PANEL_FB_BYTES);
        xSemaphoreGive(s_frame_lock);
    }
    memcpy(out, s_last_snap, PANEL_FB_BYTES);
}

/* Replace the published frame. Holds the lock only for a 1 KB memcpy. */
static void commit_frame(const uint8_t *fb)
{
    xSemaphoreTake(s_frame_lock, portMAX_DELAY);
    memcpy(s_done, fb, PANEL_FB_BYTES);
    xSemaphoreGive(s_frame_lock);
}

static void panel_task(void *arg)
{
    (void)arg;
    static uint8_t fb[PANEL_FB_BYTES];
    static uint8_t shown[PANEL_FB_BYTES];

    vTaskDelay(pdMS_TO_TICKS(BOOT_WAIT));

    gpio_reset_pin(PIN_LED);
    gpio_set_level(PIN_LED, 0);
    gpio_set_direction(PIN_LED, GPIO_MODE_OUTPUT);

    bool oled_ok = ssd1306_init() == ESP_OK;
    if (!oled_ok) {
        ESP_LOGW(TAG, "OLED init failed; retrying every %d s, snapshots keep working", RETRY_MS / 1000);
    }
    uint32_t next_try_ms = (uint32_t)(esp_timer_get_time() / 1000) + RETRY_MS;

    /* With the picker on the board (multimode) the boot logo has already
     * played there, so a short splash. Alone on the board (the single app
     * layout, no factory app) the modem plays the AXIS boot logo itself. */
    if (!esp_partition_find_first(ESP_PARTITION_TYPE_APP, ESP_PARTITION_SUBTYPE_APP_FACTORY, NULL)) {
        _Static_assert(PK_FB_BYTES == PANEL_FB_BYTES, "picker and panel frames differ");
        for (uint32_t t = 0; t < PK_LOGO_MS; t += FRAME_MS) {
            pk_draw_logo(fb, t);
            commit_frame(fb);
            if (oled_ok) {
                ssd1306_flush(fb);
            }
            vTaskDelay(pdMS_TO_TICKS(FRAME_MS));
        }
        if (oled_ok) {
            memcpy(shown, fb, sizeof(shown));
        }
    }
    panel_render_splash(fb);
    commit_frame(fb);
    if (oled_ok) {
        ssd1306_flush(fb);
        memcpy(shown, fb, sizeof(shown));
    }
    vTaskDelay(pdMS_TO_TICKS(SPLASH_MS));

    uint32_t hold_until[2] = {0, 0}; /* SD, RD */
    TickType_t last = xTaskGetTickCount();
    int fails = 0;
    for (;;) {
        vTaskDelayUntil(&last, pdMS_TO_TICKS(FRAME_MS));
        uint32_t now_ms = (uint32_t)(esp_timer_get_time() / 1000);

        lfd3k_status_t st;
        rf_trace_t trace;
        panel_net_t net;
        static panel_info_t info; /* off the 4 KB stack */
        char name[17];
        taskENTER_CRITICAL(&s_pub_lock);
        st = s_pub;
        trace = s_pub_trace;
        net = s_pub_net;
        info = s_pub_info;
        memcpy(name, s_name, sizeof(name));
        taskEXIT_CRITICAL(&s_pub_lock);

        for (int i = 0; i < 2; i++) {
            if (st.lights & (1u << (2 + i))) {
                hold_until[i] = now_ms + HOLD_MS;
            } else if ((int32_t)(hold_until[i] - now_ms) > 0) {
                st.lights |= (uint8_t)(1u << (2 + i));
            }
        }
        gpio_set_level(PIN_LED, (st.lights & ((1u << 2) | (1u << 3))) ? 1 : 0);

        if (!oled_ok && (int32_t)(now_ms - next_try_ms) >= 0) {
            next_try_ms = now_ms + RETRY_MS;
            if (ssd1306_init() == ESP_OK) {
                oled_ok = true;
                memset(shown, 0xFF, sizeof(shown)); /* force a full flush */
                ESP_LOGI(TAG, "OLED up");
            }
        }

        panel_render_set_trace(&trace);
        panel_render_set_net(&net);
        panel_render_set_info(&info);
        memset(&net, 0, sizeof(net)); /* the password never lingers on this stack */
        panel_render(&st, name, now_ms, fb);
        commit_frame(fb);

        if (oled_ok && memcmp(fb, shown, sizeof(shown)) != 0) {
            if (ssd1306_flush(fb) == ESP_OK) {
                memcpy(shown, fb, sizeof(shown));
                fails = 0;
            } else if (++fails == 1) {
                ESP_LOGW(TAG, "OLED flush failed");
            }
            if (fails >= 20) { /* about a second of errors: drop the bus and re-init later */
                ssd1306_deinit();
                oled_ok = false;
                fails = 0;
                next_try_ms = now_ms + RETRY_MS;
            }
        }
    }
}

void panel_start(void)
{
    s_frame_lock = xSemaphoreCreateMutex();
    xTaskCreatePinnedToCore(panel_task, "panel", 4096, NULL, 2, NULL, 0);
}
