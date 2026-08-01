//! X-Forwarded-For trust tests: the configured trusted proxy's XFF is honored
//! (first hop, independent rate budgets); any other peer's XFF is ignored.

mod common;

use common::*;

use clipboard_server::{IpNet, Limits, ServerConfig};

/// Runs one join attempt against an unregistered room and returns the error code.
async fn join_attempt(addr: std::net::SocketAddr, xff: Option<&str>, room: &str) -> String {
    let alice = TestIdentity::generate();
    let mut conn = match xff {
        Some(xff) => Client::connect_with_xff(addr, xff).await,
        None => Client::connect(addr).await,
    };
    let nonce = conn.hello(&alice).await;
    conn.send(&alice.join_frame(room, &nonce)).await;
    let code = conn
        .expect_error_close_broad(&["bad_auth", "rate_limited"])
        .await;
    conn.expect_closed().await;
    code
}

#[tokio::test]
async fn trusted_proxy_xff_gets_independent_rate_budgets() {
    let server = start_server(ServerConfig {
        trusted_proxy: Some(IpNet::parse("127.0.0.1").unwrap()),
        limits: Limits {
            join_attempts_per_minute: 2,
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = clipboard_core::crypto::room_id(&alice.bundle, &bob.bundle); // unregistered

    // First XFF identity exhausts its own budget.
    assert_eq!(
        join_attempt(server.addr, Some("203.0.113.7"), &room).await,
        "bad_auth"
    );
    assert_eq!(
        join_attempt(server.addr, Some("203.0.113.7"), &room).await,
        "bad_auth"
    );
    assert_eq!(
        join_attempt(server.addr, Some("203.0.113.7"), &room).await,
        "rate_limited"
    );

    // A second XFF identity behind the same proxy has its own budget.
    assert_eq!(
        join_attempt(server.addr, Some("198.51.100.9"), &room).await,
        "bad_auth"
    );
    assert_eq!(
        join_attempt(server.addr, Some("198.51.100.9"), &room).await,
        "bad_auth"
    );
    assert_eq!(
        join_attempt(server.addr, Some("198.51.100.9"), &room).await,
        "rate_limited"
    );
}

#[tokio::test]
async fn forged_xff_from_untrusted_peer_is_ignored() {
    // The trusted proxy is a different address than our TCP peer (127.0.0.1),
    // so XFF must be ignored and the peer address rate-limited instead.
    let server = start_server(ServerConfig {
        trusted_proxy: Some(IpNet::parse("10.8.0.1").unwrap()),
        limits: Limits {
            join_attempts_per_minute: 2,
            ..Limits::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let alice = TestIdentity::generate();
    let bob = TestIdentity::generate();
    let room = clipboard_core::crypto::room_id(&alice.bundle, &bob.bundle); // unregistered

    assert_eq!(
        join_attempt(server.addr, Some("203.0.113.7"), &room).await,
        "bad_auth"
    );
    // A fresh forged XFF does not reset the budget: it still counts against the peer.
    assert_eq!(
        join_attempt(server.addr, Some("198.51.100.9"), &room).await,
        "bad_auth"
    );
    assert_eq!(
        join_attempt(server.addr, Some("192.0.2.5"), &room).await,
        "rate_limited"
    );
    // And without any XFF header the peer is limited just the same.
    assert_eq!(join_attempt(server.addr, None, &room).await, "rate_limited");
}
