/* LFD3K-SM firmware skeleton: proves the Rust core links and runs. */
#include <stdint.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_log.h"
#include "lfd3k.h"

static const char *TAG = "lfd3k";

void app_main(void)
{
    uint8_t ver[32];
    size_t n = lfd3k_core_version(ver, sizeof(ver) - 1);
    ver[n] = 0;
    for (;;) {
        ESP_LOGI(TAG, "LFD3K-SM core %s", (const char *)ver);
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}
