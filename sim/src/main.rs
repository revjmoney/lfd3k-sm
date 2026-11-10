//! Phase 1 demo: two LFD3K-SM modems on a simulated LoRa channel.
//!
//! `cargo run -p lfd3k-sim -- [--snr <dB>] [--seed <n>] [--out <file>] [--trace <file>]`
//! (`--out` saves the transcript as UTF-8, `--trace` saves a frame by frame log)
//!
//! CALLER and AXIS show ATI, CALLER dials AXIS, sends a 4 KB ANSI test
//! screen, AXIS sends one back while the SNR is 10 dB down (30 s to 60 s).
//! CALLER escapes with `+++` during the dip (the slower RETRAIN shows) and
//! goes back online with ATO; after the SNR recovers it escapes again,
//! shows AT&V and hangs up with ATH.

use lfd3k_core::Tier;
use lfd3k_sim::{Harness, SimChannel, SnrProfile};

const ESC: &str = "\x1b";
const SCREEN_BYTES: usize = 4096;
const BLOCKS: [&str; 4] = ["\u{2588}", "\u{2593}", "\u{2592}", "\u{2591}"];

/// A BBS style ANSI test screen of about 4 KB, made in code.
fn ansi_screen(title: &str, shift: usize) -> Vec<u8> {
    let mut s = String::new();
    s.push_str(&format!("{ESC}[0m{ESC}[2J{ESC}[H"));
    let bar = "\u{2550}".repeat(60);
    s.push_str(&format!("{ESC}[1;37;44m\u{2554}{bar}\u{2557}{ESC}[0m\r\n"));
    s.push_str(&format!(
        "{ESC}[1;37;44m\u{2551}{:^60}\u{2551}{ESC}[0m\r\n",
        title
    ));
    s.push_str(&format!("{ESC}[1;37;44m\u{255a}{bar}\u{255d}{ESC}[0m\r\n"));
    let mut row = 0usize;
    // Each row is about 280 bytes; leave room for the footer.
    while s.len() < SCREEN_BYTES - 340 {
        for col in 0..16usize {
            let fg = (row + col + shift) % 8;
            let bg = (row * 3 + col + shift + 4) % 8;
            let bold = u8::from((row + col).is_multiple_of(2));
            let block = BLOCKS[(col + row + shift) % 4];
            s.push_str(&format!("{ESC}[{bold};3{fg};4{bg}m{block}{block}"));
        }
        s.push_str(&format!("{ESC}[0m\r\n"));
        row += 1;
    }
    s.push_str(&format!(
        "{ESC}[1;33mLFD3K-SM TEST SCREEN {ESC}[1;36m{row} ROWS, {SCREEN_BYTES} BYTES{ESC}[0m"
    ));
    // Pad to exactly SCREEN_BYTES.
    while s.len() < SCREEN_BYTES - 2 {
        s.push(' ');
    }
    s.push_str("\r\n");
    s.into_bytes()
}

