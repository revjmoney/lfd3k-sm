/* The modem stream over UART0 for the Heltec V3, whose USB-C port goes to a
 * CP2102 bridge wired to UART0 (GPIO 43 TX, 44 RX). Same API and the same
 * two-task, two-buffer design as usb_link.c (see usb_link.h), so the modem
 * task cannot tell the boards apart.
 *
 * The V3 build turns the console off (sdkconfig.defaults.v3), so this port
 * carries nothing but modem bytes once the app runs. The ROM boot text still
 * appears on it after every reset, before the app starts.
 *
 * No flow control: the CP2102's RTS and CTS are not wired to the ESP32-S3.
 * At 115200 the UART drains about 11.5 KB/s whether or not a host is
 * reading, so the writer never waits for one. */
#include "driver/uart.h"
#include "esp_check.h"
#include "freertos/stream_buffer.h"
#include "usb_link.h"

#define LINK_UART    UART_NUM_0
#define LINK_BAUD    115200
#define RX_BUF_BYTES 2048
#define TX_BUF_BYTES 4096
#define UART_CHUNK   64

static StreamBufferHandle_t s_rx;
static StreamBufferHandle_t s_tx;
static volatile TaskHandle_t s_notify;
/* Bytes accepted by usb_link_write (modem task) and bytes handed to the UART
 * driver (writer task); equal when nothing is queued or in flight. */
static volatile uint32_t s_tx_in;
static volatile uint32_t s_tx_out;

static void reader_task(void *arg)
{
    (void)arg;
    uint8_t buf[UART_CHUNK];
    for (;;) {
        int n = uart_read_bytes(LINK_UART, buf, sizeof(buf), pdMS_TO_TICKS(20));
        if (n <= 0) {
            continue;
        }
        size_t off = 0;
        while (off < (size_t)n) {
            /* Backpressure: wait for the modem task rather than drop bytes
             * (the driver's own RX ring keeps filling meanwhile). */
            off += xStreamBufferSend(s_rx, buf + off, (size_t)n - off, pdMS_TO_TICKS(50));
        }
        TaskHandle_t t = s_notify;
        if (t) {
            xTaskNotifyGive(t);
        }
    }
}

static void writer_task(void *arg)
{
    (void)arg;
    uint8_t buf[UART_CHUNK];
    for (;;) {
        size_t n = xStreamBufferReceive(s_tx, buf, sizeof(buf), portMAX_DELAY);
        if (n > 0) {
            /* Blocks here (never in the modem task) until the driver's TX
             * ring has room. */
            uart_write_bytes(LINK_UART, buf, n);
        }
        s_tx_out += (uint32_t)n;
    }
}

void usb_link_init(void)
{
    const uart_config_t cfg = {
        .baud_rate = LINK_BAUD,
        .data_bits = UART_DATA_8_BITS,
        .parity = UART_PARITY_DISABLE,
        .stop_bits = UART_STOP_BITS_1,
        .flow_ctrl = UART_HW_FLOWCTRL_DISABLE,
        .source_clk = UART_SCLK_DEFAULT,
    };
    ESP_ERROR_CHECK(uart_driver_install(LINK_UART, 4096, 1024, 0, NULL, 0));
    ESP_ERROR_CHECK(uart_param_config(LINK_UART, &cfg));
    ESP_ERROR_CHECK(uart_set_pin(LINK_UART, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE,
                                 UART_PIN_NO_CHANGE));
    s_rx = xStreamBufferCreate(RX_BUF_BYTES, 1);
    s_tx = xStreamBufferCreate(TX_BUF_BYTES, 1);
    configASSERT(s_rx && s_tx);
    xTaskCreatePinnedToCore(reader_task, "uart_rx", 3072, NULL, 5, NULL, 0);
    xTaskCreatePinnedToCore(writer_task, "uart_tx", 3072, NULL, 5, NULL, 0);
}

void usb_link_set_notify(TaskHandle_t task)
{
    s_notify = task;
}

size_t usb_link_read(uint8_t *buf, size_t len, TickType_t wait)
{
    return xStreamBufferReceive(s_rx, buf, len, wait);
}

size_t usb_link_write(const uint8_t *buf, size_t len)
{
    size_t n = xStreamBufferSend(s_tx, buf, len, 0);
    s_tx_in += (uint32_t)n;
    return n;
}

size_t usb_link_tx_space(void)
{
    return xStreamBufferSpacesAvailable(s_tx);
}

bool usb_link_flush(uint32_t timeout_ms)
{
    TickType_t t0 = xTaskGetTickCount();
    TickType_t limit = pdMS_TO_TICKS(timeout_ms);
    while (s_tx_out != s_tx_in) {
        if (xTaskGetTickCount() - t0 >= limit) {
            return false;
        }
        vTaskDelay(1);
    }
    TickType_t used = xTaskGetTickCount() - t0;
    TickType_t left = used < limit ? limit - used : 0;
    return uart_wait_tx_done(LINK_UART, left) == ESP_OK;
}
