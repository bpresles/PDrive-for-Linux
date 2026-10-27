//! Files kept available offline: downloaded ahead of use, never purged from the cache, and
//! downloaded again when they get a new revision. Saved in `$XDG_DATA_HOME/pdrive/pinned.json`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Pinned {
    /// Node UID → revision UID kept offline.
    files: HashMap<String, String>,
    /// Folders whose new files are kept offline too.
    folders: HashSet<String>,
}

pub struct Pins {
    path: PathBuf,
    pinned: Mutex<Pinned>,
}

impl Pins {
    pub fn load(path: PathBuf) -> Self {
        let pinned = std::fs::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
        Self { path, pinned: Mutex::new(pinned) }
    }

    fn pinned(&self) -> std::sync::MutexGuard<'_, Pinned> {
        self.pinned.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_pinned(&self, node_uid: &str) -> bool {
        self.pinned().files.contains_key(node_uid)
    }

    pub fn is_folder_pinned(&self, node_uid: &str) -> bool {
        self.pinned().folders.contains(node_uid)
    }

    pub fn pinned_revisions(&self) -> HashSet<String> {
        self.pinned().files.values().cloned().collect()
    }

    /// Pins a file at a revision (or moves its pin to a new revision).
    pub fn pin(&self, node_uid: &str, revision_uid: &str) {
        let changed = self.pinned().files.insert(node_uid.to_owned(), revision_uid.to_owned()).as_deref() != Some(revision_uid);
        if changed {
            self.save();
        }
    }

    pub fn unpin(&self, node_uid: &str) {
        if self.pinned().files.remove(node_uid).is_some() {
            self.save();
        }
    }

    pub fn pin_folder(&self, node_uid: &str) {
        if self.pinned().folders.insert(node_uid.to_owned()) {
            self.save();
        }
    }

    pub fn unpin_folder(&self, node_uid: &str) {
        if self.pinned().folders.remove(node_uid) {
            self.save();
        }
    }

    fn save(&self) {
        let data = serde_json::to_vec(&*self.pinned()).unwrap_or_default();
        let temporary = self.path.with_extension("json.tmp");
        let result = std::fs::write(&temporary, data).and_then(|()| std::fs::rename(&temporary, &self.path));
        if let Err(e) = result {
            eprintln!("cannot save {}: {e}", self.path.display());
        }
    }
}
