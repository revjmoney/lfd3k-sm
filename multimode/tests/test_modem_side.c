/* PC tests of the modem's side of multimode, compiling the real
 * firmware/src/mode_switch.c and firmware/src/settings_store.c against a fake
 * ESP-IDF layer (multimode/tests/fake), together with the bootloader's own
 * choose.c and the picker's picker_menu.c in one binary:
 *
 *  - the launch token the modem writes is exactly what the bootloader accepts
 *  - AT+MODE preparation refuses missing or broken slots without side effects
 *  - the modem reads and writes only its own NVS partition (lfd3k_nvs)
 *
 * The RTC FAST address 0x600FFFC0 is real here: the test maps memory at it.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "choose.h"
#include "esp_app_desc.h"
#include "esp_image_format.h"
#include "fake_esp.h"
#include "mode_switch.h"
#include "picker_menu.h"
#include "settings_store.h"
#include "tap.h"

#ifdef _WIN32
#include <windows.h>
#endif

#ifndef SRC_DIR
#error "pass -DSRC_DIR=\"<repo>/multimode\""
#endif

/* ---- RTC FAST RAM at the real address ------------------------------------ */

static volatile uint32_t *s_rtc;

static void map_rtc(void)
{
#ifdef _WIN32
    /* 0x600FE000..0x60100000 is 8 KB; map the 64 KB granule that holds it. */
    void *p = VirtualAlloc((void *)(uintptr_t)0x600F0000u, 0x20000u, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    if (p != (void *)(uintptr_t)0x600F0000u) {
        printf("cannot map RTC FAST test memory at 0x600F0000 (got %p)\n", p);
        exit(2);
    }
    s_rtc = (volatile uint32_t *)(uintptr_t)LFMP_TOKEN_ADDR;
#else
#error "this test maps memory at the fixed RTC address; Windows only"
#endif
}

static void rtc_clear(void)
{
    s_rtc[0] = s_rtc[1] = s_rtc[2] = 0;
}

static boot_token_t rtc_token(void)
{
    uint32_t w[3] = {s_rtc[0], s_rtc[1], s_rtc[2]};
    boot_token_t t;
    memcpy(&t, w, sizeof(t));
    return t;
}

/* ---- fixtures ------------------------------------------------------------ */

static esp_partition_t *OTA[4];
static esp_partition_t *FACTORY;

typedef struct {
    bool factory, own_nvs, all_ota;
} layout_opts_t;

/* The multimode partition table as the modem sees it. The modem is running
 * from ota_1; images are present everywhere but not yet made. */
static void layout(layout_opts_t o)
{
    fake_reset_all();
    FACTORY = o.factory ? fake_part_add(ESP_PARTITION_TYPE_APP, ESP_PARTITION_SUBTYPE_APP_FACTORY, "factory", 0x20000, 0x80000) : NULL;
    static const uint32_t addr[4] = {0xA0000, 0x4A0000, 0x620000, 0x7A0000};
    static const uint32_t size[4] = {0x400000, 0x180000, 0x180000, 0x200000};
    for (int i = 0; i < 4; i++) {
        char name[8];
        snprintf(name, sizeof(name), "ota_%d", i);
        if (!o.all_ota && i == 3) {
            OTA[i] = NULL;
            continue;
        }
        OTA[i] = fake_part_add(ESP_PARTITION_TYPE_APP, (esp_partition_subtype_t)(ESP_PARTITION_SUBTYPE_APP_OTA_MIN + i), name, addr[i], size[i]);
    }
    fake_part_add(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_DATA_NVS, "nvs", 0x9000, 0xF000);
    fake_part_add(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_DATA_NVS, "picker_nvs", 0x1A000, 0x6000);
    fake_nvs_part_add("nvs");
    fake_nvs_part_add("picker_nvs");
    if (o.own_nvs) {
        fake_part_add(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_DATA_NVS, "lfd3k_nvs", 0x9A0000, 0x10000);
        fake_nvs_part_add("lfd3k_nvs");
    }
    fake_set_running(OTA[1]);
    fake_set_boot(OTA[1]);
}

static const layout_opts_t FULL = {true, true, true};

static void images_everywhere(void)
{
    for (int i = 0; i < 4; i++) {
        if (OTA[i]) {
            fake_part_make_image(OTA[i]);
        }
    }
}

/* Runs mode_switch_restart and catches the esp_restart. */
static void do_restart(int mode)
{
    if (setjmp(fake_restart_jmp) == 0) {
        mode_switch_restart(mode);
    }
}

static int ota_index(const esp_partition_t *p)
{
    return p ? (int)p->subtype - (int)ESP_PARTITION_SUBTYPE_APP_OTA_MIN : -1;
}

/* ---- the token the modem writes is what the bootloader accepts ----------- */

static void modem_token_matches_bootloader(void)
{
    static const int mode_slot[4] = {1, 2, 3, 0}; /* LFD3K-SM, SIMIAN, RNODE, MESH */
    for (int mode = 0; mode < 4; mode++) {
        layout(FULL);
        images_everywhere();
        CHECK(mode_switch_available());
        CHECK(mode_switch_prepare(mode));
        CHECK_EQ(ota_index(fake_boot()), mode_slot[mode]);

        rtc_clear();
        do_restart(mode);
        CHECK_EQ(fake_restarts, 1);
        CHECK_EQ(fake_critical_enters, 1);
        CHECK_EQ(fake_critical_depth, 0); /* interrupts back on */

        boot_token_t t = rtc_token();
        CHECK(lfmp_token_valid(&t));
        CHECK_EQ(t.slot, mode_slot[mode]);
        /* The menu order and slots of the picker agree with the modem's. */
        CHECK_EQ(t.slot, pk_mode_ota(mode));
        /* Byte for byte what choose.c would make. */
        boot_token_t want;
        lfmp_token_make(&want, (uint8_t)mode_slot[mode]);
        CHECK(memcmp(&t, &want, sizeof(t)) == 0);

        /* The bootloader's decision on the reset esp_restart() produces. */
        bool clear = false;
        CHECK_EQ(choose_boot(LFMP_RST_CPU0_SW, &t, &clear), BOOT_OTADATA);
        CHECK(clear);
        clear = false;
        CHECK_EQ(choose_boot(LFMP_RST_CORE_SW, &t, &clear), BOOT_OTADATA);
        CHECK(clear);
        /* and the second check against what otadata names (set by prepare). */
        CHECK_EQ(choose_token_target(ota_index(fake_boot()), &t, true), BOOT_OTADATA);
        CHECK_EQ(choose_token_target(ota_index(fake_boot()), &t, false), BOOT_PICKER);
        /* Not valid on a power cycle. */
        clear = false;
        CHECK_EQ(choose_boot(LFMP_RST_POWER_ON, &t, &clear), BOOT_PICKER);
    }
}

static void picker_restart_writes_no_token(void)
{
    layout(FULL);
    images_everywhere();
    CHECK(mode_switch_prepare(MODE_PICKER));
    CHECK_EQ(fake_set_boot_calls, 0); /* otadata untouched */
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);
    rtc_clear();
    do_restart(MODE_PICKER);
    CHECK_EQ(fake_restarts, 1);
    boot_token_t t = rtc_token();
    CHECK(!lfmp_token_valid(&t));
    bool clear = true;
    CHECK_EQ(choose_boot(LFMP_RST_CPU0_SW, &t, &clear), BOOT_PICKER);
    CHECK(!clear);
}

