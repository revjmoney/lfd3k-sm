# Third-party crates

core and ffi have no dependencies outside the workspace. The only third-party crate is proptest, a dev-dependency of sim used by the fuzz tests (`sim/tests/fuzz.rs`), with default features off (`features = ["std"]`). It and everything it pulls in are dev-only: none of it is built into the sim binary, the core library or the firmware. Licenses are as declared in each crate's Cargo.toml.

| Crate | Version | License | Used in |
|-------|---------|---------|---------|
| proptest | 1.12.0 | MIT OR Apache-2.0 | sim tests only (dev) |
| rand | 0.10.3 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| rand_core | 0.10.1 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| rand_xorshift | 0.5.0 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 | sim tests only (dev, via chacha20) |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | sim tests only (dev, via rand) |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | sim tests only (dev, via chacha20 and getrandom) |
| libc | 0.2.191 | MIT OR Apache-2.0 | sim tests only (dev, via cpufeatures and getrandom on some targets) |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later (used under MIT OR Apache-2.0) | sim tests only (dev, via getrandom on UEFI targets; not built on Windows) |
| core_detect | 1.0.0 | MIT/Apache-2.0 | sim tests only (dev, via proptest) |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| autocfg | 1.5.1 | Apache-2.0 OR MIT | sim tests only (dev, build script of num-traits) |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| unarray | 0.1.4 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |

## Toolchain and firmware components (not crates)

| Component | Version | License | Used in |
|-----------|---------|---------|---------|
| Rust `core` and `compiler_builtins` (built with `-Zbuild-std=core`) | esp toolchain 1.97.0.0 | MIT or Apache-2.0 | ffi device build, linked into the firmware |
| ESP-IDF (via PlatformIO `framework-espidf`) | 5.4.1 (platform espressif32 6.11.0) | Apache-2.0 (bundled components carry their own permissive licenses) | firmware |
| FreeRTOS kernel (part of ESP-IDF) | as bundled | MIT | firmware |

## PC tools (not shipped)

These run on Rev's PC only and are never built into the firmware or the core. Installed by Rev, not vendored in the repo.

| Tool | Version | License | Used in |
|------|---------|---------|---------|
| pyserial | 3.5 | BSD-3-Clause | `tools/lfd3k_term.py` (bench terminal), installed with pip |
| MinGW gcc | as installed (MSYS2 mingw64) | GPL with the GCC runtime library exception | builds `tools/panel_check` only; nothing it produces is shipped |
