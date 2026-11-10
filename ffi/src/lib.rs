//! C ABI over `lfd3k-core` for the ESP-IDF firmware.
//!
//! no_std on the bare-metal device target (xtensa-esp32s3-none-elf). The host
//! build keeps std only so `cargo build --workspace` works. This is the only
//! crate in the workspace allowed to use `unsafe`.
#![cfg_attr(target_os = "none", no_std)]

/// Copies the core version string (no NUL terminator) into `out`, truncated
/// to `len` bytes, and returns the number of bytes written. A null `out`
/// writes nothing and returns 0.
///
/// # Safety
/// `out` must be null or valid for writes of `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn lfd3k_core_version(out: *mut u8, len: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let v = lfd3k_core::VERSION.as_bytes();
    let n = v.len().min(len);
    // SAFETY: the caller guarantees `out` is valid for `len` bytes and
    // `n <= len`; `v` is a distinct, live static string.
    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), out, n) };
    n
}

/// Device builds have no unwinding and nowhere to report: spin.
#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
