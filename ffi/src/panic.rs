//! Crash notes and the device panic handler.

/// Longest crash note, in bytes (the size the HAL stores in RTC RAM).
pub const NOTE_LEN: usize = 64;

/// Writes `file:line` into `out`, keeping the line number and the tail of
/// the file name when it does not fit. Returns the bytes written. No
/// allocation, no panic.
pub fn format_note(file: &str, line: u32, out: &mut [u8; NOTE_LEN]) -> usize {
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    let mut v = line;
    loop {
        i = i.saturating_sub(1);
        if let Some(d) = digits.get_mut(i) {
            *d = b'0' + (v % 10) as u8;
        }
        v /= 10;
        if v == 0 || i == 0 {
            break;
        }
    }
    let num = digits.get(i..).unwrap_or(&[]);
    let name = file.as_bytes();
    // Room for the name: the buffer minus ':' and the digits.
    let room = NOTE_LEN.saturating_sub(1 + num.len());
    let name = name
        .get(name.len().saturating_sub(room)..)
        .unwrap_or(&[]);
    let mut n = 0;
    for &b in name.iter().chain(b":".iter()).chain(num.iter()) {
        if let Some(d) = out.get_mut(n) {
            *d = b;
            n += 1;
        }
    }
    n
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let mut note = [0u8; NOTE_LEN];
    let n = match info.location() {
        Some(l) => format_note(l.file(), l.line(), &mut note),
        None => {
            let m = b"panic";
            if let Some(d) = note.get_mut(..m.len()) {
                d.copy_from_slice(m);
            }
            m.len()
        }
    };
    crate::hal::panic(note.get(..n).unwrap_or(&[]));
    // The HAL restarts the chip; if it ever returns, stay put.
    loop {
        core::hint::spin_loop();
    }
}
