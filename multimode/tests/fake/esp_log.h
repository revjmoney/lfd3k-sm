/* PC test stub: logging is swallowed (arguments still type checked). */
#ifndef FAKE_ESP_LOG_H
#define FAKE_ESP_LOG_H
#include <stdio.h>
#include "esp_err.h"
extern int fake_log_warnings;
#define ESP_LOGI(tag, fmt, ...) ((void)(tag), (void)sizeof(printf(fmt, ##__VA_ARGS__)))
#define ESP_LOGW(tag, fmt, ...) ((void)(tag), fake_log_warnings++, (void)sizeof(printf(fmt, ##__VA_ARGS__)))
#define ESP_LOGE(tag, fmt, ...) ((void)(tag), fake_log_warnings++, (void)sizeof(printf(fmt, ##__VA_ARGS__)))
#endif
