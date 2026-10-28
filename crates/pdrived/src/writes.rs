//! Write support.
//!
//! Writes go to a local staging file; the file is uploaded (as a new file, or as a new revision
//! of the existing one) once the last writer closed it and it stayed untouched for a short
//! delay. The delay lets the usual "atomic save" of editors settle: they write a temporary file
//! and rename it over the original, in which case the temporary content is uploaded as a new
//! revision of the original, keeping its history. Temporary and lock files of editors are never
//! uploaded. Folder creation, renames, moves and deletions (to the trash) are applied to Proton
//! Drive at once.
//!
//! Each staging file has a small journal next to it (where and what to upload), so that uploads
//! interrupted by a stop or a crash are resumed at the next start (see [`Drive::recover_uploads`]).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::Errno;
use pdsdk::proto::{self, NodeMoveItem};
use pdsdk::SdkError;
use serde::{Deserialize, Serialize};

use crate::activity::{Kind, State};
use crate::content::Opened;
use crate::drive::Drive;
use crate::tree::{is_local, EntryKind, NodeInfo};

/// Quiet period after the last close before uploading.
const UPLOAD_DELAY: Duration = Duration::from_secs(3);
const UPLOAD_RETRY_DELAY: Duration = Duration::from_secs(30);
const UPLOAD_ATTEMPTS: u32 = 5;

/// A file being written locally.
struct Staged {
    /// `<key>.data` holds the content, `<key>.json` the [`Journal`].
    key: String,
    path: PathBuf,
    writers: usize,
    dirty: bool,
    /// Bumped on every change, to tell whether an upload is still current.
    generation: u64,
}

pub struct Staging {
    dir: PathBuf,
    /// Distinguishes this run's staging files from those left by a previous one.
    session: String,
    files: Mutex<HashMap<u64, Staged>>,
}

impl Staging {
    pub fn new(dir: PathBuf) -> Self {
        let session = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().to_string();
        Self { dir, session, files: Mutex::new(HashMap::new()) }
    }

    fn files(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Staged>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn new_staged(&self, ino: u64) -> Staged {
        let key = format!("{}-{ino}", self.session);
        Staged { path: self.dir.join(format!("{key}.data")), key, writers: 0, dirty: false, generation: 0 }
    }

    fn journal_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    fn discard(&self, staged: &Staged) {
        let _ = std::fs::remove_file(&staged.path);
        let _ = std::fs::remove_file(self.journal_path(&staged.key));
    }
}

/// What to do with a staging file if the upload has to be resumed by a later run.
#[derive(Serialize, Deserialize)]
struct Journal {
    /// Remote file and revision the content is based on; none for a new file.
    file_uid: Option<String>,
    base_revision_uid: Option<String>,
    parent_uid: String,
    name: String,
    mtime: u64,
}

/// Editor temporary and lock files: kept locally, never uploaded.
pub fn is_temporary(name: &str) -> bool {
    name.starts_with(".~lock.")
        || name.starts_with(".goutputstream-")
        || name.starts_with(".#")
        || name.ends_with('~')
        || [".swp", ".swo", ".swx", ".kate-swp", ".part", ".crdownload"].iter().any(|ext| name.ends_with(ext))
}

fn errno_of(error: &SdkError) -> Errno {
    match error {
        SdkError::Sdk { message, .. } if message.to_lowercase().contains("exist") => Errno::EEXIST,
        _ => Errno::EIO,
    }
}

fn timestamp(time: SystemTime) -> proto::Timestamp {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    proto::Timestamp { seconds: since_epoch.as_secs() as i64, nanos: since_epoch.subsec_nanos() as i32 }
}

impl Drive {
    /// Number of files written locally and not uploaded yet.
    pub fn staging_count(&self) -> usize {
        self.staging.files().len()
    }

    /// Local path of the content of a file being written, if any.
    pub fn staged_path(&self, ino: u64) -> Option<PathBuf> {
        self.staging.files().get(&ino).map(|s| s.path.clone())
    }

