// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Local cache of decrypted file contents, one file per revision.
//!
//! Opening a file that is not cached starts a download in the background and returns at once:
//! reads are served as soon as the requested range has been written. The SDK delivers the
//! content in order, so a read waits until the download has reached the end of its range.
//!
//! A download nobody reads any more is cancelled after a grace period, so that a program reading
//! only the beginning of a big file does not fetch all of it. What was received is kept as the
//! "head" of the file, which serves later reads of that beginning (e.g. content type sniffing)
//! without downloading again.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pdsdk::{CancellationToken, DriveClient};

use crate::activity::{ActivityLog, Kind, State};
use tokio::sync::watch;

const IDLE_CANCEL_DELAY: Duration = Duration::from_secs(20);
/// Smallest beginning worth keeping: GIO sniffs at most 16 KiB.
pub const HEAD_LENGTH: u64 = 16 * 1024;

#[derive(Clone, Copy, PartialEq)]
pub enum Progress {
    /// Bytes written so far.
    Running(u64),
    Done,
    Failed,
}

pub struct Transfer {
    progress: watch::Receiver<Progress>,
    readers: AtomicUsize,
    cancelled: AtomicBool,
    cancellation: Option<CancellationToken>,
    runtime: tokio::runtime::Handle,
}

impl Transfer {
    /// Waits until `end` bytes are available (or the download ended). Returns false on failure.
    pub async fn wait_for(&self, end: u64) -> bool {
        let mut progress = self.progress.clone();
        let result = progress.wait_for(|p| !matches!(p, Progress::Running(written) if *written < end)).await;
        matches!(result.as_deref(), Ok(Progress::Running(_) | Progress::Done))
    }

    /// Cancels the download (once). Callable from any thread.
    fn cancel(self: &Arc<Self>) {
        if self.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let transfer = self.clone();
        self.runtime.spawn(async move {
            if let Some(cancellation) = &transfer.cancellation {
                let _ = cancellation.cancel().await;
            }
        });
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            self.runtime.spawn(cancellation.free());
        }
    }
}

/// A download that did not complete: keeps what was received as the head of the file if it is
/// long enough to serve sniffing reads later, otherwise drops it.
fn keep_head(partial: &std::path::Path, head: &std::path::Path, written: u64) {
    if written >= HEAD_LENGTH {
        let _ = std::fs::rename(partial, head);
    } else {
        let _ = std::fs::remove_file(partial);
    }
}

pub enum Opened {
    Complete(File),
    /// The beginning of the content (`len` bytes), without download in progress.
    Head(File, u64),
    Partial(File, Arc<Transfer>),
}

pub struct ContentCache {
    dir: PathBuf,
    transfers: Mutex<HashMap<String, Arc<Transfer>>>,
    runtime: tokio::runtime::Handle,
    activity: Arc<ActivityLog>,
    verbose: bool,
}

impl ContentCache {
    pub fn new(dir: PathBuf, runtime: tokio::runtime::Handle, activity: Arc<ActivityLog>, verbose: bool) -> Self {
        Self { dir, transfers: Mutex::new(HashMap::new()), runtime, activity, verbose }
    }

