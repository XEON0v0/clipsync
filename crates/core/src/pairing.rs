//! Pure pairing state and durable pairing metadata.
//!
//! The QR-only nonce never enters a protocol frame. Pairing becomes durable
//! only after the user confirms the T3 SAS value.
// allow: SIZE_OK - T5 requires the complete pairing state and crash-safe persistence contract here.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use rand::{RngCore, rngs::OsRng};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    PubBundle,
    crypto::{CryptoError, Identity, bundle_fp, room_id, sas_code},
};

pub const QR_NONCE_BYTES: usize = 16;
const IDENTITY_FILE: &str = "identity.json";
const PAIRING_FILE: &str = "pairing.json";

/// The complete out-of-band QR content shown by the offerer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QrPayload {
    pub server: String,
    pub code: String,
    pub bundle: PubBundle,
    #[serde(with = "nonce_base64")]
    pub nonce_a: [u8; QR_NONCE_BYTES],
}

impl QrPayload {
    /// Constructs validated QR content.
    ///
    /// # Errors
    /// Rejects invalid server URLs and malformed six-character pairing codes.
    pub fn new(
        server: &str,
        code: &str,
        bundle: PubBundle,
        nonce_a: [u8; QR_NONCE_BYTES],
    ) -> Result<Self, PairingError> {
        validate_server(server)?;
        validate_code(code)?;
        Ok(Self {
            server: server.to_owned(),
            code: code.to_owned(),
            bundle,
            nonce_a,
        })
    }

    /// Serializes the QR content as compact JSON.
    ///
    /// # Errors
    /// Returns a typed JSON error if serialization fails.
    pub fn to_json(&self) -> Result<String, PairingError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parses and validates untrusted QR JSON.
    ///
    /// # Errors
    /// Rejects malformed JSON, unknown fields, non-canonical nonce encoding,
    /// invalid codes, and unsafe server schemes.
    pub fn parse(encoded: &str) -> Result<Self, PairingError> {
        let payload: Self = serde_json::from_str(encoded)?;
        validate_server(&payload.server)?;
        validate_code(&payload.code)?;
        Ok(payload)
    }
}

/// Offerer state waiting for the relay-provided claimer bundle.
pub struct Offerer {
    identity_generation: u64,
    server: String,
    local_bundle: PubBundle,
    nonce_a: [u8; QR_NONCE_BYTES],
}

impl Offerer {
    /// Starts the offerer role from QR content bound to the local identity.
    ///
    /// # Errors
    /// Rejects QR content whose bundle is not the local identity bundle.
    pub fn start(identity: &Identity, qr: QrPayload) -> Result<Self, PairingError> {
        validate_server(&qr.server)?;
        validate_code(&qr.code)?;
        let local_bundle = identity.public_bundle();
        if qr.bundle != local_bundle {
            return Err(PairingError::BundleMismatch);
        }
        Ok(Self {
            identity_generation: identity.generation(),
            server: qr.server,
            local_bundle,
            nonce_a: qr.nonce_a,
        })
    }

    /// Accepts the distinct peer bundle returned by the pairing transport.
    ///
    /// # Errors
    /// Rejects the local bundle to prevent pairing an identity with itself.
    pub fn receive_peer(self, peer_bundle: PubBundle) -> Result<PendingConfirmation, PairingError> {
        if bundle_fp(&self.local_bundle) == bundle_fp(&peer_bundle) {
            return Err(PairingError::SameIdentity);
        }
        Ok(PendingConfirmation::new(
            PairingCandidate {
                identity_generation: self.identity_generation,
                server: self.server,
                local_bundle: self.local_bundle,
                peer_bundle,
            },
            self.nonce_a,
        ))
    }
}

/// Claimer state holding the immutable QR-pinned relay and offerer bundle.
pub struct Claimer {
    identity_generation: u64,
    pinned_server: String,
    local_bundle: PubBundle,
    pinned_bundle: PubBundle,
    nonce_a: [u8; QR_NONCE_BYTES],
}

