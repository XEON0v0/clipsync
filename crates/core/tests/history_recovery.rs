#![cfg(feature = "full")]

use std::fs;
use std::path::{Path, PathBuf};

use clipboard_core::history::{HistoryContent, HistorySource, HistoryStore, NewHistoryItem};
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

fn local_text(id: Uuid, ts_ms: i64) -> NewHistoryItem {
    NewHistoryItem {
        id,
        ts_ms,
        source: HistorySource::Local,
        content: HistoryContent::Text {
            content: "newer".to_owned(),
        },
    }
}

#[test]
fn history_cap_evicts_oldest_and_deletes_its_image() {
    // Given
    let dir = TestDir::new();
    let oldest_id = Uuid::new_v4();
    let oldest_image_path = dir.path().join("images").join(format!("{oldest_id}.img"));
    let mut store = HistoryStore::new(dir.path()).expect("history store should open");
    store
        .add(NewHistoryItem {
            id: oldest_id,
            ts_ms: 0,
            source: HistorySource::Local,
            content: HistoryContent::Image { bytes: vec![9] },
        })
        .expect("oldest image should be added");

    // When
    for ts_ms in 1..=50 {
        store
            .add(local_text(Uuid::new_v4(), ts_ms))
            .expect("newer item should be added");
    }

    // Then
    let items = store.list();
    assert_eq!(items.len(), 50);
    assert!(!items.iter().any(|item| item.id == oldest_id));
    assert!(!oldest_image_path.exists());
}

#[test]
fn history_persists_across_store_reopen() {
    // Given
    let dir = TestDir::new();
    let id = Uuid::new_v4();
    {
        let mut store = HistoryStore::new(dir.path()).expect("history store should open");
        store
            .add(NewHistoryItem {
                id,
                ts_ms: 42,
                source: HistorySource::RemoteDeferred,
                content: HistoryContent::Image { bytes: vec![4, 2] },
            })
            .expect("history item should be added");
    }

    // When
    let reopened = HistoryStore::new(dir.path()).expect("history store should reopen");

    // Then
    assert_eq!(reopened.list().len(), 1);
    assert_eq!(reopened.list()[0].id, id);
    assert_eq!(
        reopened
            .image_bytes(id)
            .expect("persisted image should load"),
        vec![4, 2]
    );
}

#[test]
fn history_corrupt_jsonl_lines_are_skipped_without_panic() {
    // Given
    let dir = TestDir::new();
    let valid_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001")
        .expect("fixture UUID should be valid");
    fs::create_dir_all(dir.path()).expect("test directory should be created");
    fs::write(
        dir.path().join("history.jsonl"),
        concat!(
            "{\"id\":\"00000000-0000-4000-8000-000000000001\",",
            "\"ts_ms\":7,\"kind\":{\"type\":\"text\",\"content\":\"kept\"},",
            "\"source\":\"local\"}\n",
            "not-json\n",
            "{\"id\":\"truncated"
        ),
    )
    .expect("corrupt fixture should be written");

    // When
    let store = HistoryStore::new(dir.path()).expect("corrupt lines should be tolerated");

    // Then
    assert_eq!(store.list().len(), 1);
    assert_eq!(store.list()[0].id, valid_id);
}

#[test]
fn history_open_removes_orphan_image_files() {
    // Given
    let dir = TestDir::new();
    let orphan_path = dir.path().join("images").join("orphan.img");
    fs::create_dir_all(orphan_path.parent().expect("orphan should have a parent"))
        .expect("images directory should be created");
    fs::write(&orphan_path, [1, 2, 3]).expect("orphan image should be written");

    // When
    let store = HistoryStore::new(dir.path()).expect("history store should open");

    // Then
    assert!(store.list().is_empty());
    assert!(!orphan_path.exists());
}
