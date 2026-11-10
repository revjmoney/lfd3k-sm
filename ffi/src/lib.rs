// no_std on the bare-metal device target (xtensa-esp32s3-none-elf). The host
// build keeps std only so `cargo build --workspace` works; Task 7 adds the
// panic handler and the exported C functions.
#![cfg_attr(target_os = "none", no_std)]
