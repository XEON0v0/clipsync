use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const METADATA_FILE: &str = "history.jsonl";
const IMAGES_DIR: &str = "images";
pub const HISTORY_CAP: usize = 50;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    Local,
    Remote,
    RemoteDeferred,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryContent {
    Text { content: String },
    Image { bytes: Vec<u8> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewHistoryItem {
    pub id: Uuid,
    pub ts_ms: i64,
    pub source: HistorySource,
    pub content: HistoryContent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HistoryKind {
    Text { content: String },
    Image { file_name: String },
}

impl HistoryKind {
    fn image_file_name(&self) -> Option<&str> {
        match self {
            Self::Text { content: _ } => None,
            Self::Image { file_name } => Some(file_name),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HistoryItem {
    pub id: Uuid,
    pub ts_ms: i64,
    pub kind: HistoryKind,
    pub source: HistorySource,
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("history I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("history metadata serialization failed: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("history item {0} was not found")]
    UnknownItem(Uuid),
    #[error("history item {0} does not contain an image")]
    NotImage(Uuid),
}

pub struct HistoryStore {
    dir: PathBuf,
    items: Vec<HistoryItem>,
}

impl HistoryStore {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, HistoryError> {
        let dir = dir.as_ref().to_path_buf();
        let images_dir = dir.join(IMAGES_DIR);
        fs::create_dir_all(&images_dir).map_err(|source| HistoryError::io(&dir, source))?;
        let mut items = load_items(&dir.join(METADATA_FILE))?;
        if items.len() > HISTORY_CAP {
            items.drain(..items.len() - HISTORY_CAP);
        }
        cleanup_orphan_images(&images_dir, &items)?;
        Ok(Self { dir, items })
    }

    pub fn add(&mut self, item: NewHistoryItem) -> Result<(), HistoryError> {
        if self.items.iter().any(|existing| existing.id == item.id) {
            return Ok(());
        }
        let (kind, image_path) = match item.content {
            HistoryContent::Text { content } => (HistoryKind::Text { content }, None),
            HistoryContent::Image { bytes } => {
                let file_name = format!("{}.img", item.id);
                let image_path = self.dir.join(IMAGES_DIR).join(&file_name);
                atomic_write(&image_path, &bytes)?;
                (HistoryKind::Image { file_name }, Some(image_path))
            }
        };
        let mut updated = self.items.clone();
        updated.push(HistoryItem {
            id: item.id,
            ts_ms: item.ts_ms,
            kind,
            source: item.source,
        });
        let evicted = if updated.len() > HISTORY_CAP {
            Some(updated.remove(0))
        } else {
            None
        };
        if let Err(error) = self.persist(&updated) {
            if let Some(path) = image_path {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        self.items = updated;
        if let Some(item) = evicted
            && let Some(file_name) = item.kind.image_file_name()
        {
            remove_file_if_exists(&self.dir.join(IMAGES_DIR).join(file_name))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn list(&self) -> Vec<HistoryItem> {
        self.items.clone()
    }

    pub fn clear(&mut self) -> Result<(), HistoryError> {
        let image_paths: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| item.kind.image_file_name())
            .map(|file_name| self.dir.join(IMAGES_DIR).join(file_name))
            .collect();
        self.persist(&[])?;
        self.items.clear();
        for path in image_paths {
            remove_file_if_exists(&path)?;
        }
        Ok(())
    }

    pub fn image_bytes(&self, id: Uuid) -> Result<Vec<u8>, HistoryError> {
        let item = self
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or(HistoryError::UnknownItem(id))?;
        match &item.kind {
            HistoryKind::Text { content: _ } => Err(HistoryError::NotImage(id)),
            HistoryKind::Image { file_name } => {
                let path = self.dir.join(IMAGES_DIR).join(file_name);
                fs::read(&path).map_err(|source| HistoryError::io(&path, source))
            }
        }
    }

    pub fn set_source(&mut self, id: Uuid, source: HistorySource) -> Result<(), HistoryError> {
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or(HistoryError::UnknownItem(id))?;
        let transition = match (&self.items[index].source, source) {
            (HistorySource::RemoteDeferred, HistorySource::Remote) => true,
            (
                HistorySource::Local | HistorySource::Remote,
                HistorySource::Local | HistorySource::Remote | HistorySource::RemoteDeferred,
            )
            | (
                HistorySource::RemoteDeferred,
                HistorySource::Local | HistorySource::RemoteDeferred,
            ) => false,
        };
        if transition {
            let mut updated = self.items.clone();
            updated[index].source = HistorySource::Remote;
            self.persist(&updated)?;
            self.items = updated;
        }
        Ok(())
    }

    fn persist(&self, items: &[HistoryItem]) -> Result<(), HistoryError> {
        let mut encoded = Vec::new();
        for item in items {
            serde_json::to_writer(&mut encoded, item)?;
            encoded.push(b'\n');
        }
        atomic_write(&self.dir.join(METADATA_FILE), &encoded)
    }
}

impl HistoryError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), HistoryError> {
    let temp_path = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    let mut file =
        File::create(&temp_path).map_err(|source| HistoryError::io(&temp_path, source))?;
    file.write_all(contents)
        .map_err(|source| HistoryError::io(&temp_path, source))?;
    file.sync_all()
        .map_err(|source| HistoryError::io(&temp_path, source))?;
    fs::rename(&temp_path, path).map_err(|source| HistoryError::io(path, source))?;
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<(), HistoryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(HistoryError::io(path, source)),
    }
}

fn load_items(path: &Path) -> Result<Vec<HistoryItem>, HistoryError> {
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(HistoryError::io(path, source)),
    };
    let mut ids = HashSet::new();
    let mut items = Vec::new();
    for line in contents.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let item: HistoryItem = match serde_json::from_slice(line) {
            Ok(item) => item,
            Err(_) => continue,
        };
        let valid_reference = item
            .kind
            .image_file_name()
            .is_none_or(|file_name| file_name == format!("{}.img", item.id));
        if valid_reference && ids.insert(item.id) {
            items.push(item);
        }
    }
    Ok(items)
}

fn cleanup_orphan_images(images_dir: &Path, items: &[HistoryItem]) -> Result<(), HistoryError> {
    let referenced: HashSet<&str> = items
        .iter()
        .filter_map(|item| item.kind.image_file_name())
        .collect();
    let entries =
        fs::read_dir(images_dir).map_err(|source| HistoryError::io(images_dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| HistoryError::io(images_dir, source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| HistoryError::io(&path, source))?;
        if !(file_type.is_file() || file_type.is_symlink()) {
            continue;
        }
        let file_name = entry.file_name();
        let is_referenced = file_name
            .to_str()
            .is_some_and(|file_name| referenced.contains(file_name));
        if !is_referenced {
            remove_file_if_exists(&path)?;
        }
    }
    Ok(())
}
