/* AT+MODE on the device (see mode_switch.h and multimode/README.md).
 * The token format, address and CRC come from the bootloader's own choose.h
 * and choose.c, compiled in from multimode/bootloader, never copied. */
#include "mode_switch.h"
#include <string.h>
#include "choose.h"
#include "esp_app_desc.h"
#include "esp_app_format.h"
#include "esp_image_format.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_partition.h"
#include "esp_system.h"
#include "freertos/FreeRTOS.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "sdkconfig.h"

/* The launch token is written to a fixed RTC FAST address; nothing in the
 * modem may own that memory (multimode/README.md, the same guards as the
 * picker). */
#if CONFIG_ESP_SYSTEM_ALLOW_RTC_FAST_MEM_AS_HEAP
#error "modem: RTC FAST heap must stay off, it could hand out the launch token address"
#endif
#if CONFIG_BOOTLOADER_RESERVE_RTC_MEM || CONFIG_BOOTLOADER_CUSTOM_RESERVE_RTC || CONFIG_BOOTLOADER_SKIP_VALIDATE_IN_DEEP_SLEEP
#error "modem: the IDF RTC reserve area must stay off (it moves into the top of RTC FAST)"
#endif
#if CONFIG_ESP32S3_RTCDATA_IN_FAST_MEM
#error "modem: RTC data must not be placed in RTC FAST"
#endif

static const char *TAG = "mode";

/* The picker's own NVS (multimode/picker/src/main.c): partition, namespace,
 * key "last" = u8 menu index, the same order as MODE_*. */
#define PICKER_NVS_PART "picker_nvs"
#define PICKER_NVS_NS   "picker"
#define PICKER_NVS_KEY  "last"

#define MODES 4

/* ota index of each mode slot, in mode order: LFD3K-SM, SIMIAN RADAR, RNODE, MESH. */
static const int s_ota[MODES] = {1, 2, 3, 0};

static const esp_partition_t *mode_part(int mode)
{
    if (mode < 0 || mode >= MODES) {
        return NULL;
    }
    return esp_partition_find_first(ESP_PARTITION_TYPE_APP,
                                     (esp_partition_subtype_t)(ESP_PARTITION_SUBTYPE_APP_OTA_MIN + s_ota[mode]), NULL);
}

/* An app image header (magic 0xE9, S3 chip id, sane segment count) followed
 * by the app descriptor magic at offset 0x20. */
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

bool mode_switch_available(void)
{
    if (!esp_partition_find_first(ESP_PARTITION_TYPE_APP, ESP_PARTITION_SUBTYPE_APP_FACTORY, NULL) ||
        !esp_partition_find_first(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_DATA_NVS, "lfd3k_nvs")) {
        return false; /* the single app table: no picker, no bootloader override */
    }
    for (int i = 0; i < MODES; i++) {
        if (!mode_part(i)) {
            return false;
        }
    }
    /* The token names the slot otadata selects; the modem must be the one in ota_1. */
    return esp_ota_get_running_partition() == mode_part(MODE_MODEM);
}

uint8_t mode_switch_slots(void)
{
    uint8_t mask = 0;
    for (int i = 0; i < MODES; i++) {
        const esp_partition_t *p = mode_part(i);
        bool ok = (i == MODE_MODEM && p == esp_ota_get_running_partition()) || slot_has_image(p);
        ESP_LOGI(TAG, "ota_%d %s", s_ota[i], ok ? "image" : "NO IMAGE");
        if (ok) {
            mask |= (uint8_t)(1u << i);
        }
    }
    return mask;
}

/* Ruling P5-2: the mode AT+MODE chose becomes the picker's last mode, so a
 * power cycle or RST starts it again. picker_nvs belongs to the picker: it is
 * never erased here, and if it does not init cleanly nothing is written. A
 * failure only logs; the switch goes on. */
static void record_picker_last(int mode)
{
    esp_err_t e = nvs_flash_init_partition(PICKER_NVS_PART);
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "picker_nvs: %s; last mode not recorded", esp_err_to_name(e));
        return;
    }
    nvs_handle_t h;
    e = nvs_open_from_partition(PICKER_NVS_PART, PICKER_NVS_NS, NVS_READWRITE, &h);
    if (e == ESP_OK) {
        uint8_t cur = 0xFF;
        if (nvs_get_u8(h, PICKER_NVS_KEY, &cur) != ESP_OK || cur != (uint8_t)mode) {
            e = nvs_set_u8(h, PICKER_NVS_KEY, (uint8_t)mode);
            if (e == ESP_OK) {
                e = nvs_commit(h);
            }
        }
        nvs_close(h);
    }
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "picker last mode not recorded: %s", esp_err_to_name(e));
    }
    nvs_flash_deinit_partition(PICKER_NVS_PART);
}

bool mode_switch_prepare(int mode)
{
    if (mode == MODE_PICKER) {
        return true; /* no otadata change, no token: the bootloader shows the picker */
    }
    const esp_partition_t *p = mode_part(mode);
    if (!p || !slot_has_image(p)) {
        return false;
    }
    if (esp_ota_get_boot_partition() == p) {
        /* otadata already points there: verify anyway (header, segments,
         * SHA-256), as the picker does, so a broken slot never reaches the
         * bootloader. */
        esp_image_metadata_t md;
        const esp_partition_pos_t pos = {.offset = p->address, .size = p->size};
        if (esp_image_verify(ESP_IMAGE_VERIFY_SILENT, &pos, &md) != ESP_OK) {
            ESP_LOGW(TAG, "ota_%d does not verify", s_ota[mode]);
            return false;
        }
        record_picker_last(mode);
        return true;
    }
    esp_err_t e = esp_ota_set_boot_partition(p); /* verifies the whole image */
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "ota_%d: set boot failed: %s", s_ota[mode], esp_err_to_name(e));
        return false;
    }
    record_picker_last(mode);
    return true;
}

void mode_switch_restart(int mode)
{
    static portMUX_TYPE mux = portMUX_INITIALIZER_UNLOCKED;
    ESP_LOGI(TAG, "restart into %s", mode == MODE_PICKER ? "the picker" : "a mode");
    if (mode >= 0 && mode < MODES) {
        boot_token_t t;
        lfmp_token_make(&t, (uint8_t)s_ota[mode]);
        uint32_t w[3];
        memcpy(w, &t, sizeof(w));
        volatile uint32_t *rtc = (volatile uint32_t *)LFMP_TOKEN_ADDR;
        /* Interrupts off for the write, the last thing before esp_restart.
         * RTC FAST is not heap here (guards above), so nothing the shutdown
         * handlers do can land on it. */
        portENTER_CRITICAL(&mux);
        rtc[0] = w[0];
        rtc[1] = w[1];
        rtc[2] = w[2];
        portEXIT_CRITICAL(&mux);
    }
    esp_restart();
}
