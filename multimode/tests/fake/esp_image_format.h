/* PC test stub of ESP-IDF esp_image_format.h / esp_app_format.h: the real
 * image header and segment header layouts (IDF 5.4), the few constants
 * mode_switch.c uses, and a fake esp_image_verify driven by the test. */
#ifndef FAKE_ESP_IMAGE_FORMAT_H
#define FAKE_ESP_IMAGE_FORMAT_H
#include <stdint.h>
#include "esp_err.h"

#define ESP_IMAGE_HEADER_MAGIC 0xE9
#define ESP_IMAGE_MAX_SEGMENTS 16
#define ESP_CHIP_ID_ESP32S3    0x0009

typedef struct {
    uint8_t magic;
    uint8_t segment_count;
    uint8_t spi_mode;
    uint8_t spi_speed : 4;
    uint8_t spi_size : 4;
    uint32_t entry_addr;
    uint8_t wp_pin;
    uint8_t spi_pin_drv[3];
    uint16_t chip_id;
    uint8_t min_chip_rev;
    uint16_t min_chip_rev_full;
    uint16_t max_chip_rev_full;
    uint8_t reserved[4];
    uint8_t hash_appended;
} __attribute__((packed)) esp_image_header_t;

typedef struct {
    uint32_t load_addr;
    uint32_t data_len;
} esp_image_segment_header_t;

typedef struct {
    uint32_t offset;
    uint32_t size;
} esp_partition_pos_t;

typedef struct {
    uint32_t start_addr;
    uint32_t image_len;
} esp_image_metadata_t;

typedef enum { ESP_IMAGE_VERIFY, ESP_IMAGE_VERIFY_SILENT } esp_image_load_mode_t;

esp_err_t esp_image_verify(esp_image_load_mode_t mode, const esp_partition_pos_t *part,
                           esp_image_metadata_t *data);

#endif
