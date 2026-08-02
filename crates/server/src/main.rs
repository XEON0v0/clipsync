//! ClipSync relay server binary.
//!
//! Configuration is via environment variables:
//! - `BIND_ADDR` (default `127.0.0.1:8787`): listen address;
//! - `TRUSTED_PROXY` (optional): exact IP or CIDR of the Caddy container; only
//!   connections from this peer may supply X-Forwarded-For.
//! - `DATA_DIR` (default `./data`): registry directory;
//! - `MAILBOX_DIR` (default `$DATA_DIR/mailboxes`, `/data/mailboxes` in the
//!   production container): one latest-clip snapshot per room.
//!
//! TLS terminates at the Caddy layer, never here.

use std::io;
use std::io::{BufRead as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use clipboard_server::{
    IpNet, MailboxOptions, PairingConfig, PairingRelay, PersistentMailbox, PersistentRegistry,
    Registry, ServerConfig, start,
};

const UNACTIVATED_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[tokio::main]
async fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--healthcheck"] {
        let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
        return check_health(healthcheck_address(&bind)?);
    }

    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| "./data".to_owned());
    let registry = Arc::new(PersistentRegistry::open(&data_dir)?);
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
            "usage: clipsync-server [--healthcheck | --prune-unactivated --older-than 24h]",
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
    let mailbox_dir = std::env::var("MAILBOX_DIR").unwrap_or_else(|_| {
        std::path::Path::new(&data_dir)
            .join("mailboxes")
            .to_string_lossy()
            .into_owned()
    });
    let mailbox = PersistentMailbox::open(&mailbox_dir, MailboxOptions::default())?;
    let server = start(bind, config, registry.clone(), pairing, mailbox.clone()).await?;
    mailbox.spawn_ttl_sweeper(std::time::Duration::from_secs(60 * 60));
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

fn healthcheck_address(bind: &str) -> io::Result<SocketAddr> {
    let mut address: SocketAddr = bind
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if address.ip().is_unspecified() {
        address.set_ip(match address.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    Ok(address)
}

fn check_health(address: SocketAddr) -> io::Result<()> {
    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;

    let mut status = String::new();
    io::BufReader::new(stream).read_line(&mut status)?;
    if matches!(status.trim_end(), "HTTP/1.1 200 OK" | "HTTP/1.0 200 OK") {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "health endpoint returned {status:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthcheck_uses_loopback_for_unspecified_bind_addresses() {
        assert_eq!(
            healthcheck_address("0.0.0.0:8080").unwrap(),
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            healthcheck_address("[::]:9090").unwrap(),
            "[::1]:9090".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn healthcheck_requires_an_http_200_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 256];
            let size = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..size]).starts_with("GET /healthz HTTP/1.1\r\n")
            );
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 9\r\n\r\ndegraded\n",
                )
                .unwrap();
        });

        let error = check_health(address).unwrap_err();
        assert!(error.to_string().contains("HTTP/1.1 503"));
        worker.join().unwrap();
    }
}
