//! Atomic room membership registry.
//!
//! Pairing codes are intentionally memory-only. Completed room membership is stored
//! as one JSON snapshot written with temp-file + fsync + rename + directory-fsync,
//! so readers observe either the previous complete registry or the next one.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::REGISTRY_MAX_ROOMS;

const REGISTRY_FILE: &str = "rooms.json";
const REGISTRY_TEMP_FILE: &str = ".rooms.json.tmp";

#[must_use]
pub fn unix_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

/// Runs periodic unactivated-room reclaim and returns its worker handle.
pub fn spawn_unactivated_sweeper(
    registry: Arc<dyn Registry>,
    period: Duration,
    ttl: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let mut interval = tokio::time::interval(period);
        interval.tick().await;
        loop {
            interval.tick().await;
            let cutoff = unix_time_ms().saturating_sub(ttl_ms);
            if let Err(error) = registry.prune_unactivated(cutoff) {
                eprintln!("unactivated room sweep failed: {error}");
            }
        }
    })
}

/// Result of atomically creating a room.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryCommit {
    Created,
    Existing,
}

/// Room membership registry used by pairing and join routing.
pub trait Registry: Send + Sync {
    fn lookup_members(&self, room_id: &str) -> Vec<String>;

    /// Creates a complete two-member room, or accepts an identical existing room.
    fn commit_room(
        &self,
        room_id: &str,
        members: &[String; 2],
        created_at_ms: i64,
    ) -> io::Result<RegistryCommit>;

    /// Persists the first successful legal join. Returns true only on transition.
    fn activate_on_first_join(
        &self,
        room_id: &str,
        member_fp: &str,
        activated_at_ms: i64,
    ) -> io::Result<bool>;

    /// Deletes never-activated rooms created before `cutoff_ms`.
    fn prune_unactivated(&self, cutoff_ms: i64) -> io::Result<usize>;
}

#[derive(Clone)]
struct RoomRecord {
    members: [String; 2],
    created_at_ms: i64,
    activated_at_ms: Option<i64>,
    activated_members: HashSet<String>,
}

impl RoomRecord {
    fn new(members: &[String; 2], created_at_ms: i64) -> io::Result<Self> {
        let members = validate_members(members)?;
        Ok(Self {
            members,
            created_at_ms,
            activated_at_ms: None,
            activated_members: HashSet::new(),
        })
    }
}

/// In-memory registry used by focused room tests.
#[derive(Default)]
pub struct InMemoryRegistry {
    rooms: Mutex<HashMap<String, RoomRecord>>,
}

impl InMemoryRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Convenience helper retained for room tests.
    pub fn register_room(&self, room_id: &str, members: &[String]) -> bool {
        let Ok(members) = <[String; 2]>::try_from(members.to_vec()) else {
            return false;
        };
        self.commit_room(room_id, &members, 0).is_ok()
    }

    #[must_use]
    pub fn activated(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .lock()
            .expect("registry mutex poisoned")
            .get(room_id)
            .map(|record| record.activated_members.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Convenience helper retained for room tests.
    pub fn activate_on_first_join(&self, room_id: &str, member_fp: &str) {
        let _ = Registry::activate_on_first_join(self, room_id, member_fp, 0);
    }
}

impl Registry for InMemoryRegistry {
    fn lookup_members(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .lock()
            .expect("registry mutex poisoned")
            .get(room_id)
            .map(|record| record.members.to_vec())
            .unwrap_or_default()
    }

    fn commit_room(
        &self,
        room_id: &str,
        members: &[String; 2],
        created_at_ms: i64,
    ) -> io::Result<RegistryCommit> {
        validate_room_id(room_id)?;
        let record = RoomRecord::new(members, created_at_ms)?;
        let mut rooms = self.rooms.lock().expect("registry mutex poisoned");
        if let Some(existing) = rooms.get(room_id) {
            return if existing.members == record.members {
                Ok(RegistryCommit::Existing)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "room id is registered to different members",
                ))
            };
        }
        if rooms.len() >= REGISTRY_MAX_ROOMS {
            return Err(io::Error::other("room registry quota exceeded"));
        }
        rooms.insert(room_id.to_owned(), record);
        Ok(RegistryCommit::Created)
    }

    fn activate_on_first_join(
        &self,
        room_id: &str,
        member_fp: &str,
        activated_at_ms: i64,
    ) -> io::Result<bool> {
        let mut rooms = self.rooms.lock().expect("registry mutex poisoned");
        let Some(record) = rooms.get_mut(room_id) else {
            return Ok(false);
        };
        if !record.members.contains(&member_fp.to_owned()) {
            return Ok(false);
        }
        record.activated_members.insert(member_fp.to_owned());
        if record.activated_at_ms.is_some() {
            return Ok(false);
        }
        record.activated_at_ms = Some(activated_at_ms);
        Ok(true)
    }

    fn prune_unactivated(&self, cutoff_ms: i64) -> io::Result<usize> {
        let mut rooms = self.rooms.lock().expect("registry mutex poisoned");
        let before = rooms.len();
        rooms.retain(|_, record| {
            record.activated_at_ms.is_some() || record.created_at_ms >= cutoff_ms
        });
        Ok(before - rooms.len())
    }
}

/// On-disk registry used by the production relay.
pub struct PersistentRegistry {
    root: PathBuf,
    rooms: Mutex<HashMap<String, RoomRecord>>,
}

