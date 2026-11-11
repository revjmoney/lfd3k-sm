/* Settings blob (key "settings") and directory blob (key "dir") in NVS, namespace
 * "lfd3k", in the modem's own partition "lfd3k_nvs" (multimode layout), or in the
 * default "nvs" when the partition table has no "lfd3k_nvs" (the modem alone).
 * Modem task only. */
#ifndef SETTINGS_STORE_H
#define SETTINGS_STORE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Initialises the modem's NVS partition, erasing and re-initialising it (and only
 * it) on NO_FREE_PAGES/NEW_VERSION. With "lfd3k_nvs" present and no settings in it
 * yet, old lfd3k/settings and lfd3k/dir blobs in the shared "nvs" are copied over
 * once and then erased there (only those two keys; the shared partition is never
 * erased). False on any other error: the caller then runs on defaults and never saves. */
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
