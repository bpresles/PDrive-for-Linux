//! Read-only FUSE file system over the remote tree.
//!
//! Callbacks that may hit the network run on the Tokio runtime and reply from there, so the
//! FUSE thread is never blocked by a download or a folder listing.
//!
//! Mutating callbacks are refused, except when this process itself replays a remote change
//! through the mount (see `watch.rs`): going through the VFS is what makes the kernel emit the
//! inotify events that file managers rely on.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fuser::{
    BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner, OpenAccMode,
    OpenFlags, RenameFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, Request, TimeOrNow,
};

use crate::drive::Drive;
use crate::tree::{Entry, EntryKind};

/// Attributes may be cached briefly; remote changes are pushed with explicit invalidations.
const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 4096;

#[derive(Clone, Copy)]
struct Owner {
    uid: u32,
    gid: u32,
}

impl Owner {
    fn attr(self, ino: u64, entry: &Entry) -> FileAttr {
        let (kind, perm, size, nlink) = match &entry.kind {
            // Owner-writable so that the kernel lets this process replay remote changes;
            // writes from other processes are refused with EROFS.
            EntryKind::Folder { .. } => (FileType::Directory, 0o755, 0, 2),
            EntryKind::File { size, .. } => (FileType::RegularFile, 0o644, *size, 1),
        };
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: entry.mtime,
            mtime: entry.mtime,
            ctime: entry.mtime,
            crtime: entry.mtime,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }
}

type OpenFiles = Arc<Mutex<HashMap<u64, File>>>;

pub struct DriveFs {
    drive: Arc<Drive>,
    runtime: tokio::runtime::Handle,
    owner: Owner,
    open_files: OpenFiles,
    next_handle: AtomicU64,
}

impl DriveFs {
    pub fn new(drive: Arc<Drive>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            drive,
            runtime,
            owner: Owner { uid: unsafe { libc::getuid() }, gid: unsafe { libc::getgid() } },
            open_files: Arc::default(),
            next_handle: AtomicU64::new(1),
        }
    }

    fn files(&self) -> std::sync::MutexGuard<'_, HashMap<u64, File>> {
        self.open_files.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies the remote change being replayed, if the request comes from this process.
    fn apply_replayed_change(&self, req: &Request) -> bool {
        from_this_process(req) && self.drive.apply_pending()
    }

    fn reply_child_entry(&self, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let tree = self.drive.tree();
        match name.to_str().and_then(|name| tree.child(parent.0, name)) {
            Some(ino) => reply.entry(&TTL, &self.owner.attr(ino, tree.get(ino).expect("child exists")), Generation(0)),
            None => reply.error(Errno::EIO),
        }
    }
}

/// FUSE reports the thread ID of the caller: compare its thread group with this process.
fn from_this_process(req: &Request) -> bool {
    let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", req.pid())) else { return false };
    status.lines().find_map(|l| l.strip_prefix("Tgid:")).and_then(|v| v.trim().parse::<u32>().ok()) == Some(std::process::id())
}

impl Filesystem for DriveFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(name) = name.to_str().map(str::to_owned) else {
            return reply.error(Errno::ENOENT);
        };
        let (drive, owner) = (self.drive.clone(), self.owner);
        self.runtime.spawn(async move {
            if let Err(e) = drive.load_children(parent.0).await {
                return reply.error(e);
            }
            let tree = drive.tree();
            match tree.child(parent.0, &name) {
                Some(ino) => reply.entry(&TTL, &owner.attr(ino, tree.get(ino).expect("child exists")), Generation(0)),
                None => reply.error(Errno::ENOENT),
            }
        });
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.drive.tree().get(ino.0) {
            Some(entry) => reply.attr(&TTL, &self.owner.attr(ino.0, entry)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let drive = self.drive.clone();
        self.runtime.spawn(async move {
            if let Err(e) = drive.load_children(ino.0).await {
                return reply.error(e);
            }
            let tree = drive.tree();
            let Some(entry) = tree.get(ino.0) else { return reply.error(Errno::ENOENT) };
            let mut listing = vec![(ino.0, FileType::Directory, ".".to_owned()), (entry.parent, FileType::Directory, "..".to_owned())];
            for &child in tree.children(ino.0).unwrap_or_default() {
                let e = tree.get(child).expect("listed child exists");
                let kind = if matches!(e.kind, EntryKind::Folder { .. }) { FileType::Directory } else { FileType::RegularFile };
                listing.push((child, kind, e.name.clone()));
            }
            for (index, (child, kind, name)) in listing.into_iter().enumerate().skip(offset as usize) {
                if reply.add(INodeNo(child), (index + 1) as u64, kind, name) {
                    break;
                }
            }
            reply.ok();
        });
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            return reply.error(Errno::EROFS);
        }
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let (drive, open_files) = (self.drive.clone(), self.open_files.clone());
        self.runtime.spawn(async move {
            let path = match drive.content_path(ino.0).await {
                Ok(path) => path,
                Err(e) => return reply.error(e),
            };
            match File::open(&path) {
                Ok(file) => {
                    open_files.lock().unwrap_or_else(|e| e.into_inner()).insert(handle, file);
                    // Content is per revision: a new revision invalidates the inode explicitly.
                    reply.opened(FileHandle(handle), FopenFlags::FOPEN_KEEP_CACHE);
                }
                Err(e) => reply.error(e.into()),
            }
        });
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        // Local reads from the content cache: fast enough to serve on the FUSE thread.
        let files = self.files();
        let Some(file) = files.get(&fh.0) else { return reply.error(Errno::EBADF) };
        let mut buffer = vec![0u8; size as usize];
        let mut filled = 0;
        while filled < buffer.len() {
            match file.read_at(&mut buffer[filled..], offset + filled as u64) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return reply.error(e.into()),
            }
        }
        reply.data(&buffer[..filled]);
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.files().remove(&fh.0);
        reply.ok();
    }

    fn setattr(
        &self,
        req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        if !self.apply_replayed_change(req) {
            return reply.error(Errno::EROFS);
        }
        self.getattr(req, ino, None, reply);
    }

    fn mknod(&self, req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, _rdev: u32, reply: ReplyEntry) {
        if !self.apply_replayed_change(req) {
            return reply.error(Errno::EROFS);
        }
        self.reply_child_entry(parent, name, reply);
    }

    fn mkdir(&self, req: &Request, parent: INodeNo, name: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
        if !self.apply_replayed_change(req) {
            return reply.error(Errno::EROFS);
        }
        self.reply_child_entry(parent, name, reply);
    }

    fn unlink(&self, req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        if !self.apply_replayed_change(req) {
            return reply.error(Errno::EROFS);
        }
        reply.ok();
    }

    fn rmdir(&self, req: &Request, _parent: INodeNo, _name: &OsStr, reply: ReplyEmpty) {
        if !self.apply_replayed_change(req) {
            return reply.error(Errno::EROFS);
        }
        reply.ok();
    }

    fn rename(
        &self,
        req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _newparent: INodeNo,
        _newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if !self.apply_replayed_change(req) {
            return reply.error(Errno::EROFS);
        }
        reply.ok();
    }
}
