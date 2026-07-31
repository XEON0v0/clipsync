//! Cryptographic primitives shared by ClipSync endpoints.
//!
//! Clipboard contents are end-to-end encrypted and authenticated against a
//! malicious relay with AEAD and canonical AAD. Static X25519 identities do
//! not provide forward secrecy or KCI resistance: compromise of a DH private
//! key exposes recorded traffic and enables forgery toward that endpoint.
//! Recovery requires `reset_pairing` on both endpoints to create new
//! identities and a new room. The SAS nonce exists only in QR payloads and is
//! never sent over the relay.
// allow: SIZE_OK - T3 locks crypto and atomic identity persistence to this module.

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(feature = "full")]
use base64::{Engine as _, engine::general_purpose::STANDARD};
#[cfg(feature = "full")]
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
#[cfg(feature = "full")]
use ed25519_dalek::{Signer, SigningKey};
#[cfg(feature = "full")]
use hkdf::Hkdf;
#[cfg(feature = "full")]
use rand::{RngCore, rngs::OsRng};
#[cfg(feature = "full")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "full")]
use std::fs::{self, File, OpenOptions};
#[cfg(feature = "full")]
use std::io::Write;
#[cfg(all(feature = "full", unix))]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(feature = "full")]
use std::path::{Path, PathBuf};
#[cfg(feature = "full")]
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

pub use crate::protocol::{PubBundle, bundle_bytes};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const JOIN_DOMAIN: &[u8] = b"clipboard-sync-join-v1";
#[cfg(feature = "full")]
const HKDF_DOMAIN: &[u8] = b"clipboard-sync-v1";
#[cfg(feature = "full")]
const SAS_DOMAIN: &[u8] = b"clipboard-sync-sas-v1";
#[cfg(feature = "full")]
const NONCE_LENGTH: usize = 24;

