/* AT+MODE on the device: which mode slots hold an image, and the switch
 * itself (otadata, launch token, restart). See multimode/README.md.
 * Modem task only. */
#ifndef MODE_SWITCH_H
#define MODE_SWITCH_H
#include <stdbool.h>
#include <stdint.h>

/* Mode indices, the same as the core's ModeTarget and the picker's menu order. */
#define MODE_MODEM  0 /* LFD3K-SM, ota_1 */
#define MODE_RADAR  1 /* SIMIAN RADAR, ota_2 */
#define MODE_RNODE  2 /* RNODE, ota_3 */
#define MODE_MESH   3 /* MESH, ota_0 */
#define MODE_PICKER 4 /* the picker (factory); not a slot */

/* True when the board runs the multimode layout (a factory app, the picker,
 * and the modem's own "lfd3k_nvs"). Without it AT+MODE stays ERROR. */
bool mode_switch_available(void);

/* Bit i set when mode i's slot holds an app image (image header magic, S3
 * chip id and app descriptor magic, read with esp_partition_read). The
 * running modem is always set. */
uint8_t mode_switch_slots(void);

/* Prepares the switch: for a mode, verifies its image, points otadata at it
 * (esp_ota_set_boot_partition, which hashes the whole image) and records it
 * as the picker's last mode (picker_nvs "picker"/"last"; a failed write there
 * only logs); for the picker, nothing. False on any failure of the verify or
 * otadata (otadata then unchanged). */
bool mode_switch_prepare(int mode);

/* Writes the launch token for the mode (none for the picker) with
 * interrupts off, then esp_restart(). Never returns. Call only after a
 * successful mode_switch_prepare and once the OK has left the USB port. */
void mode_switch_restart(int mode) __attribute__((noreturn));

#endif
