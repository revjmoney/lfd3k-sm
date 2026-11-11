/* LFD3K-SM multimode picker (factory app).
 * Every boot that is not a launched mode lands here (see bootloader override):
 * 5 s AXIS BBS logo, PRG opens the mode list, otherwise the last mode starts.
 * Starting a mode: esp_ota_set_boot_partition, launch token in RTC FAST,
 * esp_restart. The bootloader consumes the token and boots otadata. */
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include "board.h"
#include "choose.h"
#include "driver/gpio.h"
#include "esp_app_desc.h"
#include "esp_app_format.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_partition.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "picker_draw.h"
#include "picker_menu.h"
#include "ssd1306.h"

static const char *TAG = "picker";

#define PICKER_NVS_PART "picker_nvs"
#define PICKER_NVS_NS   "picker"
#define PICKER_NVS_KEY  "last"

#define POLL_MS     10
#define FRAME_MS    40
#define DEBOUNCE_N  3    /* 3 equal samples, 30 ms */
#define LAUNCH_MS   450  /* how long the STARTING card stays up */

static uint8_t s_fb[PK_FB_BYTES];
static bool s_oled;

static uint32_t now_ms(void)
{
    return (uint32_t)(esp_timer_get_time() / 1000);
}

static void show(void)
{
    if (s_oled && ssd1306_flush(s_fb) != ESP_OK) {
        s_oled = false; /* keep going blind; the modes still start */
    }
}

/* ---- slots ---- */

static const esp_partition_t *mode_part(int mode)
{
    int ota = pk_mode_ota(mode);
    if (ota < 0) {
        return NULL;
    }
    return esp_partition_find_first(ESP_PARTITION_TYPE_APP,
                                     (esp_partition_subtype_t)(ESP_PARTITION_SUBTYPE_APP_OTA_MIN + ota), NULL);
}

/* An app image header (magic 0xE9, S3 chip id, sane segment count) followed by
 * the app descriptor magic at offset 0x20. Cheap: otadata alone is not trusted. */
static bool slot_has_image(const esp_partition_t *p)
{
    if (!p) {
        return false;
    }
    uint8_t buf[sizeof(esp_image_header_t) + sizeof(esp_image_segment_header_t) + 4];
    if (esp_partition_read(p, 0, buf, sizeof(buf)) != ESP_OK) {
        return false;
    }
    esp_image_header_t hdr;
    memcpy(&hdr, buf, sizeof(hdr));
    if (hdr.magic != ESP_IMAGE_HEADER_MAGIC || hdr.segment_count == 0 || hdr.segment_count > ESP_IMAGE_MAX_SEGMENTS) {
        return false;
    }
    if (hdr.chip_id != ESP_CHIP_ID_ESP32S3) {
        return false;
    }
    uint32_t desc_magic;
    memcpy(&desc_magic, buf + sizeof(esp_image_header_t) + sizeof(esp_image_segment_header_t), 4);
    return desc_magic == ESP_APP_DESC_MAGIC_WORD;
}

/* ---- last mode ---- */

static bool s_nvs_ok;

static int load_last(void)
{
    esp_err_t e = nvs_flash_init_partition(PICKER_NVS_PART);
    if (e == ESP_ERR_NVS_NO_FREE_PAGES || e == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        /* only ever the picker's own partition; the shared "nvs" is never touched */
        nvs_flash_erase_partition(PICKER_NVS_PART);
        e = nvs_flash_init_partition(PICKER_NVS_PART);
    }
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "picker_nvs: %s", esp_err_to_name(e));
        return -1;
    }
    s_nvs_ok = true;
    nvs_handle_t h;
    if (nvs_open_from_partition(PICKER_NVS_PART, PICKER_NVS_NS, NVS_READONLY, &h) != ESP_OK) {
        return -1; /* first ever boot: namespace not there yet */
    }
    uint8_t v = 0xFF;
    e = nvs_get_u8(h, PICKER_NVS_KEY, &v);
    nvs_close(h);
    return (e == ESP_OK && v < PK_MODES) ? v : -1;
}

