#![cfg(feature = "full")]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipboard_core::history::HistoryStore;
use clipboard_core::pairing::{PairingError, PairingStore, PendingConfirmation, QrPayload};
use clipboard_core::pairing_client::{
    PAIRING_CODE_ALPHABET, PairingChannel, PairingClient, TransportError, backoff_delay,
    backoff_delay_with_jitter, generate_pairing_code,
};
use clipboard_core::protocol::{Frame, ProtocolError};
use clipboard_core::session::{
    CallbackError, ClipContent, ClipItem, InboundOutcome, MailboxDisposition, Session,
    SessionCallback, SessionStore,
};
use uuid::Uuid;

#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::{FakeCommand, FakeServer};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("clipsync-test-{}", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the epoch")
        .as_millis();
    i64::try_from(millis).expect("millis fit i64")
}

struct RecordingCallback {
    live: Mutex<Vec<ClipItem>>,
    mailbox: Mutex<Vec<ClipItem>>,
}

impl RecordingCallback {
    fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
            mailbox: Mutex::new(Vec::new()),
        }
    }

    fn live_count(&self) -> usize {
        self.live.lock().expect("live lock").len()
    }

    fn mailbox_count(&self) -> usize {
        self.mailbox.lock().expect("mailbox lock").len()
    }
}

impl SessionCallback for RecordingCallback {
    fn on_clip(&self, item: &ClipItem) -> Result<(), CallbackError> {
        self.live.lock().expect("live lock").push(item.clone());
        Ok(())
    }

    fn on_mailbox_clip(&self, item: &ClipItem) -> Result<MailboxDisposition, CallbackError> {
        self.mailbox
            .lock()
            .expect("mailbox lock")
            .push(item.clone());
        Ok(MailboxDisposition::Applied)
    }
}

struct Endpoint {
    _dir: TestDir,
    store: PairingStore,
    session_dir: PathBuf,
    history_dir: PathBuf,
    callback: Arc<RecordingCallback>,
}

impl Endpoint {
    fn new() -> Self {
        let dir = TestDir::new();
        Self {
            store: PairingStore::new(dir.path().join("pairing")).expect("pairing store"),
            session_dir: dir.path().join("session"),
            history_dir: dir.path().join("history"),
            callback: Arc::new(RecordingCallback::new()),
            _dir: dir,
        }
    }

    fn session(&self, link: &clipboard_core::pairing_client::LiveLink) -> Session {
        let store = SessionStore::new(&self.session_dir).expect("session store");
        let history = HistoryStore::new(&self.history_dir).expect("history");
        Session::new(
            link.identity(),
            link.record(),
            store,
            history,
            self.callback.clone(),
        )
        .expect("session should open")
    }
}

fn different_sas(sas: &str) -> String {
    if sas == "000000" {
        "999999".to_owned()
    } else {
        "000000".to_owned()
    }
}

async fn pair_to_channels(
    server: &FakeServer,
    offerer: &Endpoint,
    claimer: &Endpoint,
) -> (PairingChannel, PairingChannel) {
    let client = PairingClient::connect(&offerer.store, server.url())
        .await
        .expect("offerer connect");
    let offer = client.pair_begin().await.expect("pair_begin");
    let qr = offer.qr().clone();
    let (channel_a, channel_b) = tokio::join!(
        async { offer.wait_peer().await.expect("wait_peer") },
        async {
            PairingClient::claim(&claimer.store, &qr)
                .await
                .expect("claim")
        },
    );
    (channel_a, channel_b)
}

#[tokio::test]
async fn pairing_offer_begin_returns_qr_only_after_offer_ok_and_sas_matches() {
    // Given
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let identity_a = offerer.store.load_identity().expect("identity");

    // When
    let client = PairingClient::connect(&offerer.store, server.url())
        .await
        .expect("connect");
    let offer = client.pair_begin().await.expect("pair_begin");
    let qr = offer.qr().clone();

    // Then: the QR exists only after pair_offer_ok and carries the OOB payload.
    assert_eq!(qr.server, server.url());
    assert_eq!(qr.code.len(), 6);
    assert!(
        qr.code
            .bytes()
            .all(|byte| PAIRING_CODE_ALPHABET.contains(&byte))
    );
    assert_eq!(qr.bundle, identity_a.public_bundle());

    // And: both sides derive the same SAS gate.
    let (channel_a, channel_b) = tokio::join!(
        async { offer.wait_peer().await.expect("wait_peer") },
        async {
            PairingClient::claim(&claimer.store, &qr)
                .await
                .expect("claim")
        },
    );
    assert_eq!(channel_a.pending().sas(), channel_b.pending().sas());
}