/* ---- refusals leave everything as it was -------------------------------- */

static void prepare_refuses_missing_and_broken_slots(void)
{
    layout(FULL);
    images_everywhere();
    /* ota_2 (SIMIAN RADAR) is empty: erased flash reads as zeros here. */
    memset(OTA[2]->head, 0xFF, sizeof(OTA[2]->head));
    CHECK(!mode_switch_prepare(MODE_RADAR));
    CHECK_EQ(fake_set_boot_calls, 0);
    CHECK(fake_boot() == OTA[1]);
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);

    /* Header faults one at a time. */
    for (int fault = 0; fault < 5; fault++) {
        fake_part_make_image(OTA[3]);
        switch (fault) {
        case 0: OTA[3]->head[0] = 0xE8; break;                         /* image magic */
        case 1: OTA[3]->head[1] = 0; break;                            /* no segments */
        case 2: OTA[3]->head[1] = ESP_IMAGE_MAX_SEGMENTS + 1; break;   /* too many */
        case 3: OTA[3]->head[12] = 5; break;                           /* chip id: not an S3 */
        case 4: OTA[3]->head[0x20] ^= 1; break;                        /* app descriptor magic */
        }
        CHECK(!mode_switch_prepare(MODE_RNODE));
    }
    CHECK_EQ(fake_set_boot_calls, 0);
    CHECK(fake_boot() == OTA[1]);

    /* Not a slot at all. */
    CHECK(!mode_switch_prepare(-1));
    CHECK(!mode_switch_prepare(5));
    CHECK(!mode_switch_prepare(99));
    CHECK_EQ(fake_set_boot_calls, 0);
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);
}

