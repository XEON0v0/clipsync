#![cfg(feature = "full")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clipboard_core::crypto::Identity;
use clipboard_core::history::{HistoryItem, HistorySource, HistoryStore};
use clipboard_core::pairing::{Claimer, Offerer, PairingRecord, PairingStore, QrPayload};
use clipboard_core::protocol::{Frame, ProtocolError, decode_frame, encode_frame};
use clipboard_core::session::{
    CallbackError, ClipContent, ClipItem, InboundOutcome, MailboxDisposition, ReceiveStage,
    SendStage, Session, SessionCallback, SessionError, SessionStore, text_hash,
};
use uuid::Uuid;

const NOW: i64 = 1_700_000_000_000;

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

struct RecordingCallback {
    live: Mutex<Vec<ClipItem>>,
    mailbox: Mutex<Vec<ClipItem>>,
    disposition: Mutex<MailboxDisposition>,
    fail_mailbox: Mutex<Option<CallbackError>>,
}

impl RecordingCallback {
    fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
            mailbox: Mutex::new(Vec::new()),
            disposition: Mutex::new(MailboxDisposition::Applied),
            fail_mailbox: Mutex::new(None),
        }
    }

    fn live_items(&self) -> Vec<ClipItem> {
        self.live.lock().expect("live lock").clone()
    }

    fn live_count(&self) -> usize {
        self.live.lock().expect("live lock").len()
    }

    fn mailbox_count(&self) -> usize {
        self.mailbox.lock().expect("mailbox lock").len()
    }

    fn set_disposition(&self, disposition: MailboxDisposition) {
        *self.disposition.lock().expect("disposition lock") = disposition;
    }

    fn fail_mailbox_with(&self, error: CallbackError) {
        *self.fail_mailbox.lock().expect("fail mailbox lock") = Some(error);
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
        if let Some(error) = self.fail_mailbox.lock().expect("fail mailbox lock").take() {
            return Err(error);
        }
        Ok(*self.disposition.lock().expect("disposition lock"))
    }
}

struct Endpoint {
    _root: TestDir,
    identity: Identity,
    record: PairingRecord,
    session_dir: PathBuf,
    history_dir: PathBuf,
    callback: Arc<RecordingCallback>,
}

fn paired_endpoints() -> (Endpoint, Endpoint) {
    let offerer_root = TestDir::new();
    let claimer_root = TestDir::new();
    let offerer_store = PairingStore::new(offerer_root.path().join("pairing"))
        .expect("offerer pairing store should open");
    let claimer_store = PairingStore::new(claimer_root.path().join("pairing"))
        .expect("claimer pairing store should open");
    let offerer_identity = offerer_store.load_identity().expect("offerer identity");
    let claimer_identity = claimer_store.load_identity().expect("claimer identity");

    let qr = QrPayload::new(
        "wss://relay.example/ws",
        "ABC234",
        offerer_identity.public_bundle(),
        [41_u8; 16],
    )
    .expect("fixture QR should be valid");
    let offerer_pending = Offerer::start(&offerer_identity, qr.clone())
        .expect("offer should start")
        .receive_peer(claimer_identity.public_bundle())
        .expect("peer should be distinct");
    let claimer_pending = Claimer::start(&claimer_identity, qr)
        .expect("claim should start")
        .receive_peer("wss://relay.example/ws", &offerer_identity.public_bundle())
        .expect("pins should match");
    let sas = offerer_pending.sas();
    assert_eq!(sas, claimer_pending.sas());
    let offerer_record = offerer_pending
        .confirm(&sas, &offerer_store)
        .expect("offerer confirm should persist")
        .record()
        .clone();
    let claimer_record = claimer_pending
        .confirm(&sas, &claimer_store)
        .expect("claimer confirm should persist")
        .record()
        .clone();

    let make = |root: TestDir, identity: Identity, record: PairingRecord| Endpoint {
        session_dir: root.path().join("session"),
        history_dir: root.path().join("history"),
        _root: root,
        identity,
        record,
        callback: Arc::new(RecordingCallback::new()),
    };
    let offerer = make(offerer_root, offerer_identity, offerer_record);
    let claimer = make(claimer_root, claimer_identity, claimer_record);
    (offerer, claimer)
}