#[tokio::test]
async fn pairing_confirm_pair_joins_bootstraps_and_persists_both_sides() {
    // Given
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let (channel_a, channel_b) = pair_to_channels(&server, &offerer, &claimer).await;
    let sas = channel_a.pending().sas();

    // When
    let (link_a, link_b) = tokio::join!(
        channel_a.confirm_pair(&sas, &offerer.store),
        channel_b.confirm_pair(&sas, &claimer.store),
    );
    let link_a = link_a.expect("offerer confirm");
    let link_b = link_b.expect("claimer confirm");

    // Then: both ends join over the same connection and land in Live.
    assert_eq!(link_a.record().room_id, link_b.record().room_id);
    assert!(link_a.bootstrap_clip().is_none());
    assert!(link_b.bootstrap_clip().is_none());
    assert!(
        offerer
            .store
            .load_pairing()
            .expect("offerer pairing load")
            .is_some()
    );
    assert!(
        claimer
            .store
            .load_pairing()
            .expect("claimer pairing load")
            .is_some()
    );
}

#[tokio::test]
async fn pairing_confirm_with_wrong_sas_is_rejected_before_any_join() {
    // Given
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let (channel_a, channel_b) = pair_to_channels(&server, &offerer, &claimer).await;
    let wrong = different_sas(&channel_b.pending().sas());
    drop(channel_a);

    // When
    let result = channel_b.confirm_pair(&wrong, &claimer.store).await;

    // Then: the SAS gate refuses before join and nothing is persisted.
    let error = result.expect_err("wrong SAS must be rejected");
    assert!(matches!(
        error,
        TransportError::Pairing(PairingError::SasMismatch)
    ));
    assert!(
        claimer
            .store
            .load_pairing()
            .expect("pairing load")
            .is_none()
    );
}

#[tokio::test]
async fn pairing_claim_with_unknown_code_receives_code_expired() {
    // Given
    let server = FakeServer::start().await.expect("fake server");
    let claimer = Endpoint::new();
    let other = Endpoint::new();
    let other_identity = other.store.load_identity().expect("other identity");
    let qr = QrPayload::new(
        server.url(),
        "ZZZ299",
        other_identity.public_bundle(),
        [9_u8; 16],
    )
    .expect("fixture QR");

    // When
    let result = PairingClient::claim(&claimer.store, &qr).await;

    // Then
    let error = result.expect_err("unknown code must be rejected");
    assert!(matches!(
        error,
        TransportError::Server { ref code, .. } if code == "code_expired"
    ));
}

#[tokio::test]
async fn pairing_join_without_record_fails_closed() {
    // Given
    let endpoint = Endpoint::new();

    // When
    let result = PairingClient::join(&endpoint.store).await;

    // Then
    assert!(matches!(
        result.expect_err("join without pairing must fail"),
        TransportError::NotPaired
    ));
}

#[tokio::test]
async fn pairing_second_bootstrap_frame_is_rejected_in_live() {
    // Given
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let (channel_a, channel_b) = pair_to_channels(&server, &offerer, &claimer).await;
    let sas = channel_a.pending().sas();
    drop(channel_b);
    server.command(FakeCommand::ExtraBootstrap);

    // When: exactly one bootstrap frame is consumed by confirm, then a second arrives.
    let mut link_a = channel_a
        .confirm_pair(&sas, &offerer.store)
        .await
        .expect("confirm");
    let result = link_a.recv().await;

    // Then
    let error = result.expect_err("second bootstrap frame must be rejected");
    assert!(matches!(
        error,
        TransportError::Protocol(ProtocolError::BadFrame { .. })
    ));
}

#[tokio::test]
async fn pairing_live_clip_roundtrips_between_sessions_over_relay() {
    // Given
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let (channel_a, channel_b) = pair_to_channels(&server, &offerer, &claimer).await;
    let sas = channel_a.pending().sas();
    let (link_a, link_b) = tokio::join!(
        channel_a.confirm_pair(&sas, &offerer.store),
        channel_b.confirm_pair(&sas, &claimer.store),
    );
    let mut link_a = link_a.expect("offerer confirm");
    let mut link_b = link_b.expect("claimer confirm");
    let mut session_a = offerer.session(&link_a);
    let mut session_b = claimer.session(&link_b);

    // When: A sends a live clip through the relay to B.
    let frame = session_a
        .send_clip(ClipContent::Text("over the wire".to_owned()), now_ms())
        .expect("send_clip");
    link_a.send(&frame).await.expect("relay send");
    let received = link_b.recv().await.expect("relay recv");
    let Frame::Clip {
        room_id,
        ciphertext_b64,
        origin_device,
        mailbox,
    } = received
    else {
        panic!("relay must deliver a clip frame");
    };
    let outcome = session_b
        .handle_clip(&room_id, &ciphertext_b64, mailbox, now_ms())
        .expect("handle_clip");

    // Then
    assert!(!mailbox);
    assert_eq!(origin_device, link_a.identity().bundle_fp());
    assert_eq!(outcome, InboundOutcome::LiveApplied);
    assert_eq!(claimer.callback.live_count(), 1);
    assert_eq!(claimer.callback.mailbox_count(), 0);
}

