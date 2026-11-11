#include "settings_store.h"
#include "bootloader_random.h"
#include "esp_log.h"
#include "esp_random.h"
#include "nvs.h"
#include "nvs_flash.h"

static const char *TAG = "settings";
#define NS      "lfd3k"
#define KEY     "settings"
#define KEY_DIR "dir" /* the directory blob; a later phase moves both to their own partition */

bool settings_store_init(void)
{
    esp_err_t e = nvs_flash_init();
    if (e == ESP_ERR_NVS_NO_FREE_PAGES || e == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        e = nvs_flash_erase();
        if (e == ESP_OK) {
            e = nvs_flash_init();
        }
    }
    if (e != ESP_OK) {
        ESP_LOGE(TAG, "NVS init failed: %s; running on defaults, no saves", esp_err_to_name(e));
        return false;
    }
    return true;
}

static bool load_key(const char *key, uint8_t *buf, size_t cap, size_t *len)
{
    nvs_handle_t h;
    if (nvs_open(NS, NVS_READONLY, &h) != ESP_OK) {
        return false;
    }
    size_t n = cap;
    esp_err_t e = nvs_get_blob(h, key, buf, &n);
    nvs_close(h);
    if (e != ESP_OK) {
        return false;
    }
    *len = n;
    return true;
}

static bool save_key(const char *key, const uint8_t *buf, size_t len)
{
    nvs_handle_t h;
    esp_err_t e = nvs_open(NS, NVS_READWRITE, &h);
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "open failed: %s", esp_err_to_name(e));
        return false;
    }
    e = nvs_set_blob(h, key, buf, len);
    if (e == ESP_OK) {
        e = nvs_commit(h);
    }
    nvs_close(h);
    if (e != ESP_OK) {
        ESP_LOGW(TAG, "save of %s failed: %s", key, esp_err_to_name(e));
        return false;
    }
    return true;
}

bool settings_store_load(uint8_t *buf, size_t cap, size_t *len)
{
    return load_key(KEY, buf, cap, len);
}

bool settings_store_save(const uint8_t *buf, size_t len)
{
    return save_key(KEY, buf, len);
}

bool settings_store_load_dir(uint8_t *buf, size_t cap, size_t *len)
{
    return load_key(KEY_DIR, buf, cap, len);
}

bool settings_store_save_dir(const uint8_t *buf, size_t len)
{
    return save_key(KEY_DIR, buf, len);
}

void settings_store_gen_ap_pass(char out[13])
{
    static const char alpha[32] = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    /* Wi-Fi is not running yet, so esp_random alone is not a true RNG on the
     * S3; switch the SAR ADC noise source on for the draw. */
    bootloader_random_enable();
    size_t n = 0;
    while (n < 12) {
        uint32_t r = esp_random();
        for (int i = 0; i < 4 && n < 12; i++) {
            uint8_t b = (uint8_t)(r >> (8 * i));
            out[n++] = alpha[b & 31]; /* 32 divides 256: the low 5 bits are uniform */
        }
    }
    out[12] = '\0';
    bootloader_random_disable();
}
