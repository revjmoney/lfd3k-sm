/* PC tests of the bootloader's boot decision (multimode/bootloader/choose.c)
 * and its source-level promises. Build: see run.ps1. */
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "choose.h"
#include "tap.h"

#ifndef SRC_DIR
#error "pass -DSRC_DIR=\"<repo>/multimode\""
#endif

static boot_token_t tok_valid(uint8_t slot)
{
    boot_token_t t;
    memset(&t, 0xA5, sizeof(t)); /* make sure make() fills every byte */
    lfmp_token_make(&t, slot);
    return t;
}

/* Re-seal a token after a field edit, so only that field is wrong. */
static void reseal(boot_token_t *t)
{
    t->crc32 = lfmp_crc32(t, 8);
}

/* ---- token layout and CRC ------------------------------------------------ */

static void token_layout_and_crc(void)
{
    CHECK_EQ(sizeof(boot_token_t), 12);
    CHECK_EQ(offsetof(boot_token_t, magic), 0);
    CHECK_EQ(offsetof(boot_token_t, slot), 4);
    CHECK_EQ(offsetof(boot_token_t, pad), 5);
    CHECK_EQ(offsetof(boot_token_t, crc32), 8);
    CHECK_EQ(LFMP_TOKEN_ADDR, 0x600FFFC0u);
    CHECK_EQ(LFMP_TOKEN_ADDR & 3, 0);
    /* 12 bytes inside the top of the 8 KB RTC FAST RAM, clear of the top 24
     * bytes the RTC timer retain data uses. */
    CHECK(LFMP_TOKEN_ADDR >= 0x600FE000u);
    CHECK(LFMP_TOKEN_ADDR + sizeof(boot_token_t) <= 0x60100000u - 24u);
    /* "LFMP" in memory order. */
    boot_token_t t = tok_valid(2);
    CHECK(memcmp(&t, "LFMP", 4) == 0);
    /* The standard CRC-32 check value. */
    CHECK_EQ(lfmp_crc32("123456789", 9), 0xCBF43926u);
    CHECK_EQ(lfmp_crc32("", 0), 0);
}

/* ---- token_needs_magic_and_crc ------------------------------------------- */

static void token_needs_magic_and_crc(void)
{
    for (int slot = 0; slot <= 15; slot++) {
        boot_token_t t = tok_valid((uint8_t)slot);
        CHECK(lfmp_token_valid(&t));
    }
    CHECK(!lfmp_token_valid(NULL));

    /* Every single bit flip of any of the 12 bytes invalidates a token. */
    boot_token_t good = tok_valid(3);
    for (int bit = 0; bit < 96; bit++) {
        boot_token_t t = good;
        ((uint8_t *)&t)[bit / 8] ^= (uint8_t)(1u << (bit % 8));
        if (lfmp_token_valid(&t)) {
            CHECK(!"single bit flip accepted");
            printf("    bit %d\n", bit);
        }
    }

    /* Wrong magic with a CRC that matches it: still rejected. */
    boot_token_t m = tok_valid(1);
    m.magic = 0x504D464Du;
    reseal(&m);
    CHECK(!lfmp_token_valid(&m));

    /* Right magic, bad CRC. */
    boot_token_t c = tok_valid(1);
    c.crc32 ^= 1u;
    CHECK(!lfmp_token_valid(&c));
    c = tok_valid(1);
    c.crc32 = 0;
    CHECK(!lfmp_token_valid(&c));

    /* Each pad byte non zero, CRC re-sealed so only the pad rule can reject. */
    for (int i = 0; i < 3; i++) {
        boot_token_t p = tok_valid(1);
        p.pad[i] = 1;
        reseal(&p);
        CHECK(!lfmp_token_valid(&p));
    }

    /* Slot out of range 16..255, CRC re-sealed. */
    for (int slot = 16; slot <= 255; slot++) {
        boot_token_t s = tok_valid(1);
        s.slot = (uint8_t)slot;
        reseal(&s);
        if (lfmp_token_valid(&s)) {
            CHECK(!"slot out of range accepted");
            printf("    slot %d\n", slot);
        }
    }

    /* Typical garbage: RTC RAM after power on reads as anything. */
    const uint8_t fills[] = {0x00, 0xFF, 0xA5, 0x5A, 0x4C};
    for (size_t i = 0; i < sizeof(fills); i++) {
        boot_token_t g;
        memset(&g, fills[i], sizeof(g));
        CHECK(!lfmp_token_valid(&g));
    }
    srand(12345);
    int accepted = 0;
    for (int n = 0; n < 200000; n++) {
        boot_token_t g;
        for (size_t i = 0; i < sizeof(g); i++) {
            ((uint8_t *)&g)[i] = (uint8_t)rand();
        }
        accepted += lfmp_token_valid(&g);
    }
    CHECK_EQ(accepted, 0);
}

/* ---- choose_boot table: every ROM reset reason x token state -------------- */

typedef struct {
    const char *name;
    boot_token_t tok;
    bool valid;
} tstate_t;

