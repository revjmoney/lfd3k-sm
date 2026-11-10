#ifndef HAL_RADIO_H
#define HAL_RADIO_H
#include <stddef.h>
#include <stdint.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

/* Board power order, SPI2 bus, radio pins and the DIO1 interrupt, which only
 * notifies `modem_task`. Call once, before lfd3k_init. */
void hal_radio_init(TaskHandle_t modem_task);

/* Previous boot's crash note: copies up to 64 bytes into out and returns the
 * length, or 0 when none. Clears the stored note either way. */
size_t hal_radio_take_panic_note(uint8_t *out);

#endif
