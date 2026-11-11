#include "choose.h"

uint32_t lfmp_crc32(const void *data, uint32_t len)
{
    const uint8_t *p = (const uint8_t *)data;
    uint32_t crc = 0xFFFFFFFFu;
    for (uint32_t i = 0; i < len; i++) {
        crc ^= p[i];
        for (int b = 0; b < 8; b++) {
            crc = (crc >> 1) ^ (0xEDB88320u & (0u - (crc & 1u)));
        }
    }
    return crc ^ 0xFFFFFFFFu;
}

void lfmp_token_make(boot_token_t *t, uint8_t slot)
{
    t->magic = LFMP_TOKEN_MAGIC;
    t->slot = slot;
    t->pad[0] = 0;
    t->pad[1] = 0;
    t->pad[2] = 0;
    t->crc32 = lfmp_crc32(t, 8);
}

bool lfmp_token_valid(const boot_token_t *t)
{
    if (t == 0 || t->magic != LFMP_TOKEN_MAGIC) {
        return false;
    }
    if (t->slot > 15 || t->pad[0] || t->pad[1] || t->pad[2]) {
        return false;
    }
    return t->crc32 == lfmp_crc32(t, 8);
}

boot_choice_t choose_boot(uint32_t reset_reason, const boot_token_t *token, bool *clear_token)
{
    bool valid = lfmp_token_valid(token);
    if (clear_token) {
        *clear_token = false;
    }
    if (reset_reason == LFMP_RST_DEEP_SLEEP) {
        return BOOT_OTADATA;
    }
    if (valid && clear_token) {
        *clear_token = true; /* one shot, whatever happens next */
    }
    if (valid && (reset_reason == LFMP_RST_CORE_SW || reset_reason == LFMP_RST_CPU0_SW)) {
        return BOOT_OTADATA;
    }
    return BOOT_PICKER;
}

boot_choice_t choose_token_target(int otadata_index, const boot_token_t *token, bool image_ok)
{
    if (!lfmp_token_valid(token) || otadata_index < 0 || otadata_index != (int)token->slot) {
        return BOOT_PICKER;
    }
    return image_ok ? BOOT_OTADATA : BOOT_PICKER;
}