static void fill_states(tstate_t *s, int *n)
{
    int k = 0;
    memset(&s[k].tok, 0, sizeof(boot_token_t));
    s[k].name = "absent (zero)";
    s[k++].valid = false;
    for (int slot = 0; slot <= 15; slot += 5) { /* 0, 5, 10, 15 */
        s[k].tok = tok_valid((uint8_t)slot);
        s[k].name = "valid";
        s[k++].valid = true;
    }
    s[k].tok = tok_valid(1);
    s[k].tok.magic ^= 0x100u;
    s[k].name = "bad magic";
    s[k++].valid = false;
    s[k].tok = tok_valid(1);
    s[k].tok.crc32 ^= 0x80000000u;
    s[k].name = "bad crc";
    s[k++].valid = false;
    s[k].tok = tok_valid(1);
    s[k].tok.pad[2] = 0x7F;
    reseal(&s[k].tok);
    s[k].name = "nonzero pad";
    s[k++].valid = false;
    s[k].tok = tok_valid(1);
    s[k].tok.slot = 16;
    reseal(&s[k].tok);
    s[k].name = "slot 16";
    s[k++].valid = false;
    s[k].tok = tok_valid(1);
    s[k].tok.slot = 0xFF;
    reseal(&s[k].tok);
    s[k].name = "slot 255";
    s[k++].valid = false;
    memset(&s[k].tok, 0xFF, sizeof(boot_token_t));
    s[k].name = "garbage FF";
    s[k++].valid = false;
    memset(&s[k].tok, 0xA5, sizeof(boot_token_t));
    s[k].name = "garbage A5";
    s[k++].valid = false;
    *n = k;
}

static void choose_boot_full_table(void)
{
    tstate_t st[32];
    int ns;
    fill_states(st, &ns);
    int cases = 0;
    /* Every reset reason the 6 bit ROM field can hold, 0..63. */
    for (uint32_t reason = 0; reason < 64; reason++) {
        for (int i = 0; i < ns; i++) {
            bool clear = true; /* must be overwritten */
            boot_choice_t got = choose_boot(reason, &st[i].tok, &clear);
            boot_choice_t want_choice;
            bool want_clear;
            if (reason == LFMP_RST_DEEP_SLEEP) {
                want_choice = BOOT_OTADATA; /* a mode resumes itself */
                want_clear = false;         /* token left alone */
            } else if (st[i].valid && (reason == LFMP_RST_CORE_SW || reason == LFMP_RST_CPU0_SW)) {
                want_choice = BOOT_OTADATA;
                want_clear = true;
            } else {
                want_choice = BOOT_PICKER;
                want_clear = st[i].valid; /* a stray valid token is burned too */
            }
            cases++;
            if (got != want_choice || clear != want_clear) {
                CHECK(!"choose_boot table mismatch");
                printf("    reason %u token '%s': got %d clear %d, want %d clear %d\n",
                       (unsigned)reason, st[i].name, (int)got, (int)clear, (int)want_choice,
                       (int)want_clear);
            }
        }
    }
    CHECK_EQ(cases, 64 * ns);
    printf("    (%d table cases)\n", cases);
}

static void bootloader_without_token_boots_picker(void)
{
    boot_token_t none;
    memset(&none, 0, sizeof(none));
    /* The named ROM reset reasons of the ESP32-S3, 1..23 (soc/reset_reasons.h). */
    for (uint32_t reason = 1; reason <= 23; reason++) {
        bool clear = true;
        boot_choice_t c = choose_boot(reason, &none, &clear);
        if (reason == LFMP_RST_DEEP_SLEEP) {
            CHECK_EQ(c, BOOT_OTADATA);
        } else {
            CHECK_EQ(c, BOOT_PICKER);
        }
        CHECK(!clear);
    }
    /* Power on with leftover RTC garbage: picker, whatever it looks like. */
    boot_token_t junk;
    memset(&junk, 0xFF, sizeof(junk));
    bool clear = true;
    CHECK_EQ(choose_boot(LFMP_RST_POWER_ON, &junk, &clear), BOOT_PICKER);
    CHECK(!clear);
    /* A software reset without a token (a mode that restarts itself) goes to
     * the picker as well. */
    clear = true;
    CHECK_EQ(choose_boot(LFMP_RST_CORE_SW, &none, &clear), BOOT_PICKER);
    clear = true;
    CHECK_EQ(choose_boot(LFMP_RST_CPU0_SW, &none, &clear), BOOT_PICKER);
    CHECK(!clear);
    /* No token pointer at all. */
    CHECK_EQ(choose_boot(LFMP_RST_CPU0_SW, NULL, &clear), BOOT_PICKER);
}