/// Errors returned by the cryptographic boundary.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("join signature field exceeds the u32 wire limit")]
    JoinFieldTooLong,
    #[error("invalid Ed25519 public key")]
    InvalidVerifyingKey(#[source] ed25519_dalek::SignatureError),
    #[error("Ed25519 signature verification failed")]
    InvalidSignature(#[source] ed25519_dalek::SignatureError),
    #[cfg(feature = "full")]
    #[error("an identity cannot derive a session with itself")]
    SameIdentity,
    #[cfg(feature = "full")]
    #[error("X25519 peer key is non-contributory")]
    NonContributoryDh,
    #[cfg(feature = "full")]
    #[error("HKDF output length is invalid")]
    KdfExpand,
    #[cfg(feature = "full")]
    #[error("XChaCha20-Poly1305 encryption failed")]
    Encryption,
    #[cfg(feature = "full")]
    #[error("XChaCha20-Poly1305 decryption failed")]
    Decryption,
    #[cfg(feature = "full")]
    #[error("sealed value is shorter than its 24-byte nonce")]
    SealedTooShort,
    #[cfg(feature = "full")]
    #[error("identity generation must be at least 1, got {0}")]
    InvalidGeneration(u64),
    #[cfg(feature = "full")]
    #[error("identity secret must decode to exactly 32 bytes")]
    InvalidSecretLength,
    #[cfg(feature = "full")]
    #[error("identity path has no parent directory")]
    MissingParent,
    #[cfg(feature = "full")]
    #[error("identity I/O failed")]
    Io(#[from] std::io::Error),
    #[cfg(feature = "full")]
    #[error("identity JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[cfg(feature = "full")]
    #[error("identity secret encoding is invalid")]
    Base64(#[from] base64::DecodeError),
}

/// Returns the canonical SHA-256 fingerprint of a public identity bundle.
#[must_use]
pub fn bundle_fp(bundle: &PubBundle) -> String {
    hex_lower(&Sha256::digest(bundle_bytes(bundle)))
}

/// Returns the short device identifier derived only from the signing key.
#[must_use]
pub fn device_id(sign_pk: &[u8; 32]) -> String {
    let digest = Sha256::digest(sign_pk);
    hex_lower(&digest[..8])
}

/// Returns the order-independent room identifier for two public bundles.
#[must_use]
pub fn room_id(first: &PubBundle, second: &PubBundle) -> String {
    let first_bytes = bundle_bytes(first);
    let second_bytes = bundle_bytes(second);
    let mut hasher = Sha256::new();
    if first_bytes <= second_bytes {
        hasher.update(first_bytes);
        hasher.update(second_bytes);
    } else {
        hasher.update(second_bytes);
        hasher.update(first_bytes);
    }
    hex_lower(&hasher.finalize()[..16])
}

/// Builds the domain-separated, u32-big-endian-prefixed join signature input.
///
/// # Errors
/// Returns [`CryptoError::JoinFieldTooLong`] if any field exceeds `u32::MAX`.
pub fn join_sig_msg(
    nonce: &[u8],
    room_id: &str,
    device_id: &str,
    bundle: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut message = Vec::new();
    message.extend_from_slice(JOIN_DOMAIN);
    for field in [nonce, room_id.as_bytes(), device_id.as_bytes(), bundle] {
        let length = u32::try_from(field.len()).map_err(|_| CryptoError::JoinFieldTooLong)?;
        message.extend_from_slice(&length.to_be_bytes());
        message.extend_from_slice(field);
    }
    Ok(message)
}

/// Verifies an Ed25519 signature using strict verification rules.
///
/// # Errors
/// Returns a typed key or signature error when verification fails.
pub fn verify(sign_pk: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> Result<(), CryptoError> {
    let verifying_key =
        VerifyingKey::from_bytes(sign_pk).map_err(CryptoError::InvalidVerifyingKey)?;
    verifying_key
        .verify_strict(message, &Signature::from_bytes(signature))
        .map_err(CryptoError::InvalidSignature)
}

/// Ed25519 signing-key wrapper that deliberately has no `Debug` implementation.
#[cfg(feature = "full")]
pub struct Ed25519Keypair(SigningKey);

#[cfg(feature = "full")]
impl Ed25519Keypair {
    fn generate() -> Self {
        let mut secret = [0_u8; 32];
        OsRng.fill_bytes(&mut secret);
        Self(SigningKey::from_bytes(&secret))
    }

    fn from_bytes(secret: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&secret))
    }

    fn public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.0.sign(message).to_bytes()
    }
}

/// Static X25519 key wrapper that deliberately has no `Debug` implementation.
#[cfg(feature = "full")]
pub struct X25519Keypair(StaticSecret);

#[cfg(feature = "full")]
impl X25519Keypair {
    fn generate() -> Self {
        Self(StaticSecret::random_from_rng(OsRng))
    }

    fn from_bytes(secret: [u8; 32]) -> Self {
        Self(StaticSecret::from(secret))
    }

    fn public_key(&self) -> [u8; 32] {
        X25519PublicKey::from(&self.0).to_bytes()
    }
}

/// One directional AEAD key with no secret-revealing formatting.
#[cfg(feature = "full")]
pub struct SessionKey([u8; 32]);

#[cfg(feature = "full")]
impl SessionKey {
    /// Constructs a directional key from HKDF or a stored 32-byte value.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Directional keys from the local endpoint's perspective.
#[cfg(feature = "full")]
pub struct SessionKeys {
    pub send: SessionKey,
    pub recv: SessionKey,
}

/// Independently generated signing and static-DH identity.
#[cfg(feature = "full")]
pub struct Identity {
    sign: Ed25519Keypair,
    dh: X25519Keypair,
    generation: u64,
}

#[cfg(feature = "full")]
impl Identity {
    /// Generates independent Ed25519 and X25519 private keys at generation 1.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            sign: Ed25519Keypair::generate(),
            dh: X25519Keypair::generate(),
            generation: 1,
        }
    }

    /// Constructs an identity from independent serialized private keys.
    ///
    /// # Errors
    /// Returns [`CryptoError::InvalidGeneration`] when `generation` is zero.
    pub fn from_secret_bytes(
        sign_secret: [u8; 32],
        dh_secret: [u8; 32],
        generation: u64,
    ) -> Result<Self, CryptoError> {
        if generation == 0 {
            return Err(CryptoError::InvalidGeneration(generation));
        }
        Ok(Self {
            sign: Ed25519Keypair::from_bytes(sign_secret),
            dh: X25519Keypair::from_bytes(dh_secret),
            generation,
        })
    }

    /// Returns this identity's public signing and DH keys.
    #[must_use]
    pub fn public_bundle(&self) -> PubBundle {
        PubBundle {
            sign_pk: self.sign.public_key(),
            dh_pk: self.dh.public_key(),
        }
    }

    /// Returns the persisted generation counter.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Signs a message with the identity's Ed25519 key.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.sign.sign(message)
    }

    /// Returns this identity's short device identifier.
    #[must_use]
    pub fn device_id(&self) -> String {
        device_id(&self.sign.public_key())
    }

    /// Returns this identity's full public-bundle fingerprint.
    #[must_use]
    pub fn bundle_fp(&self) -> String {
        bundle_fp(&self.public_bundle())
    }

    /// Derives local send and receive keys for a distinct contributory peer.
    ///
    /// # Errors
    /// Rejects the same identity, non-contributory DH, or HKDF failure.
    pub fn session_keys(&self, peer: &PubBundle) -> Result<SessionKeys, CryptoError> {
        let self_fp = self.bundle_fp();
        let peer_fp = bundle_fp(peer);
        if self_fp == peer_fp {
            return Err(CryptoError::SameIdentity);
        }

        let shared = self.dh.0.diffie_hellman(&X25519PublicKey::from(peer.dh_pk));
        if !shared.was_contributory() {
            return Err(CryptoError::NonContributoryDh);
        }

        let (fp_min, fp_max, self_is_a) = if self_fp < peer_fp {
            (self_fp.as_str(), peer_fp.as_str(), true)
        } else {
            (peer_fp.as_str(), self_fp.as_str(), false)
        };
        let mut info = Vec::with_capacity(HKDF_DOMAIN.len() + fp_min.len() + fp_max.len() + 3);
        info.extend_from_slice(HKDF_DOMAIN);
        info.extend_from_slice(fp_min.as_bytes());
        info.extend_from_slice(fp_max.as_bytes());
        let a2b = derive_session_key(shared.as_bytes(), &info, b"a2b")?;
        let b2a = derive_session_key(shared.as_bytes(), &info, b"b2a")?;
        Ok(if self_is_a {
            SessionKeys {
                send: a2b,
                recv: b2a,
            }
        } else {
            SessionKeys {
                send: b2a,
                recv: a2b,
            }
        })
    }

    /// Loads an identity or atomically creates a generation-1 identity.
    ///
    /// # Errors
    /// Returns typed persistence or schema errors for unreadable identities.
    pub fn load_or_create(path: &Path) -> Result<Self, CryptoError> {
        match fs::read(path) {
            Ok(bytes) => Self::from_document(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate();
                identity.persist(path)?;
                Ok(identity)
            }
            Err(error) => Err(CryptoError::Io(error)),
        }
    }

    fn from_document(document: IdentityDocument) -> Result<Self, CryptoError> {
        Self::from_secret_bytes(
            decode_secret(&document.sign_sk)?,
            decode_secret(&document.dh_sk)?,
            document.generation,
        )
    }

    fn persist(&self, path: &Path) -> Result<(), CryptoError> {
        let parent = path.parent().ok_or(CryptoError::MissingParent)?;
        let document = IdentityDocument {
            generation: self.generation,
            sign_sk: STANDARD.encode(self.sign.secret_bytes()),
            dh_sk: STANDARD.encode(self.dh.0.to_bytes()),
        };
        let mut random_suffix = [0_u8; 8];
        OsRng.fill_bytes(&mut random_suffix);
        let temp_path = parent.join(format!(
            ".identity.json.{}.tmp",
            u64::from_be_bytes(random_suffix)
        ));
        let mut pending = PendingIdentityFile::create(temp_path)?;
        pending.file.write_all(&serde_json::to_vec(&document)?)?;
        pending.file.sync_all()?;
        fs::rename(&pending.path, path)?;
        pending.commit();
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

#[cfg(feature = "full")]
#[derive(Deserialize, Serialize)]
struct IdentityDocument {
    generation: u64,
    sign_sk: String,
    dh_sk: String,
}

#[cfg(feature = "full")]
struct PendingIdentityFile {
    path: PathBuf,
    file: File,
    committed: bool,
}

#[cfg(feature = "full")]
impl PendingIdentityFile {
    fn create(path: PathBuf) -> Result<Self, CryptoError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file,
            committed: false,
        })
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

