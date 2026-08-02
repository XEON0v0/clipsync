//! Mailbox integration tests (T9): join-bootstrap delivery semantics over the real
//! websocket stack backed by the persistent mailbox, restart reload, and the
//! `/healthz` degradation contract when persistence fails.

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::*;

use clipboard_core::protocol::Frame;
use clipboard_server::{
    InMemoryRegistry, Limits, MailboxOptions, PairingUnavailable, PersistentMailbox, ServerConfig,
    ServerHandle, start,
};
use tempfile::TempDir;

struct PersistentServer {
    addr: SocketAddr,
    registry: Arc<InMemoryRegistry>,
    mailbox: Arc<PersistentMailbox>,
    _handle: ServerHandle,
}

async fn start_persistent(dir: &Path, options: MailboxOptions) -> PersistentServer {
    let registry = Arc::new(InMemoryRegistry::new());
    let mailbox = PersistentMailbox::open(dir, options).expect("mailbox opens");
    let mut limits = Limits::default();
    limits.join_attempts_per_minute = 100;
    let handle = start(
        "127.0.0.1:0".parse().unwrap(),
        ServerConfig {
            limits,
            ..ServerConfig::default()
        },
        registry.clone(),
        Arc::new(PairingUnavailable),
        mailbox.clone(),
    )
    .await
    .expect("server binds");
    PersistentServer {
        addr: handle.addr(),
        registry,
        mailbox,
        _handle: handle,
    }
}

fn fast_options() -> MailboxOptions {
    MailboxOptions {
        retry_interval: Duration::from_millis(50),
        ..MailboxOptions::default()
    }
}

/// Polls until the room's mailbox file exists and contains `needle`, returning its
/// contents. Proves the room actor processed the clip that produced `needle`.
async fn wait_file_contains(dir: &Path, room: &str, needle: &str) -> String {
    let path = dir.join(format!("{room}.json"));
    eventually(|| {
        std::fs::read_to_string(&path)
            .map(|contents| contents.contains(needle))
            .unwrap_or(false)
    })
    .await;
    std::fs::read_to_string(&path).expect("mailbox file readable")
}

fn mailbox_file(dir: &Path, room: &str) -> PathBuf {
    dir.join(format!("{room}.json"))
}

/// Blocking HTTP GET against the plain HTTP listener; returns the status code.
fn http_get_status(addr: SocketAddr, path: &str) -> u16 {
    use std::io::{Read, Write};

    let mut stream = std::net::TcpStream::connect(addr).expect("http connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .expect("http write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("http read");
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().expect("http status line");
    status_line
        .split_whitespace()
        .nth(1)
        .expect("http status code")
        .parse()
        .expect("numeric status code")
}

async fn healthz_status(addr: SocketAddr) -> u16 {
    tokio::task::spawn_blocking(move || http_get_status(addr, "/healthz"))
        .await
        .expect("blocking http")
}

async fn wait_healthz(addr: SocketAddr, expected: u16) {
    timeout(TEST_TIMEOUT, async {
        loop {
            if healthz_status(addr).await == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("healthz did not become {expected} in time"));
}

/// Restores write permission on drop so tempdir cleanup cannot fail.
#[cfg(unix)]
struct DirModeGuard {
    path: PathBuf,
}

#[cfg(unix)]
impl DirModeGuard {
    fn read_only(path: &Path) -> Self {
        let guard = Self {
            path: path.to_path_buf(),
        };
        guard.set(0o555);
        guard
    }

    fn set(&self, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(mode))
            .expect("chmod mailbox dir");
    }
}

#[cfg(unix)]
impl Drop for DirModeGuard {
    fn drop(&mut self) {
        self.set(0o755);
    }
}

#[tokio::test]
async fn mailbox_bootstrap_delivers_latest_of_three_exactly_one_frame() {
    let dir = TempDir::new().unwrap();
    let server = start_persistent(dir.path(), fast_options()).await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;

    // Bob is offline: three clips land in the mailbox, latest wins.
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"clip-1")))
        .await;
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"clip-2")))
        .await;
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"clip-3")))
        .await;
    // Barrier: the disk snapshot containing clip-3 proves the actor processed all
    // three clips before bob's join begins.
    let on_disk = wait_file_contains(dir.path(), &room, &b64(b"clip-3")).await;
    assert!(!on_disk.contains(&b64(b"clip-2")), "latest wins on disk");

    let mut bob_conn = Client::connect(server.addr).await;
    let bootstrap = bob_conn.join(&bob, &room).await;
    match bootstrap {
        Frame::Clip {
            room_id,
            ciphertext_b64,
            origin_device,
            mailbox,
        } => {
            assert_eq!(room_id, room);
            assert_eq!(ciphertext_b64, b64(b"clip-3"));
            assert_eq!(origin_device, alice.fp());
            assert!(mailbox, "bootstrap clips carry mailbox=true");
        }
        other => panic!("expected bootstrap clip, got {other:?}"),
    }
    // Exactly one bootstrap frame: a second one must never appear.
    assert!(matches!(
        bob_conn.read(Duration::from_millis(300)).await,
        Read::Timeout
    ));

    // Live frames after the bootstrap always carry mailbox=false.
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"clip-4")))
        .await;
    match bob_conn.recv_frame().await {
        Frame::Clip {
            ciphertext_b64,
            origin_device,
            mailbox,
            ..
        } => {
            assert_eq!(ciphertext_b64, b64(b"clip-4"));
            assert_eq!(origin_device, alice.fp());
            assert!(!mailbox, "live clips carry mailbox=false");
        }
        other => panic!("expected live clip, got {other:?}"),
    }
    assert!(matches!(
        bob_conn.read(Duration::from_millis(300)).await,
        Read::Timeout
    ));
}

