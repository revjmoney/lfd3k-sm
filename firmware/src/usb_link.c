#include "driver/usb_serial_jtag.h"
#include "esp_check.h"
#include "freertos/stream_buffer.h"
#include "usb_link.h"

#define RX_BUF_BYTES 2048
#define TX_BUF_BYTES 4096
#define USB_CHUNK    64

static StreamBufferHandle_t s_rx;
static StreamBufferHandle_t s_tx;
static volatile TaskHandle_t s_notify;
/* Bytes the writer took from s_tx and has not handed to the driver yet. */
static volatile size_t s_in_flight;

static void reader_task(void *arg)
{
    (void)arg;
    uint8_t buf[USB_CHUNK];
    for (;;) {
        int n = usb_serial_jtag_read_bytes(buf, sizeof(buf), pdMS_TO_TICKS(20));
        if (n <= 0) {
            continue;
        }
        size_t off = 0;
        while (off < (size_t)n) {
            /* Backpressure: wait for the modem task rather than drop bytes. */
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
    uint8_t buf[USB_CHUNK];
    for (;;) {
        size_t n = xStreamBufferReceive(s_tx, buf, sizeof(buf), portMAX_DELAY);
        s_in_flight = n;
        size_t off = 0;
        while (off < n) {
            /* Blocks here (never in the modem task) while no host reads. */
            int w = usb_serial_jtag_write_bytes(buf + off, n - off, pdMS_TO_TICKS(100));
            if (w > 0) {
                off += (size_t)w;
            }
        }
        s_in_flight = 0;
    }
}

void usb_link_init(void)
{
    usb_serial_jtag_driver_config_t cfg = {
        .rx_buffer_size = 256,
        .tx_buffer_size = 256,
    };
    ESP_ERROR_CHECK(usb_serial_jtag_driver_install(&cfg));
    s_rx = xStreamBufferCreate(RX_BUF_BYTES, 1);
    s_tx = xStreamBufferCreate(TX_BUF_BYTES, 1);
    configASSERT(s_rx && s_tx);
    xTaskCreatePinnedToCore(reader_task, "usb_rx", 3072, NULL, 5, NULL, 0);
    xTaskCreatePinnedToCore(writer_task, "usb_tx", 3072, NULL, 5, NULL, 0);
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
    return xStreamBufferSend(s_tx, buf, len, 0);
}

size_t usb_link_tx_space(void)
{
    return xStreamBufferSpacesAvailable(s_tx);
}

bool usb_link_flush(uint32_t timeout_ms)
{
    TickType_t t0 = xTaskGetTickCount();
    TickType_t limit = pdMS_TO_TICKS(timeout_ms);
    while (!xStreamBufferIsEmpty(s_tx) || s_in_flight != 0) {
        if (xTaskGetTickCount() - t0 >= limit) {
            return false;
        }
        vTaskDelay(1);
    }
    TickType_t used = xTaskGetTickCount() - t0;
    TickType_t left = used < limit ? limit - used : 0;
    return usb_serial_jtag_wait_tx_done(left) == ESP_OK;
}