static void token_cleared_before_boot(void)
{
    /* Decision half: any valid token outside a deep sleep wake is consumed. */
    boot_token_t t = tok_valid(2);
    const uint32_t reasons[] = {LFMP_RST_POWER_ON, LFMP_RST_CORE_SW, LFMP_RST_CPU0_SW, 0x0F, 0x04, 0x09};
    for (size_t i = 0; i < sizeof(reasons) / sizeof(reasons[0]); i++) {
        bool clear = false;
        (void)choose_boot(reasons[i], &t, &clear);
        CHECK(clear);
    }
    /* A null clear_token pointer must not crash. */
    CHECK_EQ(choose_boot(LFMP_RST_CPU0_SW, &t, NULL), BOOT_OTADATA);

    /* Source half (the bootloader cannot run on the PC): the clear happens in
     * read_and_consume_token(), and call_start_cpu0 calls that before it loads
     * any image. */
    FILE *f = fopen(SRC_DIR "/picker/bootloader_components/main/bootloader_start.c", "rb");
    CHECK(f != NULL);
    if (!f) {
        return;
    }
    static char src[32768];
    size_t n = fread(src, 1, sizeof(src) - 1, f);
    fclose(f);
    src[n] = 0;
    const char *fn = strstr(src, "static boot_choice_t read_and_consume_token(void)");
    CHECK(fn != NULL);
    const char *entry = strstr(src, "void __attribute__((noreturn)) call_start_cpu0(void)");
    CHECK(entry != NULL);
    if (!fn || !entry) {
        return;
    }
    /* Inside the function: reads 3 words, decides, then zeroes all 3. */
    const char *dec = strstr(fn, "choose_boot(reason, &tok, &clear)");
    const char *z0 = strstr(fn, "rtc[0] = 0;");
    const char *z1 = strstr(fn, "rtc[1] = 0;");
    const char *z2 = strstr(fn, "rtc[2] = 0;");
    CHECK(dec && z0 && z1 && z2 && dec < z0 && z0 < z1 && z1 < z2);
    CHECK(z2 && z2 < entry); /* inside read_and_consume_token */
    /* In the entry function: init, consume, then pick the partition, then load. */
    const char *init = strstr(entry, "bootloader_init()");
    const char *call = strstr(entry, "read_and_consume_token();");
    const char *sel = strstr(entry, "select_partition_number(");
    const char *load = strstr(entry, "bootloader_utility_load_boot_image(");
    CHECK(init && call && sel && load && init < call && call < sel && sel < load);
}

static void deep_sleep_wake_follows_otadata(void)
{
    /* Meshtastic power saving: a deep sleep wake never shows the picker,
     * whatever the RTC memory holds, and never touches the token. */
    tstate_t st[32];
    int ns;
    fill_states(st, &ns);
    for (int i = 0; i < ns; i++) {
        bool clear = true;
        CHECK_EQ(choose_boot(LFMP_RST_DEEP_SLEEP, &st[i].tok, &clear), BOOT_OTADATA);
        CHECK(!clear);
    }
    /* Only reason 5 does this (with no token). */
    boot_token_t none;
    memset(&none, 0, sizeof(none));
    for (uint32_t r = 0; r < 64; r++) {
        bool clear;
        boot_choice_t c = choose_boot(r, &none, &clear);
        CHECK((c == BOOT_OTADATA) == (r == 5));
    }
    CHECK_EQ(LFMP_RST_DEEP_SLEEP, 5);
}

/* ---- second check: token slot vs otadata --------------------------------- */

static void token_slot_vs_otadata(void)
{
    for (int slot = 0; slot <= 15; slot++) {
        boot_token_t t = tok_valid((uint8_t)slot);
        for (int ota = -1; ota <= 16; ota++) {
            for (int ok = 0; ok <= 1; ok++) {
                boot_choice_t want = (ota == slot && ok) ? BOOT_OTADATA : BOOT_PICKER;
                boot_choice_t got = choose_token_target(ota, &t, ok != 0);
                if (got != want) {
                    CHECK(!"choose_token_target mismatch");
                    printf("    slot %d otadata %d ok %d got %d\n", slot, ota, ok, (int)got);
                }
            }
        }
    }
    /* A mismatch (otadata names ota_1, the token ota_2) goes to the picker
     * even when ota_1 verifies: the stock loader never runs a neighbour. */
    boot_token_t t = tok_valid(2);
    CHECK_EQ(choose_token_target(1, &t, true), BOOT_PICKER);
    CHECK_EQ(choose_token_target(2, &t, true), BOOT_OTADATA);
    CHECK_EQ(choose_token_target(2, &t, false), BOOT_PICKER);
    CHECK_EQ(choose_token_target(-1, &t, true), BOOT_PICKER); /* factory or none */
    /* An invalid token never matches anything. */
    boot_token_t bad = tok_valid(2);
    bad.crc32++;
    CHECK_EQ(choose_token_target(2, &bad, true), BOOT_PICKER);
    CHECK_EQ(choose_token_target(2, NULL, true), BOOT_PICKER);
}

int main(void)
{
    RUN(token_layout_and_crc);
    RUN(token_needs_magic_and_crc);
    RUN(choose_boot_full_table);
    RUN(bootloader_without_token_boots_picker);
    RUN(token_cleared_before_boot);
    RUN(deep_sleep_wake_follows_otadata);
    RUN(token_slot_vs_otadata);
    TAP_DONE();
}
