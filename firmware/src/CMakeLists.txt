idf_component_register(SRCS "main.c" "hal_radio.c" "usb_link.c" "panel.c" "panel_render.c" "ssd1306.c"
                       INCLUDE_DIRS "." "../include"
                       REQUIRES driver esp_driver_i2c esp_driver_gpio esp_timer esp_system esp_hw_support esp_driver_usb_serial_jtag)

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
