/* PC test stub of esp_partition.h. */
#ifndef FAKE_ESP_PARTITION_H
#define FAKE_ESP_PARTITION_H
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

typedef enum { ESP_PARTITION_TYPE_APP = 0, ESP_PARTITION_TYPE_DATA = 1 } esp_partition_type_t;
typedef enum {
    ESP_PARTITION_SUBTYPE_APP_FACTORY = 0,
    ESP_PARTITION_SUBTYPE_APP_OTA_MIN = 0x10,
    ESP_PARTITION_SUBTYPE_DATA_OTA = 0,
    ESP_PARTITION_SUBTYPE_DATA_NVS = 2,
} esp_partition_subtype_t;

typedef struct {
    esp_partition_type_t type;
    esp_partition_subtype_t subtype;
    uint32_t address;
    uint32_t size;
    char label[17];
    uint8_t head[64]; /* test only: what esp_partition_read returns from offset 0 */
} esp_partition_t;

const esp_partition_t *esp_partition_find_first(esp_partition_type_t type, esp_partition_subtype_t subtype,
                                                const char *label);
esp_err_t esp_partition_read(const esp_partition_t *p, size_t offset, void *dst, size_t size);
#endif
