# Third-party crates

core and ffi have no dependencies outside the workspace, so nothing below is ever built into the core library or the firmware. sim has two third-party crates: crossterm, a normal dependency used only by the PC tool `lfd3k-pcmodem --raw` (raw console and key events), and proptest, a dev-dependency used by the fuzz tests (`sim/tests/fuzz.rs`) with default features off (`features = ["std"]`). Licenses are as declared in each crate's Cargo.toml (checked with `cargo metadata`).

### crossterm (sim, PC tool only)

Default features off; `events` and `windows` on. Which crates build depends on the PC's OS (`cargo tree -p lfd3k-sim -e normal`).

| Crate | Version | License | Used in |
|-------|---------|---------|---------|
| crossterm | 0.29.0 | MIT | lfd3k-pcmodem `--raw` |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | via crossterm |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | via crossterm |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 | via parking_lot |
| lock_api | 0.4.14 | MIT OR Apache-2.0 | via parking_lot |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 | via lock_api |
| smallvec | 1.16.5 | MIT OR Apache-2.0 | via parking_lot_core |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | via parking_lot_core |
| document-features | 0.2.12 | MIT OR Apache-2.0 | crossterm build time only (proc macro, docs) |
| litrs | 1.0.1 | MIT OR Apache-2.0 | via document-features, build time only |
| crossterm_winapi | 0.9.1 | MIT | Windows only, via crossterm |
| winapi | 0.3.9 | MIT/Apache-2.0 | Windows only, via crossterm |
| winapi-x86_64-pc-windows-gnu, winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 | Windows GNU targets only, via winapi |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | Windows only, via parking_lot_core |
| mio | 1.2.4 | MIT | Unix only, via crossterm |
| signal-hook | 0.3.18 | Apache-2.0/MIT | Unix only, via crossterm |
| signal-hook-mio | 0.2.5 | MIT OR Apache-2.0 | Unix only, via crossterm |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 | Unix only, via signal-hook |
| rustix | 1.1.6 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT (used under MIT) | Unix only, via crossterm |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT (used under MIT) | Linux only, via rustix |
| libc | 0.2.191 | MIT OR Apache-2.0 | Unix only, via mio, signal-hook and parking_lot_core |
| errno | 0.3.14 | MIT OR Apache-2.0 | Unix only, via signal-hook-registry |
| log | 0.4.35 | MIT OR Apache-2.0 | Unix only, via mio |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT (used under MIT) | WASI only (never built here), via mio |
| redox_syscall | 0.5.18 | MIT | Redox only (never built here), via parking_lot_core |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | in Cargo.lock as a Windows dependency of errno, mio and rustix, which crossterm only uses on Unix, so it is not built here |

### proptest (sim tests only)

It and everything it pulls in are dev-only for proptest: none of it is built into the core library or the firmware. bitflags and cfg-if (and libc on Unix) are also built into the sim binary, through crossterm (table above).

| Crate | Version | License | Used in |
|-------|---------|---------|---------|
| proptest | 1.12.0 | MIT OR Apache-2.0 | sim tests only (dev) |
| rand | 0.10.3 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| rand_core | 0.10.1 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| rand_xorshift | 0.5.0 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 | sim tests only (dev, via chacha20) |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | sim tests only (dev, via rand) |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | sim tests (dev, via chacha20 and getrandom); also crossterm, above |
| libc | 0.2.191 | MIT OR Apache-2.0 | sim tests (dev, via cpufeatures and getrandom on some targets); also crossterm on Unix, above |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later (used under MIT OR Apache-2.0) | sim tests only (dev, via getrandom on UEFI targets; not built on Windows) |
| core_detect | 1.0.0 | MIT/Apache-2.0 | sim tests only (dev, via proptest) |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | sim tests (dev, via proptest); also crossterm, above |
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

## Fonts (rendered glyph bitmaps only)

| Font | License | Used in |
|------|---------|---------|
| Noto Sans SC, Noto Sans JP, Noto Sans KR, Noto Sans Devanagari, Noto Sans Bengali, Noto Sans Arabic, Noto Sans Hebrew, Noto Sans Thai | SIL Open Font License 1.1 | rendered glyph bitmaps only: the idle show words in `firmware/src/idle_words.h`, made on the PC by `tools/gen_idle_words.py`; the font files stay in `vendor/fonts/` (git-ignored, not in the repo or the firmware). Copyright 2022 The Noto Project Authors (Arabic, Devanagari, Thai; Bengali 2025 and Hebrew 2024, same holder). The CJK fonts (SC, JP, KR) carry Copyright 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'. Licence text: `vendor/fonts/OFL.txt` and https://openfontlicense.org |

## PC tools (not shipped)

These run on Rev's PC only and are never built into the firmware or the core. Installed by Rev, not vendored in the repo.

| Tool | Version | License | Used in |
|------|---------|---------|---------|
| pyserial | 3.5 | BSD-3-Clause | `tools/lfd3k_term.py` (bench terminal), installed with pip |
| Pillow with libraqm | 12.3 | MIT-CMU (Pillow; its bundled libraqm, FreeType, HarfBuzz and FriBiDi carry their own licenses; PC only, nothing of it is linked into the firmware) | `tools/gen_idle_words.py` (shapes and renders the script words) and `tools/idle_preview.py` (GIF and contact sheet) |
| MinGW gcc | as installed (MSYS2 mingw64) | GPL with the GCC runtime library exception | builds `tools/panel_check` only; nothing it produces is shipped |
