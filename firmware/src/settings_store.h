/* Settings blob (key "settings") and directory blob (key "dir") in NVS, namespace
 * "lfd3k". Modem task only. */
#ifndef SETTINGS_STORE_H
#define SETTINGS_STORE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* nvs_flash_init, erasing and re-initialising on NO_FREE_PAGES/NEW_VERSION.
 * False on any other error: the caller then runs on defaults and never saves. */
bool settings_store_init(void);
/* Reads the blob into buf (cap bytes). False when missing or on error. */
bool settings_store_load(uint8_t *buf, size_t cap, size_t *len);
/* Writes and commits the blob. False on error. */
bool settings_store_save(const uint8_t *buf, size_t len);
/* The same two for the directory blob (key "dir"). */
bool settings_store_load_dir(uint8_t *buf, size_t cap, size_t *len);
bool settings_store_save_dir(const uint8_t *buf, size_t len);
/* 12 random characters from ABCDEFGHJKLMNPQRSTUVWXYZ23456789 plus NUL. */
void settings_store_gen_ap_pass(char out[13]);

#endif
