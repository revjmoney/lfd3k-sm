/* PC test stub of nvs_flash.h. */
#ifndef FAKE_NVS_FLASH_H
#define FAKE_NVS_FLASH_H
#include "esp_err.h"
#include "nvs.h"
esp_err_t nvs_flash_init(void);
esp_err_t nvs_flash_erase(void);
esp_err_t nvs_flash_init_partition(const char *part);
esp_err_t nvs_flash_erase_partition(const char *part);
esp_err_t nvs_flash_deinit_partition(const char *part);
#endif
