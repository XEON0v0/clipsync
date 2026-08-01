pub mod protocol;

#[cfg(feature = "verify")]
pub mod crypto;

#[cfg(feature = "full")]
pub mod full {}
#[cfg(feature = "verify")]
pub mod verify {}

pub use protocol::{PubBundle, bundle_bytes};

#[cfg(feature = "verify")]
pub use crypto::verify as verify_signature;
#[cfg(feature = "verify")]
pub use crypto::{CryptoError, bundle_fp, device_id, join_sig_msg, room_id};

#[cfg(feature = "full")]
pub use crypto::{
    Ed25519Keypair, Identity, SessionKey, SessionKeys, X25519Keypair, aad, open, sas_code, seal,
};

#[cfg(feature = "full")]
pub mod history;

#[cfg(feature = "full")]
pub mod pairing;