impl Claimer {
    /// Starts claiming only after pinning the QR relay and offerer bundle.
    ///
    /// # Errors
    /// Immediately rejects a QR containing the local identity bundle.
    pub fn start(identity: &Identity, qr: QrPayload) -> Result<Self, PairingError> {
        validate_server(&qr.server)?;
        validate_code(&qr.code)?;
        let local_bundle = identity.public_bundle();
        if bundle_fp(&local_bundle) == bundle_fp(&qr.bundle) {
            return Err(PairingError::SameIdentity);
        }
        Ok(Self {
            identity_generation: identity.generation(),
            pinned_server: qr.server,
            local_bundle,
            pinned_bundle: qr.bundle,
            nonce_a: qr.nonce_a,
        })
    }

    /// Checks the transport result against both QR pins.
    ///
    /// # Errors
    /// Rejects any server or peer bundle that differs from the QR content.
    pub fn receive_peer(
        self,
        server: &str,
        peer_bundle: &PubBundle,
    ) -> Result<PendingConfirmation, PairingError> {
        if server != self.pinned_server {
            return Err(PairingError::ServerMismatch);
        }
        if peer_bundle != &self.pinned_bundle {
            return Err(PairingError::BundleMismatch);
        }
        Ok(PendingConfirmation::new(
            PairingCandidate {
                identity_generation: self.identity_generation,
                server: self.pinned_server,
                local_bundle: self.local_bundle,
                peer_bundle: self.pinned_bundle,
            },
            self.nonce_a,
        ))
    }
}

/// Pairing state that can advance only through exact SAS confirmation.
pub struct PendingConfirmation {
    candidate: PairingCandidate,
    nonce_a: [u8; QR_NONCE_BYTES],
}

impl PendingConfirmation {
    fn new(candidate: PairingCandidate, nonce_a: [u8; QR_NONCE_BYTES]) -> Self {
        Self { candidate, nonce_a }
    }

    #[must_use]
    pub fn server(&self) -> &str {
        &self.candidate.server
    }

    #[must_use]
    pub const fn peer_bundle(&self) -> &PubBundle {
        &self.candidate.peer_bundle
    }

    #[must_use]
    pub fn sas(&self) -> String {
        sas_code(
            &self.nonce_a,
            &self.candidate.local_bundle,
            &self.candidate.peer_bundle,
        )
    }

    /// Confirms the exact displayed SAS and atomically persists the pairing.
    ///
    /// # Errors
    /// A mismatch leaves the state uncompleted and writes no pairing record.
    pub fn confirm(self, sas: &str, store: &PairingStore) -> Result<PairingDone, PairingError> {
        if sas != self.sas() {
            return Err(PairingError::SasMismatch);
        }
        let record = PairingRecord::from_candidate(self.candidate);
        store.persist_pairing(&record)?;
        Ok(PairingDone { record })
    }
}

struct PairingCandidate {
    identity_generation: u64,
    server: String,
    local_bundle: PubBundle,
    peer_bundle: PubBundle,
}

/// Durable pairing metadata tied to exactly one identity generation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingRecord {
    pub identity_generation: u64,
    pub server: String,
    pub room_id: String,
    pub local_bundle_fp: String,
    pub peer_bundle: PubBundle,
    pub peer_bundle_fp: String,
}

impl PairingRecord {
    fn from_candidate(candidate: PairingCandidate) -> Self {
        Self {
            identity_generation: candidate.identity_generation,
            server: candidate.server,
            room_id: room_id(&candidate.local_bundle, &candidate.peer_bundle),
            local_bundle_fp: bundle_fp(&candidate.local_bundle),
            peer_bundle_fp: bundle_fp(&candidate.peer_bundle),
            peer_bundle: candidate.peer_bundle,
        }
    }
}

/// Completed pairing state produced only by successful confirmation and persistence.
pub struct PairingDone {
    record: PairingRecord,
}

