//! Per-file sync state for the file manager integration, and the offline actions.

use std::path::Path;
use std::sync::Arc;

use crate::content::Opened;
use crate::drive::Drive;
use crate::tree::{is_local, EntryKind};
use crate::writes::is_temporary;

/// State shown as an emblem by the Nautilus extension.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileStatus {
    /// Only in the cloud: opening it downloads it.
    Online,
    /// Downloaded and readable without network (until purged from the cache).
    Available,
    /// Kept available offline.
    Pinned,
    /// Local changes being uploaded.
    Uploading,
    /// Only local, never uploaded (editor temporary files).
    Local,
    /// Not in the mount, or not loaded.
    Unknown,
}

impl FileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FileStatus::Online => "online",
            FileStatus::Available => "available",
            FileStatus::Pinned => "pinned",
            FileStatus::Uploading => "uploading",
            FileStatus::Local => "local",
            FileStatus::Unknown => "unknown",
        }
    }
}

/// Global state shown in the sidebar.
pub struct ActivitySummary {
    pub uploads: usize,
    pub downloads: usize,
}

impl Drive {
    /// Inode of an absolute path inside the mount, among the loaded entries.
    pub fn ino_of_path(&self, path: &Path) -> Option<u64> {
        let relative = path.strip_prefix(&self.mountpoint).ok()?;
        self.tree().lookup_path(relative)
    }

    pub fn status_of(&self, ino: u64) -> FileStatus {
        let (uid, kind_revision, name, is_folder) = {
            let tree = self.tree();
            let Some(entry) = tree.get(ino) else { return FileStatus::Unknown };
            match &entry.kind {
                EntryKind::Folder { .. } => (entry.uid.clone(), String::new(), entry.name.clone(), true),
                EntryKind::File { revision_uid, .. } => (entry.uid.clone(), revision_uid.clone(), entry.name.clone(), false),
            }
        };
        if is_folder {
            return if self.pins.is_folder_pinned(&uid) { FileStatus::Pinned } else { FileStatus::Online };
        }
        if is_local(&uid) && is_temporary(&name) {
            FileStatus::Local
        } else if self.staged_path(ino).is_some() {
            FileStatus::Uploading
        } else if self.pins.is_pinned(&uid) {
            FileStatus::Pinned
        } else if self.content.is_cached(&kind_revision) {
            FileStatus::Available
        } else {
            FileStatus::Online
        }
    }

    pub fn status_of_path(&self, path: &Path) -> FileStatus {
        self.ino_of_path(path).map_or(FileStatus::Unknown, |ino| self.status_of(ino))
    }

    /// Overall state exposed on D-Bus: `syncing`, `idle`, `unmounted` or `error`.
    pub fn state(&self) -> &'static str {
        if !self.mounted.load(std::sync::atomic::Ordering::SeqCst) {
            "unmounted"
        } else if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
            "error"
        } else {
            let activity = self.activity();
            if activity.uploads + activity.downloads > 0 { "syncing" } else { "idle" }
        }
    }

    pub fn activity(&self) -> ActivitySummary {
        ActivitySummary { uploads: self.staging_count(), downloads: self.content.active_transfers() }
    }

    /// Tells the file manager that the state of a file changed.
    pub fn notify_status(&self, ino: u64) {
        let Some(connection) = self.dbus.get() else { return };
        let Some(path) = self.tree().path_of(ino) else { return };
        crate::dbus::emit_file_status_changed(connection, &self.mountpoint.join(path));
    }

    /// Downloads files (folders: everything below them) and keeps them available offline.
    pub async fn keep_offline(self: &Arc<Self>, ino: u64) {
        for file in self.files_below(ino).await {
            let Some((uid, revision_uid)) = self.remote_file(file) else { continue };
            self.pins.pin(&uid, &revision_uid);
            self.notify_status(file);
            if self.content.is_cached(&revision_uid) {
                continue;
            }
            match self.open_content(file, true).await {
                Ok(Opened::Partial(_, transfer)) => {
                    transfer.wait_for(u64::MAX).await;
                    self.content.release(transfer);
                }
                Ok(_) => {}
                Err(e) => eprintln!("cannot download {revision_uid} to keep it offline: {e:?}"),
            }
            self.notify_status(file);
        }
        if let Some(uid) = self.tree().get(ino).filter(|e| matches!(e.kind, EntryKind::Folder { .. })).map(|e| e.uid.clone()) {
            self.pins.pin_folder(&uid);
            self.notify_status(ino);
        }
    }

    /// Stops keeping files offline and removes their downloaded content.
    pub async fn free_up_space(self: &Arc<Self>, ino: u64) {
        if let Some(uid) = self.tree().get(ino).filter(|e| matches!(e.kind, EntryKind::Folder { .. })).map(|e| e.uid.clone()) {
            self.pins.unpin_folder(&uid);
            self.notify_status(ino);
        }
        for file in self.files_below(ino).await {
            let Some((uid, revision_uid)) = self.remote_file(file) else { continue };
            self.pins.unpin(&uid);
            // Files being written keep their content until uploaded.
            if self.staged_path(file).is_none() {
                self.content.evict(&revision_uid);
            }
            self.notify_status(file);
        }
    }

    /// A pinned file got a new revision: keep the new one offline.
    pub async fn refresh_pinned(self: &Arc<Self>, ino: u64) {
        let Some((uid, _)) = self.remote_file(ino) else { return };
        if self.pins.is_pinned(&uid) {
            self.keep_offline(ino).await;
        }
    }

    fn remote_file(&self, ino: u64) -> Option<(String, String)> {
        let tree = self.tree();
        let entry = tree.get(ino)?;
        match &entry.kind {
            EntryKind::File { revision_uid, .. } if !is_local(&entry.uid) && !revision_uid.is_empty() => {
                Some((entry.uid.clone(), revision_uid.clone()))
            }
            _ => None,
        }
    }

    /// The file itself, or all the files below a folder (listing sub-folders as needed).
    async fn files_below(self: &Arc<Self>, ino: u64) -> Vec<u64> {
        let mut files = Vec::new();
        let mut folders = vec![ino];
        while let Some(folder) = folders.pop() {
            let is_folder = matches!(self.tree().get(folder).map(|e| &e.kind), Some(EntryKind::Folder { .. }));
            if !is_folder {
                files.push(folder);
                continue;
            }
            if self.load_children(folder).await.is_err() {
                continue;
            }
            folders.extend(self.tree().children(folder).unwrap_or_default());
        }
        files
    }
}
