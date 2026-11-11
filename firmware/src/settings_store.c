#include "settings_store.h"
#include <string.h>
#include "bootloader_random.h"
#include "esp_log.h"
#include "esp_partition.h"
#include "esp_random.h"
#include "nvs.h"
#include "nvs_flash.h"
#include "sdkconfig.h"

/* In the multimode layout the modem never initialises the shared "nvs", so
 * the Wi-Fi driver must not need it (esp_wifi_init would fail at every boot). */
#if CONFIG_ESP_WIFI_NVS_ENABLED
#error "Wi-Fi NVS must stay off: the modem never inits the shared nvs (sdkconfig.defaults)"
#endif

static const char *TAG = "settings";
#define NS      "lfd3k"
#define KEY     "settings"
#define KEY_DIR "dir"

/* The modem's own NVS partition in the multimode layout
 * (multimode/partitions-multimode.csv). Without it (the single app table)
 * everything stays in the default "nvs", as before. */
#define OWN_PART    "lfd3k_nvs"
#define SHARED_PART "nvs"

static const char *s_part = SHARED_PART;

/* Blob buffer for the one time copy; the blobs are at most 1024 bytes. */
#define MIGRATE_CAP 1024
static uint8_t s_mig[MIGRATE_CAP];

/* Initialise one partition, erasing and re-initialising it on
 * NO_FREE_PAGES/NEW_VERSION. Only ever called for the partition the modem
 * owns: "lfd3k_nvs", or "nvs" when the board has no "lfd3k_nvs". */
static esp_err_t init_own(const char *part)
{
    esp_err_t e = nvs_flash_init_partition(part);
    if (e == ESP_ERR_NVS_NO_FREE_PAGES || e == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        e = nvs_flash_erase_partition(part);
        if (e == ESP_OK) {
            e = nvs_flash_init_partition(part);
        }
    }
    return e;
}

static bool has_key(nvs_handle_t h, const char *key)
{
    size_t n = 0;
    return nvs_get_blob(h, key, NULL, &n) == ESP_OK;
}

/* First boot under the multimode layout: copy an old lfd3k/settings and
 * lfd3k/dir blob from the shared "nvs" into our own partition, then erase
 * only those two keys there. Runs only while our partition has no settings.
 * The shared partition belongs to the other modes: it is never erased or
 * re-initialised here, and if it does not init cleanly nothing is copied. */
static void migrate_from_shared(void)
{
    nvs_handle_t own;
    if (nvs_open_from_partition(OWN_PART, NS, NVS_READWRITE, &own) != ESP_OK) {
        return;
    }
    bool have = has_key(own, KEY);
    if (have) {
        nvs_close(own);
        return;
    }
    if (nvs_flash_init_partition(SHARED_PART) != ESP_OK) {
        nvs_close(own); /* empty, full or newer format: leave it alone */
        return;
    }
    /* Read only: a read-write open would create the namespace there. */
    nvs_handle_t old;
    if (nvs_open_from_partition(SHARED_PART, NS, NVS_READONLY, &old) != ESP_OK) {
        nvs_close(own); /* no lfd3k namespace there: nothing to move */
        nvs_flash_deinit_partition(SHARED_PART);
        return;
    }
    const char *keys[2] = {KEY, KEY_DIR};
    bool copied[2] = {false, false};
    bool failed = false;
    for (int i = 0; i < 2; i++) {
        size_t n = sizeof(s_mig);
        esp_err_t e = nvs_get_blob(old, keys[i], s_mig, &n);
        if (e == ESP_ERR_NVS_NOT_FOUND) {
            continue;
        }
        if (e != ESP_OK) {
            ESP_LOGW(TAG, "old %s not readable: %s", keys[i], esp_err_to_name(e));
            continue; /* left where it is */
        }
        e = nvs_set_blob(own, keys[i], s_mig, n);
        if (e == ESP_OK) {
            copied[i] = true;
        } else {
            failed = true;
            ESP_LOGW(TAG, "copy of %s failed: %s", keys[i], esp_err_to_name(e));
        }
    }
    memset(s_mig, 0, sizeof(s_mig));
    nvs_close(old);
    /* Erase the old keys only once the copies are committed. The namespace
     * exists there (it held them), so the read-write open creates nothing. */
    if ((copied[0] || copied[1]) && nvs_commit(own) == ESP_OK && !failed &&
        nvs_open_from_partition(SHARED_PART, NS, NVS_READWRITE, &old) == ESP_OK) {
        for (int i = 0; i < 2; i++) {
            if (copied[i]) {
                nvs_erase_key(old, keys[i]);
            }
        }
        nvs_commit(old);
        nvs_close(old);
        ESP_LOGI(TAG, "moved old settings%s from nvs to %s", copied[1] ? " and directory" : "", OWN_PART);
    }
    nvs_close(own);
    nvs_flash_deinit_partition(SHARED_PART);
}

bool settings_store_init(void)
{
    bool own = esp_partition_find_first(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_DATA_NVS, OWN_PART) != NULL;
    s_part = own ? OWN_PART : SHARED_PART;
    esp_err_t e = init_own(s_part);
    if (e != ESP_OK) {
        ESP_LOGE(TAG, "NVS %s init failed: %s; running on defaults, no saves", s_part, esp_err_to_name(e));
        return false;
    }
    if (own) {
        migrate_from_shared();
    }
    ESP_LOGI(TAG, "settings in NVS partition %s", s_part);
    return true;
}

static bool load_key(const char *key, uint8_t *buf, size_t cap, size_t *len)
{
    nvs_handle_t h;
    if (nvs_open_from_partition(s_part, NS, NVS_READONLY, &h) != ESP_OK) {
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
    esp_err_t e = nvs_open_from_partition(s_part, NS, NVS_READWRITE, &h);
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
