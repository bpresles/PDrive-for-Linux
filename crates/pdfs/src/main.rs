//! Mounts "My files" read-only with FUSE (writes from other processes are refused).
//!
//!     pdfs MOUNTPOINT
//!
//! Folders are listed on demand (with their Proton thumbnails written to the desktop thumbnail
//! cache), files are downloaded and decrypted on first open into
//! `$XDG_CACHE_HOME/nautilus-drive-poc/content` and readable while downloading, and remote
//! changes are applied from Drive events. Stop with Ctrl+C (or `fusermount3 -u MOUNTPOINT`). Set PDPOC_VERBOSE=1 for logs.

mod content;
mod drive;
mod fs;
mod replay;
mod thumbnails;
mod tree;
mod watch;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use fuser::{Config, MountOption};
use tokio::signal::unix::{signal, SignalKind};

use content::ContentCache;
use drive::Drive;
use thumbnails::ThumbnailCache;
use fs::DriveFs;

#[tokio::main]
async fn main() -> Result<()> {
    let Some(mountpoint) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: pdfs MOUNTPOINT");
        std::process::exit(2);
    };
    let verbose = std::env::var_os("PDPOC_VERBOSE").is_some();

    let cache_home = cache_home()?;
    let content_dir = cache_home.join("nautilus-drive-poc").join("content");
    std::fs::create_dir_all(&content_dir).with_context(|| format!("cannot create {}", content_dir.display()))?;

    let client = pdcore::connect(verbose).await?;
    let root = client.my_files_folder().await?;
    let scope = root.tree_event_scope_id.clone();
    let mountpoint = mountpoint.canonicalize().with_context(|| format!("cannot access {}", mountpoint.display()))?;
    let content = ContentCache::new(content_dir, tokio::runtime::Handle::current(), verbose);
    let drive = Arc::new(Drive::new(client, root.uid, content, ThumbnailCache::new(&cache_home), mountpoint.clone(), verbose));

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("drive-poc".into()),
        MountOption::Subtype("pdfs".into()),
        MountOption::NoAtime,
        MountOption::DefaultPermissions,
    ];
    let mut session = fuser::Session::new(DriveFs::new(drive.clone(), tokio::runtime::Handle::current()), &mountpoint, &config)
        .with_context(|| format!("cannot mount on {}", mountpoint.display()))?;
    let _ = drive.notifier.set(session.notifier());
    let mut unmounter = session.unmount_callable();
    // The session loop returns when the file system is unmounted, whoever unmounts it.
    let mut session_loop = tokio::task::spawn_blocking(move || session.run());
    println!("Mounted on {} (read-only). Press Ctrl+C to unmount.", mountpoint.display());

    let watcher = tokio::spawn(watch::run(drive.clone(), scope));

    let mut terminate = signal(SignalKind::terminate())?;
    let ended = tokio::select! {
        _ = tokio::signal::ctrl_c() => None,
        _ = terminate.recv() => None,
        result = &mut session_loop => Some(result),
    };
    watcher.abort();
    let result = match ended {
        Some(result) => result,
        None => {
            unmounter.unmount().with_context(|| {
                format!("unmount failed: the mount may be busy, close it and run `fusermount3 -u {}`", mountpoint.display())
            })?;
            session_loop.await
        }
    };
    result?.context("FUSE session failed")?;
    println!("Unmounted.");
    Ok(())
}

fn cache_home() -> Result<PathBuf> {
    Ok(match std::env::var_os("XDG_CACHE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".cache"),
    })
}
