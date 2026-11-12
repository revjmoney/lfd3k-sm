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

It and everything it pulls in are dev-only for proptest (chacha20, cpufeatures, getrandom, rand_core and r-efi are also built into the voice crate, below): none of it is built into the core library or the firmware. bitflags and cfg-if (and libc on Unix) are also built into the sim binary, through crossterm (table above).

| Crate | Version | License | Used in |
|-------|---------|---------|---------|
| proptest | 1.12.0 | MIT OR Apache-2.0 | sim tests only (dev) |
| rand | 0.10.3 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| rand_core | 0.10.1 | MIT OR Apache-2.0 | sim tests (dev, via proptest); also the voice crate, below |
| rand_xorshift | 0.5.0 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 | sim tests (dev, via proptest); also the voice crate, below |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 | sim tests (dev, via chacha20); also the voice crate, below |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | sim tests (dev, via rand); also the voice crate, below |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | sim tests (dev, via chacha20 and getrandom); also crossterm, above |
| libc | 0.2.191 | MIT OR Apache-2.0 | sim tests (dev, via cpufeatures and getrandom on some targets); also crossterm on Unix, above |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later (used under MIT OR Apache-2.0) | sim tests (dev, via getrandom on UEFI targets; not built on Windows); also the voice crate, below |
| core_detect | 1.0.0 | MIT/Apache-2.0 | sim tests only (dev, via proptest) |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | sim tests (dev, via proptest); also crossterm, above |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| autocfg | 1.5.1 | Apache-2.0 OR MIT | sim tests only (dev, build script of num-traits) |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |
| unarray | 0.1.4 | MIT OR Apache-2.0 | sim tests only (dev, via proptest) |

## Voice (the `voice` crate, std; never built into core, ffi or the firmware)

| Component | Version | License | Why |
|-----------|---------|---------|-----|
| Codec 2 (C library, vendored in `voice/codec2/`: codec sources only, no FreeDV modems) | 1.2.0 (drowe67/codec2 tag 1.2.0, archive codec2-1.2.0.tar.gz, 7,688,549 bytes, sha256 cbccae52b2c2ecc5d2757e407da567eb681241ff8dadce39d779a7219dbcf449) | LGPL-2.1 (`voice/codec2/LICENSE`) | Voice codec for SF7 to SF12 (3200 to 700C bps). The codebook C files in `voice/codec2/generated/` were produced from the archive's own `src/codebook/*.txt` by its `generate_codebook.c`; `include/codec2/version.h` is hand-written. Built by `voice/build.rs` with `cc`; on the MSVC target it needs clang-cl (C99 VLAs), found from `LFD3K_CLANG_CL` or PATH. |
| opus (Rust bindings) | 0.4.0 | MIT/Apache-2.0 | Opus codec for SF5 and SF6 |
| opusic-sys (builds the bundled libopus from source) | 0.7.7 | BSD-3-Clause (as declared; libopus itself is BSD-3-Clause) | libopus for the `opus` crate; its build uses cmake |
| cc (build dependency) | 1.11.1 | MIT OR Apache-2.0 | Compiles Codec 2 |
| cmake, find-msvc-tools, shlex (build dependencies) | 0.1.58, 0.1.15, 2.0.1 | MIT OR Apache-2.0 | Build helpers of opusic-sys and cc |

### The FLOWTRON SCRAMBLER (voice crate)

Standard primitives only (spec section 7), from the RustCrypto and dalek projects. chacha20poly1305 has default features off (`alloc`, `zeroize` on); ed25519-dalek has default features off (`fast`, `zeroize`); x25519-dalek has default features off (`precomputed-tables`, `getrandom`, `zeroize`); sha2 has `zeroize` on (wipes hash and HMAC state); hmac has `zeroize` on; spake2 has default features off (`getrandom` on). Licenses as declared in each crate's Cargo.toml (checked with `cargo metadata`). Crates marked "also above" are already listed for another part.

