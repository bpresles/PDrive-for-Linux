//! Replays remote changes through the mount.
//!
//! The kernel only emits inotify events for operations that go through the VFS, and file
//! managers (Nautilus through GIO) rely on them to refresh open folders. So a remote change
//! affecting a loaded folder is replayed through the mount: a rename, a node creation, a
//! removal or a timestamp update. The FUSE layer accepts these operations from this process
//! only, and answers them by applying the pending change to the tree.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::drive::{Change, Drive};

/// Operation replayed through the mount, with paths relative to the mount point.
pub enum Replay {
    Rename(PathBuf, PathBuf),
    CreateFile(PathBuf),
    CreateFolder(PathBuf),
    RemoveFile(PathBuf),
    RemoveFolder(PathBuf),
    Touch(PathBuf, SystemTime),
}

/// Replays an operation through the mount; falls back to a silent update if it fails.
pub async fn run(drive: &Arc<Drive>, change: Change, replay: Replay) -> bool {
    // One replay at a time: the pending change slot is shared with the FUSE callbacks.
    let _guard = drive.replay_lock.lock().await;
    // Not mounted (ejected): nobody to notify, just update the tree.
    if !drive.mounted.load(std::sync::atomic::Ordering::SeqCst) {
        drive.apply(change);
        return false;
    }
    drive.set_pending(change);
    let root = drive.mountpoint.clone();
    let result = tokio::task::spawn_blocking(move || run_operation(&root, replay)).await;

    if let Ok(Err(e)) = &result {
        eprintln!("cannot replay a remote change through the mount: {e}");
    }
    // The FUSE callback consumes the change unless the operation failed before reaching it:
    // then apply it here, without inotify events.
    let replayed = !drive.apply_pending();
    drive.flush_deferred();
    replayed
}

fn run_operation(root: &Path, replay: Replay) -> std::io::Result<()> {
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