fn open_session(endpoint: &Endpoint) -> Session {
    let store = SessionStore::new(&endpoint.session_dir).expect("session store should reopen");
    let history = HistoryStore::new(&endpoint.history_dir).expect("history should open");
    Session::new(
        &endpoint.identity,
        &endpoint.record,
        store,
        history,
        endpoint.callback.clone(),
    )
    .expect("session should open")
}

fn deliver(
    sender: &mut Session,
    receiver: &mut Session,
    content: ClipContent,
    ts_ms: i64,
) -> InboundOutcome {
    let frame = sender
        .send_clip(content, ts_ms)
        .expect("send_clip should succeed");
    let Frame::Clip {
        room_id,
        ciphertext_b64,
        mailbox,
        ..
    } = frame
    else {
        panic!("send_clip must return a clip frame");
    };
    receiver
        .handle_clip(&room_id, &ciphertext_b64, mailbox, ts_ms)
        .expect("handle_clip should succeed")
}

fn history_items(session: &Session) -> Vec<HistoryItem> {
    session.history().list()
}

fn clip_fields(frame: Frame) -> (String, String, bool) {
    let Frame::Clip {
        room_id,
        ciphertext_b64,
        mailbox,
        ..
    } = frame
    else {
        panic!("send_clip must return a clip frame");
    };
    (room_id, ciphertext_b64, mailbox)
}

#[test]
fn session_send_clip_reserves_seq_before_sealing_and_records_local_history() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut session = open_session(&offerer);
    let mut peer_session = open_session(&claimer);
    let before = session.next_seq();

    // When
    let frame = session
        .send_clip(ClipContent::Text("hello".to_owned()), NOW)
        .expect("send_clip should succeed");

    // Then
    assert_eq!(before, 1);
    assert_eq!(session.next_seq(), before + 1);
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame.clone());
    assert_eq!(room_id, offerer.record.room_id);
    assert!(!mailbox);
    let Frame::Clip { origin_device, .. } = &frame else {
        panic!("clip frame");
    };
    assert!(origin_device.is_empty());
    assert!(
        !STANDARD
            .decode(&ciphertext_b64)
            .expect("valid base64")
            .is_empty()
    );
    let items = history_items(&session);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::Local);
    assert_eq!(items[0].ts_ms, NOW);
    let outcome = peer_session
        .handle_clip(&room_id, &ciphertext_b64, mailbox, NOW)
        .expect("peer should accept the sent frame");
    assert_eq!(outcome, InboundOutcome::LiveApplied);
    assert_eq!(items[0].id, history_items(&peer_session)[0].id);
}

#[test]
fn session_send_clip_allocates_monotonic_seq_without_reuse() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);

    // When
    let first = deliver(
        &mut sender,
        &mut receiver,
        ClipContent::Text("one".to_owned()),
        NOW,
    );
    let second = deliver(
        &mut sender,
        &mut receiver,
        ClipContent::Text("two".to_owned()),
        NOW,
    );

    // Then
    assert_eq!(first, InboundOutcome::LiveApplied);
    assert_eq!(second, InboundOutcome::LiveApplied);
    let live = claimer.callback.live_items();
    let seqs: Vec<u64> = live.iter().map(|item| item.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
    assert_eq!(receiver.last_seq(), 2);
}

#[test]
fn session_send_seq_survives_crash_after_reservation_without_reuse() {
    // Given
    let (offerer, _claimer) = paired_endpoints();
    let reserved;
    {
        let mut session = open_session(&offerer);
        reserved = session.next_seq();
        let result = session.send_clip_with_fault(
            ClipContent::Text("crash me".to_owned()),
            NOW,
            Some(SendStage::SeqReserved),
        );
        assert!(matches!(result, Err(SessionError::InjectedFault)));
    }

    // When: restart and inspect the recovered counter.
    let restarted = open_session(&offerer);
    let recovered = restarted.next_seq();

    // Then: the crash may skip a sequence number but must never reuse one.
    assert!(recovered > reserved);
}

