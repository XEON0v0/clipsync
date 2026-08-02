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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn room_id() -> String {
        "aa".repeat(16)
    }

    fn recipient_fp() -> String {
        "bb".repeat(32)
    }

    fn origin_fp() -> String {
        "cc".repeat(32)
    }

    fn clip(payload: &str) -> PendingClip {
        PendingClip {
            origin_device: origin_fp(),
            ciphertext_b64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                payload,
            ),
        }
    }

    fn fast_options() -> MailboxOptions {
        MailboxOptions {
            retry_interval: Duration::from_millis(20),
            ..MailboxOptions::default()
        }
    }

    async fn eventually(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("condition did not become true in time");
    }

    fn mailbox_file(dir: &Path, room_id: &str) -> std::path::PathBuf {
        dir.join(format!("{room_id}.json"))
    }

    /// Writes a mailbox snapshot file directly, bypassing the publication worker.
    fn write_disk_file(dir: &Path, room_id: &str, recipient_fp: &str, payload: &str, ts_ms: i64) {
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, payload);
        let json = format!(
            "{{\"version\":1,\"room_id\":\"{room_id}\",\"recipient_fp\":\"{recipient_fp}\",\"origin_fp\":\"{}\",\"ciphertext_b64\":\"{encoded}\",\"ts_ms\":{ts_ms}}}",
            origin_fp()
        );
        std::fs::write(mailbox_file(dir, room_id), json).unwrap();
    }

    #[tokio::test]
    async fn mailbox_publishes_and_reloads_after_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("hello"));
        mailbox.wait_writes(1).await;
        assert!(mailbox_file(dir.path(), &room_id()).exists());
        assert_eq!(mailbox.completed_writes(), 1);

        let pending = mailbox.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, recipient_fp());
        assert_eq!(pending[0].1, clip("hello"));
        drop(mailbox);

        // A fresh instance rescans the directory and replays the same clip.
        let reopened = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        let pending = reopened.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("hello"));
    }

    #[tokio::test]
    async fn mailbox_single_publication_coalesces_pending_latest() {
        let dir = tempfile::TempDir::new().unwrap();
        let gate = Arc::new(Semaphore::new(0));
        let mailbox = PersistentMailbox::open(
            dir.path(),
            MailboxOptions {
                write_gate: Some(gate.clone()),
                ..fast_options()
            },
        )
        .unwrap();

        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("N"));
        // Wait until the worker picked up N and is blocked inside the gated write.
        eventually(|| mailbox.write_in_flight()).await;

        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("N+1"));
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("N+2"));
        // Nothing may publish while N is in flight.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mailbox.completed_writes(), 0);

        // N completes first.
        gate.add_permits(1);
        mailbox.wait_writes(1).await;
        let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room_id())).unwrap();
        assert!(on_disk.contains(&clip("N").ciphertext_b64));

        // N+1 is never written on its own: the worker coalesced it into N+2 and is
        // gated again before the next publication.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mailbox.completed_writes(), 1);

        gate.add_permits(1);
        mailbox.wait_writes(2).await;
        let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room_id())).unwrap();
        assert!(on_disk.contains(&clip("N+2").ciphertext_b64));
        assert!(!on_disk.contains(&clip("N+1").ciphertext_b64));
        // Exactly two physical writes: N, then the coalesced N+2.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mailbox.completed_writes(), 2);
    }

    #[tokio::test]
    async fn mailbox_crash_during_write_preserves_old_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("old"));
        mailbox.wait_writes(1).await;
        let before = std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap();
        drop(mailbox);

        // Simulate a crash mid-write: a truncated temp file is left behind. The
        // rename never happened, so the old complete file must be intact and the
        // temp garbage must be ignored by the startup scan.
        std::fs::write(
            dir.path().join(format!(".{}.json.tmp", room_id())),
            b"{\"version\":1,\"room_id\":\"aa",
        )
        .unwrap();

        let reopened = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        let pending = reopened.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("old"));
        assert_eq!(std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap(), before);
    }

    #[tokio::test]
    async fn mailbox_rejects_invalid_room_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending("../evil", &recipient_fp(), clip("nope"));
        mailbox.clip_pending(&"AA".repeat(16), &recipient_fp(), clip("nope"));
        mailbox.clip_pending(&"aa".repeat(8), &recipient_fp(), clip("nope"));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(mailbox.completed_writes(), 0);
        assert!(mailbox.health_probe().is_healthy());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn mailbox_corrupt_file_is_skipped_on_startup() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(mailbox_file(dir.path(), &room_id()), b"{not json").unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        assert!(mailbox.load_pending(&room_id()).is_empty());
        assert!(mailbox.health_probe().is_healthy());
    }

    #[tokio::test]
    async fn mailbox_ttl_sweep_removes_expired_clips() {
        let dir = tempfile::TempDir::new().unwrap();
        let now = crate::registry::unix_time_ms();
        let seven_days_ms = 7 * 24 * 60 * 60 * 1000;
        // Fresh file: survives the sweep.
        write_disk_file(dir.path(), &room_id(), &recipient_fp(), "fresh", now);
        // Expired file: removed at startup scan and by the sweep.
        let old_room = "dd".repeat(16);
        write_disk_file(dir.path(), &old_room, &recipient_fp(), "stale", now - seven_days_ms - 1);

        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        assert!(!mailbox_file(dir.path(), &old_room).exists());
        assert_eq!(mailbox.load_pending(&room_id()).len(), 1);
        assert!(mailbox.load_pending(&old_room).is_empty());

        // The periodic sweep removes files that age out while running.
        let removed = mailbox.sweep_expired(now + seven_days_ms + 1);
        assert_eq!(removed, 1);
        assert!(!mailbox_file(dir.path(), &room_id()).exists());
        assert!(mailbox.load_pending(&room_id()).is_empty());
    }

    #[tokio::test]
    async fn mailbox_quota_exceeded_marks_degraded_and_keeps_pending() {
        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(
            dir.path(),
            MailboxOptions {
                max_bytes: 16,
                ..fast_options()
            },
        )
        .unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("too-big-for-quota"));
        eventually(|| !mailbox.health_probe().is_healthy()).await;
        assert_eq!(mailbox.completed_writes(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // The unpublished latest is retained for a later retry.
        let pending = mailbox.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("too-big-for-quota"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mailbox_readonly_dir_keeps_memory_serving_and_recovers() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let mailbox = PersistentMailbox::open(dir.path(), fast_options()).unwrap();
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("old"));
        mailbox.wait_writes(1).await;
        let old_bytes = std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap();

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let restore = scopeguard_restore(dir.path().to_path_buf());
        mailbox.clip_pending(&room_id(), &recipient_fp(), clip("new"));
        eventually(|| !mailbox.health_probe().is_healthy()).await;

        // Old file intact; memory still serves the pending-latest clip.
        assert_eq!(
            std::fs::read(mailbox_file(dir.path(), &room_id())).unwrap(),
            old_bytes
        );
        let pending = mailbox.load_pending(&room_id());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, clip("new"));

        drop(restore);
        mailbox.wait_writes(2).await;
        eventually(|| mailbox.health_probe().is_healthy()).await;
        let on_disk = std::fs::read_to_string(mailbox_file(dir.path(), &room_id())).unwrap();
        assert!(on_disk.contains(&clip("new").ciphertext_b64));
    }

    /// Restores write permission on drop (test failure safety).
    #[cfg(unix)]
    fn scopeguard_restore(path: std::path::PathBuf) -> impl Drop {
        struct Guard(std::path::PathBuf);
        impl Drop for Guard {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        Guard(path)
    }
}
