#![cfg(feature = "full")]

use std::fs;
use std::path::{Path, PathBuf};

use clipboard_core::history::{
    HistoryContent, HistoryError, HistoryItem, HistoryKind, HistorySource, HistoryStore,
    NewHistoryItem,
};
use uuid::Uuid;

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

fn local_text(id: Uuid, ts_ms: i64, content: &str) -> NewHistoryItem {
    NewHistoryItem {
        id,
        ts_ms,
        source: HistorySource::Local,
        content: HistoryContent::Text {
            content: content.to_owned(),
        },
    }
}

#[test]
fn history_add_list_and_clear_flow() {
    // Given
    let dir = TestDir::new();
    let id = Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");

    // When
    store
        .add(NewHistoryItem {
            id,
            ts_ms: 1_234,
            source: HistorySource::Local,
            content: HistoryContent::Text {
                content: "copied text".to_owned(),
            },
        })
        .expect("text history should be added");

    // Then
    assert_eq!(
        store.list(),
        vec![HistoryItem {
            id,
            ts_ms: 1_234,
            source: HistorySource::Local,
            kind: HistoryKind::Text {
                content: "copied text".to_owned(),
            },
        }]
    );

    // When
    store.clear().expect("history should clear");

    // Then
    assert!(store.list().is_empty());
}

#[test]
fn history_duplicate_add_is_idempotent_without_reordering() {
    // Given
    let dir = TestDir::new();
    let first_id = Uuid::new_v4();
    let second_id = Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");
    store
        .add(local_text(first_id, 1, "first"))
        .expect("first item should be added");
    store
        .add(local_text(second_id, 2, "second"))
        .expect("second item should be added");

    // When
    store
        .add(local_text(first_id, 3, "replacement"))
        .expect("duplicate add should succeed");

    // Then
    let items = store.list();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, first_id);
    assert_eq!(items[1].id, second_id);
    assert_eq!(
        items[0].kind,
        HistoryKind::Text {
            content: "first".to_owned(),
        }
    );
}

#[test]
fn history_set_source_applies_deferred_remote_without_reordering() {
    // Given
    let dir = TestDir::new();
    let first_id = Uuid::new_v4();
    let deferred_id = Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");
    store
        .add(local_text(first_id, 1, "first"))
        .expect("first item should be added");
    store
        .add(NewHistoryItem {
            id: deferred_id,
            ts_ms: 2,
            source: HistorySource::RemoteDeferred,
            content: HistoryContent::Text {
                content: "mailbox".to_owned(),
            },
        })
        .expect("deferred item should be added");

    // When
    store
        .set_source(deferred_id, HistorySource::Remote)
        .expect("deferred item should be marked remote");

    // Then
    let items = store.list();
    assert_eq!(items[0].id, first_id);
    assert_eq!(items[1].id, deferred_id);
    assert_eq!(items[1].source, HistorySource::Remote);
}

#[test]
fn history_set_source_leaves_local_unchanged() {
    // Given
    let dir = TestDir::new();
    let id = Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");
    store
        .add(local_text(id, 1, "local"))
        .expect("local item should be added");

    // When
    store
        .set_source(id, HistorySource::Remote)
        .expect("setting source should be idempotent");

    // Then
    assert_eq!(store.list()[0].source, HistorySource::Local);
}

#[test]
fn history_set_source_leaves_remote_unchanged() {
    // Given
    let dir = TestDir::new();
    let id = Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");
    store
        .add(NewHistoryItem {
            id,
            ts_ms: 1,
            source: HistorySource::Remote,
            content: HistoryContent::Text {
                content: "remote".to_owned(),
            },
        })
        .expect("remote item should be added");

    // When
    store
        .set_source(id, HistorySource::Remote)
        .expect("setting source should be idempotent");

    // Then
    assert_eq!(store.list()[0].source, HistorySource::Remote);
}

#[test]
fn history_set_source_unknown_id_returns_typed_error() {
    // Given
    let dir = TestDir::new();
    let missing_id = Uuid::new_v4();
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");

    // When
    let error = store
        .set_source(missing_id, HistorySource::Remote)
        .expect_err("unknown item should fail");

    // Then
    assert!(matches!(error, HistoryError::UnknownItem(id) if id == missing_id));
}

#[test]
fn history_image_bytes_roundtrip() {
    // Given
    let dir = TestDir::new();
    let id = Uuid::new_v4();
    let expected = vec![0, 1, 2, 0xff];
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");

    // When
    store
        .add(NewHistoryItem {
            id,
            ts_ms: 1,
            source: HistorySource::Local,
            content: HistoryContent::Image {
                bytes: expected.clone(),
            },
        })
        .expect("image item should be added");

    // Then
    assert_eq!(
        store.image_bytes(id).expect("image bytes should load"),
        expected
    );
    assert_eq!(
        store.list()[0].kind,
        HistoryKind::Image {
            file_name: format!("{id}.img"),
        }
    );
}

#[test]
fn history_clear_deletes_image_files() {
    // Given
    let dir = TestDir::new();
    let id = Uuid::new_v4();
    let image_path = dir.path().join("images").join(format!("{id}.img"));
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");
    store
        .add(NewHistoryItem {
            id,
            ts_ms: 1,
            source: HistorySource::Local,
            content: HistoryContent::Image {
                bytes: vec![1, 2, 3],
            },
        })
        .expect("image item should be added");

    // When
    store.clear().expect("history should clear");

    // Then
    assert!(!image_path.exists());
}