#[test]
fn session_live_delivery_applies_callback_once_and_records_remote_history() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);

    // When
    let outcome = deliver(
        &mut sender,
        &mut receiver,
        ClipContent::Text("live text".to_owned()),
        NOW,
    );

    // Then
    assert_eq!(outcome, InboundOutcome::LiveApplied);
    assert_eq!(claimer.callback.live_count(), 1);
    assert_eq!(claimer.callback.mailbox_count(), 0);
    let live = claimer.callback.live_items();
    assert_eq!(live[0].content, ClipContent::Text("live text".to_owned()));
    assert_eq!(live[0].seq, 1);
    assert_eq!(live[0].ts_ms, NOW);
    let items = history_items(&receiver);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::Remote);
}

#[test]
fn session_duplicate_item_id_is_idempotent_and_never_recallbacks() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    let frame = sender
        .send_clip(ClipContent::Text("dup".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);

    // When: the relay delivers the same frame twice.
    let first = receiver
        .handle_clip(&room_id, &ciphertext_b64, mailbox, NOW)
        .expect("first delivery");
    let second = receiver
        .handle_clip(&room_id, &ciphertext_b64, mailbox, NOW)
        .expect("second delivery");

    // Then
    assert_eq!(first, InboundOutcome::LiveApplied);
    assert_eq!(second, InboundOutcome::Duplicate);
    assert_eq!(claimer.callback.live_count(), 1);
    assert_eq!(history_items(&receiver).len(), 1);
}

#[test]
fn session_replayed_seq_at_or_below_high_water_never_callbacks() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    let first = sender
        .send_clip(ClipContent::Text("first".to_owned()), NOW)
        .expect("first send");
    let second = sender
        .send_clip(ClipContent::Text("second".to_owned()), NOW)
        .expect("second send");
    let (first_room, first_ct, first_mb) = clip_fields(first);
    let (second_room, second_ct, second_mb) = clip_fields(second);
    receiver
        .handle_clip(&first_room, &first_ct, first_mb, NOW)
        .expect("first delivery");
    receiver
        .handle_clip(&second_room, &second_ct, second_mb, NOW)
        .expect("second delivery");

    // When: the relay replays an old authenticated frame below the high-water.
    let replayed = receiver
        .handle_clip(&first_room, &first_ct, first_mb, NOW)
        .expect("replay delivery should succeed");

    // Then
    assert_eq!(replayed, InboundOutcome::Duplicate);
    assert_eq!(claimer.callback.live_count(), 2);
    assert_eq!(receiver.last_seq(), 2);
    assert_eq!(history_items(&receiver).len(), 2);
}

#[test]
fn session_mailbox_clip_is_deferred_and_applied_disposition_promotes_source() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    claimer
        .callback
        .set_disposition(MailboxDisposition::Applied);
    let frame = sender
        .send_clip(
            ClipContent::Text("mailbox text".to_owned()),
            NOW - 3_600_000,
        )
        .expect("send should succeed");
    let (room_id, ciphertext_b64, _) = clip_fields(frame);

    // When: the frame arrives from the mailbox.
    let outcome = receiver
        .handle_clip(&room_id, &ciphertext_b64, true, NOW)
        .expect("mailbox delivery should succeed");

    // Then
    assert_eq!(outcome, InboundOutcome::MailboxApplied);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    assert_eq!(claimer.callback.live_count(), 0);
    let items = history_items(&receiver);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::Remote);
}

#[test]
fn session_mailbox_clip_with_deferred_disposition_stays_remote_deferred() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    claimer
        .callback
        .set_disposition(MailboxDisposition::Deferred);
    let frame = sender
        .send_clip(ClipContent::Text("android mailbox".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, _) = clip_fields(frame);

    // When
    let outcome = receiver
        .handle_clip(&room_id, &ciphertext_b64, true, NOW)
        .expect("mailbox delivery should succeed");

    // Then
    assert_eq!(outcome, InboundOutcome::MailboxDeferred);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    let items = history_items(&receiver);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::RemoteDeferred);
}

