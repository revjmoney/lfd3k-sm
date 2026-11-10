idf_component_register(SRCS "main.c" "hal_radio.c" "usb_link.c" "panel.c" "panel_render.c" "ssd1306.c"
                       INCLUDE_DIRS "." "../include"
                       REQUIRES driver esp_driver_i2c esp_driver_gpio esp_timer esp_system esp_hw_support esp_driver_usb_serial_jtag)

# Rust core, built by build_core.ps1 (cargo +esp, xtensa-esp32s3-none-elf).
set(LFD3K_CORE_LIB "${CMAKE_CURRENT_LIST_DIR}/../lib/liblfd3k_ffi.a")
if(NOT EXISTS "${LFD3K_CORE_LIB}")
    message(FATAL_ERROR "Missing ${LFD3K_CORE_LIB}: run firmware/build_core.ps1 first")
endif()
add_prebuilt_library(lfd3k_ffi "${LFD3K_CORE_LIB}")
target_link_libraries(${COMPONENT_LIB} PRIVATE lfd3k_ffi)