static void prepare_refuses_when_verify_or_otadata_fails(void)
{
    /* otadata change fails: no switch, last mode not recorded (ruling P5-3 is
     * the caller answering ERROR). */
    layout(FULL);
    images_everywhere();
    fake_set_boot_err = ESP_FAIL;
    CHECK(!mode_switch_prepare(MODE_MESH));
    CHECK(fake_boot() == OTA[1]);
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);

    /* otadata already points at the slot, but the image does not verify. */
    layout(FULL);
    images_everywhere();
    fake_set_boot(OTA[2]);
    fake_verify_err = ESP_FAIL;
    CHECK(!mode_switch_prepare(MODE_RADAR));
    CHECK_EQ(fake_verify_calls, 1);
    CHECK_EQ(fake_set_boot_calls, 0);
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);

    /* The same slot verifying: accepted, nothing set again. */
    fake_verify_err = ESP_OK;
    CHECK(mode_switch_prepare(MODE_RADAR));
    CHECK_EQ(fake_set_boot_calls, 0);
}

/* ---- ruling P5-2: AT+MODE records the picker's last mode ---------------- */

static void prepare_records_picker_last_mode(void)
{
    layout(FULL);
    images_everywhere();
    fake_nvs_put_u8("picker_nvs", "picker", "other", 7); /* a neighbour key survives */
    CHECK(mode_switch_prepare(MODE_RNODE));
    uint8_t v = 0xFF;
    CHECK(fake_nvs_get_u8("picker_nvs", "picker", "last", &v));
    CHECK_EQ(v, MODE_RNODE);
    CHECK(fake_nvs_get_u8("picker_nvs", "picker", "other", &v));
    CHECK_EQ(v, 7);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, NULL), 0); /* never erased */
    /* Only the picker's partition was touched. */
    CHECK_EQ(fake_nvs_touches("nvs"), 0);
    CHECK_EQ(fake_nvs_touches("lfd3k_nvs"), 0);
    CHECK_EQ(fake_nvs_ops(OP_DEFAULT_PART, NULL), 0);

    /* Same mode again: no needless flash write. */
    int sets = fake_nvs_ops(OP_SET, "picker_nvs");
    CHECK(mode_switch_prepare(MODE_RNODE));
    CHECK_EQ(fake_nvs_ops(OP_SET, "picker_nvs"), sets);

    /* A different mode overwrites it. */
    CHECK(mode_switch_prepare(MODE_MESH));
    CHECK(fake_nvs_get_u8("picker_nvs", "picker", "last", &v));
    CHECK_EQ(v, MODE_MESH);

    /* The picker target records nothing. */
    sets = fake_nvs_ops(OP_SET, "picker_nvs");
    CHECK(mode_switch_prepare(MODE_PICKER));
    CHECK_EQ(fake_nvs_ops(OP_SET, "picker_nvs"), sets);
    CHECK(fake_nvs_get_u8("picker_nvs", "picker", "last", &v));
    CHECK_EQ(v, MODE_MESH);
}