#[test]
fn session_stale_live_clip_falls_back_to_mailbox_path() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    claimer
        .callback
        .set_disposition(MailboxDisposition::Deferred);
    let stale_ts = NOW - 6 * 60 * 1000;
    let frame = sender
        .send_clip(ClipContent::Text("stale".to_owned()), stale_ts)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);
    assert!(!mailbox);

    // When: a live frame outside the 5-minute freshness window arrives.
    let outcome = receiver
        .handle_clip(&room_id, &ciphertext_b64, mailbox, NOW)
        .expect("stale delivery should succeed");

    // Then: it is treated like a mailbox delivery, never as fresh live.
    assert_eq!(outcome, InboundOutcome::MailboxDeferred);
    assert_eq!(claimer.callback.live_count(), 0);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    assert_eq!(
        history_items(&receiver)[0].source,
        HistorySource::RemoteDeferred
    );
}

#[test]
fn session_future_skew_within_two_minutes_is_live_beyond_is_deferred() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    let within = NOW + 90 * 1000;
    let beyond = NOW + 3 * 60 * 1000;

    // When
    let fresh = deliver(
        &mut sender,
        &mut receiver,
        ClipContent::Text("clock ahead but tolerated".to_owned()),
        within,
    );
    let frame = sender
        .send_clip(ClipContent::Text("clock too far ahead".to_owned()), beyond)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);
    let skewed = receiver
        .handle_clip(&room_id, &ciphertext_b64, mailbox, NOW)
        .expect("skewed delivery should succeed");

    // Then
    assert_eq!(fresh, InboundOutcome::LiveApplied);
    assert!(matches!(
        skewed,
        InboundOutcome::MailboxApplied | InboundOutcome::MailboxDeferred
    ));
    assert_eq!(claimer.callback.live_count(), 1);
}

#[test]
fn session_tampered_ciphertext_is_dropped_without_state_change_or_callback() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    let frame = sender
        .send_clip(ClipContent::Text("authentic".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);
    let mut sealed = STANDARD.decode(&ciphertext_b64).expect("valid base64");
    let last = sealed.len() - 1;
    sealed[last] ^= 0x01;
    let tampered = STANDARD.encode(sealed);

    // When
    let outcome = receiver
        .handle_clip(&room_id, &tampered, mailbox, NOW)
        .expect("tampered delivery should not error the session");

    // Then
    assert_eq!(outcome, InboundOutcome::Unauthenticated);
    assert_eq!(claimer.callback.live_count(), 0);
    assert_eq!(receiver.last_seq(), 0);
    assert!(history_items(&receiver).is_empty());
}

#[test]
fn session_wrong_room_frame_is_dropped_as_unauthenticated() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    let frame = sender
        .send_clip(ClipContent::Text("wrong room".to_owned()), NOW)
        .expect("send should succeed");
    let (_, ciphertext_b64, mailbox) = clip_fields(frame);
    let foreign_room = "0".repeat(32);

    // When
    let outcome = receiver
        .handle_clip(&foreign_room, &ciphertext_b64, mailbox, NOW)
        .expect("wrong-room delivery should not error the session");

    // Then
    assert_eq!(outcome, InboundOutcome::Unauthenticated);
    assert_eq!(claimer.callback.live_count(), 0);
}

#[test]
fn session_crash_after_delivery_commit_replays_pending_into_history_without_callback() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let frame = sender
        .send_clip(ClipContent::Text("crash window one".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);

    // When: crash between DeliveryState commit and history add.
    {
        let mut receiver = open_session(&claimer);
        let result = receiver.handle_clip_with_fault(
            &room_id,
            &ciphertext_b64,
            mailbox,
            NOW,
            Some(ReceiveStage::DeliveryCommitted),
        );
        assert!(matches!(result, Err(SessionError::InjectedFault)));
    }

    // Then: restart replays the pending item into history only, never the callback.
    let restarted = open_session(&claimer);
    assert_eq!(claimer.callback.live_count(), 0);
    assert_eq!(claimer.callback.mailbox_count(), 0);
    let items = history_items(&restarted);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::Remote);
}