    /// Revision UIDs are longer than the 255-byte file name limit: name cache files by their hash.
    fn path_for(&self, revision_uid: &str) -> PathBuf {
        use sha2::Digest;
        self.dir.join(sha2::Sha256::digest(revision_uid.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    pub fn is_cached(&self, revision_uid: &str) -> bool {
        self.path_for(revision_uid).exists()
    }

    /// Takes a local file (e.g. just uploaded) as the complete content of a revision.
    pub fn adopt(&self, revision_uid: &str, file: &std::path::Path) {
        if std::fs::rename(file, self.path_for(revision_uid)).is_err() {
            let _ = std::fs::remove_file(file);
        }
    }

    /// Removes the cached content of a revision.
    pub fn evict(&self, revision_uid: &str) {
        let path = self.path_for(revision_uid);
        let _ = std::fs::remove_file(path.with_extension("head"));
        let _ = std::fs::remove_file(path);
    }

    pub fn active_transfers(&self) -> usize {
        self.transfers().len()
    }

    /// Whether the content or its head is available locally, or being downloaded.
    pub fn has_head(&self, revision_uid: &str) -> bool {
        let path = self.path_for(revision_uid);
        path.exists() || path.with_extension("head").exists() || self.transfers().contains_key(revision_uid)
    }

    fn transfers(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Transfer>>> {
        self.transfers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens the content of a revision. Without `need_all`, a cached head is returned as is;
    /// otherwise the download is started (or joined) if the content is not complete.
    /// `on_complete` gets the real size once a download started here finishes; `label` (the path
    /// in the mount) names it in the activity journal.
    pub async fn open(
        self: &Arc<Self>,
        client: Arc<DriveClient>,
        revision_uid: &str,
        label: &str,
        size: u64,
        need_all: bool,
        on_complete: impl FnOnce(u64) + Send + 'static,
    ) -> std::io::Result<Opened> {
        let path = self.path_for(revision_uid);
        if let Ok(file) = File::open(&path) {
            // Recently used content is purged last.
            let _ = file.set_modified(std::time::SystemTime::now());
            return Ok(Opened::Complete(file));
        }
        if let Some(opened) = self.join(revision_uid)? {
            return Ok(opened);
        }
        if !need_all && let Ok(file) = File::open(path.with_extension("head")) {
            let length = file.metadata()?.len();
            return Ok(Opened::Head(file, length));
        }
        match self.start(client, revision_uid, label, size, on_complete).await? {
            Some(transfer) => Ok(Opened::Partial(File::open(path.with_extension("part"))?, transfer)),
            // Started concurrently by someone else.
            None => self.join(revision_uid)?.ok_or_else(|| std::io::Error::other("download vanished")),
        }
    }

    fn join(&self, revision_uid: &str) -> std::io::Result<Option<Opened>> {
        let Some(transfer) = self.transfers().get(revision_uid).cloned() else { return Ok(None) };
        transfer.readers.fetch_add(1, Ordering::SeqCst);
        let file = File::open(self.path_for(revision_uid).with_extension("part"))?;
        Ok(Some(Opened::Partial(file, transfer)))
    }

    /// Starts a download for one reader. Returns `None` if one was started concurrently.
    async fn start(
        self: &Arc<Self>,
        client: Arc<DriveClient>,
        revision_uid: &str,
        label: &str,
        size: u64,
        on_complete: impl FnOnce(u64) + Send + 'static,
    ) -> std::io::Result<Option<Arc<Transfer>>> {
        let path = self.path_for(revision_uid);
        let (partial, head) = (path.with_extension("part"), path.with_extension("head"));

        let cancellation = CancellationToken::new().await.map_err(std::io::Error::other)?;
        let (sender, receiver) = watch::channel(Progress::Running(0));
        let transfer = Arc::new(Transfer {
            progress: receiver,
            readers: AtomicUsize::new(1),
            cancelled: AtomicBool::new(false),
            cancellation: Some(cancellation),
            runtime: self.runtime.clone(),
        });
        let mut writer = {
            let mut transfers = self.transfers();
            if transfers.contains_key(revision_uid) {
                return Ok(None);
            }
            let writer = File::create(&partial)?;
            transfers.insert(revision_uid.to_owned(), transfer.clone());
            writer
        };

        let activity = self.activity.begin(Kind::Download, label, size);
        let (cache, revision_uid, task_transfer) = (self.clone(), revision_uid.to_owned(), transfer.clone());
        self.runtime.spawn(async move {
            if cache.verbose {
                eprintln!("[download] start {revision_uid}");
            }
            let progress = Arc::new(sender);
            let sink_progress = progress.clone();
            let mut written = 0u64;
            let sink = Box::new(move |data: &[u8]| {
                writer.write_all(data)?;
                written += data.len() as u64;
                sink_progress.send_replace(Progress::Running(written));
                Ok(())
            });
            let result = client.download(&revision_uid, sink, task_transfer.cancellation.as_ref()).await;
            let written = match *progress.borrow() {
                Progress::Running(written) => written,
                _ => 0,
            };
            // A download cancelled after its last byte is complete anyway.
            let complete = result.is_ok() || (written > 0 && written >= size);
            let outcome = if complete && std::fs::rename(&partial, &path).is_ok() {
                let _ = std::fs::remove_file(&head);
                on_complete(written);
                Progress::Done
            } else {
                keep_head(&partial, &head, written);
                if let Err(e) = &result
                    && !task_transfer.cancelled.load(Ordering::SeqCst)
                {
                    eprintln!("cannot download revision {revision_uid}: {e}");
                }
                Progress::Failed
            };
            if cache.verbose {
                eprintln!("[download] {} {revision_uid}", if outcome == Progress::Done { "done" } else { "stopped" });
            }
            cache.transfers().remove(&revision_uid);
            match outcome {
                Progress::Done => cache.activity.finish(activity, State::Done),
                // Nobody read it long enough: not worth showing.
                _ if task_transfer.cancelled.load(Ordering::SeqCst) => cache.activity.discard(activity),
                _ => cache.activity.finish(activity, State::Failed),
            }
            progress.send_replace(outcome);
        });
        Ok(Some(transfer))
    }

    /// Deletes the least recently used content until the cache is under `max_bytes`, except the
    /// `kept` revisions. Files still open stay readable (Linux keeps deleted files alive while open).
    pub fn purge(&self, max_bytes: u64, kept: &std::collections::HashSet<String>) {
        let kept: std::collections::HashSet<PathBuf> = kept.iter().map(|r| self.path_for(r)).collect();
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_none_or(|ext| ext == "head"))
            .filter(|e| !kept.contains(&e.path()))
            .filter_map(|e| {
                let metadata = e.metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len(), e.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
        if total <= max_bytes {
            return;
        }
        files.sort();
        let mut removed = 0;
        for (_, size, path) in files {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= size;
                removed += 1;
            }
        }
        if self.verbose {
            eprintln!("[cache] purged {removed} file(s), {} MiB left", total / (1024 * 1024));
        }
    }

    /// A reader of a partial download went away: cancel the download if nobody else reads it.
    pub fn release(&self, transfer: Arc<Transfer>) {
        if transfer.readers.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        self.runtime.spawn(async move {
            tokio::time::sleep(IDLE_CANCEL_DELAY).await;
            let running = matches!(*transfer.progress.borrow(), Progress::Running(_));
            if running && transfer.readers.load(Ordering::SeqCst) == 0 {
                transfer.cancel();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    fn cache(dir: &std::path::Path) -> Arc<ContentCache> {
        Arc::new(ContentCache::new(dir.to_owned(), tokio::runtime::Handle::current(), Arc::new(ActivityLog::default()), false))
    }

    /// Writes `size` bytes in the cache file `path`, last used `age` seconds ago.
    fn put(path: &std::path::Path, size: usize, age: u64) {
        std::fs::write(path, vec![0u8; size]).unwrap();
        File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - Duration::from_secs(age)).unwrap();
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn cache_file_names_are_hashes_of_the_revision_uid() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let long_uid = "volume~".repeat(100);
        let path = cache.path_for(&long_uid);
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 64);
        assert!(name.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(path, cache.path_for(&long_uid));
        assert_ne!(path, cache.path_for("other"));
        // SHA-256 of the empty string, to pin the algorithm.
        assert_eq!(cache.path_for("").file_name().unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[tokio::test]
    async fn adopt_takes_a_file_as_the_content_of_a_revision() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let source = dir.path().join("upload.snapshot");
        std::fs::write(&source, b"content").unwrap();
        assert!(!cache.is_cached("rev"));
        cache.adopt("rev", &source);
        assert!(cache.is_cached("rev") && cache.has_head("rev"));
        assert!(!source.exists());
        assert_eq!(std::fs::read(cache.path_for("rev")).unwrap(), b"content");
    }

    #[tokio::test]
    async fn adopt_replaces_previous_content_and_drops_the_source_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        std::fs::write(cache.path_for("rev"), b"old").unwrap();
        let source = dir.path().join("snapshot");
        std::fs::write(&source, b"new").unwrap();
        cache.adopt("rev", &source);
        assert_eq!(std::fs::read(cache.path_for("rev")).unwrap(), b"new");
        // Rename impossible (target is a non-empty directory): the source is removed anyway.
        let blocked = cache.path_for("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("x"), b"x").unwrap();
        std::fs::write(&source, b"data").unwrap();
        cache.adopt("blocked", &source);
        assert!(!source.exists());
    }

    #[tokio::test]
    async fn evict_removes_content_and_head() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let path = cache.path_for("rev");
        std::fs::write(&path, b"x").unwrap();
        std::fs::write(path.with_extension("head"), b"x").unwrap();
        std::fs::write(cache.path_for("other"), b"x").unwrap();
        cache.evict("rev");
        assert!(!cache.has_head("rev"));
        assert_eq!(names(dir.path()).len(), 1);
        // Evicting what is absent is harmless.
        cache.evict("nothing");
    }

    #[tokio::test]
    async fn has_head_is_true_for_a_head_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        assert!(!cache.has_head("rev"));
        std::fs::write(cache.path_for("rev").with_extension("head"), b"x").unwrap();
        assert!(cache.has_head("rev") && !cache.is_cached("rev"));
    }

    #[tokio::test]
    async fn purge_does_nothing_under_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        put(&cache.path_for("a"), 100, 10);
        put(&cache.path_for("b"), 100, 20);
        cache.purge(200, &HashSet::new());
        assert_eq!(names(dir.path()).len(), 2);
    }

    #[tokio::test]
    async fn purge_removes_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        put(&cache.path_for("old"), 100, 300);
        put(&cache.path_for("mid"), 100, 200);
        put(&cache.path_for("new"), 100, 100);
        cache.purge(150, &HashSet::new());
        // 300 bytes, limit 150: the two oldest go.
        assert!(!cache.is_cached("old") && !cache.is_cached("mid") && cache.is_cached("new"));
        cache.purge(0, &HashSet::new());
        assert!(names(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn purge_keeps_pinned_revisions_even_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        put(&cache.path_for("pinned"), 100, 1000);
        put(&cache.path_for("free"), 100, 10);
        cache.purge(0, &HashSet::from(["pinned".to_owned()]));
        assert!(cache.is_cached("pinned") && !cache.is_cached("free"));
    }

    #[tokio::test]
    async fn purge_ignores_pinned_size_in_the_limit() {
        // Pinned content is not counted: the limit applies to what can be purged.
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        put(&cache.path_for("pinned"), 1000, 1000);
        put(&cache.path_for("free"), 100, 10);
        cache.purge(100, &HashSet::from(["pinned".to_owned()]));
        assert!(cache.is_cached("pinned") && cache.is_cached("free"));
    }

    #[tokio::test]
    async fn purge_removes_heads_but_never_partial_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let (a, b) = (cache.path_for("a"), cache.path_for("b"));
        put(&a.with_extension("head"), 100, 500);
        put(&b.with_extension("part"), 100, 900);
        put(&b, 100, 10);
        cache.purge(100, &HashSet::new());
        assert!(!a.with_extension("head").exists());
        assert!(b.with_extension("part").exists() && b.exists());
        // Unknown extensions are left alone too.
        put(&dir.path().join("note.txt"), 100, 900);
        cache.purge(0, &HashSet::new());
        assert_eq!(names(dir.path()).len(), 2);
    }

    #[tokio::test]
    async fn purge_tolerates_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        cache(&dir.path().join("absent")).purge(0, &HashSet::new());
    }

    #[test]
    fn keep_head_keeps_a_long_enough_beginning() {
        let dir = tempfile::tempdir().unwrap();
        let (partial, head) = (dir.path().join("x.part"), dir.path().join("x.head"));
        std::fs::write(&partial, vec![1u8; HEAD_LENGTH as usize]).unwrap();
        keep_head(&partial, &head, HEAD_LENGTH);
        assert!(!partial.exists());
        assert_eq!(std::fs::metadata(&head).unwrap().len(), HEAD_LENGTH);
    }

    #[test]
    fn keep_head_drops_a_short_beginning() {
        let dir = tempfile::tempdir().unwrap();
        let (partial, head) = (dir.path().join("x.part"), dir.path().join("x.head"));
        std::fs::write(&partial, vec![1u8; HEAD_LENGTH as usize - 1]).unwrap();
        keep_head(&partial, &head, HEAD_LENGTH - 1);
        assert!(!partial.exists() && !head.exists());
    }

    fn transfer(receiver: watch::Receiver<Progress>) -> Transfer {
        Transfer {
            progress: receiver,
            readers: AtomicUsize::new(1),
            cancelled: AtomicBool::new(false),
            cancellation: None,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    #[tokio::test]
    async fn wait_for_returns_when_the_range_is_written() {
        let (sender, receiver) = watch::channel(Progress::Running(0));
        let transfer = Arc::new(transfer(receiver));
        let waiter = tokio::spawn({
            let transfer = transfer.clone();
            async move { transfer.wait_for(10).await }
        });
        tokio::task::yield_now().await;
        sender.send_replace(Progress::Running(5));
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        sender.send_replace(Progress::Running(10));
        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn wait_for_follows_the_end_of_the_download() {
        let (sender, receiver) = watch::channel(Progress::Done);
        assert!(transfer(receiver).wait_for(u64::MAX).await);
        sender.send_replace(Progress::Failed);
        let (_sender, receiver) = watch::channel(Progress::Failed);
        assert!(!transfer(receiver).wait_for(0).await);
    }

    #[tokio::test]
    async fn wait_for_fails_if_the_download_task_vanishes() {
        let (sender, receiver) = watch::channel(Progress::Running(0));
        let transfer = transfer(receiver);
        drop(sender);
        assert!(!transfer.wait_for(1).await);
    }
}
