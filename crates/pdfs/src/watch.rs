//! Keeps the mounted tree up to date from the Drive event stream of the volume.
//!
//! The kernel only emits inotify events for operations that go through the VFS, and file
//! managers (Nautilus through GIO) rely on them to refresh open folders. So a remote change
//! affecting a loaded folder is replayed through the mount: a rename, a node creation, a
//! removal or a timestamp update, which the FUSE layer accepts from this process only and
//! answers by applying the change to the tree.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use pdsdk::proto::{drive_event::Event, DriveEvent};

use crate::drive::{Change, Drive};
use crate::tree::{EntryKind, NodeInfo, NodeKind};

/// Proton asks third-party clients to sync from events rather than by walking the tree; the
/// event endpoint itself is polled at this interval.
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

pub async fn run(drive: Arc<Drive>, scope: String) {
    // Without a cursor the SDK only returns the latest event ID: start from there.
    let mut cursor = loop {
        match drive.client.events(&scope, None).await {
            Ok(events) => break events.into_iter().map(|e| e.id).rfind(|id| !id.is_empty()),
            Err(e) => {
                eprintln!("cannot get the event cursor: {e}");
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    };

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let events = match drive.client.events(&scope, cursor.clone()).await {
            Ok(events) => events,
            Err(e) => {
                eprintln!("cannot fetch events: {e}");
                continue;
            }
        };
        for event in events {
            apply(&drive, &event).await;
            if !event.id.is_empty() {
                cursor = Some(event.id);
            }
        }
    }
}

/// Operation replayed through the mount, with paths relative to the mount point.
enum Replay {
    Rename(PathBuf, PathBuf),
    CreateFile(PathBuf),
    CreateFolder(PathBuf),
    RemoveFile(PathBuf),
    RemoveFolder(PathBuf),
    Touch(PathBuf, SystemTime),
}

async fn apply(drive: &Arc<Drive>, event: &DriveEvent) {
    let planned = match &event.event {
        Some(Event::NodeUpdated(e)) if e.is_trashed => plan_removal(drive, &e.node_uid),
        Some(Event::NodeUpdated(e)) => plan_update(drive, &e.node_uid, e.parent_node_uid.as_deref()).await,
        Some(Event::NodeDeleted(e)) => plan_removal(drive, &e.node_uid),
        Some(Event::ContinuityLost(_)) => {
            eprintln!("event continuity lost: reloading folder listings");
            let invalidations = drive.tree().reset_listings();
            drive.invalidate(invalidations);
            None
        }
        Some(Event::ScopeAccessLost(_)) => {
            eprintln!("access to the volume events was lost");
            None
        }
        Some(Event::CursorAdvanced(_) | Event::SharedWithMeUpdated(_)) | None => None,
    };
    let Some((change, replay)) = planned else { return };

    let replayed = match replay {
        Some(replay) => replay_through_mount(drive, change, replay).await,
        None => {
            let invalidations = drive.apply(change);
            drive.invalidate(invalidations);
            true
        }
    };
    if drive.verbose {
        eprintln!("[event] {} applied{}", event.id, if replayed { "" } else { " (without live notification)" });
    }
}

fn plan_removal(drive: &Drive, node_uid: &str) -> Option<(Change, Option<Replay>)> {
    let tree = drive.tree();
    let ino = tree.ino_of(node_uid)?;
    let path = tree.path_of(ino)?;
    let replay = match tree.get(ino)?.kind {
        EntryKind::Folder { .. } => Replay::RemoveFolder(path),
        EntryKind::File { .. } => Replay::RemoveFile(path),
    };
    Some((Change::Remove { uid: node_uid.to_owned() }, Some(replay)))
}

/// A node was created, renamed, moved or got a new revision.
async fn plan_update(drive: &Drive, node_uid: &str, parent_uid: Option<&str>) -> Option<(Change, Option<Replay>)> {
    let relevant = {
        let tree = drive.tree();
        tree.ino_of(node_uid).is_some() || parent_uid.and_then(|p| tree.ino_of(p)).is_some_and(|p| tree.is_loaded(p))
    };
    if !relevant {
        return None;
    }

    let node = match drive.client.nodes(vec![node_uid.to_owned()]).await {
        Ok(nodes) => nodes.into_iter().next().and_then(NodeInfo::from_node),
        Err(e) => {
            eprintln!("cannot load updated node {node_uid}: {e}");
            return None;
        }
    };
    let Some(node) = node else {
        // Trashed or unreadable now: hide it.
        return plan_removal(drive, node_uid);
    };

    let tree = drive.tree();
    let Some(parent) = node.parent_uid.as_deref().and_then(|p| tree.ino_of(p)).filter(|p| tree.is_loaded(*p)) else {
        // Moved to a folder that is not loaded (or out of the mount): it will be listed there on demand.
        drop(tree);
        return plan_removal(drive, node_uid);
    };
    let new_path = tree.path_of(parent)?.join(&node.name);

    let replay = match tree.ino_of(node_uid) {
        None => Some(match node.kind {
            NodeKind::Folder => Replay::CreateFolder(new_path),
            NodeKind::File { .. } => Replay::CreateFile(new_path),
        }),
        Some(ino) => {
            let entry = tree.get(ino)?;
            if entry.parent != parent || entry.name != node.name {
                Some(Replay::Rename(tree.path_of(ino)?, new_path))
            } else {
                let content_changed = match (&entry.kind, &node.kind) {
                    (EntryKind::File { revision_uid: old, .. }, NodeKind::File { revision_uid: new, .. }) => old != new,
                    _ => false,
                };
                (content_changed || entry.mtime != node.mtime).then_some(Replay::Touch(new_path, node.mtime))
            }
        }
    };
    Some((Change::Update { parent, node }, replay))
}

/// Replays an operation through the mount; falls back to a silent update if it fails.
async fn replay_through_mount(drive: &Arc<Drive>, change: Change, replay: Replay) -> bool {
    drive.set_pending(change);
    let root = drive.mountpoint.clone();
    let result = tokio::task::spawn_blocking(move || run_replay(&root, replay)).await;

    if let Ok(Err(e)) = &result {
        eprintln!("cannot replay a remote change through the mount: {e}");
    }
    // The FUSE callback consumes the change unless the operation failed before reaching it:
    // then apply it here, without inotify events.
    let replayed = !drive.apply_pending();
    drive.flush_deferred();
    replayed
}

fn run_replay(root: &Path, replay: Replay) -> std::io::Result<()> {
    let c_path = |path: &Path| CString::new(root.join(path).as_os_str().as_bytes());
    let check = |result: libc::c_int| if result == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) };
    match replay {
        Replay::Rename(from, to) => std::fs::rename(root.join(from), root.join(to)),
        Replay::CreateFolder(path) => std::fs::create_dir(root.join(path)),
        Replay::CreateFile(path) => check(unsafe { libc::mknod(c_path(&path)?.as_ptr(), libc::S_IFREG | 0o644, 0) }),
        Replay::RemoveFile(path) => std::fs::remove_file(root.join(path)),
        Replay::RemoveFolder(path) => std::fs::remove_dir(root.join(path)),
        // Set the timestamp on the path: opening the file would download it.
        Replay::Touch(path, mtime) => {
            let since_epoch = mtime.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            let times = [
                libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
                libc::timespec { tv_sec: since_epoch.as_secs() as libc::time_t, tv_nsec: since_epoch.subsec_nanos().into() },
            ];
            check(unsafe { libc::utimensat(libc::AT_FDCWD, c_path(&path)?.as_ptr(), times.as_ptr(), 0) })
        }
    }
}