#[test]
fn session_crash_after_history_add_replays_idempotently_without_callback() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let frame = sender
        .send_clip(ClipContent::Text("crash window two".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);

    // When: crash between history add and pending clear.
    {
        let mut receiver = open_session(&claimer);
        let result = receiver.handle_clip_with_fault(
            &room_id,
            &ciphertext_b64,
            mailbox,
            NOW,
            Some(ReceiveStage::HistoryAdded),
        );
        assert!(matches!(result, Err(SessionError::InjectedFault)));
    }

    // Then: restart replays into history idempotently and never fires the callback.
    let restarted = open_session(&claimer);
    assert_eq!(claimer.callback.live_count(), 0);
    let items = history_items(&restarted);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::Remote);
}

#[test]
fn session_crash_after_pending_clear_leaves_history_without_callback() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let frame = sender
        .send_clip(ClipContent::Text("crash window three".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);

    // When: crash between pending clear and the clipboard callback.
    {
        let mut receiver = open_session(&claimer);
        let result = receiver.handle_clip_with_fault(
            &room_id,
            &ciphertext_b64,
            mailbox,
            NOW,
            Some(ReceiveStage::PendingCleared),
        );
        assert!(matches!(result, Err(SessionError::InjectedFault)));
    }

    // Then: the item is durable in history and the callback never fires on restart.
    let restarted = open_session(&claimer);
    assert_eq!(claimer.callback.live_count(), 0);
    let items = history_items(&restarted);
    assert_eq!(items.len(), 1);
}

#[test]
fn session_crash_after_mailbox_applied_before_set_source_recovers_manually() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    claimer
        .callback
        .set_disposition(MailboxDisposition::Applied);
    let frame = sender
        .send_clip(ClipContent::Text("applied then crash".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, _) = clip_fields(frame);

    // When: crash after the callback wrote the clipboard but before set_source.
    {
        let mut receiver = open_session(&claimer);
        let result = receiver.handle_clip_with_fault(
            &room_id,
            &ciphertext_b64,
            true,
            NOW,
            Some(ReceiveStage::MailboxApplied),
        );
        assert!(matches!(result, Err(SessionError::InjectedFault)));
    }

    // Then: restart never re-fires the callback; the entry may stay RemoteDeferred
    // and the user can apply it manually with idempotent recovery.
    let mut restarted = open_session(&claimer);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    let items = history_items(&restarted);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::RemoteDeferred);
    let id = items[0].id;
    restarted.apply_deferred(id).expect("manual apply");
    restarted
        .apply_deferred(id)
        .expect("manual apply is idempotent");
    assert_eq!(history_items(&restarted)[0].source, HistorySource::Remote);
    assert_eq!(claimer.callback.mailbox_count(), 1);
}

#[test]
fn session_crash_after_set_source_keeps_promoted_history_without_callback_retry() {
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    claimer
        .callback
        .set_disposition(MailboxDisposition::Applied);
    let frame = sender
        .send_clip(ClipContent::Text("promoted then crash".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, _) = clip_fields(frame);

    {
        let mut receiver = open_session(&claimer);
        let result = receiver.handle_clip_with_fault(
            &room_id,
            &ciphertext_b64,
            true,
            NOW,
            Some(ReceiveStage::SourcePromoted),
        );
        assert!(matches!(result, Err(SessionError::InjectedFault)));
    }

    let restarted = open_session(&claimer);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    assert_eq!(history_items(&restarted)[0].source, HistorySource::Remote);
}

#[test]
fn session_mailbox_callback_error_keeps_history_and_never_retries() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    claimer
        .callback
        .fail_mailbox_with(CallbackError::Rejected("shell busy".to_owned()));
    let frame = sender
        .send_clip(ClipContent::Text("callback fails".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, _) = clip_fields(frame);

    // When
    let result = {
        let mut receiver = open_session(&claimer);
        receiver.handle_clip(&room_id, &ciphertext_b64, true, NOW)
    };

    // Then: the error is reported, history is kept, and restart never retries.
    assert!(matches!(result, Err(SessionError::Callback(_))));
    let restarted = open_session(&claimer);
    assert_eq!(claimer.callback.mailbox_count(), 1);
    let items = history_items(&restarted);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source, HistorySource::RemoteDeferred);
}

