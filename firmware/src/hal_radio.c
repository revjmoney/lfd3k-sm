/* C HAL for the Rust core: SX1262 on SPI2, GC1109 front end, time, panic note. */
#include <string.h>
#include "driver/gpio.h"
#include "driver/spi_master.h"
#include "esp_attr.h"
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "board.h"
#include "hal_radio.h"
#include "lfd3k.h"

#define PANIC_MAGIC 0x4C46334Bu /* "LF3K" */
#define BOOT_MAGIC  0x424F4F54u /* "BOOT": quick-crash counter below is valid */
#define QUICK_MS    10000u      /* a panic this soon after boot counts as quick */
#define SAFE_AFTER  2u          /* this many quick panics in a row: safe mode */

typedef struct {
    uint32_t magic;
    uint16_t len;
    uint8_t note[64];
    uint32_t boot_magic;
    uint32_t up_ms; /* uptime when the last panic note was written */
    uint8_t quick;  /* quick panics in a row */
} panic_slot_t;

static RTC_NOINIT_ATTR panic_slot_t s_panic;

/* Safe mode: the board keeps panicking right after boot, so the radio and its
 * front end stay off. BUSY reads stuck high, the driver reports RADIO FAIL -1,
 * and AT, ATI (with the crash note) and AT&V still work over USB. */
static bool s_safe;

static bool reset_was_warm(void)
{
    esp_reset_reason_t why = esp_reset_reason();
    return why == ESP_RST_SW || why == ESP_RST_PANIC || why == ESP_RST_TASK_WDT ||
           why == ESP_RST_INT_WDT;
}

static bool note_valid(void)
{
    return reset_was_warm() && s_panic.magic == PANIC_MAGIC && s_panic.len > 0 &&
           s_panic.len <= sizeof(s_panic.note);
}

/* Runs once at boot, before anything powers the radio. */
static void boot_check(void)
{
    if (!reset_was_warm() || s_panic.boot_magic != BOOT_MAGIC) {
        s_panic.boot_magic = BOOT_MAGIC; /* cold boot: RTC RAM is garbage */
        s_panic.quick = 0;
    }
    if (note_valid() && s_panic.up_ms < QUICK_MS) {
        if (s_panic.quick < 255) {
            s_panic.quick++;
        }
    } else {
        s_panic.quick = 0;
    }
    s_safe = s_panic.quick >= SAFE_AFTER;
    if (s_safe) {
        ESP_LOGE("hal", "%u quick panics in a row: safe mode, radio off", (unsigned)s_panic.quick);
    }
}

#define SPI_MAX 258
static DMA_ATTR WORD_ALIGNED_ATTR uint8_t s_txb[260];
static DMA_ATTR WORD_ALIGNED_ATTR uint8_t s_rxb[260];

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

/* Level first, then output with both pulls off, so no pin glitches (gpio_reset_pin
 * would turn the pull-up on for a moment, which briefly lifts GPIO46 PA mode). */
static void out_pin(int pin, int level)
{
    gpio_set_level(pin, level);
    gpio_config_t c = {
        .pin_bit_mask = 1ULL << pin,
        .mode = GPIO_MODE_OUTPUT,
        .pull_up_en = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    gpio_config(&c);
    gpio_set_level(pin, level);
}

void hal_radio_init(TaskHandle_t modem_task)
{
    boot_check();

    /* Hold the SX1262 in reset before the front end gets power, so after a warm
     * reset in the middle of a transmit nothing goes out through the PA. The
     * driver's reset() releases it. PA mode off first, then power. */
    out_pin(PIN_LORA_RST, 0);
#if BOARD_HAS_FEM
    out_pin(PIN_FEM_PA, 0);
#endif
    out_pin(PIN_VEXT, 0); /* Vext on (LOW): OLED and GPS */
#if BOARD_HAS_FEM
    out_pin(PIN_FEM_POWER, s_safe ? 0 : 1);
    out_pin(PIN_FEM_EN, s_safe ? 0 : 1);
#endif

    gpio_config_t busy = {
        .pin_bit_mask = 1ULL << PIN_LORA_BUSY,
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    gpio_config(&busy);

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
    esp_err_t isr = gpio_install_isr_service(0);
    if (isr != ESP_OK && isr != ESP_ERR_INVALID_STATE) {
        ESP_ERROR_CHECK(isr);
    }
    ESP_ERROR_CHECK(gpio_isr_handler_add(PIN_LORA_DIO1, dio1_isr, NULL));
}

size_t hal_radio_take_panic_note(uint8_t *out)
{
    size_t n = 0;
    if (note_valid()) {
        n = s_panic.len;
        memcpy(out, s_panic.note, n);
    }
    s_panic.magic = 0;
    return n;
}

void lfd3k_hal_spi_xfer(const uint8_t *tx, uint8_t *rx, size_t len)
{
    if (len == 0 || tx == NULL || len > SPI_MAX) {
        if (rx != NULL && len > 0) {
            memset(rx, 0, len);
        }
        return;
    }
    memcpy(s_txb, tx, len);
    spi_transaction_t t = {
        .length = len * 8,
        .tx_buffer = s_txb,
        .rx_buffer = rx ? s_rxb : NULL,
    };
    esp_err_t e = spi_device_polling_transmit(s_spi, &t);
    if (e != ESP_OK) {
        ESP_LOGE("hal", "spi xfer err %d", (int)e);
        if (rx != NULL) {
            memset(rx, 0, len);
        }
        return;
    }
    if (rx != NULL) {
        memcpy(rx, s_rxb, len);
    }
}

bool lfd3k_hal_busy(void)
{
    if (s_safe) {
        return true; /* radio left off: the driver times out to RADIO FAIL -1 */
    }
    return gpio_get_level(PIN_LORA_BUSY) != 0;
}

void lfd3k_hal_reset(void)
{
    gpio_set_level(PIN_LORA_RST, 0);
    esp_rom_delay_us(1000);
    if (s_safe) {
        return; /* stays in reset */
    }
    gpio_set_level(PIN_LORA_RST, 1);
    /* Let the chip start its boot and raise BUSY before the first command, so
     * the driver's BUSY wait sees the real boot instead of a stale low. */
    esp_rom_delay_us(1000);
}

void lfd3k_hal_fem_tx(bool on)
{
#if BOARD_HAS_FEM
    gpio_set_level(PIN_FEM_PA, (on && !s_safe) ? 1 : 0);
#else
    (void)on; /* no front end: DIO2 switches the antenna by itself */
#endif
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
    if (len) {
        memcpy(s_panic.note, note, len);
    }
    s_panic.len = (uint16_t)len;
    s_panic.up_ms = (uint32_t)(esp_timer_get_time() / 1000);
    s_panic.magic = PANIC_MAGIC;
    esp_restart();
    for (;;) {
    }
}
