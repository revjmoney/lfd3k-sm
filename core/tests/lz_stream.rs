//! Stream LZ above ARQ: round trips, resends, a dictionary reset, and the
//! ratio against per frame LZ. Ratios here are measured on test data (the
//! AXIS style screens built below, not a live BBS).

use lfd3k_core::arq::{ArqRx, ArqTx, CHUNK};
use lfd3k_core::lz::{compress, PRESET};
use lfd3k_core::lz_stream::{StreamDec, StreamEnc, STREAM_RAM, WINDOW};

/// A typical BBS menu, same shape as the per frame LZ tests: clear, a box,
/// a colored title, a numbered list, a status line, a prompt.
fn axis_like_screen(variant: usize) -> Vec<u8> {
    let esc = "\x1b";
    let mut s = Vec::new();
    let title = ["MAIN MENU", "MESSAGE BASES", "FILE AREAS", "DOOR GAMES"][variant % 4];
    s.extend(format!("{esc}[0m{esc}[2J{esc}[H{esc}[1;37;44m").bytes());
    s.push(0xC9);
    s.extend(std::iter::repeat_n(0xCD, 60));
    s.push(0xBB);
    s.extend(format!("{esc}[0m\r\n").bytes());
    s.push(0xBA);
    s.extend(format!("{esc}[1;33;44m{title:^60}{esc}[1;37;44m").bytes());
    s.push(0xBA);
    s.extend(format!("{esc}[0m\r\n").bytes());
    s.push(0xC8);
    s.extend(std::iter::repeat_n(0xCD, 60));
    s.push(0xBC);
    s.extend(format!("{esc}[0m\r\n\r\n").bytes());
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
        s.extend(
            format!(
                "   {esc}[1;3{fg}m[{n}]{esc}[0;37m {it}{pad}{esc}[1;30m........{esc}[0m\r\n",
                n = i + 1,
                pad = " ".repeat(20usize.saturating_sub(it.len())),
            )
            .bytes(),
        );
    }
    s.extend(std::iter::repeat_n(0xC4, 60));
    s.extend(
        format!(
            "\r\n{esc}[1;36mCalls today: {esc}[1;37m{}{esc}[1;36m   Users online: {esc}[1;37m{}{esc}[0m\r\n",
            40 + variant,
            1 + variant % 3
        )
        .bytes(),
    );
    s.extend(format!("{esc}[1;33mEnter your choice: {esc}[0m").bytes());
    s
}

/// Colors and block cells, the heavy art case from the per frame tests.
fn block_art() -> Vec<u8> {
    let mut art = Vec::new();
    for row in 0..20 {
        for col in 0..16 {
            art.extend(
                format!(
                    "\x1b[{};3{};4{}m",
                    (row + col) % 2,
                    (row + col) % 8,
                    (row * 3 + col) % 8
                )
                .bytes(),
            );
            art.extend_from_slice(&[0xDB, 0xDB]);
        }
        art.extend_from_slice(b"\x1b[0m\r\n");
    }
    art
}

fn compress_stream(plain: &[u8]) -> Vec<u8> {
    let mut enc = StreamEnc::new();
    let mut out = Vec::new();
    let mut off = 0;
    let mut buf = [0u8; 128];
    while off < plain.len() {
        let n = enc.push(&plain[off..]);
        assert!(n > 0, "no progress at {off} of {}", plain.len());
        off += n;
        loop {
            let k = enc.drain(&mut buf);
            if k == 0 {
                break;
            }
            out.extend_from_slice(&buf[..k]);
        }
    }
    enc.flush_tail();
    enc.pump();
    loop {
        let k = enc.drain(&mut buf);
        if k == 0 {
            break;
        }
        out.extend_from_slice(&buf[..k]);
    }
    assert_eq!(enc.pending(), 0);
    out
}

fn decode_stream(comp: &[u8]) -> Vec<u8> {
    let mut dec = StreamDec::new();
    let mut out = Vec::new();
    let mut off = 0;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < comp.len() + out.len() + 16, "decoder stuck");
        let mut buf = [0u8; 64];
        match dec.pull(&mut buf) {
            Ok(0) => {
                if off >= comp.len() {
                    break;
                }
                let n = dec.feed(&comp[off..]);
                assert!(n > 0);
                off += n;
            }
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => panic!("decode failed: {e:?}"),
        }
    }
    assert!(!dec.errored());
    out
}

/// Per frame LZ, 200 byte chunks, plain when the frame would not save 8 bytes.
fn air_per_frame(data: &[u8]) -> usize {
    let mut total = 0;
    let mut out = [0u8; 400];
    for c in data.chunks(CHUNK) {
        total += compress(c, &mut out).unwrap_or(c.len());
    }
    total
}