static void picker_nvs_trouble_never_blocks_or_wipes(void)
{
    /* picker_nvs full or a newer format: the switch still goes, the partition
     * is left alone (it belongs to the picker). */
    layout(FULL);
    images_everywhere();
    fake_nvs_part_fail_init("picker_nvs", ESP_ERR_NVS_NO_FREE_PAGES);
    CHECK(mode_switch_prepare(MODE_MESH));
    CHECK(ota_index(fake_boot()) == 0);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "picker_nvs"), 0);
    CHECK_EQ(fake_nvs_ops(OP_SET, "picker_nvs"), 0);
    CHECK(fake_log_warnings > 0);
    CHECK_EQ(fake_nvs_entries("picker_nvs"), 0);
}

/* ---- which slots hold an image / is the layout multimode ---------------- */

static void slots_mask_and_availability(void)
{
    layout(FULL);
    fake_part_make_image(OTA[2]);
    fake_part_make_image(OTA[0]);
    /* ota_1 (this modem) counts although its fake header is blank; ota_3 is empty. */
    CHECK_EQ(mode_switch_slots(), 0x0B); /* MODEM, RADAR (ota_2), MESH (ota_0) */
    CHECK(mode_switch_available());
    fake_part_make_image(OTA[3]);
    CHECK_EQ(mode_switch_slots(), 0x0F);

    /* Running from another slot: the token names ota_1, so not available. */
    fake_set_running(OTA[2]);
    CHECK(!mode_switch_available());
    CHECK_EQ(mode_switch_slots() & 1, 0); /* modem bit only for the running ota_1 */

    /* Missing pieces of the multimode layout. */
    layout((layout_opts_t){false, true, true});
    CHECK(!mode_switch_available()); /* no factory (no picker) */
    layout((layout_opts_t){true, false, true});
    CHECK(!mode_switch_available()); /* no lfd3k_nvs: the single app table */
    layout((layout_opts_t){true, true, false});
    CHECK(!mode_switch_available()); /* a mode slot missing */
}

/* ---- modem_uses_own_nvs_partition --------------------------------------- */

static const uint8_t OLD_SETTINGS[40] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10};
static const uint8_t OLD_DIR[100] = {0xD1, 0xD2, 0xD3};

static void seed_shared_with_other_modes(void)
{
    /* What RNode, Simian and Meshtastic keep in the shared nvs. */
    fake_nvs_put_blob("nvs", "rnode", "eeprom", "ROMDATA", 7);
    fake_nvs_put_blob("nvs", "nvs.net80211", "sta.ssid", "HOMEWIFI", 8);
    fake_nvs_put_u8("nvs", "meshtastic", "region", 3);
}

static void shared_untouched_except_two_keys(void)
{
    uint8_t v;
    char buf[16];
    size_t n = sizeof(buf);
    CHECK(fake_nvs_get_blob("nvs", "rnode", "eeprom", buf, &n) && n == 7 && memcmp(buf, "ROMDATA", 7) == 0);
    n = sizeof(buf);
    CHECK(fake_nvs_get_blob("nvs", "nvs.net80211", "sta.ssid", buf, &n) && n == 8);
    CHECK(fake_nvs_get_u8("nvs", "meshtastic", "region", &v) && v == 3);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "nvs"), 0);
    CHECK_EQ(fake_nvs_ops(OP_DEFAULT_PART, NULL), 0);
    CHECK_EQ(fake_nvs_ops(OP_SET, "nvs"), 0); /* nothing is ever written to it */
}

