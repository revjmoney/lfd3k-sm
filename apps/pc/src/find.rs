//! `--modem find`: look for a modem on the network by its mDNS service
//! `_lfd3k._tcp` (TXT `name` = station, `ver`, and `voice` = the TCP voice
//! port; a modem whose firmware has no `voice` key cannot take a voice app
//! and is passed over).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};

/// The modem's service type.
const SERVICE: &str = "_lfd3k._tcp.local.";

/// The TCP voice port from a service's TXT keys (`voice=4220`), keys
/// matched in any case. None when there is no `voice` key or it is not a
/// port from 1 to 65535.
pub fn voice_port_from_txt(txt: &[(&str, &str)]) -> Option<u16> {
    let (_, v) = txt.iter().find(|(k, _)| k.eq_ignore_ascii_case("voice"))?;
    match v.trim().parse::<u16>() {
        Ok(p) if p > 0 => Some(p),
        _ => None,
    }
}

/// The station name from TXT `name`, any case.
fn station_from_txt(txt: &[(&str, &str)]) -> Option<String> {
    txt.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("name"))
        .map(|(_, v)| v.trim().to_string())
}

/// Browse for up to `timeout_ms` and give the first modem with a voice
/// port (or the one whose station name is `station`, any case): its
/// station name and its IPv4 address with the voice port.
pub fn find_modem(station: Option<&str>, timeout_ms: u32) -> Result<(String, SocketAddr), String> {
    let mdns = ServiceDaemon::new().map_err(|e| format!("mDNS: {e}"))?;
    let rx = mdns
        .browse(SERVICE)
        .map_err(|e| format!("mDNS browse: {e}"))?;
    let until = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let mut seen: Vec<String> = Vec::new();
    let mut found = None;
    while found.is_none() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(ev) = rx.recv_timeout(left) else {
            break;
        };
        let ServiceEvent::ServiceResolved(s) = ev else {
            continue;
        };
        let props: Vec<(String, String)> = s
            .txt_properties
            .iter()
            .map(|p| (p.key().to_string(), p.val_str().to_string()))
            .collect();
        let txt: Vec<(&str, &str)> = props
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let name = station_from_txt(&txt).unwrap_or_default();
        let Some(port) = voice_port_from_txt(&txt) else {
            seen.push(format!("{name} (no voice port)"));
            continue;
        };
        if station.is_some_and(|want| !want.eq_ignore_ascii_case(&name)) {
            seen.push(name);
            continue;
        }
        let mut v4: Vec<_> = s
            .addresses
            .iter()
            .map(|a| a.to_ip_addr())
            .filter_map(|a| match a {
                IpAddr::V4(v) => Some(v),
                IpAddr::V6(_) => None,
            })
            .collect();
        v4.sort();
        let Some(ip) = v4.first().copied() else {
            seen.push(format!("{name} (no IPv4 address)"));
            continue;
        };
        found = Some((name, SocketAddr::from((ip, port))));
    }
    let _ = mdns.stop_browse(SERVICE);
    let _ = mdns.shutdown();
    found.ok_or_else(|| {
        let what = station.map_or_else(
            || "no LFD3K-SM modem with a voice port".to_string(),
            |s| format!("no LFD3K-SM modem named {s} with a voice port"),
        );
        if seen.is_empty() {
            format!("{what} found on the network in {} s", timeout_ms / 1000)
        } else {
            format!(
                "{what} found in {} s (heard: {})",
                timeout_ms / 1000,
                seen.join(", ")
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_port_from_txt_cases() {
        assert_eq!(voice_port_from_txt(&[("voice", "4220")]), Some(4220));
        assert_eq!(
            voice_port_from_txt(&[("name", "REV"), ("VOICE", " 4220 ")]),
            Some(4220)
        );
        assert_eq!(voice_port_from_txt(&[("name", "REV"), ("ver", "1")]), None);
        assert_eq!(voice_port_from_txt(&[]), None);
        assert_eq!(voice_port_from_txt(&[("voice", "abc")]), None);
        assert_eq!(voice_port_from_txt(&[("voice", "0")]), None);
    }
}