/// Push `comp` through ARQ. The first copy of a sequence `drop` accepts is
/// lost; the resend must be the same bytes and is delivered. Duplicates of
/// a frame already delivered do not change the in order stream.
fn deliver_with_loss(comp: &[u8], drop: impl Fn(u8) -> bool) -> Vec<u8> {
    let mut tx = ArqTx::new();
    let mut rx = ArqRx::new();
    let mut off = 0;
    let mut now = 0u32;
    let mut seen: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut dropped: Vec<u8> = Vec::new();
    let mut guard = 0;
    while off < comp.len() || !tx.idle() {
        guard += 1;
        assert!(guard < 10_000, "ARQ did not finish");
        if off < comp.len() {
            off += tx.push(&comp[off..]);
        }
        let mut batch = Vec::new();
        while let Some((seq, d)) = tx.next_to_send(now, 1_000) {
            if let Some((_, prev)) = seen.iter().find(|(s, _)| *s == seq) {
                assert_eq!(prev, d, "resend of seq {seq} changed bytes");
            }
            seen.push((seq, d.to_vec()));
            batch.push((seq, d.to_vec()));
        }
        for (seq, d) in &batch {
            let lose = drop(*seq) && !dropped.contains(seq);
            if lose {
                dropped.push(*seq);
            } else {
                assert!(rx.on_data(*seq, d));
            }
        }
        for (seq, d) in &batch {
            if !dropped.contains(seq) || seen.iter().filter(|(s, _)| s == seq).count() > 1 {
                assert!(rx.on_data(*seq, d));
            }
        }
        let (ack, sack) = rx.ack_state();
        tx.on_ack(ack, sack);
        now = now.wrapping_add(1_000);
        if batch.is_empty() && off < comp.len() && tx.free() == 0 {
            now = now.wrapping_add(1_000);
        }
    }
    let mut got = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        let n = rx.read(&mut buf);
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, comp);
    got
}

#[test]
fn ram_is_fixed_and_documented() {
    // History 1024, chain heads 512, chain links 2048, pending 256,
    // decoder history 1024, decoder hold 160, plus the small fields.
    assert_eq!(WINDOW, 1024);
    assert_eq!(PRESET.len(), 567);
    eprintln!("stream LZ RAM (encoder + decoder): {STREAM_RAM} bytes");
}

#[test]
fn round_trip_under_retransmit_and_reset() {
    let mut plain = Vec::new();
    for v in 0..3 {
        plain.extend(axis_like_screen(v));
    }
    let comp = compress_stream(&plain);
    let got = deliver_with_loss(&comp, |seq| seq % 4 == 1 || seq % 4 == 2);
    assert_eq!(decode_stream(&got), plain);

    // Reset in the middle, then lose frames on both sides of the token.
    let mut enc = StreamEnc::new();
    let mut comp = Vec::new();
    let first = axis_like_screen(0);
    let second = block_art();
    let push_all = |enc: &mut StreamEnc, src: &[u8], comp: &mut Vec<u8>| {
        let mut off = 0;
        let mut buf = [0u8; 128];
        while off < src.len() {
            let n = enc.push(&src[off..]);
            assert!(n > 0);
            off += n;
            loop {
                let k = enc.drain(&mut buf);
                if k == 0 {
                    break;
                }
                comp.extend_from_slice(&buf[..k]);
            }
        }
    };
    push_all(&mut enc, &first, &mut comp);
    enc.flush_tail();
    enc.request_reset();
    assert!(!enc.reset_owed());
    let mut buf = [0u8; 128];
    loop {
        let k = enc.drain(&mut buf);
        if k == 0 {
            break;
        }
        comp.extend_from_slice(&buf[..k]);
    }
    push_all(&mut enc, &second, &mut comp);
    enc.flush_tail();
    loop {
        let k = enc.drain(&mut buf);
        if k == 0 {
            break;
        }
        comp.extend_from_slice(&buf[..k]);
    }
    let wire = deliver_with_loss(&comp, |seq| seq % 3 == 0);
    let mut want = first.clone();
    want.extend_from_slice(&second);
    assert_eq!(decode_stream(&wire), want);
}

#[test]
fn measured_ratios_on_test_screens() {
    // Four menus one after another: per frame LZ restarts on every chunk,
    // stream LZ keeps the previous screen in the window.
    let mut session = Vec::new();
    for v in 0..4 {
        session.extend(axis_like_screen(v));
    }
    let art = block_art();
    let cases = [
        ("four menus", session.as_slice()),
        ("block art", art.as_slice()),
    ];
    for (name, data) in cases {
        let frame_air = air_per_frame(data);
        let stream = compress_stream(data);
        assert_eq!(decode_stream(&stream), data);
        let frame_x10 = (data.len() as u64) * 10 / (frame_air as u64);
        let stream_x10 = (data.len() as u64) * 10 / (stream.len() as u64);
        eprintln!(
            "measured on test data: {name}: {} bytes, per frame LZ {frame_air} bytes (ratio x10 {frame_x10}), stream LZ {} bytes (ratio x10 {stream_x10})",
            data.len(),
            stream.len()
        );
        assert!(
            stream.len() < frame_air,
            "{name}: stream {} did not beat per frame {frame_air}",
            stream.len()
        );
    }
}
