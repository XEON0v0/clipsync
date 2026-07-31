#![cfg(feature = "full")]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clipboard_core::crypto::{CryptoError, Identity};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn create() -> Self {
        let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clipboard-core-crypto-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create isolated crypto test directory");
        Self(path)
    }

    fn identity_path(&self) -> PathBuf {
        self.0.join("identity.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove isolated crypto test directory");
    }
}

#[cfg(unix)]
#[test]
fn crypto_identity_reload_preserves_keys_generation_and_mode() {
    use std::os::unix::fs::PermissionsExt;

    // Given
    let directory = TestDirectory::create();
    let path = directory.identity_path();
    let created = Identity::load_or_create(&path).expect("create identity");
    let expected_bundle = created.public_bundle();
    drop(created);

    // When
    let reloaded = Identity::load_or_create(&path).expect("reload identity");
    let mode = std::fs::metadata(&path)
        .expect("identity metadata")
        .permissions()
        .mode()
        & 0o777;

    // Then
    assert_eq!(reloaded.public_bundle(), expected_bundle);
    assert_eq!(reloaded.generation(), 1);
    assert_eq!(mode, 0o600);
}

#[test]
fn crypto_identity_reload_rejects_zero_generation() {
    // Given
    let directory = TestDirectory::create();
    let path = directory.identity_path();
    let encoded_key = STANDARD.encode([1_u8; 32]);
    let document = serde_json::json!({
        "generation": 0,
        "sign_sk": encoded_key,
        "dh_sk": STANDARD.encode([2_u8; 32]),
    });
    std::fs::write(
        &path,
        serde_json::to_vec(&document).expect("serialize invalid fixture"),
    )
    .expect("write invalid identity fixture");

    // When
    let result = Identity::load_or_create(&path);

    // Then
    assert!(matches!(result, Err(CryptoError::InvalidGeneration(0))));
}