| Crate | Version | License | Why |
|-------|---------|---------|-----|
| x25519-dalek | 3.0.0 | BSD-3-Clause | Per call X25519 key exchange |
| ed25519-dalek | 3.0.0 | BSD-3-Clause | Device and person keys, hello and cert signatures |
| curve25519-dalek | 5.0.0 | BSD-3-Clause | Curve arithmetic, via the two dalek crates |
| curve25519-dalek-derive | 0.1.1 | MIT/Apache-2.0 | Proc macro of curve25519-dalek, build time only |
| fiat-crypto | 0.3.0 | MIT OR Apache-2.0 OR BSD-1-Clause (used under MIT OR Apache-2.0) | Field arithmetic, via curve25519-dalek |
| rustc_version, semver | 0.4.1, 1.0.28 | MIT OR Apache-2.0 | Build script of curve25519-dalek, build time only |
| ed25519, signature | 3.0.0, 3.0.0 | Apache-2.0 OR MIT | Signature types and traits, via ed25519-dalek |
| subtle | 2.6.1 | BSD-3-Clause | Constant time compares, via the dalek crates |
| chacha20poly1305 | 0.11.0 | Apache-2.0 OR MIT | Seals every voice frame (full 16 byte tag) |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 | Stream cipher, via chacha20poly1305 (also above, sim tests) |
| poly1305, universal-hash | 0.9.1, 0.6.1 | Apache-2.0 OR MIT, MIT OR Apache-2.0 | Authenticator, via chacha20poly1305 |
| aead, cipher, inout | 0.6.1, 0.5.2, 0.2.6 | MIT OR Apache-2.0 | AEAD and cipher traits, via chacha20poly1305 |
| hkdf, hmac | 0.13.0, 0.13.0 | MIT OR Apache-2.0 | HKDF-SHA256 for the two direction keys and the link keys; hmac is also a direct dependency for the HMAC-SHA256 key confirm when linking devices (`voice/src/link.rs`) |
| spake2 | 0.5.0-pre.0 (a prerelease, pinned exactly: the stable 0.4 is built on curve25519-dalek 4, sha2 0.10 and hkdf 0.12, which would add a second copy of each) | MIT OR Apache-2.0 | SPAKE2 (Ed25519 group) keyed by the 6 digit code when linking your own devices (spec 7.2, `voice/src/link.rs`) |
| sha2 | 0.11.1 | MIT OR Apache-2.0 | SHA-256 (commit, words, HKDF) and SHA-512 (Ed25519) |
| digest, block-buffer, crypto-common, hybrid-array, typenum, const-oid | 0.11.3, 0.12.1, 0.2.2, 0.4.15, 1.20.1, 0.10.3 | MIT OR Apache-2.0 (const-oid Apache-2.0 OR MIT) | Hash and array plumbing, via sha2 and the others |
| cmov, ctutils | 0.5.4, 0.4.8 | Apache-2.0 OR MIT | Constant time helpers, via the RustCrypto crates |
| cpufeatures, cfg-if | 0.3.1, 1.0.5 | MIT OR Apache-2.0 | CPU feature checks, via the ciphers (also above) |
| rand_core | 0.10.1 | MIT OR Apache-2.0 | Random traits, via the RustCrypto crates (also above, sim tests) |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | OS random source for every key (also above, sim tests) |
| libc, r-efi | 0.2.191, 6.0.0 | MIT OR Apache-2.0; r-efi MIT OR Apache-2.0 OR LGPL-2.1-or-later (used under MIT OR Apache-2.0) | Via getrandom on Unix and UEFI targets (not built on Windows; also above) |
| zeroize | 1.9.1 | Apache-2.0 OR MIT | Wipes private keys and session keys on drop |
| serde, serde_core, serde_derive | 1.0.229 | MIT OR Apache-2.0 | Trust store file |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | Trust store file is JSON |
| itoa, memchr, zmij | 1.0.19, 2.8.3, 1.0.23 | MIT OR Apache-2.0; memchr Unlicense OR MIT; zmij MIT | Via serde_json |
| proc-macro2, quote, unicode-ident, syn | 1.0.107, 1.0.47, 1.0.27, 2.0.119 and 3.0.10 | MIT OR Apache-2.0 (unicode-ident also Unicode-3.0) | Proc macro support for serde_derive and curve25519-dalek-derive, build time only |

The PGP word list in `voice/src/words.rs` (the two words) is public domain (Juola and Zimmermann); checked word for word against a published copy.

## Toolchain and firmware components (not crates)

| Component | Version | License | Used in |
|-----------|---------|---------|---------|
| Rust `core` and `compiler_builtins` (built with `-Zbuild-std=core`) | esp toolchain 1.97.0.0 | MIT or Apache-2.0 | ffi device build, linked into the firmware |
| ESP-IDF (via PlatformIO `framework-espidf`) | 5.4.1 (platform espressif32 6.11.0) | Apache-2.0 (bundled components carry their own permissive licenses) | firmware |
| FreeRTOS kernel (part of ESP-IDF) | as bundled | MIT | firmware |
| espressif/mdns (ESP-IDF component registry, declared in `firmware/src/idf_component.yml`, pinned by `firmware/dependencies.lock`, fetched into the git-ignored `firmware/managed_components/`) | 1.15.1 | Apache-2.0 | firmware: advertises `_lfd3k._tcp`, browses `_telnet._tcp` (`firmware/src/mdns_link.c`) |

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