static void modem_uses_own_nvs_partition(void)
{
    layout(FULL);
    seed_shared_with_other_modes();
    fake_nvs_put_blob("nvs", "lfd3k", "settings", OLD_SETTINGS, sizeof(OLD_SETTINGS));
    fake_nvs_put_blob("nvs", "lfd3k", "dir", OLD_DIR, sizeof(OLD_DIR));

    CHECK(settings_store_init());

    /* First boot under the new layout: the old blobs moved once. */
    uint8_t buf[1100];
    size_t n = 0;
    CHECK(fake_nvs_get_blob("lfd3k_nvs", "lfd3k", "settings", buf, &n) && n == sizeof(OLD_SETTINGS));
    CHECK(memcmp(buf, OLD_SETTINGS, n) == 0);
    CHECK(fake_nvs_get_blob("lfd3k_nvs", "lfd3k", "dir", buf, &n) && n == sizeof(OLD_DIR));
    CHECK(memcmp(buf, OLD_DIR, n) == 0);
    CHECK(!fake_nvs_has("nvs", "lfd3k", "settings"));
    CHECK(!fake_nvs_has("nvs", "lfd3k", "dir"));
    shared_untouched_except_two_keys();
    CHECK_EQ(fake_nvs_ops(OP_ERASE_KEY, "nvs"), 2);
    CHECK_EQ(fake_nvs_ops_ns(OP_OPEN_RW, "nvs", "lfd3k"), 1); /* only to erase those keys */
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);

    /* Ordinary use: load and save go to lfd3k_nvs and nowhere else. */
    size_t got = 0;
    CHECK(settings_store_load(buf, sizeof(buf), &got) && got == sizeof(OLD_SETTINGS));
    uint8_t newblob[64];
    memset(newblob, 0x5A, sizeof(newblob));
    CHECK(settings_store_save(newblob, sizeof(newblob)));
    CHECK(settings_store_save_dir(newblob, 30));
    CHECK(settings_store_load(buf, sizeof(buf), &got) && got == 64 && buf[0] == 0x5A);
    CHECK(settings_store_load_dir(buf, sizeof(buf), &got) && got == 30);
    CHECK(fake_nvs_get_blob("lfd3k_nvs", "lfd3k", "settings", NULL, &got) && got == 64);
    shared_untouched_except_two_keys();
    CHECK(!fake_nvs_has("nvs", "lfd3k", "settings"));

    /* A later boot with settings present: the shared partition is not even
     * initialised, let alone opened. */
    int shared_touches = fake_nvs_touches("nvs");
    CHECK(settings_store_init());
    CHECK_EQ(fake_nvs_touches("nvs"), shared_touches);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, NULL), 0);
}

static void migration_leaves_shared_alone_when_it_cannot_read_it(void)
{
    /* The shared partition is empty or in a format we do not know (another
     * mode has not run yet): never erased, never initialised by us beyond a
     * failed attempt, and our own settings still work. */
    layout(FULL);
    fake_nvs_part_fail_init("nvs", ESP_ERR_NVS_NO_FREE_PAGES);
    CHECK(settings_store_init());
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "nvs"), 0);
    uint8_t b[8] = {1};
    size_t n;
    CHECK(!settings_store_load(b, sizeof(b), &n));
    CHECK(settings_store_save(b, sizeof(b)));
    CHECK(settings_store_load(b, sizeof(b), &n) && n == 8);

    layout(FULL);
    fake_nvs_part_fail_init("nvs", ESP_ERR_NVS_NEW_VERSION_FOUND);
    CHECK(settings_store_init());
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "nvs"), 0);
}

static void migration_without_old_namespace_creates_nothing(void)
{
    layout(FULL);
    seed_shared_with_other_modes(); /* no "lfd3k" namespace there */
    CHECK(settings_store_init());
    CHECK_EQ(fake_nvs_ops_ns(OP_OPEN_RW, "nvs", "lfd3k"), 0);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_KEY, NULL), 0);
    CHECK_EQ(fake_nvs_entries("lfd3k_nvs"), 0);
    shared_untouched_except_two_keys();
    CHECK_EQ(fake_nvs_entries("nvs"), 3);
}

