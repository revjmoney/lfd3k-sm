/* PC test stub of esp_err.h with the real values. */
#ifndef FAKE_ESP_ERR_H
#define FAKE_ESP_ERR_H
typedef int esp_err_t;
#define ESP_OK 0
#define ESP_FAIL -1
#define ESP_ERR_NO_MEM 0x101
#define ESP_ERR_INVALID_STATE 0x103
#define ESP_ERR_NOT_FOUND 0x105
#define ESP_ERR_NVS_NOT_INITIALIZED 0x1101
#define ESP_ERR_NVS_NOT_FOUND 0x1102
#define ESP_ERR_NVS_INVALID_LENGTH 0x1106
#define ESP_ERR_NVS_NO_FREE_PAGES 0x110D
#define ESP_ERR_NVS_NEW_VERSION_FOUND 0x1110
const char *esp_err_to_name(esp_err_t e);
#endif
