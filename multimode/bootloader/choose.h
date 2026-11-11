/* Multimode boot decision and the launch token, shared by the bootloader
 * override and the picker app. Pure C: no ESP headers, no hardware access,
 * so the same file also compiles on the PC for tests. */
#ifndef LFMP_CHOOSE_H
#define LFMP_CHOOSE_H
#include <stdbool.h>
#include <stdint.h>

/* The token lives at this fixed RTC FAST address (data bus view of the S3's
 * 8 KB RTC FAST RAM, 0x600FE000..0x60100000). It is 64 bytes below the top:
 * clear of the app's RTC timer data in the top 24 bytes and of IDF's optional
 * bootloader retain area just under it, and far above the RTC code and data
 * sections that apps link at the bottom. See multimode/README.md. */
#define LFMP_TOKEN_ADDR  0x600FFFC0u
#define LFMP_TOKEN_MAGIC 0x504D464Cu /* bytes 'L' 'F' 'M' 'P' in memory order */

typedef struct {
    uint32_t magic;  /* LFMP_TOKEN_MAGIC */
    uint8_t slot;    /* ota subtype index the picker chose (0..15), informational */
    uint8_t pad[3];  /* zero */
    uint32_t crc32;  /* lfmp_crc32 over the first 8 bytes */
} boot_token_t;

/* ESP32-S3 ROM reset reasons (soc/reset_reasons.h). Copied as numbers so this
 * file needs no ESP header; the bootloader checks them with static asserts. */
#define LFMP_RST_POWER_ON   0x01u /* power on (also chip brownout) */
#define LFMP_RST_CORE_SW    0x03u /* RTC_CNTL_SW_SYS_RST: system software reset */
#define LFMP_RST_DEEP_SLEEP 0x05u /* wake from deep sleep */
#define LFMP_RST_CPU0_SW    0x0Cu /* RTC_CNTL_SW_PROCPU_RST: what esp_restart() and
                                     the IDF panic handler produce on the S3 */

typedef enum { BOOT_PICKER, BOOT_OTADATA } boot_choice_t;

/* Standard reflected CRC-32 (poly 0xEDB88320, init and final xor 0xFFFFFFFF). */
uint32_t lfmp_crc32(const void *data, uint32_t len);

/* Fills a valid token for the given slot. */
void lfmp_token_make(boot_token_t *t, uint8_t slot);

/* True when magic, pad, slot range and CRC all check out. */
bool lfmp_token_valid(const boot_token_t *t);

/* reset: the ROM reset reason; token: the RTC token as read (may be garbage);
 * returns the choice and whether the token must be cleared. Pure: no hardware
 * access, PC-testable.
 * Deep-sleep wake: otadata (a mode resumes itself), token untouched.
 * Software reset (system or CPU0, which includes a panic reset) with a valid
 * token: clear it, otadata.
 * Everything else: picker. A valid token seen on any other reset is cleared
 * too, so it can never fire later. */
boot_choice_t choose_boot(uint32_t reset_reason, const boot_token_t *token, bool *clear_token);

/* Second check on the token path (software reset with a valid token), after
 * otadata was read: otadata_index is the ota slot otadata selects (-1 for none
 * or factory), image_ok whether that slot's image verified. Boots otadata only
 * when the slot is the one the token names and its image is good; anything
 * else goes to the picker, so the stock loader never falls back to a
 * neighbouring mode. Pure. */
boot_choice_t choose_token_target(int otadata_index, const boot_token_t *token, bool image_ok);

#endif