static void migration_copies_only_what_exists_and_fits(void)
{
    /* Settings only, no directory. */
    layout(FULL);
    fake_nvs_put_blob("nvs", "lfd3k", "settings", OLD_SETTINGS, sizeof(OLD_SETTINGS));
    CHECK(settings_store_init());
    CHECK(fake_nvs_has("lfd3k_nvs", "lfd3k", "settings"));
    CHECK(!fake_nvs_has("lfd3k_nvs", "lfd3k", "dir"));
    CHECK(!fake_nvs_has("nvs", "lfd3k", "settings"));
    CHECK_EQ(fake_nvs_ops(OP_ERASE_KEY, "nvs"), 1);

    /* A directory blob of exactly the 1024 byte buffer moves ... */
    layout(FULL);
    static uint8_t big[1100];
    for (size_t i = 0; i < sizeof(big); i++) {
        big[i] = (uint8_t)(i * 7);
    }
    fake_nvs_put_blob("nvs", "lfd3k", "settings", OLD_SETTINGS, sizeof(OLD_SETTINGS));
    fake_nvs_put_blob("nvs", "lfd3k", "dir", big, 1024);
    CHECK(settings_store_init());
    size_t n = 0;
    static uint8_t out[1100];
    CHECK(fake_nvs_get_blob("lfd3k_nvs", "lfd3k", "dir", out, &n) && n == 1024 && memcmp(out, big, 1024) == 0);
    CHECK(!fake_nvs_has("nvs", "lfd3k", "dir"));

    /* ... one that does not fit stays where it is, unharmed, not half moved. */
    layout(FULL);
    fake_nvs_put_blob("nvs", "lfd3k", "settings", OLD_SETTINGS, sizeof(OLD_SETTINGS));
    fake_nvs_put_blob("nvs", "lfd3k", "dir", big, 1100);
    CHECK(settings_store_init());
    CHECK(fake_nvs_has("nvs", "lfd3k", "dir"));
    CHECK(!fake_nvs_has("lfd3k_nvs", "lfd3k", "dir"));
    CHECK(fake_nvs_has("lfd3k_nvs", "lfd3k", "settings"));
}

static void own_partition_trouble_only_ever_erases_itself(void)
{
    layout(FULL);
    seed_shared_with_other_modes();
    fake_nvs_put_blob("lfd3k_nvs", "lfd3k", "settings", "old", 3);
    fake_nvs_part_fail_init("lfd3k_nvs", ESP_ERR_NVS_NO_FREE_PAGES);
    CHECK(settings_store_init());
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "lfd3k_nvs"), 1);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "nvs"), 0);
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "picker_nvs"), 0);
    shared_untouched_except_two_keys();

    /* Cannot be initialised at all (init keeps failing): reported, no saves. */
    layout(FULL);
    fake_nvs_part_fail_init("lfd3k_nvs", ESP_FAIL);
    CHECK(!settings_store_init());
    CHECK_EQ(fake_nvs_ops(OP_ERASE_PART, "nvs"), 0);
}

static void single_app_table_keeps_the_default_nvs(void)
{
    /* The modem alone (heltec_v4_single): no lfd3k_nvs, so "nvs" is its own. */
    layout((layout_opts_t){false, false, true});
    CHECK(settings_store_init());
    uint8_t b[8] = {9};
    size_t n;
    CHECK(settings_store_save(b, sizeof(b)));
    CHECK(settings_store_load(b, sizeof(b), &n) && n == 8);
    CHECK(fake_nvs_has("nvs", "lfd3k", "settings"));
    CHECK_EQ(fake_nvs_touches("lfd3k_nvs"), 0);
    CHECK_EQ(fake_nvs_touches("picker_nvs"), 0);
    CHECK_EQ(fake_nvs_ops(OP_DEFAULT_PART, NULL), 0);
}

