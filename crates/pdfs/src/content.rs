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
    verbose: bool,
}

impl ContentCache {
    pub fn new(dir: PathBuf, runtime: tokio::runtime::Handle, verbose: bool) -> Self {
        Self { dir, transfers: Mutex::new(HashMap::new()), runtime, verbose }
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
    /// `on_complete` gets the real size once a download started here finishes.
    pub async fn open(
        self: &Arc<Self>,
        client: Arc<DriveClient>,
        revision_uid: &str,
        size: u64,
        need_all: bool,
        on_complete: impl FnOnce(u64) + Send + 'static,
    ) -> std::io::Result<Opened> {
        let path = self.path_for(revision_uid);
        if let Ok(file) = File::open(&path) {
            return Ok(Opened::Complete(file));
        }
        if let Some(opened) = self.join(revision_uid)? {
            return Ok(opened);
        }
        if !need_all
            && let Ok(file) = File::open(path.with_extension("head")) {
                let length = file.metadata()?.len();
                return Ok(Opened::Head(file, length));
            }
        match self.start(client, revision_uid, size, on_complete).await? {
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
                if written >= HEAD_LENGTH {
                    // Keep what was received to serve sniffing reads later.
                    let _ = std::fs::rename(&partial, &head);
                } else {
                    let _ = std::fs::remove_file(&partial);
                }
                if let Err(e) = &result
                    && !task_transfer.cancelled.load(Ordering::SeqCst) {
                        eprintln!("cannot download revision {revision_uid}: {e}");
                    }
                Progress::Failed
            };
            if cache.verbose {
                eprintln!("[download] {} {revision_uid}", if outcome == Progress::Done { "done" } else { "stopped" });
            }
            cache.transfers().remove(&revision_uid);
            progress.send_replace(outcome);
        });
        Ok(Some(transfer))
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
