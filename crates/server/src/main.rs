//! ClipSync relay server binary.
//!
//! Configuration is via environment variables:
//! - `BIND_ADDR` (default `127.0.0.1:8787`): listen address;
//! - `TRUSTED_PROXY` (optional): exact IP or CIDR of the Caddy container; only
//!   connections from this peer may supply X-Forwarded-For.
//!
//! TLS terminates at the Caddy layer, never here.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use clipboard_server::{
    InMemoryRegistry, IpNet, NoopMailboxSink, PairingUnavailable, ServerConfig, start,
};

#[tokio::main]
async fn main() -> io::Result<()> {
    let bind: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_owned())
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let trusted_proxy = std::env::var("TRUSTED_PROXY")
        .ok()
        .map(|value| {
            IpNet::parse(&value).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
        })
        .transpose()?;
    let config = ServerConfig {
        trusted_proxy,
        ..ServerConfig::default()
    };
    // T8 replaces this with the atomic on-disk registry; until then no room is
    // registered and every join is refused with bad_auth.
    let registry = Arc::new(InMemoryRegistry::new());
    // T8 wires the real pairing exchange; T6 defines the seam.
    let pairing = Arc::new(PairingUnavailable);
    // T9 wires the mailbox persistence worker; T6 defines the seam.
    let mailbox = Arc::new(NoopMailboxSink);
    let server = start(bind, config, registry, pairing, mailbox).await?;
    eprintln!("clipsync relay listening on ws://{}/ws", server.addr());
    std::future::pending::<()>().await;
    #[allow(unreachable_code)]
    Ok(())
}
