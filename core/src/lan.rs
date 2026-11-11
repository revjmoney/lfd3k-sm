//! Telnet servers found on the LAN (an AXIS board, say) by the platform's
//! mDNS browse of `_telnet._tcp`, handed over with `Modem::set_found_hosts`.
//!
//! The list is information only: nothing here changes a setting. The
//! menu's Forwards screen shows each host that is not a forward yet, and
//! only the owner's Enter on that row adds it (as `AT+FWD` and `AT+PUB`
//! would). Names come from the network, so they are cut to `FOUND_NAME`
//! printable ASCII bytes before anything shows them.

use crate::settings::{valid_fwd_name, Settings};

/// Hosts kept from one browse.
pub const FOUND_MAX: usize = 4;
/// Longest instance name kept.
pub const FOUND_NAME: usize = 31;
/// The forward name used when the instance name cannot be one.
pub const FALLBACK_FWD: &str = "AXIS";

/// One host from the browse: instance name, IPv4 address, port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Found {
    name: [u8; FOUND_NAME],
    name_len: u8,
    ip: [u8; 4],
    port: u16,
}

impl Found {
    /// `None` for port 0, address 0.0.0.0 or a name with nothing printable.
    /// Bytes outside printable ASCII become `?`; edge spaces are dropped; the
    /// name is cut to `FOUND_NAME` bytes.
    pub fn new(name: &[u8], ip: [u8; 4], port: u16) -> Option<Found> {
        if port == 0 || ip == [0; 4] {
            return None;
        }
        let mut f = Found {
            name: [0; FOUND_NAME],
            name_len: 0,
            ip,
            port,
        };
        let start = name.iter().position(|&c| c != b' ')?;
        let end = name
            .iter()
            .rposition(|&c| c != b' ')
            .map_or(start, |e| e + 1);
        let trimmed = name.get(start..end).unwrap_or(&[]);
        for (d, &c) in f.name.iter_mut().zip(trimmed.iter()) {
            *d = if (0x20..0x7F).contains(&c) { c } else { b'?' };
            f.name_len += 1;
        }
        // A cut may leave a space at the end.
        while f.name_len > 0 && f.name.get(usize::from(f.name_len) - 1) == Some(&b' ') {
            f.name_len -= 1;
        }
        (f.name_len > 0).then_some(f)
    }

    /// The instance name (printable ASCII).
    pub fn name(&self) -> &str {
        self.name
            .get(..usize::from(self.name_len))
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or("")
    }

    pub fn ip(&self) -> [u8; 4] {
        self.ip
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The address as `a.b.c.d` in `out`; returns the length.
    pub fn ip_text(&self, out: &mut [u8; 15]) -> usize {
        let mut n = 0;
        let mut put = |c: u8, n: &mut usize| {
            if let Some(d) = out.get_mut(*n) {
                *d = c;
                *n += 1;
            }
        };
        for (i, &b) in self.ip.iter().enumerate() {
            if i > 0 {
                put(b'.', &mut n);
            }
            if b >= 100 {
                put(b'0' + b / 100, &mut n);
            }
            if b >= 10 {
                put(b'0' + (b / 10) % 10, &mut n);
            }
            put(b'0' + b % 10, &mut n);
        }
        n
    }

    /// Some forward already points at this host and port.
    pub fn is_forward(&self, set: &Settings) -> bool {
        let mut t = [0u8; 15];
        let n = self.ip_text(&mut t);
        let ip = core::str::from_utf8(t.get(..n).unwrap_or(&[])).unwrap_or("");
        set.fwd
            .iter()
            .flatten()
            .any(|f| f.port() == self.port && f.host().eq_ignore_ascii_case(ip))
    }

    /// The forward name Enter would add it under: the instance name when it
    /// is a valid forward name (and the AT parser would keep it as is), else
    /// `AXIS`; `None` when that name already belongs to another forward
    /// (Enter never rewrites an existing forward).
    pub fn fwd_name<'a>(&'a self, set: &Settings) -> Option<&'a str> {
        let pick = self.fwd_pick();
        set.forward(pick).is_none().then_some(pick)
    }

    /// The instance name when it is a valid forward name, else `AXIS`.
    pub fn fwd_pick(&self) -> &str {
        let own = self.name();
        if valid_fwd_name(own) && own.trim() == own {
            own
        } else {
            FALLBACK_FWD
        }
    }
}

/// The hosts from the latest browse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FoundList {
    hosts: [Option<Found>; FOUND_MAX],
}

impl Default for FoundList {
    fn default() -> Self {
        Self::new()
    }
}

impl FoundList {
    pub const fn new() -> FoundList {
        FoundList {
            hosts: [None; FOUND_MAX],
        }
    }

    pub fn clear(&mut self) {
        self.hosts = [None; FOUND_MAX];
    }

    /// Replace the list (the first `FOUND_MAX` valid hosts, a repeated
    /// address and port kept once). True when it changed.
    pub fn set<I: IntoIterator<Item = Found>>(&mut self, hosts: I) -> bool {
        let mut next = [None; FOUND_MAX];
        let mut k = 0;
        for h in hosts {
            if k >= FOUND_MAX {
                break;
            }
            let dup = next
                .iter()
                .flatten()
                .any(|o: &Found| o.ip == h.ip && o.port == h.port);
            if dup {
                continue;
            }
            if let Some(d) = next.get_mut(k) {
                *d = Some(h);
                k += 1;
            }
        }
        let changed = next != self.hosts;
        self.hosts = next;
        changed
    }

    /// Every host, in browse order.
    pub fn iter(&self) -> impl Iterator<Item = &Found> {
        self.hosts.iter().flatten()
    }

    /// The `i`th host that is not a forward yet (what the menu shows).
    pub fn new_host(&self, set: &Settings, i: u8) -> Option<&Found> {
        self.iter()
            .filter(|h| !h.is_forward(set))
            .nth(usize::from(i))
    }

    /// How many hosts are not forwards yet.
    pub fn new_count(&self, set: &Settings) -> u8 {
        self.iter().filter(|h| !h.is_forward(set)).count() as u8
    }
}
