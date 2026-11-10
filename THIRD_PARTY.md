# Third-party crates

| Crate | Version | License | Used in |
|-------|---------|---------|---------|

No third-party crates yet: core, sim and ffi have no dependencies outside the workspace.

## Toolchain and firmware components (not crates)

| Component | Version | License | Used in |
|-----------|---------|---------|---------|
| Rust `core` and `compiler_builtins` (built with `-Zbuild-std=core`) | esp toolchain 1.97.0.0 | MIT or Apache-2.0 | ffi device build, linked into the firmware |
| ESP-IDF (via PlatformIO `framework-espidf`) | 5.4.1 (platform espressif32 6.11.0) | Apache-2.0 (bundled components carry their own permissive licenses) | firmware |
| FreeRTOS kernel (part of ESP-IDF) | as bundled | MIT | firmware |
