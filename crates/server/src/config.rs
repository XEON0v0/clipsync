//! Server configuration, resource limits, trusted-proxy matching, and test hooks.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc};

/// Resource limits and deadlines. Production defaults implement the protocol contract;
/// tests override individual fields to exercise guards quickly and deterministically.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Global simultaneous websocket connections.
    pub max_connections: usize,
    /// Bounded outbound queue per connection, in frames.
    pub outbox_max_frames: usize,
    /// Aggregate pending outbound bytes per connection; slow consumers past this
    /// budget are disconnected.
    pub outbox_max_bytes: usize,
    /// Bounded room-actor inbox, in events.
    pub inbox_max_events: usize,
    /// Aggregate pending room-actor inbox bytes.
    pub inbox_max_bytes: usize,
    /// Deadline from websocket upgrade to a valid `hello`.
    pub handshake_deadline: Duration,
    /// Idle deadline from `hello_ok` to a `pair_*`/`join` frame. No deadline once live.
    pub join_idle_deadline: Duration,
    /// Per-client-IP join attempts per `rate_window`.
    pub join_attempts_per_minute: u32,
    /// Per-client-IP inbound bytes per `rate_window`.
    pub bytes_per_minute: u64,
    /// Rate-limit window length.
    pub rate_window: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 256,
            outbox_max_frames: 64,
            outbox_max_bytes: 48 * 1024 * 1024,
            inbox_max_events: 64,
            inbox_max_bytes: 48 * 1024 * 1024,
            handshake_deadline: Duration::from_secs(10),
            join_idle_deadline: Duration::from_secs(300),
            join_attempts_per_minute: 5,
            bytes_per_minute: 64 * 1024 * 1024,
            rate_window: Duration::from_secs(60),
        }
    }
}

/// Optional instrumentation for integration tests. Never set in production.
#[derive(Clone, Default)]
pub struct TestHooks {
    /// When set, the room actor acquires one permit before processing each event,
    /// letting tests sequence events deterministically.
    pub room_event_gate: Option<Arc<Semaphore>>,
    /// When set, every connection writer task acquires one permit before each send,
    /// letting tests stall outbound writes to fill bounded queues.
    pub writer_gate: Option<Arc<Semaphore>>,
    /// When set, receives a short label (`"join"`/`"clip"`/`"disconnect"`) after each
    /// event is enqueued into a room actor inbox.
    pub event_log: Option<mpsc::UnboundedSender<&'static str>>,
}

/// Server-wide configuration.
#[derive(Clone, Default)]
pub struct ServerConfig {
    /// Trusted proxy address (exact IP or CIDR). X-Forwarded-For is honored only when
    /// the TCP peer address matches; otherwise the TCP peer address is used.
    pub trusted_proxy: Option<IpNet>,
    /// Resource limits; see [`Limits`].
    pub limits: Limits,
    /// Test-only instrumentation hooks.
    pub hooks: TestHooks,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("trusted_proxy", &self.trusted_proxy)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl ServerConfig {
    /// Resolves the client IP used for rate limiting: the first X-Forwarded-For hop
    /// when the TCP peer matches the configured trusted proxy, otherwise the peer.
    #[must_use]
    pub fn client_ip(&self, peer: IpAddr, x_forwarded_for: Option<&str>) -> IpAddr {
        if let (Some(trusted), Some(header)) = (&self.trusted_proxy, x_forwarded_for)
            && trusted.contains(peer)
            && let Some(first_hop) = header.split(',').next()
            && let Ok(ip) = first_hop.trim().parse::<IpAddr>()
        {
            return ip;
        }
        peer
    }
}

/// An exact IP address or an explicit CIDR range. Only the configured value is
/// trusted; private ranges are never trusted implicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpNet {
    V4(Ipv4Addr, u8),
    V6(Ipv6Addr, u8),
}

