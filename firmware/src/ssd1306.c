#include "ssd1306.h"
#include "board.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define I2C_HZ      400000
#define I2C_TIMEOUT 100 /* ms */

static i2c_master_bus_handle_t s_bus;
static i2c_master_dev_handle_t s_dev;

static esp_err_t cmds(const uint8_t *c, size_t n)
{
    uint8_t buf[40];
    if (n + 1 > sizeof(buf)) {
        return ESP_ERR_INVALID_SIZE;
    }
    buf[0] = 0x00; /* control byte: commands */
    for (size_t i = 0; i < n; i++) {
        buf[i + 1] = c[i];
    }
    return i2c_master_transmit(s_dev, buf, n + 1, I2C_TIMEOUT);
}

esp_err_t ssd1306_init(void)
{
    gpio_reset_pin(PIN_OLED_RST);
    gpio_set_level(PIN_OLED_RST, 1);
    gpio_set_direction(PIN_OLED_RST, GPIO_MODE_OUTPUT);
    vTaskDelay(pdMS_TO_TICKS(5));
    gpio_set_level(PIN_OLED_RST, 0);
    vTaskDelay(pdMS_TO_TICKS(10));
    gpio_set_level(PIN_OLED_RST, 1);
    vTaskDelay(pdMS_TO_TICKS(10));

    i2c_master_bus_config_t bus = {
        .i2c_port = I2C_NUM_0,
        .sda_io_num = PIN_OLED_SDA,
        .scl_io_num = PIN_OLED_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    esp_err_t e = i2c_new_master_bus(&bus, &s_bus);
    if (e != ESP_OK) {
        return e;
    }
    i2c_device_config_t dev = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = OLED_I2C_ADDR,
        .scl_speed_hz = I2C_HZ,
    };
    e = i2c_master_bus_add_device(s_bus, &dev, &s_dev);
    if (e != ESP_OK) {
        return e;
    }

    static const uint8_t init[] = {
        0xAE,       /* display off */
        0xD5, 0x80, /* clock divide */
        0xA8, 0x3F, /* multiplex 64 */
        0xD3, 0x00, /* display offset */
        0x40,       /* start line 0 */
        0x8D, 0x14, /* charge pump on */
        0x20, 0x00, /* horizontal addressing */
        0xA1,       /* segment remap: column 127 at SEG0 */
        0xC8,       /* COM scan reversed (upright, connector side as the V4 ships) */
        0xDA, 0x12, /* COM pins alternative */
        0x81, 0xCF, /* contrast */
        0xD9, 0xF1, /* precharge */
        0xDB, 0x40, /* VCOMH */
        0xA4,       /* follow RAM */
        0xA6,       /* normal (not inverted) */
        0xAF,       /* display on */
    };
    e = cmds(init, sizeof(init));
    if (e != ESP_OK) {
        return e;
    }
    return ESP_OK;
}

esp_err_t ssd1306_flush(const uint8_t fb[1024])
{
    static const uint8_t home[] = {0x21, 0x00, 0x7F, 0x22, 0x00, 0x07};
    esp_err_t e = cmds(home, sizeof(home));
    if (e != ESP_OK) {
        return e;
    }
    uint8_t buf[1 + 128];
    buf[0] = 0x40; /* control byte: data */
    for (int page = 0; page < 8; page++) {
        for (int i = 0; i < 128; i++) {
            buf[1 + i] = fb[page * 128 + i];
        }
        e = i2c_master_transmit(s_dev, buf, sizeof(buf), I2C_TIMEOUT);
        if (e != ESP_OK) {
            return e;
        }
    }
    return ESP_OK;
}
