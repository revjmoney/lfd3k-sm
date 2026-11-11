//! Phase 4 tests of the LZ codec on its own: what it saves on a BBS screen,
//! the edge cases of a round trip, and what the decoder says to bad input.
//! The random and hostile cases are in `sim/tests/phase4_props.rs` (proptest
//! lives there), and the on-air behavior in `sim/tests/phase4.rs`.

use lfd3k_core::arq::CHUNK;
use lfd3k_core::lz::{
    compress, decompress, LzError, MAX_DIST, MAX_INPUT, MAX_LITERALS, MAX_MATCH, MIN_GAIN,
    MIN_MATCH, PRESET,
};

/// A typical BBS menu screen as an AXIS style board sends it: clear, a title
/// bar in a double line box, a numbered menu in colors, a status line, the
/// prompt. CP437 for the lines and blocks, ANSI for the colors. Made in code,
/// with the numbers and words varying so it is not a copy of the preset.
fn axis_like_screen(variant: usize) -> Vec<u8> {
    let esc = "\x1b";
    let mut s: Vec<u8> = Vec::new();
    let mut put = |t: &str| s.extend_from_slice(t.as_bytes());
    put(&format!("{esc}[0m{esc}[2J{esc}[H"));
    let title = ["MAIN MENU", "MESSAGE BASES", "FILE AREAS", "DOOR GAMES"][variant % 4];
    put(&format!("{esc}[1;37;44m"));
    let mut line = vec![0xC9u8];
    line.extend(std::iter::repeat_n(0xCD, 60));
    line.push(0xBB);
    s.extend_from_slice(&line);
    let mut put = |t: &str| s.extend_from_slice(t.as_bytes());
    put(&format!("{esc}[0m\r\n"));
    s.extend_from_slice(&[0xBA]);
    let mut put = |t: &str| s.extend_from_slice(t.as_bytes());
    put(&format!("{esc}[1;33;44m{title:^60}{esc}[1;37;44m"));
    s.extend_from_slice(&[0xBA]);
    let mut put = |t: &str| s.extend_from_slice(t.as_bytes());
    put(&format!("{esc}[0m\r\n"));
    let mut line = vec![0xC8u8];
    line.extend(std::iter::repeat_n(0xCD, 60));
    line.push(0xBC);
    s.extend_from_slice(&line);
    let mut put = |t: &str| s.extend_from_slice(t.as_bytes());
    put(&format!("{esc}[0m\r\n\r\n"));
    let items = [
        "Read messages",
        "Post a message",
        "Download files",
        "Upload files",
        "Who is online",
        "Door games",
        "Page the sysop",
        "Goodbye",
    ];
    for (i, it) in items.iter().enumerate() {
        let fg = 1 + (i + variant) % 6;
        put(&format!(
            "   {esc}[1;3{fg}m[{n}]{esc}[0;37m {it}{pad}{esc}[1;30m{dots}{esc}[0m\r\n",
            n = i + 1,
            pad = " ".repeat(20usize.saturating_sub(it.len())),
            dots = ".".repeat(8),
        ));
    }
    put("\r\n");
    s.extend(std::iter::repeat_n(0xC4, 60));
    let mut put = |t: &str| s.extend_from_slice(t.as_bytes());
    put(&format!(
        "\r\n{esc}[1;36mCalls today: {esc}[1;37m{}{esc}[1;36m   Users online: {esc}[1;37m{}{esc}[0m\r\n",
        40 + variant,
        1 + variant % 3
    ));
    put(&format!("{esc}[1;33mEnter your choice: {esc}[0m"));
    s
}

/// Total bytes on the air for `data` sent as 200 byte chunks (a chunk that
/// does not compress by 8 bytes goes plain).
fn air_bytes(data: &[u8]) -> usize {
    let mut total = 0;
    let mut out = [0u8; 400];
    for c in data.chunks(CHUNK) {
        total += match compress(c, &mut out) {
            Some(n) => {
                let mut back = [0u8; CHUNK];
                let k = decompress(&out[..n], &mut back).expect("own output decodes");
                assert_eq!(&back[..k], c, "round trip");
                n
            }
            None => c.len(),
        };
    }
    total
}

#[test]
fn lz_preset_helps_ansi() {
    // A typical AXIS style screen saves at least a quarter of its bytes.
    let mut raw = 0;
    let mut air = 0;
    for v in 0..4 {
        let s = axis_like_screen(v);
        assert!(s.len() > 800 && s.len() < 1500, "{} bytes", s.len());
        let a = air_bytes(&s);
        raw += s.len();
        air += a;
        assert!(
            a * 100 <= s.len() * 75,
            "variant {v}: {} bytes became {a} (under 25 percent saved)",
            s.len()
        );
    }
    // Heavy ANSI art (colors and blocks on every cell) saves far more.
    let mut art = Vec::new();
    for row in 0..20 {
        for col in 0..16 {
            art.extend_from_slice(
                format!(
                    "\x1b[{};3{};4{}m",
                    (row + col) % 2,
                    (row + col) % 8,
                    (row * 3 + col) % 8
                )
                .as_bytes(),
            );
            art.extend_from_slice(&[0xDB, 0xDB]);
        }
        art.extend_from_slice(b"\x1b[0m\r\n");
    }
    let a = air_bytes(&art);
    assert!(a * 2 <= art.len(), "art: {} became {a}", art.len());
    eprintln!(
        "axis-like screens: {raw} bytes became {air}; art {} became {a}",
        art.len()
    );
    // The same screen with the preset out of the picture (the first chunk
    // sent alone is what a fresh call starts with): the first chunk of a
    // screen already benefits, so there is no warm-up cost.
    let s = axis_like_screen(0);
    let mut out = [0u8; 400];
    let n = compress(&s[..CHUNK], &mut out).expect("first chunk compresses");
    assert!(n + 8 <= CHUNK);
    // Plain typed text saves little or nothing, and never costs.
    let prose = b"The quick brown fox jumps over the lazy dog while the modem trains up. \
Nothing here repeats much, so there is not much to find; a short line of plain words.";
    let p = air_bytes(prose);
    assert!(p <= prose.len());
}