impl PairingDone {
    #[must_use]
    pub const fn record(&self) -> &PairingRecord {
        &self.record
    }
}

/// Filesystem-backed identity and pairing-record owner.
pub struct PairingStore {
    root: PathBuf,
}

impl PairingStore {
    /// Opens or creates an isolated pairing state directory.
    ///
    /// # Errors
    /// Returns an I/O error when the directory cannot be created.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, PairingError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Loads the disk-current identity or creates generation 1.
    ///
    /// # Errors
    /// Returns typed identity persistence or schema errors.
    pub fn load_identity(&self) -> Result<Identity, PairingError> {
        Ok(Identity::load_or_create(&self.root.join(IDENTITY_FILE))?)
    }

    /// Loads only a record matching the disk-current identity generation.
    ///
    /// # Errors
    /// Returns typed JSON, identity, I/O, or record-integrity errors.
    pub fn load_pairing(&self) -> Result<Option<PairingRecord>, PairingError> {
        let identity = self.load_identity()?;
        let path = self.root.join(PAIRING_FILE);
        let encoded = match fs::read(&path) {
            Ok(encoded) => encoded,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(PairingError::Io(error)),
        };
        let record: PairingRecord = serde_json::from_slice(&encoded)?;
        if record.identity_generation != identity.generation() {
            return Ok(None);
        }
        validate_record(&record, &identity)?;
        Ok(Some(record))
    }

    fn persist_pairing(&self, record: &PairingRecord) -> Result<(), PairingError> {
        let identity = self.load_identity()?;
        if record.identity_generation != identity.generation()
            || record.local_bundle_fp != identity.bundle_fp()
        {
            return Err(PairingError::StaleIdentity);
        }
        validate_record(record, &identity)?;
        atomic_write(&self.root.join(PAIRING_FILE), &serde_json::to_vec(record)?)
    }
}

