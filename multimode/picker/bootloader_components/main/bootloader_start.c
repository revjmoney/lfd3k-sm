/*
 * LFD3K-SM multimode bootloader main.
 *
 * Based on ESP-IDF v5.4.1 components/bootloader/subproject/main/bootloader_start.c
 * (SPDX-FileCopyrightText: 2015-2022 Espressif Systems (Shanghai) CO LTD,
 * SPDX-License-Identifier: Apache-2.0), via the documented
 * custom_bootloader/bootloader_override mechanism.
 *
 * Same stock steps (bootloader_init, load partition table, otadata selection,
 * load boot image); the only change is which index gets loaded:
 * the picker (factory) on every boot, unless this boot is a deep-sleep wake or
 * a software reset that carries the picker's launch token. See choose.c.
 */
#include <stdbool.h>
#include <stdint.h>
#include "esp_log.h"
#include "esp_rom_sys.h"
#include "esp_assert.h"
#include "soc/reset_reasons.h"
#include "soc/soc.h"
#include "bootloader_init.h"
#include "bootloader_utility.h"
#include "bootloader_common.h"
#include "choose.h"

#if CONFIG_BOOTLOADER_SKIP_VALIDATE_IN_DEEP_SLEEP || CONFIG_BOOTLOADER_RESERVE_RTC_MEM
#error "multimode bootloader: keep BOOTLOADER_RESERVE_RTC_MEM off (it rewrites the top of RTC FAST on every boot, which other modes own)"
#endif
#if CONFIG_BOOTLOADER_FACTORY_RESET || CONFIG_BOOTLOADER_APP_TEST
#error "multimode bootloader: the GPIO factory reset and test app options are not used"
#endif

/* choose.h carries the reset reasons as plain numbers; keep them honest. */
ESP_STATIC_ASSERT(LFMP_RST_POWER_ON == RESET_REASON_CHIP_POWER_ON, "reset reason");
ESP_STATIC_ASSERT(LFMP_RST_CORE_SW == RESET_REASON_CORE_SW, "reset reason");
ESP_STATIC_ASSERT(LFMP_RST_DEEP_SLEEP == RESET_REASON_CORE_DEEP_SLEEP, "reset reason");
ESP_STATIC_ASSERT(LFMP_RST_CPU0_SW == RESET_REASON_CPU0_SW, "reset reason");
ESP_STATIC_ASSERT(sizeof(boot_token_t) == 12, "token layout");
ESP_STATIC_ASSERT(LFMP_TOKEN_ADDR >= SOC_RTC_DRAM_LOW && LFMP_TOKEN_ADDR + 12 <= SOC_RTC_DRAM_HIGH - 24,
                  "token must sit inside RTC FAST, below the RTC timer data");
ESP_STATIC_ASSERT((LFMP_TOKEN_ADDR & 3) == 0, "token must be word aligned");

static const char *TAG = "boot";

static int select_partition_number(bootloader_state_t *bs, boot_choice_t choice);

/* Reads the token with 32 bit accesses and decides; clears it when told to. */
static boot_choice_t read_and_consume_token(void)
{
    volatile uint32_t *rtc = (volatile uint32_t *)LFMP_TOKEN_ADDR;
    uint32_t w[3] = {rtc[0], rtc[1], rtc[2]};
    boot_token_t tok;
    __builtin_memcpy(&tok, w, sizeof(tok));

    uint32_t reason = (uint32_t)esp_rom_get_reset_reason(0);
    bool clear = false;
    boot_choice_t choice = choose_boot(reason, &tok, &clear);
    if (clear) {
        rtc[0] = 0;
        rtc[1] = 0;
        rtc[2] = 0;
    }
    ESP_LOGI(TAG, "multimode: reset %u, token %s, %s", (unsigned)reason, clear ? "used" : "none",
             choice == BOOT_PICKER ? "picker" : "otadata");
    return choice;
}

void __attribute__((noreturn)) call_start_cpu0(void)
{
    // 1. Hardware initialization
    if (bootloader_init() != ESP_OK) {
        bootloader_reset();
    }

    // 1.2 Multimode: consume the launch token before anything else runs.
    boot_choice_t choice = read_and_consume_token();

    // 2. Select the number of boot partition
    bootloader_state_t bs = {0};
    int boot_index = select_partition_number(&bs, choice);
    if (boot_index == INVALID_INDEX) {
        bootloader_reset();
    }

    // 3. Load the app image for booting
    bootloader_utility_load_boot_image(&bs, boot_index);
}

static int select_partition_number(bootloader_state_t *bs, boot_choice_t choice)
{
    // 1. Load partition table
    if (!bootloader_utility_load_partition_table(bs)) {
        ESP_LOGE(TAG, "load partition table error!");
        return INVALID_INDEX;
    }

    // 2. Picker unless the token or a deep-sleep wake says otherwise.
    if (choice == BOOT_PICKER) {
        if (bs->factory.offset != 0) {
            return FACTORY_INDEX;
        }
        ESP_LOGE(TAG, "multimode: no factory (picker) partition, using otadata");
    }
    return bootloader_utility_get_selected_boot_partition(bs);
}

// Return global reent struct if any newlib functions are linked to bootloader
struct _reent *__getreent(void)
{
    return _GLOBAL_REENT;
}