#[cfg(feature = "full")]
impl Drop for PendingIdentityFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(feature = "full")]
fn decode_secret(encoded: &str) -> Result<[u8; 32], CryptoError> {
    STANDARD
        .decode(encoded)?
        .try_into()
        .map_err(|_| CryptoError::InvalidSecretLength)
}

#[cfg(feature = "full")]
fn derive_session_key(
    shared: &[u8; 32],
    base_info: &[u8],
    direction: &[u8; 3],
) -> Result<SessionKey, CryptoError> {
    let mut info = Vec::with_capacity(base_info.len() + direction.len());
    info.extend_from_slice(base_info);
    info.extend_from_slice(direction);
    let mut key = [0_u8; 32];
    Hkdf::<Sha256>::new(None, shared)
        .expand(&info, &mut key)
        .map_err(|_| CryptoError::KdfExpand)?;
    Ok(SessionKey::from_bytes(key))
}

/// Returns canonical sender-to-receiver associated data.
#[cfg(feature = "full")]
#[must_use]
pub fn aad(room_id: &str, sender_fp: &str, receiver_fp: &str) -> Vec<u8> {
    let mut associated_data =
        Vec::with_capacity(room_id.len() + sender_fp.len() + receiver_fp.len() + 2);
    associated_data.extend_from_slice(room_id.as_bytes());
    associated_data.push(0);
    associated_data.extend_from_slice(sender_fp.as_bytes());
    associated_data.push(0);
    associated_data.extend_from_slice(receiver_fp.as_bytes());
    associated_data
}