/// Strips ANSI CSI sequences and lone CRs for a readable transcript.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => {
                if it.peek() == Some(&'[') {
                    it.next();
                    for d in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&d) {
                            break;
                        }
                    }
                } else {
                    out.push_str("<ESC>");
                }
            }
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                    out.push('\n');
                }
            }
            '\n' => out.push('\n'),
            c if (c as u32) < 0x20 => out.push_str(&format!("<{:02X}>", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The screen with each data segment framed by a marker line.
fn render(screen: &[u8], segs: &[(usize, usize)]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for &(s, e) in segs {
        out.push_str(&plain(screen.get(at..s).unwrap_or(&[])));
        let data = screen.get(s..e).unwrap_or(&[]);
        if data.is_empty() {
            out.push_str("[data mode: nothing received]\n");
        } else {
            out.push_str(&format!(
                "[data mode: {} bytes received, ANSI color codes stripped below]\n",
                data.len()
            ));
            let body = plain(data);
            out.push_str(&body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push_str("[end of data mode]\n");
        at = e;
    }
    out.push_str(&plain(screen.get(at..).unwrap_or(&[])));
    out.lines().map(|l| format!("{}\n", l.trim_end())).collect()
}

fn trace_text(h: &Harness) -> String {
    let mut s = String::from("ms from tier type flags seq len resend outcome\n");
    for e in h.trace() {
        s.push_str(&format!(
            "{} {} SF{} {} {:x} {} {} {} {:?}\n",
            e.t_ms,
            if e.from == 0 { "CALLER" } else { "AXIS" },
            e.tier.sf(),
            e.ftype
                .map_or_else(|| "?".to_string(), |t| format!("{t:?}")),
            e.flags,
            e.seq,
            e.len,
            u8::from(e.resend),
            e.outcome
        ));
    }
    s
}

fn db(x4: i16) -> String {
    format!("{:+.1} dB", f32::from(x4) / 4.0)
}

struct Args {
    snr_db: i16,
    seed: u64,
    out: Option<String>,
    trace: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        snr_db: 10,
        seed: 420,
        out: None,
        trace: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().ok_or(format!("{k} needs a value"))?;
        match k.as_str() {
            "--snr" => a.snr_db = v.parse().map_err(|_| format!("bad --snr {v}"))?,
            "--seed" => a.seed = v.parse().map_err(|_| format!("bad --seed {v}"))?,
            "--out" => a.out = Some(v),
            "--trace" => a.trace = Some(v),
            _ => return Err(format!("unknown option {k}")),
        }
    }
    Ok(a)
}

fn main() {
    let args = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "{e}\nusage: lfd3k-sim [--snr <dB>] [--seed <n>] [--out <file>] [--trace <file>]"
            );
            std::process::exit(2);
        }
    };
    let base = args.snr_db.saturating_mul(4);
    let low = base.saturating_sub(40);
    let steps: &'static [(u32, i16)] =
        Box::leak(vec![(0, base), (30_000, low), (60_000, base)].into_boxed_slice());
    let mut h = Harness::new(SimChannel {
        profile: SnrProfile::Steps(steps),
        seed: args.seed,
        drop: None,
    });

    let mut log = Vec::new();
    let mut note =
        |h: &Harness, s: &str| log.push(format!("{:>7.3} s  {s}", f64::from(h.now_ms) / 1000.0));

    // Banners, then dial.
    h.type_a("ATI\r");
    h.type_b("ATI\r");
    h.run_ms(200);
    h.type_a("ATV1\r");
    h.run_ms(100);
    note(&h, "CALLER: ATDT AXIS");
    h.type_a("ATDT AXIS\r");
    if h.run_until(20_000, |h| h.a.is_connected() && h.b.is_connected()) {
        note(&h, "both connected");
    } else {
        note(&h, "NOT CONNECTED after 20 s");
    }

    let screen_a = ansi_screen("LFD3K-SM TEST SCREEN FROM CALLER", 0);
    let screen_b = ansi_screen("AXIS BBS ANSI SCREEN, SENT BACK", 3);

    note(&h, &format!("CALLER sends {} bytes", screen_a.len()));
    h.send_a_data(&screen_a);
    let la = screen_a.len();
    h.run_until(60_000, |h| h.received_b().len() >= la);
    note(&h, &format!("AXIS has {} bytes", h.received_b().len()));

    h.run_until(60_000, |h| h.now_ms >= 35_000);
    note(
        &h,
        &format!(
            "SNR now {}; AXIS sends {} bytes",
            db(h.snr_x4_now()),
            screen_b.len()
        ),
    );
    h.send_b_data(&screen_b);
    let lb = screen_b.len();
    h.run_until(60_000, |h| h.received_a().len() >= lb);
    note(&h, &format!("CALLER has {} bytes", h.received_a().len()));

    // Escape during the dip: the slower RETRAIN shows after OK; ATO resumes.
    h.run_until(60_000, |h| h.now_ms >= 45_000);
    note(&h, "CALLER: +++ (1 s guard before and after)");
    h.run_ms(1_500);
    h.type_a("+++");
    h.run_ms(1_500);
    note(&h, "CALLER: ATO");
    h.type_a("ATO\r");
    h.run_ms(500);

    h.run_until(120_000, |h| h.now_ms >= 85_000);
    note(&h, "CALLER: +++ (1 s guard before and after)");
    h.run_ms(1_500);
    h.type_a("+++");
    h.run_ms(1_500);
    note(&h, "CALLER: AT&V");
    h.type_a("AT&V\r");
    h.run_ms(500);
    note(&h, "CALLER: ATH");
    h.type_a("ATH\r");
    h.run_ms(3_000);
    note(
        &h,
        &format!(
            "connected: CALLER {}, AXIS {}",
            h.a.is_connected(),
            h.b.is_connected()
        ),
    );

    let ra = h.received_a();
    let rb = h.received_b();
    let st = h.stats();
    let mut t = String::new();
    t.push_str("==== LFD3K-SM phase 1 demo (simulated channel) ====\n");
    t.push_str(&format!(
        "seed {}, SNR {} until 30 s, {} from 30 s to 60 s, {} after\n\n",
        args.seed,
        db(base),
        db(low),
        db(base)
    ));
    t.push_str("==== Timeline ====\n");
    for l in &log {
        t.push_str(l);
        t.push('\n');
    }
    t.push_str("\n==== CALLER screen ====\n");
    t.push_str(&render(h.screen_a_bytes(), &h.segments(0)));
    t.push_str("\n==== AXIS screen ====\n");
    t.push_str(&render(h.screen_b_bytes(), &h.segments(1)));
    t.push_str("\n==== Summary ====\n");
    for (side, name) in ["CALLER", "AXIS"].iter().enumerate() {
        let tiers: Vec<String> = st.tiers[side]
            .iter()
            .map(|(ms, tier)| format!("{} ({:.1} s)", short(*tier), f64::from(*ms) / 1000.0))
            .collect();
        t.push_str(&format!(
            "Tiers used by {name}, in order: {}\n",
            tiers.join(" > ")
        ));
    }
    t.push_str(&format!(
        "CALLER to AXIS: {} bytes sent, {} received, {}\n",
        screen_a.len(),
        rb.len(),
        if rb == screen_a { "intact" } else { "MISMATCH" }
    ));
    t.push_str(&format!(
        "AXIS to CALLER: {} bytes sent, {} received, {}\n",
        screen_b.len(),
        ra.len(),
        if ra == screen_b { "intact" } else { "MISMATCH" }
    ));
    t.push_str(&format!(
        "Simulated time: {:.1} s\n",
        f64::from(h.now_ms) / 1000.0
    ));
    t.push_str(&format!(
        "Data frames: CALLER {} ({} resent), AXIS {} ({} resent)\n",
        st.data_frames[0], st.resends[0], st.data_frames[1], st.resends[1]
    ));
    t.push_str(&format!(
        "All frames: {} sent, {} delivered, {} lost to SNR, {} undetected, {} collided, {} missed while sending, {} missed while not listening\n",
        st.frames_sent[0] + st.frames_sent[1],
        st.delivered,
        st.lost,
        st.undetected,
        st.collided,
        st.half_duplex,
        st.not_listening
    ));

    print!("{t}");
    if let Some(path) = args.out {
        if let Err(e) = std::fs::write(&path, t.as_bytes()) {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(1);
        }
    }
    if let Some(path) = args.trace {
        if let Err(e) = std::fs::write(&path, trace_text(&h)) {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(1);
        }
    }
}

fn short(t: Tier) -> String {
    format!("SF{}", t.sf())
}