static void save_last(int mode, int stored)
{
    if (!s_nvs_ok || mode == stored) {
        return; /* no write when nothing changed */
    }
    nvs_handle_t h;
    if (nvs_open_from_partition(PICKER_NVS_PART, PICKER_NVS_NS, NVS_READWRITE, &h) != ESP_OK) {
        return;
    }
    if (nvs_set_u8(h, PICKER_NVS_KEY, (uint8_t)mode) == ESP_OK) {
        nvs_commit(h);
    }
    nvs_close(h);
}

/* ---- launch ---- */

static void write_token(int ota)
{
    boot_token_t t;
    lfmp_token_make(&t, (uint8_t)ota);
    uint32_t w[3];
    memcpy(w, &t, sizeof(w));
    volatile uint32_t *rtc = (volatile uint32_t *)LFMP_TOKEN_ADDR;
    rtc[0] = w[0];
    rtc[1] = w[1];
    rtc[2] = w[2];
}

/* Returns only on failure. */
static void launch(int mode, int stored)
{
    const esp_partition_t *p = mode_part(mode);
    if (!p || !slot_has_image(p)) {
        ESP_LOGW(TAG, "%s: no image", pk_mode_name(mode));
        return;
    }
    const esp_partition_t *cur = esp_ota_get_boot_partition();
    if (cur != p) { /* skip the otadata write when it already points there */
        esp_err_t e = esp_ota_set_boot_partition(p); /* verifies the whole image */
        if (e != ESP_OK) {
            ESP_LOGW(TAG, "%s: set boot failed: %s", pk_mode_name(mode), esp_err_to_name(e));
            return;
        }
    }
    save_last(mode, stored);
    ESP_LOGI(TAG, "starting %s from %s", pk_mode_name(mode), p->label);
    write_token(pk_mode_ota(mode));
    esp_restart();
}

void app_main(void)
{
    /* Vext on (OLED power), then the panel. */
    gpio_reset_pin(PIN_VEXT);
    gpio_set_direction(PIN_VEXT, GPIO_MODE_OUTPUT);
    gpio_set_level(PIN_VEXT, 0);
    vTaskDelay(pdMS_TO_TICKS(30));
    s_oled = ssd1306_init() == ESP_OK;
    if (!s_oled) {
        ESP_LOGW(TAG, "OLED init failed; running without a display");
    }

    /* PRG as a plain input after boot. */
    gpio_reset_pin(PIN_PRG);
    gpio_set_direction(PIN_PRG, GPIO_MODE_INPUT);
    gpio_set_pull_mode(PIN_PRG, GPIO_PULLUP_ONLY);

    int stored = load_last();
    bool valid[PK_MODES];
    for (int i = 0; i < PK_MODES; i++) {
        valid[i] = slot_has_image(mode_part(i));
        ESP_LOGI(TAG, "%-12s ota_%d %s", pk_mode_name(i), pk_mode_ota(i), valid[i] ? "image" : "NO IMAGE");
    }

    bool level = gpio_get_level(PIN_PRG) == 0;
    bool raw_last = level;
    int same = 0;
    uint32_t t0 = now_ms();
    pk_state_t st;
    pk_init(&st, stored, valid, level, t0);
    uint32_t last_frame = 0;
    bool first = true;

    for (;;) {
        uint32_t now = now_ms();

        /* debounce: the level changes after DEBOUNCE_N equal samples */
        bool raw = gpio_get_level(PIN_PRG) == 0;
        same = (raw == raw_last) ? same + 1 : 1;
        raw_last = raw;
        if (same >= DEBOUNCE_N) {
            level = raw;
        }
        pk_update(&st, level, now);

        if (st.phase == PK_LAUNCH) {
            int mode = st.launch;
            for (uint32_t t = 0; t < LAUNCH_MS; t += FRAME_MS) {
                pk_draw_launch(s_fb, mode, t);
                show();
                vTaskDelay(pdMS_TO_TICKS(FRAME_MS));
            }
            pk_draw_launch(s_fb, mode, LAUNCH_MS + 200);
            show();
            launch(mode, stored); /* returns only on failure */
            pk_launch_failed(&st, now_ms());
            continue;
        }

        if (first || now - last_frame >= FRAME_MS) {
            first = false;
            last_frame = now;
            if (st.phase == PK_LOGO) {
                pk_draw_logo(s_fb, now - st.phase_ms);
            } else {
                pk_draw_list(s_fb, &st, now);
            }
            show();
        }
        vTaskDelay(pdMS_TO_TICKS(POLL_MS));
    }
}
