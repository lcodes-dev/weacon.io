use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// The most clients a limit keeps track of; past that, new ones are refused until old ones go.
const MAX_CLIENTS: usize = 100_000;
/// How many /64s of one IPv6 /48 are kept apart; the rest of the /48 counts as one client, so
/// one cheap prefix can't fill the table.
const PER_48: usize = 256;

/// At most `max` requests from one client in each `window`. A client is an IPv4 address or an
/// IPv6 /64, which one home line or server usually gets whole.
pub(crate) struct Limit {
    max: u32,
    window: Duration,
    /// The most clients kept track of.
    room: usize,
    seen: Mutex<Seen>,
}

#[derive(Default)]
struct Seen {
    clients: HashMap<IpAddr, Window>,
    /// How many clients each IPv6 /48 has in `clients`.
    wide: HashMap<Ipv6Addr, usize>,
    /// When expired windows were last swept out.
    swept: Option<Instant>,
}

struct Window {
    started: Instant,
    count: u32,
}

impl Limit {
    pub(crate) fn new(max: u32, window: Duration) -> Self {
        Self::with_room(max, window, MAX_CLIENTS)
    }

    fn with_room(max: u32, window: Duration, room: usize) -> Self {
        Self {
            max,
            window,
            room,
            seen: Mutex::default(),
        }
    }

    /// Counts a request from `from` at `now`; false when it is one too many.
    pub(crate) fn allow(&self, from: IpAddr, now: Instant) -> bool {
        let Ok(mut seen) = self.seen.lock() else {
            return false;
        };
        let window = self.window;
        if seen.swept.is_none_or(|swept| now >= swept + window) {
            seen.clients.retain(|_, kept| now < kept.started + window);
            seen.wide = seen
                .clients
                .keys()
                .fold(HashMap::new(), |mut wide, client| {
                    count_wide(&mut wide, *client);
                    wide
                });
            seen.swept = Some(now);
        }
        let client = seen.client(from);
        if !seen.clients.contains_key(&client) {
            if seen.clients.len() >= self.room {
                return false;
            }
            count_wide(&mut seen.wide, client);
        }
        let kept = seen.clients.entry(client).or_insert(Window {
            started: now,
            count: 0,
        });
        if now >= kept.started + window {
            kept.started = now;
            kept.count = 0;
        }
        kept.count = kept.count.saturating_add(1);
        kept.count <= self.max
    }
}

impl Seen {
    /// `from` as the client it counts as: an IPv4 address, or the /64 an IPv6 one is in,
    /// or its /48 once that has `PER_48` others.
    fn client(&self, from: IpAddr) -> IpAddr {
        let v6 = match from.to_canonical() {
            IpAddr::V6(v6) => v6,
            v4 @ IpAddr::V4(_) => return v4,
        };
        let one = IpAddr::V6(prefix(v6, 4));
        let wide = prefix(v6, 3);
        let full = self.wide.get(&wide).is_some_and(|count| *count >= PER_48);
        // The /48's own key is the /64 at its start, which is in it anyway.
        if full && !self.clients.contains_key(&one) {
            IpAddr::V6(wide)
        } else {
            one
        }
    }
}

fn count_wide(wide: &mut HashMap<Ipv6Addr, usize>, client: IpAddr) {
    if let IpAddr::V6(v6) = client {
        *wide.entry(prefix(v6, 3)).or_default() += 1;
    }
}

/// The first `segments` segments of `address`, the rest zero.
fn prefix(address: Ipv6Addr, segments: usize) -> Ipv6Addr {
    let mut kept = address.segments();
    for segment in kept.iter_mut().skip(segments) {
        *segment = 0;
    }
    kept.into()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    const MINUTE: Duration = Duration::from_mins(1);

    fn v4(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, last))
    }

    #[test]
    fn each_client_gets_its_own_count_in_each_window() {
        let limit = Limit::new(2, MINUTE);
        let start = Instant::now();

        assert!(limit.allow(v4(1), start));
        assert!(limit.allow(v4(1), start + MINUTE / 2));
        assert!(!limit.allow(v4(1), start + MINUTE / 2));
        assert!(limit.allow(v4(2), start + MINUTE / 2));
        assert!(limit.allow(v4(1), start + MINUTE));
    }

    #[test]
    fn an_ipv6_64_is_one_client_and_a_mapped_ipv4_is_its_address() {
        let limit = Limit::new(1, MINUTE);
        let now = Instant::now();
        let one: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let same_64: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let next_64: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        let mapped = IpAddr::V6(Ipv4Addr::new(192, 0, 2, 7).to_ipv6_mapped());

        assert!(limit.allow(one, now));
        assert!(!limit.allow(same_64, now));
        assert!(limit.allow(next_64, now));
        assert!(limit.allow(mapped, now));
        assert!(!limit.allow(v4(7), now));
        assert_eq!(prefix(Ipv6Addr::LOCALHOST, 4), Ipv6Addr::UNSPECIFIED);
    }

    #[test]
    fn expired_clients_are_swept_once_a_window_and_a_full_table_takes_no_new_ones() {
        let limit = Limit::with_room(1, MINUTE, 10);
        let start = Instant::now();
        assert!(limit.allow(v4(1), start));
        let filled = start + MINUTE / 2;
        for number in 1..10 {
            assert!(limit.allow(IpAddr::V4(Ipv4Addr::from(number)), filled));
        }

        assert!(!limit.allow(v4(2), filled));
        // Swept at a minute: only v4(1) has expired.
        assert!(limit.allow(v4(2), start + MINUTE));
        assert!(!limit.allow(v4(3), start + MINUTE));
        // The rest expire before the next sweep, which is due at two minutes.
        assert!(!limit.allow(v4(3), filled + MINUTE));
        assert!(limit.allow(v4(3), start + MINUTE * 2));
    }

    #[test]
    fn one_ipv6_48_cant_fill_the_table() {
        let limit = Limit::new(1, MINUTE);
        let now = Instant::now();
        let in_48 = |n: usize| {
            let n = u16::try_from(n).unwrap();
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, n + 1, 0, 0, 0, 1))
        };
        for n in 0..PER_48 {
            assert!(limit.allow(in_48(n), now));
        }

        assert!(limit.allow(in_48(PER_48), now));
        assert!(!limit.allow(in_48(PER_48 + 1), now));
        assert!(!limit.allow(in_48(0), now));
        assert!(limit.allow("2001:db8:2::1".parse().unwrap(), now));
        // Once swept, the /48's count starts again from what is left.
        assert!(limit.allow(in_48(PER_48 + 1), now + MINUTE));
        assert!(limit.allow(in_48(PER_48 + 2), now + MINUTE));
    }
}
