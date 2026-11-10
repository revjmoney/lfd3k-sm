/* C HAL for the Rust core: SX1262 on SPI2, GC1109 front end, time, panic note. */
#include <string.h>
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "esp_attr.h"
#include "esp_rom_sys.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "board.h"
#include "hal_radio.h"
#include "lfd3k.h"

#define PANIC_MAGIC 0x4C46334Bu /* "LF3K" */

typedef struct {
    uint32_t magic;
    uint16_t len;
    uint8_t note[64];
} panic_slot_t;

static RTC_NOINIT_ATTR panic_slot_t s_panic;

static spi_device_handle_t s_spi;
static TaskHandle_t s_notify_task;

static void IRAM_ATTR dio1_isr(void *arg)
{
    (void)arg;
    BaseType_t woken = pdFALSE;
    if (s_notify_task) {
        vTaskNotifyGiveFromISR(s_notify_task, &woken);
    }
    if (woken) {
        portYIELD_FROM_ISR();
    }
}

static void out_pin(int pin, int level)
{
    gpio_reset_pin(pin);
    gpio_set_direction(pin, GPIO_MODE_OUTPUT);
    gpio_set_level(pin, level);
}

void hal_radio_init(TaskHandle_t modem_task)
{
    /* Power order: Vext on (LOW), front end LDO, front end enable, PA mode off. */
    out_pin(PIN_VEXT, 0);
    out_pin(PIN_FEM_POWER, 1);
    out_pin(PIN_FEM_EN, 1);
    out_pin(PIN_FEM_PA, 0);

    out_pin(PIN_LORA_RST, 1);
    gpio_reset_pin(PIN_LORA_BUSY);
    gpio_set_direction(PIN_LORA_BUSY, GPIO_MODE_INPUT);

    spi_bus_config_t bus = {
        .mosi_io_num = PIN_LORA_MOSI,
        .miso_io_num = PIN_LORA_MISO,
        .sclk_io_num = PIN_LORA_SCK,
        .quadwp_io_num = -1,
        .quadhd_io_num = -1,
        .max_transfer_sz = 512,
    };
    ESP_ERROR_CHECK(spi_bus_initialize(SPI2_HOST, &bus, SPI_DMA_CH_AUTO));
    spi_device_interface_config_t dev = {
        .clock_speed_hz = 8 * 1000 * 1000,
        .mode = 0,
        .spics_io_num = PIN_LORA_NSS, /* driver holds NSS low for the whole transaction */
        .queue_size = 1,
    };
    ESP_ERROR_CHECK(spi_bus_add_device(SPI2_HOST, &dev, &s_spi));

    s_notify_task = modem_task;
    gpio_config_t dio1 = {
        .pin_bit_mask = 1ULL << PIN_LORA_DIO1,
        .mode = GPIO_MODE_INPUT,
        .intr_type = GPIO_INTR_POSEDGE,
    };
    ESP_ERROR_CHECK(gpio_config(&dio1));
    ESP_ERROR_CHECK(gpio_install_isr_service(0));
    ESP_ERROR_CHECK(gpio_isr_handler_add(PIN_LORA_DIO1, dio1_isr, NULL));
}

size_t hal_radio_take_panic_note(uint8_t *out)
{
    size_t n = 0;
    if (s_panic.magic == PANIC_MAGIC && s_panic.len > 0 && s_panic.len <= sizeof(s_panic.note)) {
        n = s_panic.len;
        memcpy(out, s_panic.note, n);
    }
    s_panic.magic = 0;
    return n;
}

void lfd3k_hal_spi_xfer(const uint8_t *tx, uint8_t *rx, size_t len)
{
    if (len == 0 || tx == NULL) {
        return;
    }
    spi_transaction_t t = {
        .length = len * 8,
        .tx_buffer = tx,
        .rx_buffer = rx, /* NULL is fine: the driver then skips the read */
    };
    (void)spi_device_polling_transmit(s_spi, &t);
}

bool lfd3k_hal_busy(void)
{
    return gpio_get_level(PIN_LORA_BUSY) != 0;
}

void lfd3k_hal_reset(void)
{
    gpio_set_level(PIN_LORA_RST, 0);
    esp_rom_delay_us(1000);
    gpio_set_level(PIN_LORA_RST, 1);
}

void lfd3k_hal_fem_tx(bool on)
{
    gpio_set_level(PIN_FEM_PA, on ? 1 : 0);
}

uint64_t lfd3k_hal_now_us(void)
{
    return (uint64_t)esp_timer_get_time();
}

void lfd3k_hal_panic(const uint8_t *note, size_t len)
{
    if (note == NULL) {
        len = 0;
    }
    if (len > sizeof(s_panic.note)) {
        len = sizeof(s_panic.note);
    }
    memcpy(s_panic.note, note, len);
    s_panic.len = (uint16_t)len;
    s_panic.magic = PANIC_MAGIC;
    esp_restart();
    for (;;) {
    }
}
