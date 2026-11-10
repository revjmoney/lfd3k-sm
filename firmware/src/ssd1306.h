/* SSD1306 128x64 over the IDF 5.4 I2C master driver. */
#ifndef SSD1306_H
#define SSD1306_H
#include <stdint.h>
#include "esp_err.h"

/* Pulses RST (GPIO21), brings up I2C (SDA 17, SCL 18, 400 kHz, 0x3C) and the
 * controller. Vext must already be on. */
esp_err_t ssd1306_init(void);

/* Sends a full frame: 1024 bytes, 8 pages of 128 column bytes, bit 0 = top. */
esp_err_t ssd1306_flush(const uint8_t fb[1024]);

#endif