/// Returns the six-digit SAS derived from a QR-only nonce and sorted bundles.
#[cfg(feature = "full")]
#[must_use]
pub fn sas_code(nonce_a: &[u8], first: &PubBundle, second: &PubBundle) -> String {
    let first_bytes = bundle_bytes(first);
    let second_bytes = bundle_bytes(second);
    let mut hasher = Sha256::new();
    hasher.update(SAS_DOMAIN);
    hasher.update(nonce_a);
    if first_bytes <= second_bytes {
        hasher.update(first_bytes);
        hasher.update(second_bytes);
    } else {
        hasher.update(second_bytes);
        hasher.update(first_bytes);
    }
    let digest = hasher.finalize();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{value:06}")
}

/// Encrypts plaintext as `nonce24 || ciphertext || tag`.
///
/// # Errors
/// Returns [`CryptoError::Encryption`] if the AEAD rejects the input length.
#[cfg(feature = "full")]
pub fn seal(key: &SessionKey, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0_u8; NONCE_LENGTH];
    OsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new((&key.0).into());
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encryption)?;
    let mut sealed = Vec::with_capacity(NONCE_LENGTH + ciphertext.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

/// Authenticates and decrypts a `nonce24 || ciphertext || tag` value.
///
/// # Errors
/// Rejects truncated values, wrong keys, modified AAD, and modified ciphertext.
#[cfg(feature = "full")]
pub fn open(key: &SessionKey, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_LENGTH {
        return Err(CryptoError::SealedTooShort);
    }
    let (nonce, ciphertext) = sealed.split_at(NONCE_LENGTH);
    XChaCha20Poly1305::new((&key.0).into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decryption)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}
