use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use axum::extract::rejection::ExtensionRejection;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::shared::Shared;

const FORWARDED_FOR: &str = "x-forwarded-for";

/// The address a request came from, for the per-client limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Client(pub(crate) IpAddr);

impl FromRequestParts<Shared> for Client {
    type Rejection = ExtensionRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        shared: &Shared,
    ) -> Result<Self, Self::Rejection> {
        let ConnectInfo(peer) =
            ConnectInfo::<SocketAddr>::from_request_parts(parts, shared).await?;
        Ok(client(peer.ip(), &parts.headers, &shared.proxies))
    }
}

/// A block of addresses, written `10.0.0.0/8` or as one address. An IPv4 block is kept as
/// the IPv6 block of the same addresses, `::ffff:10.0.0.0/104`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Net {
    address: u128,
    bits: u32,
}

impl FromStr for Net {
    type Err = &'static str;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (address, bits) = match text.split_once('/') {
            Some((address, bits)) => (address, Some(bits)),
            None => (text, None),
        };
        let address: IpAddr = address
            .parse()
            .map_err(|_| "not an address or an address/bits block")?;
        let (most, mapped) = if address.is_ipv4() {
            (32, 96)
        } else {
            (128, 0)
        };
        let bits = match bits {
            Some(bits) => bits
                .parse()
                .ok()
                .filter(|bits| *bits <= most)
                .ok_or("more bits than the address has")?,
            None => most,
        };
        Ok(Self {
            address: wide(address),
            bits: bits + mapped,
        })
    }
}

impl Net {
    fn contains(&self, address: IpAddr) -> bool {
        // No bits at all is a shift by the whole width, which matches everything.
        (self.address ^ wide(address))
            .checked_shr(128 - self.bits)
            .unwrap_or(0)
            == 0
    }
}

/// `address` as IPv6, which holds every IPv4 address too.
fn wide(address: IpAddr) -> u128 {
    match address {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().into(),
        IpAddr::V6(v6) => v6.into(),
    }
}

/// The client behind `peer`: when `peer` is a trusted proxy, the last address in its
/// `X-Forwarded-For` that isn't one. Reading stops at anything unreadable.
fn client(peer: IpAddr, headers: &HeaderMap, trusted: &[Net]) -> Client {
    let is_proxy = |address: IpAddr| trusted.iter().any(|net| net.contains(address));
    let mut client = peer;
    let forwarded = headers
        .get_all(FORWARDED_FOR)
        .iter()
        .rev()
        .flat_map(|value| value.as_bytes().rsplit(|byte| *byte == b','));
    for hop in forwarded {
        if !is_proxy(client) {
            break;
        }
        // Each hop on its own, so bad bytes the walk never reaches can't hide a good one.
        let Some(hop) = str::from_utf8(hop)
            .ok()
            .and_then(|hop| hop.trim().parse().ok())
        else {
            break;
        };
        client = hop;
    }
    Client(client)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn nets(texts: &[&str]) -> Vec<Net> {
        texts.iter().map(|text| text.parse().unwrap()).collect()
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn from(peer: &str, forwarded: &[&[u8]], trusted: &[&str]) -> String {
        let mut headers = HeaderMap::new();
        for value in forwarded {
            headers.append(FORWARDED_FOR, HeaderValue::from_bytes(value).unwrap());
        }
        client(ip(peer), &headers, &nets(trusted)).0.to_string()
    }

    #[test]
    fn blocks_hold_the_addresses_that_share_their_bits() {
        let holds = |net: &str, address: &str| net.parse::<Net>().unwrap().contains(ip(address));

        assert!(holds("10.0.0.0/8", "10.255.0.1"));
        assert!(!holds("10.0.0.0/8", "11.0.0.1"));
        assert!(holds("192.168.1.0/24", "192.168.1.255"));
        assert!(!holds("192.168.1.0/24", "192.168.2.0"));
        assert!(holds("192.168.1.7", "192.168.1.7"));
        assert!(!holds("192.168.1.7", "192.168.1.6"));
        assert!(holds("0.0.0.0/0", "203.0.113.9"));
        assert!(!holds("0.0.0.0/0", "2001:db8::1"));
        assert!(holds("fd00::/8", "fd12::1"));
        assert!(!holds("fd00::/8", "fe00::1"));
        assert!(holds("::/0", "2001:db8::1"));
        assert!(holds("2001:db8::1", "2001:db8::1"));
        assert!(!holds("2001:db8::1", "2001:db8::2"));
        // An IPv4 address written as IPv6 is that IPv4 address.
        assert!(holds("10.0.0.0/8", "::ffff:10.1.2.3"));
        assert!(holds("::ffff:10.0.0.0/104", "10.1.2.3"));
        assert!(!holds("::ffff:10.0.0.0/104", "11.1.2.3"));
    }

    #[test]
    fn blocks_that_arent_are_refused() {
        for text in [
            "",
            "proxy",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/",
            "10.0.0.0/-1",
            "/8",
        ] {
            text.parse::<Net>().unwrap_err();
        }
        "10.0.0.0/32".parse::<Net>().unwrap();
        "::/128".parse::<Net>().unwrap();
    }

    #[test]
    fn only_a_trusted_proxy_is_believed() {
        let proxy = ["10.0.0.0/8"];

        assert_eq!(from("203.0.113.9", &[b"198.51.100.1"], &[]), "203.0.113.9");
        assert_eq!(
            from("203.0.113.9", &[b"198.51.100.1"], &proxy),
            "203.0.113.9"
        );
        assert_eq!(from("10.0.0.1", &[b"198.51.100.1"], &proxy), "198.51.100.1");
        assert_eq!(from("10.0.0.1", &[], &proxy), "10.0.0.1");
        assert_eq!(
            from("::ffff:10.0.0.1", &[b"2001:db8::7"], &proxy),
            "2001:db8::7"
        );
    }

    #[test]
    fn the_client_is_the_last_hop_that_isnt_a_trusted_proxy() {
        let proxy = ["10.0.0.0/8"];
        let from = |forwarded: &[&[u8]]| from("10.0.0.1", forwarded, &proxy);

        // What the client wrote itself comes first, and is never reached.
        assert_eq!(from(&[b"1.1.1.1, 198.51.100.1 , 10.0.0.2"]), "198.51.100.1");
        assert_eq!(
            from(&[b"1.1.1.1, 198.51.100.1", b"10.0.0.2"]),
            "198.51.100.1"
        );
        assert_eq!(from(&[b"10.0.0.3, 10.0.0.2"]), "10.0.0.3");
        assert_eq!(from(&[b"198.51.100.1, nonsense"]), "10.0.0.1");
        assert_eq!(from(&[b"198.51.100.1, nonsense, 10.0.0.2"]), "10.0.0.2");
        assert_eq!(from(&[b"198.51.100.1", b"\xe9"]), "10.0.0.1");
        assert_eq!(from(&[b"\xe9, 198.51.100.1"]), "198.51.100.1");
        assert_eq!(from(&[b"198.51.100.1, \xe9, 10.0.0.2"]), "10.0.0.2");
        assert_eq!(from(&[b"198.51.100.1:443"]), "10.0.0.1");
    }
}