static void no_default_partition_calls_in_the_modem_sources(void)
{
    /* Belt and braces next to the fake: nothing in the modem's NVS code may
     * call the default-partition APIs. */
    static const char *files[] = {"/../firmware/src/settings_store.c", "/../firmware/src/mode_switch.c"};
    for (size_t i = 0; i < 2; i++) {
        char path[512];
        snprintf(path, sizeof(path), "%s%s", SRC_DIR, files[i]);
        FILE *f = fopen(path, "rb");
        CHECK(f != NULL);
        if (!f) {
            continue;
        }
        static char src[65536];
        size_t n = fread(src, 1, sizeof(src) - 1, f);
        fclose(f);
        src[n] = 0;
        CHECK(strstr(src, "nvs_flash_init()") == NULL);
        CHECK(strstr(src, "nvs_flash_erase()") == NULL);
        CHECK(strstr(src, " nvs_open(") == NULL);
        CHECK(strstr(src, "(nvs_open(") == NULL);
    }
}

static void gen_ap_pass_alphabet(void)
{
    for (int round = 0; round < 50; round++) {
        char p[13];
        memset(p, '!', sizeof(p));
        settings_store_gen_ap_pass(p);
        CHECK_EQ(p[12], 0);
        for (int i = 0; i < 12; i++) {
            CHECK(p[i] != 0 && strchr("ABCDEFGHJKLMNPQRSTUVWXYZ23456789", p[i]) != NULL);
        }
    }
}

/* ---- the picker and the modem agree on the picker's NVS names ----------- */

static bool define_of(const char *src, const char *name, char *out, size_t cap)
{
    char key[64];
    snprintf(key, sizeof(key), "#define %s ", name);
    const char *p = strstr(src, key);
    if (!p) {
        return false;
    }
    p = strchr(p + strlen(key), '"');
    if (!p) {
        return false;
    }
    const char *q = strchr(p + 1, '"');
    if (!q || (size_t)(q - p - 1) >= cap) {
        return false;
    }
    memcpy(out, p + 1, (size_t)(q - p - 1));
    out[q - p - 1] = 0;
    return true;
}

static void picker_and_modem_agree_on_last_mode_key(void)
{
    static char a[65536], b[65536];
    char path[512];
    snprintf(path, sizeof(path), "%s/picker/src/main.c", SRC_DIR);
    FILE *f = fopen(path, "rb");
    CHECK(f != NULL);
    if (!f) {
        return;
    }
    a[fread(a, 1, sizeof(a) - 1, f)] = 0;
    fclose(f);
    snprintf(path, sizeof(path), "%s/../firmware/src/mode_switch.c", SRC_DIR);
    f = fopen(path, "rb");
    CHECK(f != NULL);
    if (!f) {
        return;
    }
    b[fread(b, 1, sizeof(b) - 1, f)] = 0;
    fclose(f);
    const char *names[] = {"PICKER_NVS_PART", "PICKER_NVS_NS", "PICKER_NVS_KEY"};
    const char *want[] = {"picker_nvs", "picker", "last"};
    for (int i = 0; i < 3; i++) {
        char x[64] = "", y[64] = "";
        CHECK(define_of(a, names[i], x, sizeof(x)));
        CHECK(define_of(b, names[i], y, sizeof(y)));
        CHECK(strcmp(x, y) == 0);
        CHECK(strcmp(x, want[i]) == 0);
    }
}

int main(void)
{
    map_rtc();
    RUN(modem_token_matches_bootloader);
    RUN(picker_restart_writes_no_token);
    RUN(prepare_refuses_missing_and_broken_slots);
    RUN(prepare_refuses_when_verify_or_otadata_fails);
    RUN(prepare_records_picker_last_mode);
    RUN(picker_nvs_trouble_never_blocks_or_wipes);
    RUN(slots_mask_and_availability);
    RUN(modem_uses_own_nvs_partition);
    RUN(migration_leaves_shared_alone_when_it_cannot_read_it);
    RUN(migration_without_old_namespace_creates_nothing);
    RUN(migration_copies_only_what_exists_and_fits);
    RUN(own_partition_trouble_only_ever_erases_itself);
    RUN(single_app_table_keeps_the_default_nvs);
    RUN(no_default_partition_calls_in_the_modem_sources);
    RUN(gen_ap_pass_alphabet);
    RUN(picker_and_modem_agree_on_last_mode_key);
    TAP_DONE();
}
