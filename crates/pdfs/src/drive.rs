//! State shared by the FUSE callbacks and the event watcher.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use fuser::{Errno, INodeNo, Notifier};
use pdsdk::DriveClient;
use pdsdk::proto::{file_thumbnail, ThumbnailType};

use crate::content::{ContentCache, Opened};
use crate::replay::{self, Replay};
use crate::thumbnails::ThumbnailCache;
use crate::writes::Staging;
use crate::tree::{EntryKind, Invalidation, NodeInfo, NodeKind, Tree};

/// Fetching the thumbnails of a folder listing gives up after this delay.
const THUMBNAILS_TIMEOUT: Duration = Duration::from_secs(15);

/// A remote change waiting to be applied by the FUSE callback that the watcher triggers
/// through the mount (so that the kernel emits inotify events for it).
pub enum Change {
    Update { parent: u64, node: NodeInfo },
    Remove { uid: String },
    /// Nothing changed in the tree, but the file manager must re-read the file information
    /// (e.g. its thumbnail became available).
    Refresh,
}

pub struct Drive {
    pub client: Arc<DriveClient>,
    pub tree: Mutex<Tree>,
    pub content: Arc<ContentCache>,
    pub staging: Staging,
    thumbnails: Arc<ThumbnailCache>,
    pub notifier: OnceLock<Notifier>,
    pub verbose: bool,
    /// Absolute path of the mount point.
    pub mountpoint: std::path::PathBuf,
    pending: Mutex<Option<Change>>,
    /// Serializes replays through the mount (see `replay.rs`).
    pub replay_lock: tokio::sync::Mutex<()>,
    deferred: Mutex<Vec<Invalidation>>,
}

impl Drive {
    pub fn new(
        client: DriveClient,
        root_uid: String,
        content: ContentCache,
        staging: Staging,
        thumbnails: ThumbnailCache,
        mountpoint: PathBuf,
        verbose: bool,
    ) -> Self {
        Self {
            client: Arc::new(client),
            tree: Mutex::new(Tree::new(root_uid)),
            content: Arc::new(content),
            staging,
            thumbnails: Arc::new(thumbnails),
            notifier: OnceLock::new(),
            verbose,
            mountpoint,
            pending: Mutex::new(None),
            replay_lock: tokio::sync::Mutex::new(()),
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
            Change::Refresh => Vec::new(),
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
    pub async fn load_children(self: &Arc<Self>, ino: u64) -> Result<(), Errno> {
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
        let with_thumbnails: Vec<NodeInfo> =
            children.iter().filter(|n| matches!(n.kind, NodeKind::File { has_thumbnail: true, .. })).cloned().collect();
        {
            let mut tree = self.tree();
            // Another request may have loaded it meanwhile, or the folder may be gone.
            if tree.get(ino).is_none() || tree.is_loaded(ino) {
                return Ok(());
            }
            tree.set_children(ino, children);
        }
        // In the background: the listing must not wait for thumbnails.
        let drive = self.clone();
        tokio::spawn(async move { drive.ensure_thumbnails(with_thumbnails).await });
        Ok(())
    }

    /// Writes the Proton thumbnails of these files into the desktop thumbnail cache, for those
    /// that do not have a valid one yet, then makes the file manager re-read these files.
    pub async fn ensure_thumbnails(self: &Arc<Self>, nodes: Vec<NodeInfo>) {
        let missing: Vec<(String, PathBuf, SystemTime)> = {
            let tree = self.tree();
            nodes
                .into_iter()
                .filter_map(|n| {
                    let path = tree.path_of(tree.ino_of(&n.uid)?)?;
                    (!self.thumbnails.has_valid(&self.mountpoint.join(&path), n.mtime)).then_some((n.uid, path, n.mtime))
                })
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let uids = missing.iter().map(|(uid, ..)| uid.clone()).collect();
        let thumbnails = match tokio::time::timeout(THUMBNAILS_TIMEOUT, self.client.thumbnails(uids, ThumbnailType::Thumbnail)).await {
            Ok(Ok(thumbnails)) => thumbnails,
            Ok(Err(e)) => return eprintln!("cannot fetch thumbnails: {e}"),
            Err(_) => return eprintln!("fetching thumbnails timed out"),
        };
        let (cache, mountpoint) = (self.thumbnails.clone(), self.mountpoint.clone());
        let written = tokio::task::spawn_blocking(move || {
            let mut written = Vec::new();
            for thumbnail in thumbnails {
                let Some(file_thumbnail::Result::Data(data)) = thumbnail.result else { continue };
                let Some((_, path, mtime)) = missing.iter().find(|(uid, ..)| *uid == thumbnail.file_uid) else { continue };
                match cache.write(&mountpoint.join(path), *mtime, &data) {
                    Ok(()) => written.push((path.clone(), *mtime)),
                    Err(e) => eprintln!("cannot write the thumbnail of {}: {e:#}", path.display()),
                }
            }
            written
        })
        .await
        .unwrap_or_default();
        if self.verbose {
            eprintln!("[thumbnails] {} written", written.len());
        }
        // An attribute change (same timestamp) makes the file manager pick the thumbnail up.
        for (path, mtime) in written {
            replay::run(self, Change::Refresh, Replay::Touch(path, mtime)).await;
        }
    }

    /// Revision UID, size and whether Proton has a thumbnail, for a file.
    pub fn file_revision(&self, ino: u64) -> Result<(String, u64, bool), Errno> {
        match &self.tree().get(ino).ok_or(Errno::ENOENT)?.kind {
            EntryKind::File { revision_uid, size, has_thumbnail } => Ok((revision_uid.clone(), *size, *has_thumbnail)),
            EntryKind::Folder { .. } => Err(Errno::EISDIR),
        }
    }

    /// Opens the content of a file. Without `need_all`, the cached beginning of the file may be
    /// returned; otherwise its download is started if it is not cached.
    pub async fn open_content(self: &Arc<Self>, ino: u64, need_all: bool) -> Result<Opened, Errno> {
        let (revision_uid, size, _) = self.file_revision(ino)?;
        let drive = Arc::downgrade(self);
        // The real size can differ from the size claimed by the uploader.
        let on_complete = move |size| {
            if let Some(drive) = drive.upgrade() {
                drive.tree().set_file_size(ino, size);
            }
        };
        self.content.open(self.client.clone(), &revision_uid, size, need_all, on_complete).await.map_err(|e| {
            eprintln!("cannot open revision {revision_uid}: {e}");
            Errno::EIO
        })
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