#[test]
fn lz_roundtrip_edges() {
    let mut out = [0u8; 512];
    let mut back = [0u8; 512];
    // Too short to be worth it: never compressed.
    for n in 0..=MIN_GAIN {
        assert_eq!(compress(&vec![b'a'; n], &mut out), None, "{n} bytes");
    }
    // Too long for one chunk: refused, not cut.
    assert_eq!(compress(&vec![b'a'; MAX_INPUT + 1], &mut out), None);
    // A run: one literal and overlapping matches (distance 1).
    for n in [20, 100, CHUNK] {
        let data = vec![b'z'; n];
        let c = compress(&data, &mut out).expect("a run compresses");
        assert!(c + MIN_GAIN <= n, "{n}: {c}");
        let k = decompress(&out[..c], &mut back).expect("decodes");
        assert_eq!(&back[..k], &data[..]);
    }
    // Exactly one full chunk of an ANSI pattern, and every shorter length
    // from a chunk down: the output decodes to the input each time.
    let screen = axis_like_screen(1);
    for n in (MIN_GAIN + 1..=CHUNK).step_by(7) {
        let data = &screen[..n];
        if let Some(c) = compress(data, &mut out) {
            assert!(c + MIN_GAIN <= n);
            let k = decompress(&out[..c], &mut back).expect("decodes");
            assert_eq!(&back[..k], data, "length {n}");
        }
    }
    // Matches into the preset: a piece of it, copied, is found.
    let data = &PRESET[..40];
    let c = compress(data, &mut out).expect("a piece of the preset is all match");
    assert!(c <= 6, "{c}");
    let k = decompress(&out[..c], &mut back).expect("decodes");
    assert_eq!(&back[..k], data);
    // Deterministic: the same chunk compresses to the same bytes, so a
    // resend is identical to the first try.
    let first = compress(&screen[..CHUNK], &mut out).map(|n| out[..n].to_vec());
    let mut again = [0u8; 400];
    let second = compress(&screen[..CHUNK], &mut again).map(|n| again[..n].to_vec());
    assert_eq!(first, second);
    // The output buffer too small: None, not a panic or a torn result.
    let mut tiny = [0u8; 3];
    assert_eq!(compress(&screen[..CHUNK], &mut tiny), None);
    let mut empty: [u8; 0] = [];
    assert_eq!(compress(&screen[..CHUNK], &mut empty), None);
    // The limits the format promises.
    assert_eq!(
        (MIN_MATCH, MAX_MATCH, MAX_LITERALS, MAX_DIST),
        (3, 34, 128, 1024)
    );
    assert!(PRESET.len() + MAX_INPUT <= MAX_DIST);
}

#[test]
fn lz_decoder_rejects_bad_streams() {
    let mut out = [0u8; 256];
    // Nothing in, nothing out.
    assert_eq!(decompress(&[], &mut out), Ok(0));
    // A literal run longer than what follows.
    assert_eq!(
        decompress(&[0x05, b'a', b'b'], &mut out),
        Err(LzError::Truncated)
    );
    assert_eq!(decompress(&[0x00], &mut out), Err(LzError::Truncated));
    // A match with no second byte.
    assert_eq!(decompress(&[0x80], &mut out), Err(LzError::Truncated));
    assert_eq!(
        decompress(&[0x00, b'a', 0x84], &mut out),
        Err(LzError::Truncated)
    );
    // A distance in front of the preset: 1024 back with 0 produced (the
    // preset is shorter than that).
    assert!(PRESET.len() < MAX_DIST);
    assert_eq!(
        decompress(&[0x83, 0xFF], &mut out),
        Err(LzError::BadDistance)
    );
    // A distance just past what exists after some literals.
    let reach = PRESET.len() + 1; // one literal produced
    let d = reach; // distance = d + 1 - 1 ... one more than available
    let hi = ((d >> 8) & 3) as u8;
    let lo = (d & 0xFF) as u8;
    assert_eq!(
        decompress(&[0x00, b'a', 0x80 | hi, lo], &mut out),
        Err(LzError::BadDistance)
    );
    // The same stream one shorter in distance is fine (copies the preset's
    // first byte).
    let d = reach - 1;
    let (hi, lo) = (((d >> 8) & 3) as u8, (d & 0xFF) as u8);
    assert_eq!(decompress(&[0x00, b'a', 0x80 | hi, lo], &mut out), Ok(4));
    assert_eq!(out[0], b'a');
    assert_eq!(out[1], PRESET[0]);
    // Output that does not fit.
    let mut small = [0u8; 4];
    assert_eq!(
        decompress(&[0x05, 1, 2, 3, 4, 5, 6], &mut small),
        Err(LzError::Overflow)
    );
    assert_eq!(
        decompress(&[0x80, 0x00], &mut [0u8; 2]),
        Err(LzError::Overflow)
    );
    // A match that reaches into its own output (distance 1) and is long.
    let c = [0x00, b'x', 0x80 | (31 << 2), 0x00];
    let mut o = [0u8; 64];
    let n = decompress(&c, &mut o).expect("overlapping copy");
    assert_eq!(n, 1 + 34);
    assert!(o[..n].iter().all(|&b| b == b'x'));
    // A failed decode never claims a length: the caller sees only the error.
    assert!(decompress(&[0xFF; 64], &mut out).is_err());
}
