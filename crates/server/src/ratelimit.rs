//! Per-client-IP rate limiting with fixed windows: join attempts and inbound bytes.

use std::net::IpAddr;
use std::time::Instant;

use dashmap::DashMap;

use crate::config::Limits;

struct Window {
    started: Instant,
    joins: u32,
    bytes: u64,
}

/// Fixed-window per-IP counters for join attempts and inbound bytes.
pub struct IpRateLimiter {
    windows: DashMap<IpAddr, Window>,
    max_joins: u32,
    max_bytes: u64,
    window: std::time::Duration,
}

impl IpRateLimiter {
    #[must_use]
    pub fn new(limits: &Limits) -> Self {
        Self {
            windows: DashMap::new(),
            max_joins: limits.join_attempts_per_minute,
            max_bytes: limits.bytes_per_minute,
            window: limits.rate_window,
        }
    }

    /// Counts one join attempt; returns whether it is within the budget.
    pub fn check_join(&self, ip: IpAddr) -> bool {
        let mut entry = self.windows.entry(ip).or_insert_with(|| Window {
            started: Instant::now(),
            joins: 0,
            bytes: 0,
        });
        if entry.started.elapsed() >= self.window {
            *entry = Window {
                started: Instant::now(),
                joins: 0,
                bytes: 0,
            };
        }
        entry.joins += 1;
        entry.joins <= self.max_joins
    }

    /// Counts inbound bytes; returns whether the connection stays within the budget.
    pub fn add_bytes(&self, ip: IpAddr, bytes: u64) -> bool {
        let mut entry = self.windows.entry(ip).or_insert_with(|| Window {
            started: Instant::now(),
            joins: 0,
            bytes: 0,
        });
        if entry.started.elapsed() >= self.window {
            *entry = Window {
                started: Instant::now(),
                joins: 0,
                bytes: 0,
            };
        }
        entry.bytes = entry.bytes.saturating_add(bytes);
        entry.bytes <= self.max_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn limiter(max_joins: u32, max_bytes: u64) -> IpRateLimiter {
        IpRateLimiter::new(&Limits {
            join_attempts_per_minute: max_joins,
            bytes_per_minute: max_bytes,
            rate_window: Duration::from_secs(60),
            ..Limits::default()
        })
    }

    #[test]
    fn join_attempts_are_capped_per_window() {
        let limiter = limiter(2, u64::MAX);
        let ip: IpAddr = "203.0.113.1".parse().unwrap();
        assert!(limiter.check_join(ip));
        assert!(limiter.check_join(ip));
        assert!(!limiter.check_join(ip));
        // A different IP has an independent budget.
        let other: IpAddr = "203.0.113.2".parse().unwrap();
        assert!(limiter.check_join(other));
    }

    #[test]
    fn bytes_are_capped_per_window() {
        let limiter = limiter(u32::MAX, 100);
        let ip: IpAddr = "203.0.113.1".parse().unwrap();
        assert!(limiter.add_bytes(ip, 60));
        assert!(limiter.add_bytes(ip, 40));
        assert!(!limiter.add_bytes(ip, 1));
    }
}
