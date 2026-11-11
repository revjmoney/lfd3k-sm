/* PC test stub of esp_ota_ops.h. */
#ifndef FAKE_ESP_OTA_OPS_H
#define FAKE_ESP_OTA_OPS_H
#include "esp_err.h"
#include "esp_partition.h"
const esp_partition_t *esp_ota_get_running_partition(void);
const esp_partition_t *esp_ota_get_boot_partition(void);
esp_err_t esp_ota_set_boot_partition(const esp_partition_t *p);
#endif
