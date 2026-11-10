/* C interface to the LFD3K-SM Rust core (ffi crate). */
#ifndef LFD3K_H
#define LFD3K_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Copies the core version (no NUL) into out, at most len bytes.
 * Returns the bytes written; 0 when out is NULL. */
size_t lfd3k_core_version(uint8_t *out, size_t len);

#ifdef __cplusplus
}
#endif

#endif
