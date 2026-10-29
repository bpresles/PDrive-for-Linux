// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Mounts "My files" with FUSE.
//!
//!     pdrived [MOUNTPOINT]
//!
//! The mount point defaults to the one of the settings (see `config.rs`). Folders are listed on
//! demand (with their Proton thumbnails written to the desktop thumbnail cache), files are
//! downloaded and decrypted on first open into `$XDG_CACHE_HOME/pdrive/content` and
//! readable while downloading, and remote changes are applied from Drive events. Writes are
//! staged locally and uploaded once the file is closed (see `writes.rs`); uploads interrupted by
//! a stop are resumed at the next start. Normally started and supervised by `pdrive-app`; it can
//! also be run alone (debugging). Stop with Ctrl+C or SIGTERM; when the mount is ejected
//! (`fusermount3 -u MOUNTPOINT`, or from the file manager), the daemon waits for a mount request.
//! Set PDRIVE_VERBOSE=1 for logs.

mod activity;
mod config;
mod content;
mod dbus;
mod drive;
mod fs;
mod mountpoint;
mod pins;
mod replay;
mod status;
mod thumbnails;
mod tree;
mod watch;
mod writes;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fuser::{Config, MountOption};
use tokio::signal::unix::{SignalKind, signal};

use activity::ActivityLog;
use config::{APP_DIR, Settings};
use content::ContentCache;
use drive::Drive;
use fs::DriveFs;
use mountpoint::lazy_unmount;
use pins::Pins;
use thumbnails::ThumbnailCache;
use writes::Staging;

const CACHE_PURGE_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[tokio::main]
async fn main() -> Result<()> {
    pdi18n::init();
    config::migrate_legacy_dirs()?;
    let settings = Settings::load()?;
    let mountpoint = match std::env::args_os().nth(1) {
        Some(path) => PathBuf::from(path),
        None => config::expand_home(&settings.mountpoint)?,
    };
    let verbose = std::env::var_os("PDRIVE_VERBOSE").is_some();

    let cache_home = config::xdg_dir("XDG_CACHE_HOME", ".cache")?;
    let content_dir = cache_home.join(APP_DIR).join("content");
    let staging_dir = cache_home.join(APP_DIR).join("staging");
    for dir in [&content_dir, &staging_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mountpoint = mountpoint::resolve(&mountpoint)?;

    let (client, api) = pdcore::connect_with_api(verbose).await?;
    let root = client.my_files_folder().await?;
    let scope = root.tree_event_scope_id.clone();
    let activity = Arc::new(ActivityLog::default());
    let content = ContentCache::new(content_dir, tokio::runtime::Handle::current(), activity.clone(), verbose);
    let staging = Staging::new(staging_dir);
    let data_dir = config::xdg_dir("XDG_DATA_HOME", ".local/share")?.join(APP_DIR);
    std::fs::create_dir_all(&data_dir).with_context(|| format!("cannot create {}", data_dir.display()))?;
    let pins = Pins::load(data_dir.join("pinned.json"));
    let drive = Arc::new(Drive::new(client, api, activity, root.uid, content, staging, pins, ThumbnailCache::new(&cache_home), mountpoint, verbose));

    dbus::start(drive.clone(), tokio::runtime::Handle::current());
    let background = [
        tokio::spawn(watch::run(drive.clone(), scope)),
        tokio::spawn(drive.clone().watch_account()),
        tokio::spawn({
            let drive = drive.clone();
            async move { drive.recover_uploads().await }
        }),
        tokio::spawn({
            let (drive, max_bytes) = (drive.clone(), settings.cache_size_bytes());
            async move {
                loop {
                    drive.content.purge(max_bytes, &drive.pins.pinned_revisions());
                    tokio::time::sleep(CACHE_PURGE_INTERVAL).await;
                }
            }
        }),
    ];

    // Mounted until stopped. When the mount is ejected (unmounted from outside, or with the
    // "Unmount" sidebar action), the daemon keeps running and mounts again on request ("Mount").
    let mut terminate = signal(SignalKind::terminate())?;
    let mut wanted = drive.wanted_mounted.subscribe();
    let result = loop {
        let mut session = mount(&drive)?;
        let mut unmounter = session.unmount_callable();
        // The session loop returns when the file system is unmounted, whoever unmounts it.
        let mut session_loop = tokio::task::spawn_blocking(move || session.run());
        println!("Mounted on {}.", drive.mount_target.display());

        let ejected = tokio::select! {
            _ = tokio::signal::ctrl_c() => None,
            _ = terminate.recv() => None,
            result = &mut session_loop => Some(result),
            // "Unmount" from the sidebar: like an eject.
            _ = wanted.wait_for(|wanted| !*wanted) => {
                if let Err(e) = unmounter.unmount() {
                    eprintln!("unmount failed ({e}): detaching the mount");
                    lazy_unmount(&drive.mount_target);
                }
                Some((&mut session_loop).await)
            }
        };
        drive.set_session(None);
        match ejected {
            Some(result) => {
                // Depending on how it was unmounted, the session ends with ECONNABORTED.
                let result = result.map(|r| r.or_else(|e| if e.raw_os_error() == Some(libc::ECONNABORTED) { Ok(()) } else { Err(e) }));
                if let Err(e) = result.map_err(anyhow::Error::from).and_then(|r| r.context("FUSE session failed")) {
                    break Err(e);
                }
                println!("Unmounted: waiting for a mount request.");
                // Ejected from outside: the user wants it unmounted until "Mount".
                drive.request_unmount();
                tokio::select! {
                    _ = wanted.wait_for(|wanted| *wanted) => continue,
                    _ = tokio::signal::ctrl_c() => break Ok(()),
                    _ = terminate.recv() => break Ok(()),
                }
            }
            None => {
                if let Err(e) = unmounter.unmount() {
                    // Busy (e.g. open in a file manager): detach it now, it goes away once released.
                    eprintln!("unmount failed ({e}): detaching the mount");
                    lazy_unmount(&drive.mount_target);
                }
                break session_loop.await.map_err(anyhow::Error::from).and_then(|r| r.context("FUSE session failed"));
            }
        }
    };
    for task in background {
        task.abort();
    }
    println!("Stopped. Pending uploads, if any, are resumed at the next start.");
    result
}

fn mount(drive: &Arc<Drive>) -> Result<fuser::Session<DriveFs>> {
    let target = &drive.mount_target;
    mountpoint::prepare_dir(target)?;
    let mut config = Config::default();
    config.mount_options =
        vec![MountOption::FSName("pdrive".into()), MountOption::Subtype("pdrive".into()), MountOption::NoAtime, MountOption::DefaultPermissions];
    let session = fuser::Session::new(DriveFs::new(drive.clone(), tokio::runtime::Handle::current()), target, &config)
        .with_context(|| format!("cannot mount on {}", target.display()))?;
    drive.set_session(Some(session.notifier()));
    Ok(session)
}
