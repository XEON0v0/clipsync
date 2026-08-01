//! Room membership registry seam.
//!
//! T6 defines and fake-tests this boundary; T8 provides the atomic on-disk
//! implementation (including 24h unactivated reclaim and the 100-room quota).
//! Implementations must guarantee: a room is either absent or has its full
//! registered member set (two bundle fingerprints); half-written rooms are
//! never visible.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Room membership registry.
pub trait Registry: Send + Sync {
    /// Returns the registered member bundle fingerprints for `room_id`, or an empty
    /// vector when the room is not registered.
    fn lookup_members(&self, room_id: &str) -> Vec<String>;

    /// Records that `member_fp` completed its first join. Implementations must be
    /// idempotent; T8 uses this for 24h unactivated reclaim.
    fn activate_on_first_join(&self, room_id: &str, member_fp: &str);
}

#[derive(Default)]
struct RoomRecord {
    members: Vec<String>,
    activated: HashSet<String>,
}

/// In-memory registry: the T6 fake for tests and the placeholder wired into `main`
/// until T8 lands the disk implementation.
#[derive(Default)]
pub struct InMemoryRegistry {
    rooms: Mutex<HashMap<String, RoomRecord>>,
}

impl InMemoryRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a room with its full member set. Test/pairing-seam helper; T8's
    /// disk implementation owns production registration via the pairing flow.
    pub fn register_room(&self, room_id: &str, members: &[String]) -> bool {
        if members.len() != 2 || members[0] == members[1] {
            return false;
        }
        self.rooms.lock().expect("registry mutex poisoned").insert(
            room_id.to_owned(),
            RoomRecord {
                members: members.to_vec(),
                activated: HashSet::new(),
            },
        );
        true
    }

    /// Returns the members that activated so far. Test helper.
    #[must_use]
    pub fn activated(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .lock()
            .expect("registry mutex poisoned")
            .get(room_id)
            .map(|record| record.activated.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Registry for InMemoryRegistry {
    fn lookup_members(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .lock()
            .expect("registry mutex poisoned")
            .get(room_id)
            .map(|record| record.members.clone())
            .unwrap_or_default()
    }

    fn activate_on_first_join(&self, room_id: &str, member_fp: &str) {
        if let Some(record) = self
            .rooms
            .lock()
            .expect("registry mutex poisoned")
            .get_mut(room_id)
        {
            record.activated.insert(member_fp.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_room_has_no_members() {
        let registry = InMemoryRegistry::new();
        assert!(registry.lookup_members("ab".repeat(16).as_str()).is_empty());
    }

    #[test]
    fn registered_room_returns_full_member_set_and_activation_is_idempotent() {
        let registry = InMemoryRegistry::new();
        let room_id = "cd".repeat(16);
        let members = vec!["aa".repeat(32), "bb".repeat(32)];
        assert!(registry.register_room(&room_id, &members));
        assert_eq!(registry.lookup_members(&room_id), members);
        assert!(registry.activated(&room_id).is_empty());
        registry.activate_on_first_join(&room_id, &members[0]);
        registry.activate_on_first_join(&room_id, &members[0]);
        assert_eq!(registry.activated(&room_id), vec![members[0].clone()]);
    }

    #[test]
    fn register_room_rejects_non_binary_or_duplicate_membership() {
        let registry = InMemoryRegistry::new();
        let room_id = "ef".repeat(16);
        let member = "aa".repeat(32);
        assert!(!registry.register_room(&room_id, &[]));
        assert!(!registry.register_room(&room_id, std::slice::from_ref(&member)));
        assert!(!registry.register_room(&room_id, &[member.clone(), member]));
        assert!(registry.lookup_members(&room_id).is_empty());
    }
}
