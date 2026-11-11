# Replaces the stock bootloader main (ESP-IDF bootloader_override mechanism).
# choose.c is the pure decision shared with the picker and the PC tests.
set(LFMP_BOOT_DIR "${CMAKE_CURRENT_LIST_DIR}/../../../bootloader")

idf_component_register(SRCS "bootloader_start.c" "${LFMP_BOOT_DIR}/choose.c"
                       INCLUDE_DIRS "${LFMP_BOOT_DIR}"
                       REQUIRES bootloader bootloader_support)

idf_build_get_property(target IDF_TARGET)

# Use the linker script files from the actual bootloader
set(scripts "${IDF_PATH}/components/bootloader/subproject/main/ld/${target}/bootloader.ld"
            "${IDF_PATH}/components/bootloader/subproject/main/ld/${target}/bootloader.rom.ld")

target_linker_script(${COMPONENT_LIB} INTERFACE "${scripts}")
