// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! State shared by the FUSE callbacks and the event watcher.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use fuser::{Errno, INodeNo, Notifier};
use pdcore::api::ApiClient;
use pdsdk::DriveClient;
use pdsdk::proto::{ThumbnailType, file_thumbnail};

use crate::activity::{ActivityLog, Kind};
use crate::content::{ContentCache, Opened};
use crate::listing::InFlight;
use crate::pins::Pins;
use crate::replay::{self, Replay};
use crate::thumbnails::ThumbnailCache;
use crate::tree::{EntryKind, Invalidation, NodeInfo, NodeKind, Tree};
use crate::writes::Staging;

/// Fetching the thumbnails of a folder listing gives up after this delay.
const THUMBNAILS_TIMEOUT: Duration = Duration::from_secs(15);

/// Folder listings made ahead of time at once.
const PREFETCH_PARALLELISM: usize = 4;

/// A remote change waiting to be applied by the FUSE callback that the watcher triggers
/// through the mount (so that the kernel emits inotify events for it).
pub enum Change {
    Update {
        parent: u64,
        node: NodeInfo,
    },
    Remove {
        uid: String,
    },
    /// Nothing changed in the tree, but the file manager must re-read the file information
    /// (e.g. its thumbnail became available).
    Refresh,
}

/// Address and storage of the account, as last fetched.
#[derive(Clone, Default, PartialEq)]
pub struct Account {
    pub email: String,
    pub used_space: u64,
    pub max_space: u64,
}

/// Minimum delay between two calls to the user endpoint.
const QUOTA_REFRESH_DELAY: Duration = Duration::from_secs(60);

pub struct Drive {
    pub client: Arc<DriveClient>,
    pub api: Arc<ApiClient>,
    pub activity_log: Arc<ActivityLog>,
    pub account: Mutex<Account>,
    /// An upload was given up and not succeeded since: shown as the "error" state.
    pub failed: AtomicBool,
    quota_changed: tokio::sync::Notify,
    pub tree: Mutex<Tree>,
    pub content: Arc<ContentCache>,
    pub staging: Staging,
    pub pins: Pins,
    /// Session bus connection of the D-Bus service, for signals.
    pub dbus: OnceLock<gio::DBusConnection>,
    thumbnails: Arc<ThumbnailCache>,
    /// Kernel notifications of the current FUSE session, while mounted.
    notifier: Mutex<Option<Notifier>>,
    pub mounted: AtomicBool,
    /// Whether the user wants the drive mounted ("Mount"/"Unmount" sidebar actions, eject). A
    /// state rather than requests: repeated or stale clicks cannot pile up.
    pub wanted_mounted: tokio::sync::watch::Sender<bool>,
    pub verbose: bool,
    /// Absolute path of the mount point as seen by clients (see `mountpoint.rs`).
    pub mountpoint: std::path::PathBuf,
    /// Where the file system is really mounted.
    pub mount_target: std::path::PathBuf,
    pending: Mutex<Option<Change>>,
    /// Serializes replays through the mount (see `replay.rs`).
    pub replay_lock: tokio::sync::Mutex<()>,
    deferred: Mutex<Vec<Invalidation>>,
    /// Folder listings in flight.
    listings: InFlight,
    /// Sub-folders queued or being listed ahead of time.
    prefetching: Mutex<HashSet<u64>>,
    prefetch_slots: tokio::sync::Semaphore,
    /// Files with a Proton thumbnail in folders listed ahead of time, by folder.
    deferred_thumbnails: Mutex<HashMap<u64, Vec<NodeInfo>>>,
}