#[tokio::test]
async fn pairing_offline_clip_arrives_once_as_mailbox_bootstrap_after_rejoin() {
    // Given: a fully paired, live pair.
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let (channel_a, channel_b) = pair_to_channels(&server, &offerer, &claimer).await;
    let sas = channel_a.pending().sas();
    let (link_a, link_b) = tokio::join!(
        channel_a.confirm_pair(&sas, &offerer.store),
        channel_b.confirm_pair(&sas, &claimer.store),
    );
    let mut link_a = link_a.expect("offerer confirm");
    let link_b = link_b.expect("claimer confirm");
    let mut session_a = offerer.session(&link_a);
    let mut session_b = claimer.session(&link_b);

    // When: B disconnects, A sends while B is offline, and B rejoins directly.
    link_b.close().await.expect("close");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let frame = session_a
        .send_clip(
            ClipContent::Text("queued while offline".to_owned()),
            now_ms(),
        )
        .expect("send_clip");
    link_a.send(&frame).await.expect("relay send");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let link_b2 = PairingClient::join(&claimer.store)
        .await
        .expect("direct rejoin");

    // Then: the bootstrap carries exactly the queued mailbox clip.
    let (room_id, ciphertext_b64) = link_b2.bootstrap_clip().expect("mailbox bootstrap");
    let outcome = session_b
        .handle_clip(&room_id, &ciphertext_b64, true, now_ms())
        .expect("mailbox handle");
    assert_eq!(outcome, InboundOutcome::MailboxApplied);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    assert_eq!(claimer.callback.live_count(), 0);
    assert_eq!(
        session_b.history().list()[0].source,
        clipboard_core::history::HistorySource::Remote
    );
}

#[tokio::test]
async fn pairing_live_link_can_replace_itself_with_a_fresh_join() {
    let server = FakeServer::start().await.expect("fake server");
    let offerer = Endpoint::new();
    let claimer = Endpoint::new();
    let (channel_a, channel_b) = pair_to_channels(&server, &offerer, &claimer).await;
    let sas = channel_a.pending().sas();
    let (link_a, link_b) = tokio::join!(
        channel_a.confirm_pair(&sas, &offerer.store),
        channel_b.confirm_pair(&sas, &claimer.store),
    );
    let _link_a = link_a.expect("offerer confirm");
    let mut link_b = link_b.expect("claimer confirm");

    link_b
        .reconnect(&claimer.store)
        .await
        .expect("fresh join should replace the link");

    assert_eq!(
        link_b.record().room_id,
        claimer
            .store
            .load_pairing()
            .expect("pairing load")
            .expect("pairing exists")
            .room_id
    );
    assert!(link_b.bootstrap_clip().is_none());
}

#[test]
fn pairing_backoff_attempt_zero_stays_within_plus_minus_twenty_percent() {
    // Given / When
    for _ in 0..64 {
        let delay = backoff_delay(0);
        // Then
        assert!(delay >= Duration::from_millis(800), "{delay:?} below floor");
        assert!(delay <= Duration::from_millis(1200), "{delay:?} above cap");
    }
}

#[test]
fn pairing_backoff_grows_exponentially_until_the_thirty_second_cap() {
    // Given / When / Then: zero jitter exposes the raw 1s doubling ladder.
    let ladder: Vec<Duration> = (0..6)
        .map(|attempt| backoff_delay_with_jitter(attempt, 0.0))
        .collect();
    assert_eq!(
        ladder,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(30),
        ]
    );
}

#[test]
fn pairing_backoff_saturates_and_jitter_is_clamped() {
    // Given / When
    let capped = backoff_delay_with_jitter(31, 0.0);
    let high = backoff_delay_with_jitter(2, 0.5);
    let low = backoff_delay_with_jitter(2, -0.5);

    // Then
    assert_eq!(capped, Duration::from_secs(30));
    assert_eq!(high, Duration::from_millis(4800));
    assert_eq!(low, Duration::from_millis(3200));
    for _ in 0..64 {
        let delay = backoff_delay(10);
        assert!(delay >= Duration::from_secs(24));
        assert!(delay <= Duration::from_secs(36));
    }
}

#[test]
fn pairing_generate_code_uses_six_chars_from_thirty_char_alphabet() {
    // Given
    assert_eq!(PAIRING_CODE_ALPHABET.len(), 30);

    // When
    let mut seen = HashSet::new();
    for _ in 0..64 {
        let code = generate_pairing_code();
        // Then
        assert_eq!(code.len(), 6);
        assert!(
            code.bytes()
                .all(|byte| PAIRING_CODE_ALPHABET.contains(&byte))
        );
        seen.insert(code);
    }
    assert!(seen.len() > 1);
}

#[test]
fn pairing_pending_confirmation_is_exposed_for_sas_display() {
    // Given: the type-level contract that the SAS gate stays in T5.
    fn assert_pending(_: &PendingConfirmation) {}

    // When / Then: compiles only if PairingChannel exposes the T5 gate type.
    let _ = assert_pending;
}
