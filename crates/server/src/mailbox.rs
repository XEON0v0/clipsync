//! Mailbox persistence seam.
//!
//! The room actor keeps the authoritative mailbox **in memory** (`Join` reads only
//! memory). Persistence is owned by T9's single publication worker, which serializes
//! writes and merges pending-latest behind this seam; the actor never blocks on fsync.
//! The storage quota (100 rooms x 24 MiB) is enforced by that worker, not here.

/// One pending mailbox clip destined for an offline member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingClip {
    /// Authenticated sender bundle fingerprint (overwritten by the server).
    pub origin_device: String,
    /// Base64 ciphertext payload, opaque to the relay.
    pub ciphertext_b64: String,
}

/// Notification sink the room actor pushes mailbox changes into. All methods must be
/// non-blocking; T9 implements this with a channel into its publication worker.
pub trait MailboxSink: Send + Sync {
    /// A clip was stored in the in-memory mailbox for an offline member (latest wins).
    fn clip_pending(&self, room_id: &str, recipient_fp: &str, clip: PendingClip);

    /// A pending clip was delivered as the join bootstrap frame and removed.
    fn clip_consumed(&self, room_id: &str, recipient_fp: &str);

    /// Pending clips restored into a room actor's memory when the actor is created.
    /// The T6 default is empty; T9 replays its persisted mailbox here.
    fn load_pending(&self, room_id: &str) -> Vec<(String, PendingClip)> {
        let _ = room_id;
        Vec::new()
    }
}

/// Default sink for T6: persistence arrives with T9.
pub struct NoopMailboxSink;

impl MailboxSink for NoopMailboxSink {
    fn clip_pending(&self, room_id: &str, recipient_fp: &str, clip: PendingClip) {
        let _ = (room_id, recipient_fp, clip);
    }

    fn clip_consumed(&self, room_id: &str, recipient_fp: &str) {
        let _ = (room_id, recipient_fp);
    }
}
