cmake_minimum_required(VERSION 3.22.1)
project(lfd3kvoice LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(GLUE "${ANDROID_NDK}/sources/android/native_app_glue")
add_library(native_app_glue STATIC "${GLUE}/android_native_app_glue.c")
target_include_directories(native_app_glue PUBLIC "${GLUE}")

set(IMGUI "${CMAKE_CURRENT_SOURCE_DIR}/third_party/imgui")
add_library(lfd3kvoice SHARED
    main.cpp
    ui.cpp
    audio.cpp
    usb.cpp
    jni_host.cpp
    "${IMGUI}/imgui.cpp"
    "${IMGUI}/imgui_draw.cpp"
    "${IMGUI}/imgui_tables.cpp"
    "${IMGUI}/imgui_widgets.cpp"
    "${IMGUI}/backends/imgui_impl_android.cpp"
    "${IMGUI}/backends/imgui_impl_opengl3.cpp"
)
target_include_directories(lfd3kvoice PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}"
    "${IMGUI}"
    "${IMGUI}/backends"
)
target_compile_definitions(lfd3kvoice PRIVATE IMGUI_IMPL_OPENGL_ES3)
target_compile_options(lfd3kvoice PRIVATE -Wall -Wextra -Wno-unused-parameter)
set_source_files_properties(
    "${IMGUI}/imgui.cpp"
    "${IMGUI}/imgui_draw.cpp"
    "${IMGUI}/imgui_tables.cpp"
    "${IMGUI}/imgui_widgets.cpp"
    "${IMGUI}/backends/imgui_impl_android.cpp"
    "${IMGUI}/backends/imgui_impl_opengl3.cpp"
    PROPERTIES COMPILE_OPTIONS "-w")

# Six levels up from src/main/cpp is the repo root. cargo-ndk writes the
# static library under the Rust target triple, not the Android ABI name.
if(ANDROID_ABI STREQUAL "arm64-v8a")
    set(RUST_TARGET aarch64-linux-android)
elseif(ANDROID_ABI STREQUAL "armeabi-v7a")
    set(RUST_TARGET armv7-linux-androideabi)
elseif(ANDROID_ABI STREQUAL "x86_64")
    set(RUST_TARGET x86_64-linux-android)
elseif(ANDROID_ABI STREQUAL "x86")
    set(RUST_TARGET i686-linux-android)
else()
    message(FATAL_ERROR "no Rust target for ABI ${ANDROID_ABI}")
endif()
set(RUST_LIB "${CMAKE_CURRENT_SOURCE_DIR}/../../../../../../target/${RUST_TARGET}/release/liblfd3k_voice_android.a")
if(NOT EXISTS "${RUST_LIB}")
    message(FATAL_ERROR "Rust static library not built yet: ${RUST_LIB}")
endif()
add_library(lfd3k_engine STATIC IMPORTED)
set_target_properties(lfd3k_engine PROPERTIES IMPORTED_LOCATION "${RUST_LIB}")

find_library(log-lib log)
find_library(android-lib android)
find_library(egl-lib EGL)
find_library(gles-lib GLESv3)
find_library(aaudio-lib aaudio)

target_link_libraries(lfd3kvoice
    native_app_glue
    lfd3k_engine
    ${android-lib}
    ${log-lib}
    ${egl-lib}
    ${gles-lib}
    ${aaudio-lib}
    m
    dl
)
target_link_options(lfd3kvoice PRIVATE "-u" "ANativeActivity_onCreate")
