use serde::{Deserialize, Serialize};

pub const PUBLIC_KEY_LENGTH: usize = 32;
pub const BUNDLE_BYTES_LENGTH: usize = PUBLIC_KEY_LENGTH * 2;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PubBundle {
    pub sign_pk: [u8; PUBLIC_KEY_LENGTH],
    pub dh_pk: [u8; PUBLIC_KEY_LENGTH],
}

#[must_use]
pub fn bundle_bytes(bundle: &PubBundle) -> [u8; BUNDLE_BYTES_LENGTH] {
    let mut bytes = [0_u8; BUNDLE_BYTES_LENGTH];
    let (sign_pk, dh_pk) = bytes.split_at_mut(PUBLIC_KEY_LENGTH);
    sign_pk.copy_from_slice(&bundle.sign_pk);
    dh_pk.copy_from_slice(&bundle.dh_pk);
    bytes
}
