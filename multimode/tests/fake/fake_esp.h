/* Test side of the fake ESP-IDF layer (partition table, OTA, image verify,
 * restart, NVS with an operation log). Used by test_modem_side.c. */
#ifndef FAKE_ESP_H
#define FAKE_ESP_H
#include <setjmp.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"
#include "esp_partition.h"

#define ESP_ERR_NVS_READ_ONLY 0x1107

extern int fake_log_warnings;
extern int fake_critical_depth;
extern int fake_critical_enters;

/* esp_restart */
extern jmp_buf fake_restart_jmp;
extern int fake_restarts;

/* partitions and OTA */
void fake_reset_all(void);
esp_partition_t *fake_part_add(esp_partition_type_t type, esp_partition_subtype_t subtype, const char *label,
                               uint32_t address, uint32_t size);
void fake_part_make_image(esp_partition_t *p); /* header + app descriptor magic */
void fake_set_running(const esp_partition_t *p);
void fake_set_boot(const esp_partition_t *p);
const esp_partition_t *fake_boot(void);
extern esp_err_t fake_verify_err;
extern esp_err_t fake_set_boot_err;
extern int fake_verify_calls;
extern int fake_set_boot_calls;

/* NVS */
typedef enum {
    OP_INIT,
    OP_DEINIT,
    OP_ERASE_PART,
    OP_OPEN_RO,
    OP_OPEN_RW,
    OP_SET,
    OP_ERASE_KEY,
    OP_COMMIT,
    OP_DEFAULT_PART, /* nvs_flash_init(), nvs_open(), nvs_flash_erase(): never allowed */
} fake_nvs_op_t;

void fake_nvs_part_add(const char *name);
void fake_nvs_part_fail_init(const char *name, esp_err_t err); /* until erased */
void fake_nvs_put_blob(const char *part, const char *ns, const char *key, const void *data, size_t len);
void fake_nvs_put_u8(const char *part, const char *ns, const char *key, uint8_t v);
bool fake_nvs_get_blob(const char *part, const char *ns, const char *key, void *out, size_t *len);
bool fake_nvs_get_u8(const char *part, const char *ns, const char *key, uint8_t *out);
bool fake_nvs_has(const char *part, const char *ns, const char *key);
int fake_nvs_entries(const char *part); /* number of keys in a partition */
/* How many logged operations of this kind touched this partition (NULL = any partition). */
int fake_nvs_ops(fake_nvs_op_t op, const char *part);
/* Same, for operations on one namespace only. */
int fake_nvs_ops_ns(fake_nvs_op_t op, const char *part, const char *ns);
/* Number of logged operations that touched this partition at all. */
int fake_nvs_touches(const char *part);

/* Random bytes source for settings_store_gen_ap_pass. */
extern uint32_t fake_random_next;
#endif
