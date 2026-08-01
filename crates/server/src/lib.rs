//! ClipSync relay server: connection state machine, room routing, and join authentication.
//!
//! The relay is structurally incapable of decrypting clipboard content: it depends on
//! `clipboard-core` only through the `verify` feature (protocol types plus Ed25519/SHA-256
//! verification). Content crypto (chacha20poly1305, x25519-dalek, hkdf) is excluded at the
//! Cargo feature level and guarded by `scripts/audit/server-no-content-crypto.yml`.
//!
//! Security invariants implemented here:
//! - websocket frame and message limits are both 24 MiB; binary frames are rejected;
//! - `origin_device` and `device_id` are never trusted from the wire: `origin_device` is
//!   overwritten with the authenticated bundle fingerprint and `device_id` is recomputed;
//! - the join challenge nonce is CSPRNG 32 bytes, connection-bound, and single-use;
//! - X-Forwarded-For is honored only when the TCP peer equals the configured trusted
//!   proxy IP/CIDR;
//! - no lock guard is ever held across an `.await`.

pub mod config;
pub mod connection;
pub mod mailbox;
pub mod pairing;
pub mod ratelimit;
pub mod registry;
pub mod room;
pub mod server;

pub use config::{IpNet, Limits, ServerConfig, TestHooks};
pub use mailbox::{MailboxSink, NoopMailboxSink, PendingClip};
pub use pairing::{PairingHandler, PairingUnavailable};
pub use ratelimit::IpRateLimiter;
pub use registry::{InMemoryRegistry, Registry};
pub use server::{ServerHandle, ServerState, start};

/// Global quota for the room registry. Enforced by T8's registry implementation on the
/// pairing path (over-quota pairing is refused and logged); recorded here because T6
/// owns the registry seam.
pub const REGISTRY_MAX_ROOMS: usize = 100;

/// Global mailbox storage quota: [`REGISTRY_MAX_ROOMS`] rooms x 24 MiB. Enforced by T9's
/// mailbox persistence worker; recorded here because T6 owns the mailbox seam.
pub const MAILBOX_MAX_BYTES: u64 = REGISTRY_MAX_ROOMS as u64 * 24 * 1024 * 1024;
