//! Write support.
//!
//! Writes go to a local staging file; the file is uploaded (as a new file, or as a new revision
//! of the existing one) once the last writer closed it and it stayed untouched for a short
//! delay. The delay lets the usual "atomic save" of editors settle: they write a temporary file
//! and rename it over the original, in which case the temporary content is uploaded as a new
//! revision of the original, keeping its history. Temporary and lock files of editors are never
//! uploaded. Folder creation, renames, moves and deletions (to the trash) are applied to Proton
//! Drive at once.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::Errno;
use pdsdk::proto::{self, NodeMoveItem};
use pdsdk::SdkError;

use crate::content::Opened;
use crate::drive::Drive;
use crate::tree::{is_local, EntryKind, NodeInfo};

/// Quiet period after the last close before uploading.
const UPLOAD_DELAY: Duration = Duration::from_secs(3);
const UPLOAD_RETRY_DELAY: Duration = Duration::from_secs(30);
const UPLOAD_ATTEMPTS: u32 = 5;

/// A file being written locally.
struct Staged {
    path: PathBuf,
    writers: usize,
    dirty: bool,
    /// Bumped on every change, to tell whether an upload is still current.
    generation: u64,
}

pub struct Staging {
    dir: PathBuf,
    files: Mutex<HashMap<u64, Staged>>,
}

impl Staging {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, files: Mutex::new(HashMap::new()) }
    }

    fn files(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Staged>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }
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
        let path = self.staging.dir.join(format!("{ino}"));
        let mut staged = File::create(&path)?;
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
        let mut files = self.staging.files();
        // Staged concurrently: keep the first one.
        let entry = files.entry(ino).or_insert(Staged { path: path.clone(), writers: 0, dirty: false, generation: 0 });
        Ok(entry.path.clone())
    }

    /// Opens a file for writing (creating its staging copy).
    pub async fn open_write(self: &Arc<Self>, ino: u64, truncate: bool) -> Result<File, Errno> {
        let path = self.stage(ino, truncate).await?;
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
                Ok(()) => return,
                Err(e) => {
                    eprintln!("upload failed (attempt {attempt}/{UPLOAD_ATTEMPTS}): {e}");
                    tokio::time::sleep(UPLOAD_RETRY_DELAY).await;
                }
            }
        }
        eprintln!("giving up uploading inode {ino}: its content stays local until it is written again");
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
            let snapshot = self.staging.dir.join(format!("{ino}.upload.{}", staged.generation));
            std::fs::copy(&staged.path, &snapshot).map_err(|e| e.to_string())?;
            (snapshot, staged.generation)
        };
        let size = std::fs::metadata(&snapshot).map_err(|e| e.to_string())?.len();
        if self.verbose {
            eprintln!("[upload] {} {name} ({size} bytes)", if is_local(&uid) { "new file" } else { "new revision of" });
        }
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
                return Err(e.to_string());
            }
        };

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
                let _ = std::fs::remove_file(&staged.path);
                files.remove(&ino);
            } else {
                // Written again during the upload: that change has its own upload.
                staged.dirty = staged.generation != generation;
            }
        }
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
        let path = self.staging.dir.join(format!("{ino}"));
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
        // A new file is "dirty" even if nothing is written: an empty file is uploaded.
        self.staging.files().insert(ino, Staged { path, writers: 1, dirty: true, generation: 1 });
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
            self.client.trash(&uid).await.map_err(|e| {
                eprintln!("cannot move {name} to the trash: {e}");
                errno_of(&e)
            })?;
        }
        if let Some(staged) = self.staging.files().remove(&ino) {
            let _ = std::fs::remove_file(staged.path);
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
        let path = self.staging.dir.join(format!("{target}"));
        std::fs::rename(&staged.path, &path)?;
        let size = std::fs::metadata(&path)?.len();
        {
            let mut files = self.staging.files();
            let generation = files.get(&target).map_or(1, |s| s.generation + 1);
            let writers = files.get(&target).map_or(0, |s| s.writers);
            files.insert(target, Staged { path, writers, dirty: true, generation });
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

/// MIME type from the name and the first bytes, as GIO guesses it.
fn media_type(name: &str, path: &std::path::Path) -> String {
    let mut head = vec![0u8; 4096];
    let length = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    head.truncate(length);
    let (content_type, _) = gio::content_type_guess(Some(name), Some(&head[..]));
    gio::content_type_get_mime_type(&content_type).map_or_else(|| "application/octet-stream".to_owned(), |m| m.to_string())
}