    /// Makes sure a file has a staging copy, with its current content unless `empty`.
    async fn stage(self: &Arc<Self>, ino: u64, empty: bool) -> Result<PathBuf, Errno> {
        if let Some(path) = self.staged_path(ino) {
            return Ok(path);
        }
        let has_content = matches!(&self.tree().get(ino).ok_or(Errno::ENOENT)?.kind, EntryKind::File { revision_uid, .. } if !revision_uid.is_empty());
        let new_staged = self.staging.new_staged(ino);
        let mut staged = File::create(&new_staged.path)?;
        if has_content && !empty {
            let mut source = match self.open_content(ino, true).await? {
                Opened::Complete(file) => file,
                Opened::Partial(file, transfer) => {
                    let complete = transfer.wait_for(u64::MAX).await;
                    self.content.release(transfer);
                    if !complete {
                        return Err(Errno::EIO);
                    }
                    file
                }
                Opened::Head(..) => return Err(Errno::EIO),
            };
            source.rewind()?;
            std::io::copy(&mut source, &mut staged)?;
        }
        let path = {
            let mut files = self.staging.files();
            // Staged concurrently: keep the first one.
            files.entry(ino).or_insert(new_staged).path.clone()
        };
        self.write_journal(ino);
        Ok(path)
    }

    /// Records where and what to upload, in case the upload has to be resumed by a later run.
    fn write_journal(&self, ino: u64) {
        let journal = {
            let tree = self.tree();
            let Some(entry) = tree.get(ino) else { return };
            let EntryKind::File { revision_uid, .. } = &entry.kind else { return };
            let Some(parent) = tree.get(entry.parent) else { return };
            Journal {
                file_uid: (!is_local(&entry.uid)).then(|| entry.uid.clone()),
                base_revision_uid: (!revision_uid.is_empty()).then(|| revision_uid.clone()),
                parent_uid: parent.uid.clone(),
                name: entry.name.clone(),
                mtime: entry.mtime.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            }
        };
        let Some(key) = self.staging.files().get(&ino).map(|s| s.key.clone()) else { return };
        let path = self.staging.journal_path(&key);
        let temporary = path.with_extension("json.tmp");
        let result = serde_json::to_vec(&journal)
            .map_err(std::io::Error::other)
            .and_then(|data| std::fs::write(&temporary, data))
            .and_then(|()| std::fs::rename(&temporary, &path));
        if let Err(e) = result {
            eprintln!("cannot write the upload journal of {}: {e}", journal.name);
        }
    }

    /// Opens a file for writing (creating its staging copy).
    pub async fn open_write(self: &Arc<Self>, ino: u64, truncate: bool) -> Result<File, Errno> {
        let path = self.stage(ino, truncate).await?;
        self.notify_status(ino);
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
        let mut files = self.staging.files();
        let staged = files.get_mut(&ino).ok_or(Errno::EIO)?;
        staged.writers += 1;
        if truncate {
            file.set_len(0)?;
            staged.dirty = true;
            staged.generation += 1;
            drop(files);
            self.touch_local(ino, 0);
        }
        Ok(file)
    }

    /// Records a write that left the file `size` bytes long.
    pub fn written(&self, ino: u64, size: u64) {
        if let Some(staged) = self.staging.files().get_mut(&ino) {
            staged.dirty = true;
            staged.generation += 1;
        }
        self.touch_local(ino, size);
    }

    fn touch_local(&self, ino: u64, size: u64) {
        let mut tree = self.tree();
        tree.set_file_size(ino, size);
        tree.set_mtime(ino, SystemTime::now());
    }

    /// A writer closed the file: upload it once things settle.
    pub fn release_write(self: &Arc<Self>, ino: u64) {
        let upload = {
            let mut files = self.staging.files();
            let Some(staged) = files.get_mut(&ino) else { return };
            staged.writers = staged.writers.saturating_sub(1);
            staged.writers == 0 && staged.dirty
        };
        if upload {
            self.schedule_upload(ino);
        }
    }

    /// Truncates (or extends) a file.
    pub async fn truncate(self: &Arc<Self>, ino: u64, size: u64) -> Result<(), Errno> {
        let path = self.stage(ino, size == 0).await?;
        std::fs::OpenOptions::new().write(true).open(&path)?.set_len(size)?;
        let idle = {
            let mut files = self.staging.files();
            let staged = files.get_mut(&ino).ok_or(Errno::EIO)?;
            staged.dirty = true;
            staged.generation += 1;
            staged.writers == 0
        };
        self.touch_local(ino, size);
        if idle {
            self.schedule_upload(ino);
        }
        Ok(())
    }

