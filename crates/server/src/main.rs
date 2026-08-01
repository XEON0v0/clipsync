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
    IpNet, NoopMailboxSink, PairingConfig, PairingRelay, PersistentRegistry, Registry,
    ServerConfig, start,
};

const UNACTIVATED_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[tokio::main]
async fn main() -> io::Result<()> {
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_owned());
    let registry = Arc::new(PersistentRegistry::open(&data_dir)?);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        if args == ["--prune-unactivated", "--older-than", "24h"] {
            let ttl_ms = i64::try_from(UNACTIVATED_TTL.as_millis()).unwrap_or(i64::MAX);
            let removed = registry.prune_unactivated(
                clipboard_server::registry::unix_time_ms().saturating_sub(ttl_ms),
            )?;
            println!("pruned {removed} unactivated room(s)");
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: clipsync-server [--prune-unactivated --older-than 24h]",
        ));
    }
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
    let pairing = Arc::new(PairingRelay::new(
        registry.clone(),
        PairingConfig::default(),
    ));
    // T9 wires the mailbox persistence worker; T6 defines the seam.
    let mailbox = Arc::new(NoopMailboxSink);
    let server = start(bind, config, registry.clone(), pairing, mailbox).await?;
    clipboard_server::registry::spawn_unactivated_sweeper(
        registry,
        std::time::Duration::from_secs(60 * 60),
        UNACTIVATED_TTL,
    );
    eprintln!("clipsync relay listening on ws://{}/ws", server.addr());
    std::future::pending::<()>().await;
    #[allow(unreachable_code)]
    Ok(())
}