impl PersistentRegistry {
    /// Opens the registry, skipping and warning about structurally invalid room rows.
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let path = root.join(REGISTRY_FILE);
        let mut rooms = HashMap::new();
        if path.exists() {
            let disk: DiskRegistry = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            for row in disk.rooms {
                match row.into_record() {
                    Ok((room_id, record)) if !rooms.contains_key(&room_id) => {
                        rooms.insert(room_id, record);
                    }
                    Ok((room_id, _)) => {
                        eprintln!("skipping duplicate registry room {room_id}");
                    }
                    Err(error) => eprintln!("skipping invalid registry room: {error}"),
                }
            }
        }
        Ok(Self {
            root,
            rooms: Mutex::new(rooms),
        })
    }

    #[must_use]
    pub fn activated_at_ms(&self, room_id: &str) -> Option<i64> {
        self.rooms
            .lock()
            .expect("registry mutex poisoned")
            .get(room_id)
            .and_then(|record| record.activated_at_ms)
    }

    fn persist(&self, rooms: &HashMap<String, RoomRecord>) -> io::Result<()> {
        let disk = DiskRegistry::from_rooms(rooms);
        let encoded = serde_json::to_vec_pretty(&disk).map_err(io::Error::other)?;
        let temp = self.root.join(REGISTRY_TEMP_FILE);
        let path = self.root.join(REGISTRY_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

impl Registry for PersistentRegistry {
    fn lookup_members(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .lock()
            .expect("registry mutex poisoned")
            .get(room_id)
            .map(|record| record.members.to_vec())
            .unwrap_or_default()
    }

    fn commit_room(
        &self,
        room_id: &str,
        members: &[String; 2],
        created_at_ms: i64,
    ) -> io::Result<RegistryCommit> {
        validate_room_id(room_id)?;
        let record = RoomRecord::new(members, created_at_ms)?;
        let mut rooms = self.rooms.lock().expect("registry mutex poisoned");
        if let Some(existing) = rooms.get(room_id) {
            return if existing.members == record.members {
                Ok(RegistryCommit::Existing)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "room id is registered to different members",
                ))
            };
        }
        if rooms.len() >= REGISTRY_MAX_ROOMS {
            return Err(io::Error::other("room registry quota exceeded"));
        }
        let mut next = rooms.clone();
        next.insert(room_id.to_owned(), record);
        self.persist(&next)?;
        *rooms = next;
        Ok(RegistryCommit::Created)
    }

    fn activate_on_first_join(
        &self,
        room_id: &str,
        member_fp: &str,
        activated_at_ms: i64,
    ) -> io::Result<bool> {
        let mut rooms = self.rooms.lock().expect("registry mutex poisoned");
        let Some(current) = rooms.get(room_id) else {
            return Ok(false);
        };
        if current.activated_at_ms.is_some() || !current.members.contains(&member_fp.to_owned()) {
            return Ok(false);
        }
        let mut next = rooms.clone();
        let record = next.get_mut(room_id).expect("room cloned above");
        record.activated_at_ms = Some(activated_at_ms);
        record.activated_members.insert(member_fp.to_owned());
        self.persist(&next)?;
        *rooms = next;
        Ok(true)
    }

    fn prune_unactivated(&self, cutoff_ms: i64) -> io::Result<usize> {
        let mut rooms = self.rooms.lock().expect("registry mutex poisoned");
        let mut next = rooms.clone();
        next.retain(|_, record| {
            record.activated_at_ms.is_some() || record.created_at_ms >= cutoff_ms
        });
        let removed = rooms.len() - next.len();
        if removed != 0 {
            self.persist(&next)?;
            *rooms = next;
        }
        Ok(removed)
    }
}

#[derive(Deserialize, Serialize)]
struct DiskRegistry {
    rooms: Vec<DiskRoom>,
}

impl DiskRegistry {
    fn from_rooms(rooms: &HashMap<String, RoomRecord>) -> Self {
        let mut rows: Vec<_> = rooms
            .iter()
            .map(|(room_id, record)| DiskRoom {
                room_id: room_id.clone(),
                members: record.members.to_vec(),
                created_at_ms: record.created_at_ms,
                activated_at_ms: record.activated_at_ms,
            })
            .collect();
        rows.sort_by(|a, b| a.room_id.cmp(&b.room_id));
        Self { rooms: rows }
    }
}

#[derive(Deserialize, Serialize)]
struct DiskRoom {
    room_id: String,
    members: Vec<String>,
    created_at_ms: i64,
    activated_at_ms: Option<i64>,
}

impl DiskRoom {
    fn into_record(self) -> io::Result<(String, RoomRecord)> {
        validate_room_id(&self.room_id)?;
        let members: [String; 2] = self.members.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "room must have exactly two members",
            )
        })?;
        let mut record = RoomRecord::new(&members, self.created_at_ms)?;
        record.activated_at_ms = self.activated_at_ms;
        Ok((self.room_id, record))
    }
}

fn validate_room_id(room_id: &str) -> io::Result<()> {
    if room_id.len() == 32
        && room_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "room id must be 32 lowercase hex characters",
        ))
    }
}

fn validate_members(members: &[String; 2]) -> io::Result<[String; 2]> {
    if members[0] == members[1] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "room members must be distinct",
        ));
    }
    if members.iter().any(|member| {
        member.len() != 64
            || !member
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "room members must be 64 lowercase hex fingerprints",
        ));
    }
    let mut canonical = members.clone();
    canonical.sort();
    Ok(canonical)
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
