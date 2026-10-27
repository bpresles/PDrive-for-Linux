//! State shared by the FUSE callbacks and the event watcher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use fuser::{Errno, INodeNo, Notifier};
use pdsdk::DriveClient;

use crate::tree::{EntryKind, Invalidation, NodeInfo, Tree};

/// A remote change waiting to be applied by the FUSE callback that the watcher triggers
/// through the mount (so that the kernel emits inotify events for it).
pub enum Change {
    Update { parent: u64, node: NodeInfo },
    Remove { uid: String },
}

pub struct Drive {
    pub client: DriveClient,
    pub tree: Mutex<Tree>,
    /// Decrypted file contents, one file per revision.
    content_dir: PathBuf,
    downloads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub notifier: OnceLock<Notifier>,
    pub verbose: bool,
    /// Absolute path of the mount point.
    pub mountpoint: std::path::PathBuf,
    pending: Mutex<Option<Change>>,
    deferred: Mutex<Vec<Invalidation>>,
}

impl Drive {
    pub fn new(client: DriveClient, root_uid: String, content_dir: PathBuf, mountpoint: PathBuf, verbose: bool) -> Self {
        Self {
            client,
            tree: Mutex::new(Tree::new(root_uid)),
            content_dir,
            downloads: Mutex::new(HashMap::new()),
            notifier: OnceLock::new(),
            verbose,
            mountpoint,
            pending: Mutex::new(None),
            deferred: Mutex::new(Vec::new()),
        }
    }

    pub fn set_pending(&self, change: Change) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(change);
    }

    /// Applies the pending change, if any. Kernel invalidations cannot be sent from inside a
    /// FUSE callback, so they are kept for [`Drive::flush_deferred`].
    pub fn apply_pending(&self) -> bool {
        let Some(change) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return false;
        };
        let invalidations = self.apply(change);
        self.deferred.lock().unwrap_or_else(|e| e.into_inner()).extend(invalidations);
        true
    }

    /// Applies a change to the tree and returns the kernel caches to invalidate.
    pub fn apply(&self, change: Change) -> Vec<Invalidation> {
        let mut tree = self.tree();
        match change {
            Change::Update { parent, node } => tree.apply_update(parent, node),
            Change::Remove { uid } => tree.remove_uid(&uid),
        }
    }

    pub fn flush_deferred(&self) {
        let invalidations = std::mem::take(&mut *self.deferred.lock().unwrap_or_else(|e| e.into_inner()));
        self.invalidate(invalidations);
    }

    pub fn tree(&self) -> std::sync::MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Fetches and decrypts the children of a remote folder.
    pub async fn fetch_children(&self, folder_uid: &str) -> pdsdk::Result<Vec<NodeInfo>> {
        let uids = self.client.folder_children(folder_uid).await?;
        let nodes = self.client.nodes(uids).await?;
        Ok(nodes.into_iter().filter_map(NodeInfo::from_node).collect())
    }

    /// Makes sure the listing of a folder is loaded.
    pub async fn load_children(&self, ino: u64) -> Result<(), Errno> {
        let uid = {
            let tree = self.tree();
            let entry = tree.get(ino).ok_or(Errno::ENOENT)?;
            match &entry.kind {
                EntryKind::Folder { children: Some(_) } => return Ok(()),
                EntryKind::Folder { children: None } => entry.uid.clone(),
                EntryKind::File { .. } => return Err(Errno::ENOTDIR),
            }
        };
        let children = self.fetch_children(&uid).await.map_err(|e| {
            eprintln!("cannot list folder {uid}: {e}");
            Errno::EIO
        })?;
        let mut tree = self.tree();
        // Another request may have loaded it meanwhile, or the folder may be gone.
        if tree.get(ino).is_some() && !tree.is_loaded(ino) {
            tree.set_children(ino, children);
        }
        Ok(())
    }

    /// Returns the local path of the decrypted content of a file, downloading it if needed.
    pub async fn content_path(&self, ino: u64) -> Result<PathBuf, Errno> {
        let revision_uid = match &self.tree().get(ino).ok_or(Errno::ENOENT)?.kind {
            EntryKind::File { revision_uid, .. } => revision_uid.clone(),
            EntryKind::Folder { .. } => return Err(Errno::EISDIR),
        };
        let path = self.content_dir.join(file_name_for(&revision_uid));

        // One download per revision at a time; later openers wait for it.
        let lock = self.downloads.lock().unwrap_or_else(|e| e.into_inner()).entry(revision_uid.clone()).or_default().clone();
        let _guard = lock.lock().await;
        if !path.exists() {
            let partial = path.with_extension("part");
            if self.verbose {
                eprintln!("[download] {revision_uid}");
            }
            let result = self.client.download_to_file(&revision_uid, &partial).await;
            if let Err(e) = result.and_then(|()| std::fs::rename(&partial, &path).map_err(|e| pdsdk::SdkError::Protocol(e.to_string()))) {
                let _ = std::fs::remove_file(&partial);
                eprintln!("cannot download revision {revision_uid}: {e}");
                return Err(Errno::EIO);
            }
        }
        // The real size can differ from the size claimed by the uploader.
        if let Ok(metadata) = std::fs::metadata(&path) {
            self.tree().set_file_size(ino, metadata.len());
        }
        Ok(path)
    }

    /// Tells the kernel to drop stale cached entries.
    pub fn invalidate(&self, invalidations: Vec<Invalidation>) {
        let Some(notifier) = self.notifier.get() else { return };
        for invalidation in invalidations {
            // ENOENT only means the kernel had nothing cached.
            let _ = match invalidation {
                Invalidation::Entry(parent, name) => notifier.inval_entry(INodeNo(parent), name.as_ref()),
                Invalidation::Inode(ino) => notifier.inval_inode(INodeNo(ino), 0, 0),
            };
        }
    }
}

/// Revision UIDs are longer than the 255-byte file name limit: name cache files by their hash.
fn file_name_for(revision_uid: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(revision_uid.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}