#[test]
fn session_persistence_failure_at_delivery_commit_fails_closed() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let frame = sender
        .send_clip(ClipContent::Text("persistence down".to_owned()), NOW)
        .expect("send should succeed");
    let (room_id, ciphertext_b64, mailbox) = clip_fields(frame);
    let mut receiver = open_session(&claimer);
    fs::remove_dir_all(&claimer.session_dir).expect("session dir should be removable");

    // When: the DeliveryState transaction cannot persist.
    let result = receiver.handle_clip(&room_id, &ciphertext_b64, mailbox, NOW);

    // Then: the callback is suppressed and nothing else is mutated (fail closed).
    assert!(result.is_err());
    assert_eq!(claimer.callback.live_count(), 0);
    assert!(history_items(&receiver).is_empty());
    assert_eq!(receiver.last_seq(), 0);
}

#[test]
fn session_is_echo_recognizes_only_sent_and_applied_text_hashes() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);

    // When
    let sent_hash = text_hash("echo me");
    assert!(!sender.is_echo(&sent_hash));
    sender
        .send_clip(ClipContent::Text("echo me".to_owned()), NOW)
        .expect("send should succeed");

    // Then
    assert!(sender.is_echo(&sent_hash));
    assert!(!sender.is_echo(&text_hash("other text")));

    // And: applying a remote live clip registers the echo on the receiver.
    let outcome = deliver(
        &mut sender,
        &mut receiver,
        ClipContent::Text("remote echo".to_owned()),
        NOW,
    );
    assert_eq!(outcome, InboundOutcome::LiveApplied);
    assert!(receiver.is_echo(&text_hash("remote echo")));
}

#[test]
fn session_image_clip_roundtrips_through_live_path() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);
    let mut receiver = open_session(&claimer);
    let bytes: Vec<u8> = (0_u8..=255).collect();

    // When
    let outcome = deliver(
        &mut sender,
        &mut receiver,
        ClipContent::Image(bytes.clone()),
        NOW,
    );

    // Then
    assert_eq!(outcome, InboundOutcome::LiveApplied);
    let live = claimer.callback.live_items();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].content, ClipContent::Image(bytes));
}

#[test]
fn session_rejects_image_over_ten_mib_before_reserving_sequence() {
    let (offerer, _claimer) = paired_endpoints();
    let mut sender = open_session(&offerer);

    let result = sender.send_clip(ClipContent::Image(vec![0; 10 * 1024 * 1024 + 1]), NOW);

    assert!(matches!(
        result,
        Err(SessionError::Protocol(ProtocolError::Oversize { limit, .. }))
            if limit == 10 * 1024 * 1024
    ));
    assert_eq!(sender.next_seq(), 1);
    assert!(history_items(&sender).is_empty());
}

#[test]
fn session_rejects_record_of_another_identity() {
    // Given
    let (offerer, claimer) = paired_endpoints();
    let store = SessionStore::new(&claimer.session_dir).expect("store");
    let history = HistoryStore::new(&claimer.history_dir).expect("history");

    // When: a record belonging to the peer direction is presented with the local identity.
    let result = Session::new(
        &claimer.identity,
        &offerer.record,
        store,
        history,
        claimer.callback.clone(),
    );

    // Then
    assert!(matches!(result, Err(SessionError::RecordMismatch)));
}

#[test]
fn session_decode_frame_accepts_produced_wire_shape() {
    // Given
    let (offerer, _claimer) = paired_endpoints();
    let mut session = open_session(&offerer);
    let frame = session
        .send_clip(ClipContent::Text("wire".to_owned()), NOW)
        .expect("send should succeed");

    // When
    let encoded = encode_frame(&frame).expect("frame should encode");
    let decoded = decode_frame(&encoded).expect("frame should decode");

    // Then
    assert_eq!(decoded, frame);
}