/// Rotates identity after the caller has stopped sessions and callbacks.
///
/// The new identity becomes committed only after its rename is made durable by
/// a parent-directory fsync and this API returns success. Old-generation state
/// is deleted afterward on a best-effort basis because it cannot be loaded by
/// the new generation.
///
/// # Errors
/// Returns before commit for identity, generation, serialization, or I/O
/// failures. Startup then selects only the canonical on-disk identity.
pub fn reset_pairing_state_after_quiesce(
    store: &PairingStore,
) -> Result<Identity, PairingError> {
    reset_with_observer(store, &mut NoopResetObserver)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ResetStage {
    TempWritten,
    FileSynced,
    IdentityRenamed,
    IdentityDirectorySynced,
}

trait ResetObserver {
    fn checkpoint(&mut self, stage: ResetStage) -> Result<(), PairingError>;
}

struct NoopResetObserver;

impl ResetObserver for NoopResetObserver {
    fn checkpoint(&mut self, _stage: ResetStage) -> Result<(), PairingError> {
        Ok(())
    }
}

fn reset_with_observer(
    store: &PairingStore,
    observer: &mut impl ResetObserver,
) -> Result<Identity, PairingError> {
    let current = store.load_identity()?;
    let generation = current
        .generation()
        .checked_add(1)
        .ok_or(PairingError::GenerationOverflow)?;
    let mut sign_secret = [0_u8; 32];
    let mut dh_secret = [0_u8; 32];
    OsRng.fill_bytes(&mut sign_secret);
    OsRng.fill_bytes(&mut dh_secret);
    let replacement = Identity::from_secret_bytes(sign_secret, dh_secret, generation)?;
    let document = IdentityDocument {
        generation,
        sign_sk: STANDARD.encode(sign_secret),
        dh_sk: STANDARD.encode(dh_secret),
    };

    let identity_path = store.root.join(IDENTITY_FILE);
    let temp_path = store
        .root
        .join(format!(".identity.json.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut pending = PendingFile::new(temp_path, options)?;
    pending.file.write_all(&serde_json::to_vec(&document)?)?;
    observer.checkpoint(ResetStage::TempWritten)?;
    pending.file.sync_all()?;
    observer.checkpoint(ResetStage::FileSynced)?;
    fs::rename(&pending.path, identity_path)?;
    pending.commit();
    observer.checkpoint(ResetStage::IdentityRenamed)?;
    File::open(&store.root)?.sync_all()?;
    observer.checkpoint(ResetStage::IdentityDirectorySynced)?;

    cleanup_old_pairing_state(&store.root);
    Ok(replacement)
}

#[derive(Serialize)]
struct IdentityDocument {
    generation: u64,
    sign_sk: String,
    dh_sk: String,
}

fn cleanup_old_pairing_state(root: &Path) {
    let pairing_path = root.join(PAIRING_FILE);
    match fs::remove_file(pairing_path) {
        Ok(()) => {
            let _ = File::open(root).and_then(|directory| directory.sync_all());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

#[derive(Debug, Error)]
pub enum PairingError {
    #[error("pairing server URL is invalid or uses a disallowed scheme")]
    InvalidServer,
    #[error("pairing code must be six uppercase ASCII letters or digits")]
    InvalidCode,
    #[error("an identity cannot pair with itself")]
    SameIdentity,
    #[error("pairing transport server does not match the QR-pinned server")]
    ServerMismatch,
    #[error("pairing transport bundle does not match the expected identity")]
    BundleMismatch,
    #[error("the confirmed SAS does not match the expected value")]
    SasMismatch,
    #[error("pairing state belongs to a stale identity generation")]
    StaleIdentity,
    #[error("pairing record failed integrity validation")]
    InvalidRecord,
    #[error("identity generation cannot advance beyond u64::MAX")]
    GenerationOverflow,
    #[error("pairing I/O failed")]
    Io(#[from] std::io::Error),
    #[error("pairing identity is invalid")]
    Crypto(#[from] CryptoError),
    #[error("pairing JSON is invalid")]
    Json(#[from] serde_json::Error),
}

fn validate_record(record: &PairingRecord, identity: &Identity) -> Result<(), PairingError> {
    let local_bundle = identity.public_bundle();
    let valid = record.identity_generation > 0
        && record.identity_generation == identity.generation()
        && record.local_bundle_fp == bundle_fp(&local_bundle)
        && record.peer_bundle_fp == bundle_fp(&record.peer_bundle)
        && record.local_bundle_fp != record.peer_bundle_fp
        && record.room_id == room_id(&local_bundle, &record.peer_bundle)
        && validate_server(&record.server).is_ok();
    if valid {
        Ok(())
    } else {
        Err(PairingError::InvalidRecord)
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), PairingError> {
    let parent = path.parent().ok_or(PairingError::InvalidRecord)?;
    let temp_path = parent.join(format!(".pairing.json.{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut pending = PendingFile::new(temp_path, options)?;
    pending.file.write_all(contents)?;
    pending.file.sync_all()?;
    fs::rename(&pending.path, path)?;
    pending.commit();
    File::open(parent)?.sync_all()?;
    Ok(())
}

struct PendingFile {
    path: PathBuf,
    file: File,
    committed: bool,
}

impl PendingFile {
    fn new(path: PathBuf, options: OpenOptions) -> Result<Self, PairingError> {
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file,
            committed: false,
        })
    }

    const fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn validate_server(server: &str) -> Result<(), PairingError> {
    let authority_and_path = if let Some(rest) = server.strip_prefix("wss://") {
        rest
    } else if cfg!(debug_assertions) {
        server.strip_prefix("ws://").ok_or(PairingError::InvalidServer)?
    } else {
        return Err(PairingError::InvalidServer);
    };
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .ok_or(PairingError::InvalidServer)?;
    let valid = !authority.is_empty()
        && !authority.contains('@')
        && !server.contains('#')
        && server.bytes().all(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_control());
    if valid {
        Ok(())
    } else {
        Err(PairingError::InvalidServer)
    }
}

fn validate_code(code: &str) -> Result<(), PairingError> {
    if code.len() == 6
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        Ok(())
    } else {
        Err(PairingError::InvalidCode)
    }
}

mod nonce_base64 {
    use super::*;

    pub fn serialize<S>(nonce: &[u8; QR_NONCE_BYTES], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(nonce))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; QR_NONCE_BYTES], D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let decoded = STANDARD.decode(&encoded).map_err(de::Error::custom)?;
        if STANDARD.encode(&decoded) != encoded {
            return Err(de::Error::custom(
                "nonce_a must use canonical padded STANDARD base64",
            ));
        }
        decoded.try_into().map_err(|bytes: Vec<u8>| {
            de::Error::custom(format!(
                "nonce_a must decode to {QR_NONCE_BYTES} bytes, got {}",
                bytes.len()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailAt(ResetStage);

    impl ResetObserver for FailAt {
        fn checkpoint(&mut self, stage: ResetStage) -> Result<(), PairingError> {
            if stage == self.0 {
                Err(PairingError::Io(std::io::Error::other(
                    "injected reset crash",
                )))
            } else {
                Ok(())
            }
        }
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "clipsync-test-{}",
                Uuid::new_v4()
            )))
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store_with_pairing() -> (TestRoot, PairingStore, PubBundle) {
        let root = TestRoot::new();
        let store = PairingStore::new(&root.0).expect("pairing store should open");
        let identity = store.load_identity().expect("identity should be created");
        let peer = Identity::from_secret_bytes(
            [61_u8; 32],
            [62_u8; 32],
            identity.generation(),
        )
        .expect("peer fixture generation should be valid");
        let record = PairingRecord::from_candidate(PairingCandidate {
            identity_generation: identity.generation(),
            server: "wss://relay.example/ws".to_owned(),
            local_bundle: identity.public_bundle(),
            peer_bundle: peer.public_bundle(),
        });
        store
            .persist_pairing(&record)
            .expect("pairing record should persist");
        (root, store, identity.public_bundle())
    }

    fn assert_old_generation_after_fault(stage: ResetStage) {
        let (_root, store, old_bundle) = store_with_pairing();

        let result = reset_with_observer(&store, &mut FailAt(stage));

        assert!(result.is_err());
        let identity = store.load_identity().expect("old identity should reload");
        assert_eq!(identity.generation(), 1);
        assert_eq!(identity.public_bundle(), old_bundle);
        assert!(
            store
                .load_pairing()
                .expect("old pairing should reload")
                .is_some()
        );
    }

    fn assert_new_generation_after_fault(stage: ResetStage) {
        let (_root, store, old_bundle) = store_with_pairing();

        let result = reset_with_observer(&store, &mut FailAt(stage));

        assert!(result.is_err());
        let identity = store.load_identity().expect("new identity should roll forward");
        assert_eq!(identity.generation(), 2);
        assert_ne!(identity.public_bundle(), old_bundle);
        assert!(
            store
                .load_pairing()
                .expect("old pairing should be ignored")
                .is_none()
        );
    }

    #[test]
    fn pairing_reset_fault_after_temp_write_rolls_back_complete_old_generation() {
        assert_old_generation_after_fault(ResetStage::TempWritten);
    }

    #[test]
    fn pairing_reset_fault_after_file_fsync_rolls_back_complete_old_generation() {
        assert_old_generation_after_fault(ResetStage::FileSynced);
    }

    #[test]
    fn pairing_reset_fault_after_rename_rolls_forward_complete_new_generation() {
        assert_new_generation_after_fault(ResetStage::IdentityRenamed);
    }

    #[test]
    fn pairing_reset_fault_after_dir_fsync_rolls_forward_complete_new_generation() {
        assert_new_generation_after_fault(ResetStage::IdentityDirectorySynced);
    }
}
