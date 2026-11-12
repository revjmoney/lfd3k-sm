# The multimode launch token (struct, address, CRC) is shared with the
# bootloader override and the picker: choose.c and choose.h by path, never copied.
set(LFMP_BOOT_DIR "${CMAKE_CURRENT_LIST_DIR}/../../multimode/bootloader")
# The AXIS boot logo (single app layout) is the picker's own drawing code,
# compiled in by path, never copied.
set(LFMP_PICKER_DIR "${CMAKE_CURRENT_LIST_DIR}/../../multimode/picker/src")

# Board: V4 by default; -DLFD3K_BOARD=V3 (the heltec_v3 envs) builds for the
# WiFi LoRa 32 V3 (board.h): no front end, the host link on UART0.
if(LFD3K_BOARD STREQUAL "V3")
    set(LFD3K_LINK_SRC "uart_link.c")
    set(LFD3K_LINK_REQ esp_driver_uart)
else()
    set(LFD3K_LINK_SRC "usb_link.c")
    set(LFD3K_LINK_REQ esp_driver_usb_serial_jtag)
endif()

idf_component_register(SRCS "main.c" "hal_radio.c" "${LFD3K_LINK_SRC}" "panel.c" "panel_render.c" "idle_show.c" "ssd1306.c" "settings_store.c" "wifi_link.c" "mdns_link.c" "tcp_link.c" "wire_pump.c" "udp_link.c" "mode_switch.c"
                            "${LFMP_BOOT_DIR}/choose.c"
                            "${LFMP_PICKER_DIR}/picker_draw.c" "${LFMP_PICKER_DIR}/picker_menu.c"
                       INCLUDE_DIRS "." "../include" "${LFMP_BOOT_DIR}" "${LFMP_PICKER_DIR}"
                       REQUIRES driver esp_driver_i2c esp_driver_gpio esp_timer esp_system esp_hw_support ${LFD3K_LINK_REQ} nvs_flash esp_partition app_update esp_wifi esp_netif esp_event lwip esp_driver_tsens
                       PRIV_REQUIRES bootloader_support)

# Rust core, built by build_core.ps1 (cargo +esp, xtensa-esp32s3-none-elf).
set(LFD3K_CORE_LIB "${CMAKE_CURRENT_LIST_DIR}/../lib/liblfd3k_ffi.a")
if(NOT EXISTS "${LFD3K_CORE_LIB}")
    message(FATAL_ERROR "Missing ${LFD3K_CORE_LIB}: run firmware/build_core.ps1 first")
endif()
# Fail loudly if any Rust source is newer than the prebuilt library, so a
# stale core never gets flashed by accident.
file(GLOB_RECURSE LFD3K_RUST_SRCS
     "${CMAKE_CURRENT_LIST_DIR}/../../core/src/*.rs"
     "${CMAKE_CURRENT_LIST_DIR}/../../ffi/src/*.rs"
     "${CMAKE_CURRENT_LIST_DIR}/../../sx1262/src/*.rs")
foreach(rs IN LISTS LFD3K_RUST_SRCS)
    if("${rs}" IS_NEWER_THAN "${LFD3K_CORE_LIB}")
        message(FATAL_ERROR "Stale Rust core: ${rs} is newer than ${LFD3K_CORE_LIB}. Run firmware/build_core.ps1 first, then build again.")
    endif()
endforeach()
add_prebuilt_library(lfd3k_ffi "${LFD3K_CORE_LIB}")
# A new .a must relink the app.
set_property(TARGET ${COMPONENT_LIB} APPEND PROPERTY LINK_DEPENDS "${LFD3K_CORE_LIB}")
# Re-run configure (and so this check) whenever the library or a Rust source changes.
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${LFD3K_CORE_LIB}" ${LFD3K_RUST_SRCS})
target_link_libraries(${COMPONENT_LIB} PRIVATE lfd3k_ffi)
if(LFD3K_BOARD STREQUAL "V3")
    target_compile_definitions(${COMPONENT_LIB} PRIVATE LFD3K_BOARD_V3)
endif()
