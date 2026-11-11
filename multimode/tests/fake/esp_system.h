/* PC test stub: esp_restart never returns; the test catches it with longjmp. */
#ifndef FAKE_ESP_SYSTEM_H
#define FAKE_ESP_SYSTEM_H
void esp_restart(void) __attribute__((noreturn));
#endif