impl IpNet {
    /// Parses `IP` or `IP/prefix` (IPv4 or IPv6). A bare address is an exact host.
    ///
    /// # Errors
    /// Returns a description when the address or prefix is malformed.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => {
                let prefix = prefix
                    .parse::<u8>()
                    .map_err(|_| format!("invalid CIDR prefix in {value:?}"))?;
                (address, Some(prefix))
            }
            None => (value, None),
        };
        match IpAddr::from_str(address).map_err(|_| format!("invalid IP address in {value:?}"))? {
            IpAddr::V4(addr) => {
                let prefix = prefix.unwrap_or(32);
                if prefix > 32 {
                    return Err(format!("IPv4 prefix /{prefix} exceeds /32"));
                }
                Ok(Self::V4(addr, prefix))
            }
            IpAddr::V6(addr) => {
                let prefix = prefix.unwrap_or(128);
                if prefix > 128 {
                    return Err(format!("IPv6 prefix /{prefix} exceeds /128"));
                }
                Ok(Self::V6(addr, prefix))
            }
        }
    }

    /// Returns whether `ip` falls inside this network.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip) {
            (Self::V4(network, prefix), IpAddr::V4(addr)) => {
                prefix_matches(&network.octets(), &addr.octets(), *prefix)
            }
            (Self::V6(network, prefix), IpAddr::V6(addr)) => {
                prefix_matches(&network.octets(), &addr.octets(), *prefix)
            }
            _ => false,
        }
    }
}

fn prefix_matches(network: &[u8], address: &[u8], prefix: u8) -> bool {
    let full_bytes = usize::from(prefix / 8);
    let remaining_bits = prefix % 8;
    if network[..full_bytes] != address[..full_bytes] {
        return false;
    }
    if remaining_bits == 0 {
        return true;
    }
    let mask = 0xff_u8 << (8 - remaining_bits);
    network[full_bytes] & mask == address[full_bytes] & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_addresses_and_cidrs() {
        assert_eq!(
            IpNet::parse("192.168.1.10").unwrap(),
            IpNet::V4(Ipv4Addr::new(192, 168, 1, 10), 32)
        );
        assert_eq!(
            IpNet::parse("10.0.0.0/8").unwrap(),
            IpNet::V4(Ipv4Addr::new(10, 0, 0, 0), 8)
        );
        assert_eq!(
            IpNet::parse("::1").unwrap(),
            IpNet::V6("::1".parse().unwrap(), 128)
        );
        assert!(IpNet::parse("10.0.0.0/33").is_err());
        assert!(IpNet::parse("not-an-ip").is_err());
        assert!(IpNet::parse("10.0.0.0/x").is_err());
    }

    #[test]
    fn contains_matches_prefix_bits() {
        let net = IpNet::parse("10.1.2.0/24").unwrap();
        assert!(net.contains("10.1.2.77".parse().unwrap()));
        assert!(!net.contains("10.1.3.1".parse().unwrap()));
        let exact = IpNet::parse("127.0.0.1").unwrap();
        assert!(exact.contains("127.0.0.1".parse().unwrap()));
        assert!(!exact.contains("127.0.0.2".parse().unwrap()));
        // Non-byte-aligned prefix.
        let odd = IpNet::parse("10.0.0.0/7").unwrap();
        assert!(odd.contains("11.0.0.1".parse().unwrap()));
        assert!(!odd.contains("12.0.0.1".parse().unwrap()));
        // Address family mismatch never matches.
        let v6 = IpNet::parse("::ffff:10.0.0.1/128").unwrap();
        assert!(!v6.contains("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn client_ip_honors_xff_only_from_trusted_proxy() {
        let config = ServerConfig {
            trusted_proxy: Some(IpNet::parse("127.0.0.1").unwrap()),
            ..ServerConfig::default()
        };
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let via_xff: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            config.client_ip(peer, Some("203.0.113.7, 10.0.0.1")),
            via_xff
        );
        // Malformed header falls back to the peer.
        assert_eq!(config.client_ip(peer, Some("garbage")), peer);
        // Untrusted peer: XFF ignored.
        let stranger: IpAddr = "198.51.100.9".parse().unwrap();
        assert_eq!(config.client_ip(stranger, Some("203.0.113.7")), stranger);
        // No trusted proxy configured: XFF ignored.
        let open = ServerConfig::default();
        assert_eq!(open.client_ip(peer, Some("203.0.113.7")), peer);
    }
}