impl Drive {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: DriveClient,
        api: Arc<ApiClient>,
        activity_log: Arc<ActivityLog>,
        root_uid: String,
        content: ContentCache,
        staging: Staging,
        pins: Pins,
        thumbnails: ThumbnailCache,
        mountpoint: crate::mountpoint::Mountpoint,
        verbose: bool,
    ) -> Self {
        Self {
            client: Arc::new(client),
            api,
            activity_log,
            account: Mutex::new(Account::default()),
            failed: AtomicBool::new(false),
            quota_changed: tokio::sync::Notify::new(),
            tree: Mutex::new(Tree::new(root_uid)),
            content: Arc::new(content),
            staging,
            pins,
            dbus: OnceLock::new(),
            thumbnails: Arc::new(thumbnails),
            notifier: Mutex::new(None),
            mounted: AtomicBool::new(false),
            wanted_mounted: tokio::sync::watch::Sender::new(true),
            verbose,
            mountpoint: mountpoint.public,
            mount_target: mountpoint.target,
            pending: Mutex::new(None),
            replay_lock: tokio::sync::Mutex::new(()),
            deferred: Mutex::new(Vec::new()),
            listings: InFlight::default(),
            prefetching: Mutex::new(HashSet::new()),
            prefetch_slots: tokio::sync::Semaphore::new(PREFETCH_PARALLELISM),
            deferred_thumbnails: Mutex::new(HashMap::new()),
        }
    }

    /// Fetches the account address and quota at once, then again after uploads and deletions
    /// (see [`Drive::quota_changed`]), at most once per [`QUOTA_REFRESH_DELAY`].
    pub async fn watch_account(self: Arc<Self>) {
        loop {
            match pdcore::account_info(&self.api).await {
                Ok(info) => {
                    let account = Account { email: info.email, used_space: info.used_space, max_space: info.max_space };
                    *self.account.lock().unwrap_or_else(|e| e.into_inner()) = account;
                }
                Err(e) => eprintln!("cannot fetch the account storage: {e:#}"),
            }
            self.quota_changed.notified().await;
            tokio::time::sleep(QUOTA_REFRESH_DELAY).await;
        }
    }

    /// Storage was probably used or freed: refresh the quota soon.
    pub fn quota_changed(&self) {
        self.quota_changed.notify_one();
    }

    /// Records a rename or a move of a node, under its new path.
    pub fn record_move(&self, parent: u64, new_parent: u64, new_name: &str) {
        let kind = if parent == new_parent { Kind::Rename } else { Kind::Move };
        let folder = self.tree().path_of(new_parent).unwrap_or_default();
        self.activity_log.record(kind, folder.join(new_name).to_string_lossy(), 0);
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

    /// Makes sure the listing of a folder is loaded, and lists its sub-folders in the background
    /// (see [`Drive::prefetch_subfolders`]).
    pub async fn load_children(self: &Arc<Self>, ino: u64) -> Result<(), Errno> {
        self.load_folder(ino, true).await?;
        // Thumbnails of a folder listed ahead of time are fetched when the user gets there.
        let deferred = self.deferred_thumbnails.lock().unwrap_or_else(|e| e.into_inner()).remove(&ino);
        if let Some(nodes) = deferred {
            let drive = self.clone();
            tokio::spawn(async move { drive.ensure_thumbnails(nodes).await });
        }
        self.prefetch_subfolders(ino);
        Ok(())
    }

    /// Lists, in the background, the sub-folders of a folder that the user is looking at, so
    /// that opening one of them (or a file manager counting its items) does not wait for the
    /// network. One level only, at most [`PREFETCH_PARALLELISM`] requests at a time.
    ///
    /// This is not polling: each folder is listed at most once, because the user listed its
    /// parent, and never listed again by this code; later changes come from the Drive events
    /// (`watch.rs`). A folder whose listing fails is only retried when the user looks at its
    /// parent again. Thumbnails are not fetched for these listings (see `load_folder`).
    fn prefetch_subfolders(self: &Arc<Self>, ino: u64) {
        let subfolders = self.tree().unloaded_subfolders(ino);
        for subfolder in subfolders {
            // Already queued by a previous listing of the parent.
            if !self.prefetching.lock().unwrap_or_else(|e| e.into_inner()).insert(subfolder) {
                continue;
            }
            let drive = self.clone();
            tokio::spawn(async move {
                if let Ok(_slot) = drive.prefetch_slots.acquire().await {
                    // An error is logged by `load_folder`; the user's own request will retry.
                    let _ = drive.load_folder(subfolder, false).await;
                }
                drive.prefetching.lock().unwrap_or_else(|e| e.into_inner()).remove(&subfolder);
            });
        }
    }

    /// Loads the listing of a folder unless it is loaded. Requests for a folder already being
    /// listed wait for that listing. Thumbnails of the files are only fetched for a listing the
    /// user asked for: for the others (prefetch) they are kept for later, because fetching them
    /// costs requests and writes into the thumbnail cache for folders nobody may open.
    async fn load_folder(self: &Arc<Self>, ino: u64, wanted_by_user: bool) -> Result<(), Errno> {
        self.listings.run(ino, self.fetch_folder(ino, wanted_by_user)).await
    }

    async fn fetch_folder(self: &Arc<Self>, ino: u64, wanted_by_user: bool) -> Result<(), Errno> {
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
        let with_thumbnails: Vec<NodeInfo> = children.iter().filter(|n| matches!(n.kind, NodeKind::File { has_thumbnail: true, .. })).cloned().collect();
        {
            let mut tree = self.tree();
            // The folder may be gone, or loaded meanwhile by a change event.
            if tree.get(ino).is_none() || tree.is_loaded(ino) {
                return Ok(());
            }
            tree.set_children(ino, children);
        }
        if with_thumbnails.is_empty() {
            return Ok(());
        }
        if wanted_by_user {
            // In the background: the listing must not wait for thumbnails.
            let drive = self.clone();
            tokio::spawn(async move { drive.ensure_thumbnails(with_thumbnails).await });
        } else {
            self.deferred_thumbnails.lock().unwrap_or_else(|e| e.into_inner()).insert(ino, with_thumbnails);
        }
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
                drive.notify_status(ino);
            }
        };
        let label = self.tree().path_of(ino).unwrap_or_default().to_string_lossy().into_owned();
        self.content.open(self.client.clone(), &revision_uid, &label, size, need_all, on_complete).await.map_err(|e| {
            eprintln!("cannot open revision {revision_uid}: {e}");
            Errno::EIO
        })
    }

    /// Sets (or clears) the current FUSE session.
    pub fn set_session(&self, notifier: Option<Notifier>) {
        let mounted = notifier.is_some();
        self.mounted.store(mounted, Ordering::SeqCst);
        *self.notifier.lock().unwrap_or_else(|e| e.into_inner()) = notifier;
        // File managers showing the mount point only see the change if told to reload.
        if let Some(connection) = self.dbus.get() {
            crate::dbus::emit_mount_state_changed(connection, mounted);
        }
    }

    pub fn request_mount(&self) {
        self.wanted_mounted.send_replace(true);
    }

    pub fn request_unmount(&self) {
        self.wanted_mounted.send_replace(false);
    }

    /// Tells the kernel to drop stale cached entries.
    pub fn invalidate(&self, invalidations: Vec<Invalidation>) {
        let notifier = self.notifier.lock().unwrap_or_else(|e| e.into_inner());
        let Some(notifier) = notifier.as_ref() else { return };
        for invalidation in invalidations {
            // ENOENT only means the kernel had nothing cached.
            let _ = match invalidation {
                Invalidation::Entry(parent, name) => notifier.inval_entry(INodeNo(parent), name.as_ref()),
                Invalidation::Inode(ino) => notifier.inval_inode(INodeNo(ino), 0, 0),
            };
        }
    }
}