#[tokio::test]
async fn mailbox_origin_self_join_gets_mailbox_empty_bootstrap() {
    let dir = TempDir::new().unwrap();
    let server = start_persistent(dir.path(), fast_options()).await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"for-bob")))
        .await;
    wait_file_contains(dir.path(), &room, &b64(b"for-bob")).await;

    // The origin reconnects: its own pending clip must never bootstrap to itself.
    drop(alice_conn);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut alice_rejoin = Client::connect(server.addr).await;
    let bootstrap = alice_rejoin.join(&alice, &room).await;
    assert_eq!(
        bootstrap,
        Frame::MailboxEmpty,
        "origin's own join must not receive its own clip"
    );

    // The intended recipient still gets the clip.
    let mut bob_conn = Client::connect(server.addr).await;
    let bootstrap = bob_conn.join(&bob, &room).await;
    match bootstrap {
        Frame::Clip {
            ciphertext_b64,
            origin_device,
            mailbox,
            ..
        } => {
            assert_eq!(ciphertext_b64, b64(b"for-bob"));
            assert_eq!(origin_device, alice.fp());
            assert!(mailbox);
        }
        other => panic!("expected bootstrap clip for bob, got {other:?}"),
    }
}

#[tokio::test]
async fn mailbox_restart_reloads_pending_clip() {
    let dir = TempDir::new().unwrap();
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();

    let room = {
        let server = start_persistent(dir.path(), fast_options()).await;
        let room = register_room(&server, &alice, &bob);
        let mut alice_conn = Client::connect(server.addr).await;
        alice_conn.join_live(&alice, &room).await;
        alice_conn
            .send(&Client::clip_frame(&room, &b64(b"survives-restart")))
            .await;
        wait_file_contains(dir.path(), &room, &b64(b"survives-restart")).await;
        room
        // server handle and mailbox drop here: process "restart"
    };
    // Let the aborted accept task release the old mailbox before the rescan.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let server = start_persistent(dir.path(), fast_options()).await;
    server
        .registry
        .register_room(&room, &[alice.fp(), bob.fp()]);
    let mut bob_conn = Client::connect(server.addr).await;
    let bootstrap = bob_conn.join(&bob, &room).await;
    match bootstrap {
        Frame::Clip {
            ciphertext_b64,
            origin_device,
            mailbox,
            ..
        } => {
            assert_eq!(ciphertext_b64, b64(b"survives-restart"));
            assert_eq!(origin_device, alice.fp());
            assert!(mailbox);
        }
        other => panic!("expected reloaded bootstrap clip, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn mailbox_healthz_degraded_on_write_failure_and_recovers() {
    let dir = TempDir::new().unwrap();
    let server = start_persistent(dir.path(), fast_options()).await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = register_room(&server, &alice, &bob);

    wait_healthz(server.addr, 200).await;

    let mut alice_conn = Client::connect(server.addr).await;
    alice_conn.join_live(&alice, &room).await;
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"old-file")))
        .await;
    wait_file_contains(dir.path(), &room, &b64(b"old-file")).await;

    // Persistence outage: the mailbox directory becomes read-only.
    let guard = DirModeGuard::read_only(dir.path());
    alice_conn
        .send(&Client::clip_frame(&room, &b64(b"new-clip")))
        .await;
    wait_healthz(server.addr, 503).await;

    // The previous file is untouched.
    let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room)).unwrap();
    assert!(on_disk.contains(&b64(b"old-file")));
    assert!(!on_disk.contains(&b64(b"new-clip")));

    // Memory keeps serving: bob's bootstrap delivers the unpublished latest clip.
    let mut bob_conn = Client::connect(server.addr).await;
    let bootstrap = bob_conn.join(&bob, &room).await;
    match bootstrap {
        Frame::Clip {
            ciphertext_b64, mailbox, ..
        } => {
            assert_eq!(ciphertext_b64, b64(b"new-clip"));
            assert!(mailbox);
        }
        other => panic!("expected in-memory bootstrap clip, got {other:?}"),
    }

    // Recovery: the latest snapshot lands and healthz flips back.
    drop(guard);
    wait_healthz(server.addr, 200).await;
    wait_file_contains(dir.path(), &room, &b64(b"new-clip")).await;
}
