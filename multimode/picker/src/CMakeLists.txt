# Picker app. The SSD1306 driver, the 5x7 font and the pin map are shared with
# the LFD3K-SM firmware by path (not copied); choose.c is shared with the
# bootloader override.
set(LFD3K_FW_SRC "${CMAKE_CURRENT_LIST_DIR}/../../../firmware/src")
set(LFMP_BOOT_DIR "${CMAKE_CURRENT_LIST_DIR}/../../bootloader")

idf_component_register(SRCS "main.c" "picker_menu.c" "picker_draw.c"
                            "${LFD3K_FW_SRC}/ssd1306.c" "${LFMP_BOOT_DIR}/choose.c"
                       INCLUDE_DIRS "." "${LFD3K_FW_SRC}" "${LFMP_BOOT_DIR}"
                       REQUIRES driver esp_driver_i2c esp_driver_gpio esp_timer esp_system
                                esp_partition nvs_flash app_update bootloader_support log)