    fn schedule_upload(self: &Arc<Self>, ino: u64) {
        self.write_journal(ino);
        let drive = self.clone();
        tokio::spawn(async move {
            let Some(generation) = drive.staging.files().get(&ino).map(|s| s.generation) else { return };
            tokio::time::sleep(UPLOAD_DELAY).await;
            // Changed meanwhile: the latest change schedules its own upload.
            let current = drive.staging.files().get(&ino).is_some_and(|s| s.generation == generation && s.writers == 0 && s.dirty);
            if current {
                drive.upload(ino).await;
            }
        });
    }

    async fn upload(self: &Arc<Self>, ino: u64) {
        for attempt in 1..=UPLOAD_ATTEMPTS {
            match self.try_upload(ino).await {
                Ok(()) => {
                    self.failed.store(false, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
                Err(e) => {
                    eprintln!("upload failed (attempt {attempt}/{UPLOAD_ATTEMPTS}): {e}");
                    tokio::time::sleep(UPLOAD_RETRY_DELAY).await;
                }
            }
        }
        eprintln!("giving up uploading inode {ino}: its content stays local until it is written again");
        self.failed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    async fn try_upload(self: &Arc<Self>, ino: u64) -> Result<(), String> {
        // Where and what to upload, as of now.
        let (uid, revision_uid, parent_uid, name, mtime) = {
            let tree = self.tree();
            let Some(entry) = tree.get(ino) else { return Ok(()) };
            let EntryKind::File { revision_uid, .. } = &entry.kind else { return Ok(()) };
            let parent_uid = tree.get(entry.parent).map(|p| p.uid.clone()).ok_or("parent folder is gone")?;
            (entry.uid.clone(), revision_uid.clone(), parent_uid, entry.name.clone(), entry.mtime)
        };
        if is_temporary(&name) {
            return Ok(());
        }
        // Snapshot the staging file so that writes during the upload do not corrupt it.
        let (snapshot, generation) = {
            let files = self.staging.files();
            let Some(staged) = files.get(&ino) else { return Ok(()) };
            let snapshot = self.staging.dir.join(format!("{}.upload.{}", staged.key, staged.generation));
            std::fs::copy(&staged.path, &snapshot).map_err(|e| e.to_string())?;
            (snapshot, staged.generation)
        };
        let size = std::fs::metadata(&snapshot).map_err(|e| e.to_string())?.len();
        if self.verbose {
            eprintln!("[upload] {} {name} ({size} bytes)", if is_local(&uid) { "new file" } else { "new revision of" });
        }
        let activity = {
            let path = self.tree().path_of(ino).unwrap_or_else(|| name.clone().into());
            self.activity_log.begin(Kind::Upload, path.to_string_lossy(), size)
        };
        let result = if is_local(&uid) {
            let media_type = media_type(&name, &snapshot);
            self.client.upload_new_file(&parent_uid, &name, &media_type, &snapshot, size, Some(timestamp(mtime))).await
        } else {
            self.client.upload_revision(&revision_uid, &snapshot, size, Some(timestamp(mtime))).await
        };
        let uploaded = match result {
            Ok(uploaded) => uploaded,
            Err(e) => {
                let _ = std::fs::remove_file(&snapshot);
                self.activity_log.finish(activity, State::Failed);
                return Err(e.to_string());
            }
        };
        self.activity_log.finish(activity, State::Done);
        self.quota_changed();

        self.content.adopt(&uploaded.revision_uid, &snapshot);
        // The file may have been renamed, moved or deleted while it was uploaded under the name
        // and in the folder it had when the upload started: catch up.
        let now = {
            let mut tree = self.tree();
            tree.set_uploaded(ino, uploaded.node_uid.clone(), uploaded.revision_uid.clone());
            tree.get(ino).map(|e| (e.name.clone(), tree.get(e.parent).map(|p| p.uid.clone()).unwrap_or_default()))
        };
        match now {
            None => {
                if let Err(e) = self.client.trash(&uploaded.node_uid).await {
                    eprintln!("cannot trash {name}, deleted during its upload: {e}");
                }
            }
            Some((current_name, current_parent_uid)) if current_name != name || current_parent_uid != parent_uid => {
                let item = NodeMoveItem {
                    node_uid: uploaded.node_uid.clone(),
                    current_parent_uid: parent_uid.clone(),
                    current_name: name.clone(),
                    target_name: current_name.clone(),
                };
                if let Err(e) = self.client.move_node(item, &current_parent_uid).await {
                    eprintln!("cannot rename {name} to {current_name} after its upload: {e}");
                }
            }
            Some(_) => {}
        }
        let mut files = self.staging.files();
        if let Some(staged) = files.get_mut(&ino) {
            if staged.generation == generation && staged.writers == 0 {
                self.staging.discard(staged);
                files.remove(&ino);
            } else {
                // Written again during the upload: that change has its own upload.
                staged.dirty = staged.generation != generation;
            }
        }
        self.notify_status(ino);
        if self.verbose {
            eprintln!("[upload] done {name}");
        }
        Ok(())
    }

    /// Creates an empty local file (uploaded when written and closed).
    pub fn create_file(&self, parent: u64, name: &str) -> Result<(u64, File), Errno> {
        let ino = {
            let mut tree = self.tree();
            if tree.child(parent, name).is_some() {
                return Err(Errno::EEXIST);
            }
            tree.insert_local_file(parent, name.to_owned(), SystemTime::now())
        };
        let staged = Staged { writers: 1, dirty: true, generation: 1, ..self.staging.new_staged(ino) };
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&staged.path)?;
        // A new file is "dirty" even if nothing is written: an empty file is uploaded.
        self.staging.files().insert(ino, staged);
        self.write_journal(ino);
        Ok((ino, file))
    }

    pub async fn make_folder(self: &Arc<Self>, parent: u64, name: &str) -> Result<u64, Errno> {
        let parent_uid = self.remote_folder_uid(parent)?;
        let folder = self.client.create_folder(&parent_uid, name).await.map_err(|e| {
            eprintln!("cannot create folder {name}: {e}");
            errno_of(&e)
        })?;
        let uid = folder.uid.clone();
        let info = NodeInfo::from_node(proto::Node { kind: Some(proto::node::Kind::Folder(folder)) }).ok_or(Errno::EIO)?;
        let mut tree = self.tree();
        tree.apply_update(parent, info);
        let ino = tree.ino_of(&uid).ok_or(Errno::EIO)?;
        // A new folder is empty: no need to list it.
        tree.set_children(ino, Vec::new());
        Ok(ino)
    }

    fn remote_folder_uid(&self, ino: u64) -> Result<String, Errno> {
        let tree = self.tree();
        let entry = tree.get(ino).ok_or(Errno::ENOENT)?;
        match entry.kind {
            EntryKind::Folder { .. } => Ok(entry.uid.clone()),
            EntryKind::File { .. } => Err(Errno::ENOTDIR),
        }
    }

    /// Removes a file or an empty folder: remote nodes go to the Proton Drive trash.
    pub async fn remove(self: &Arc<Self>, parent: u64, name: &str, folder: bool) -> Result<(), Errno> {
        if folder {
            let ino = self.tree().child(parent, name).ok_or(Errno::ENOENT)?;
            self.load_children(ino).await?;
        }
        let (ino, uid) = {
            let tree = self.tree();
            let ino = tree.child(parent, name).ok_or(Errno::ENOENT)?;
            let entry = tree.get(ino).ok_or(Errno::ENOENT)?;
            match (&entry.kind, folder) {
                (EntryKind::Folder { .. }, false) => return Err(Errno::EISDIR),
                (EntryKind::File { .. }, true) => return Err(Errno::ENOTDIR),
                (EntryKind::Folder { children }, true) if children.as_ref().is_some_and(|c| !c.is_empty()) => {
                    return Err(Errno::ENOTEMPTY);
                }
                _ => {}
            }
            (ino, entry.uid.clone())
        };
        if !is_local(&uid) {
            let path = self.tree().path_of(ino).unwrap_or_else(|| name.into());
            self.client.trash(&uid).await.map_err(|e| {
                eprintln!("cannot move {path:?} to the trash: {e}");
                errno_of(&e)
            })?;
            self.activity_log.record(Kind::Trash, path.to_string_lossy(), 0);
            self.quota_changed();
        }
        if let Some(staged) = self.staging.files().remove(&ino) {
            self.staging.discard(&staged);
        }
        self.tree().remove_ino(ino);
        Ok(())
    }

    /// Renames and/or moves a node, replacing the target if it exists.
    pub async fn rename(self: &Arc<Self>, parent: u64, name: &str, new_parent: u64, new_name: &str, no_replace: bool) -> Result<(), Errno> {
        let new_parent_uid = self.remote_folder_uid(new_parent)?;
        let (source, source_uid, source_is_file, target) = {
            let tree = self.tree();
            let source = tree.child(parent, name).ok_or(Errno::ENOENT)?;
            let entry = tree.get(source).ok_or(Errno::ENOENT)?;
            let target = tree.child(new_parent, new_name).filter(|t| *t != source);
            (source, entry.uid.clone(), matches!(entry.kind, EntryKind::File { .. }), target)
        };
        if let Some(target) = target {
            if no_replace {
                return Err(Errno::EEXIST);
            }
            let target_is_file = matches!(self.tree().get(target).map(|e| &e.kind), Some(EntryKind::File { .. }));
            match (source_is_file, target_is_file) {
                (true, false) => return Err(Errno::EISDIR),
                (false, true) => return Err(Errno::ENOTDIR),
                // Atomic save: the new content becomes a new revision of the target.
                (true, true) if is_local(&source_uid) => return self.replace_content(source, target).await,
                _ => self.remove(new_parent, new_name, !target_is_file).await?,
            }
        }

        if !is_local(&source_uid) {
            let current_parent_uid = self.tree().get(parent).map(|p| p.uid.clone()).ok_or(Errno::ENOENT)?;
            let item = NodeMoveItem {
                node_uid: source_uid,
                current_parent_uid,
                current_name: name.to_owned(),
                target_name: new_name.to_owned(),
            };
            self.client.move_node(item, &new_parent_uid).await.map_err(|e| {
                eprintln!("cannot move {name} to {new_name}: {e}");
                errno_of(&e)
            })?;
            self.record_move(parent, new_parent, new_name);
        }
        self.tree().move_entry(source, new_parent, new_name.to_owned());
        // A local file renamed from a temporary name must now be uploaded.
        if is_local(&self.tree().get(source).map(|e| e.uid.clone()).unwrap_or_default()) && is_temporary(name) && !is_temporary(new_name) {
            self.schedule_upload(source);
        }
        Ok(())
    }

    /// Moves the staged content of a local file into an existing file, then drops the local one.
    async fn replace_content(self: &Arc<Self>, source: u64, target: u64) -> Result<(), Errno> {
        let staged = self.staging.files().remove(&source).ok_or(Errno::EIO)?;
        let _ = std::fs::remove_file(self.staging.journal_path(&staged.key));
        let size = std::fs::metadata(&staged.path)?.len();
        {
            let mut files = self.staging.files();
            let (generation, writers) = match files.remove(&target) {
                Some(previous) => {
                    self.staging.discard(&previous);
                    (previous.generation + 1, previous.writers)
                }
                None => (1, 0),
            };
            files.insert(target, Staged { writers, dirty: true, generation, ..staged });
        }
        let mtime = self.tree().get(source).map_or_else(SystemTime::now, |e| e.mtime);
        {
            let mut tree = self.tree();
            tree.remove_ino(source);
            tree.set_file_size(target, size);
            tree.set_mtime(target, mtime);
        }
        self.schedule_upload(target);
        Ok(())
    }
}

impl Drive {
    /// Resumes the uploads that a previous run could not finish. A file changed remotely since
    /// (or whose name was taken) is uploaded as a conflict copy next to it: nothing is overwritten.
    pub async fn recover_uploads(self: &Arc<Self>) {
        let Ok(entries) = std::fs::read_dir(&self.staging.dir) else { return };
        let mut journals: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        journals.sort();
        for journal_path in journals {
            let Some(key) = journal_path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
            if key.starts_with(&format!("{}-", self.staging.session)) {
                continue;
            }
            let data = self.staging.dir.join(format!("{key}.data"));
            let journal: Journal = match std::fs::read(&journal_path).ok().and_then(|j| serde_json::from_slice::<Journal>(&j).ok()) {
                Some(journal) if data.exists() && !is_temporary(&journal.name) => journal,
                _ => {
                    let _ = std::fs::remove_file(&journal_path);
                    let _ = std::fs::remove_file(&data);
                    continue;
                }
            };
            match self.resume_upload(&journal, &data).await {
                Ok(name) => {
                    eprintln!("[recovery] uploaded {name}, left over by the previous run");
                    self.activity_log.record(Kind::Upload, name, 0);
                    self.quota_changed();
                    let _ = std::fs::remove_file(&journal_path);
                }
                // Kept for the next run.
                Err(e) => eprintln!("[recovery] cannot upload {} yet: {e}", journal.name),
            }
        }
        // Content left without a journal comes from files that were never closed: keep it aside.
        if let Ok(entries) = std::fs::read_dir(&self.staging.dir) {
            for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
                if name.starts_with(&format!("{}-", self.staging.session)) {
                    continue;
                }
                if name.contains(".upload.") || name.ends_with(".tmp") {
                    let _ = std::fs::remove_file(&path);
                } else if name.ends_with(".data") && !path.with_extension("json").exists() {
                    eprintln!("[recovery] unsaved content without destination kept in {}", path.display());
                }
            }
        }
    }

    /// Uploads a journaled staging file; returns the name it got.
    async fn resume_upload(self: &Arc<Self>, journal: &Journal, data: &std::path::Path) -> Result<String, SdkError> {
        let size = std::fs::metadata(data).map_err(|e| SdkError::Protocol(e.to_string()))?.len();
        let mtime = Some(proto::Timestamp { seconds: journal.mtime as i64, nanos: 0 });
        let attempt = match (&journal.file_uid, &journal.base_revision_uid) {
            (Some(_), Some(base)) => self.client.upload_revision(base, data, size, mtime).await,
            _ => {
                let media_type = media_type(&journal.name, data);
                self.client.upload_new_file(&journal.parent_uid, &journal.name, &media_type, data, size, mtime).await
            }
        };
        let (uploaded, name) = match attempt {
            Ok(uploaded) => (uploaded, journal.name.clone()),
            Err(e) if is_transient(&e) => return Err(e),
            Err(e) => {
                let name = conflict_name(&journal.name);
                eprintln!("[recovery] {} cannot be uploaded as is ({e}): uploading it as {name}", journal.name);
                let media_type = media_type(&journal.name, data);
                let uploaded = self.client.upload_new_file(&journal.parent_uid, &name, &media_type, data, size, mtime).await?;
                (uploaded, name)
            }
        };
        self.content.adopt(&uploaded.revision_uid, data);
        Ok(name)
    }
}

fn is_transient(error: &SdkError) -> bool {
    matches!(error, SdkError::Sdk { domain: proto::ErrorDomain::Network | proto::ErrorDomain::Transport, .. })
}

/// "report.odt" → "report (conflict 2026-10-27 22.15).odt"
fn conflict_name(name: &str) -> String {
    let stamp = glib::DateTime::now_local().and_then(|d| d.format("%Y-%m-%d %H.%M")).map(|s| s.to_string()).unwrap_or_default();
    match name.rfind('.').filter(|&i| i > 0) {
        Some(dot) => format!("{} (conflict {stamp}){}", &name[..dot], &name[dot..]),
        None => format!("{name} (conflict {stamp})"),
    }
}

/// MIME type from the name and the first bytes, as GIO guesses it.
fn media_type(name: &str, path: &std::path::Path) -> String {
    let mut head = vec![0u8; 4096];
    let length = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    head.truncate(length);
    let (content_type, _) = gio::content_type_guess(Some(name), Some(&head[..]));
    gio::content_type_get_mime_type(&content_type).map_or_else(|| "application/octet-stream".to_owned(), |m| m.to_string())
}

