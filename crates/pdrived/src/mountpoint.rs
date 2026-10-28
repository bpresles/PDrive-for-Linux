//! Where the file system is really mounted, and the path it is reached through.
//!
//! GIO lists the FUSE mounts of the home folder as volumes (`g_unix_mount_entry_guess_should_display`
//! and gvfs' udisks2 monitor), which would add a second sidebar entry next to the CloudProviders
//! account. Both hide a mount whose path has a "/." component, but `x-gvfs-hide` only works
//! when the option reaches GIO, and `fusermount3` writes neither `/etc/mtab` nor `utab`. So the
//! file system is mounted in a hidden folder (the target) and the configured mount point (the
//! public path, e.g. `~/ProtonDrive`) is a symbolic link to it. The public path is the one
//! exposed to clients: file manager URIs, thumbnail cache keys, D-Bus.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::{self, APP_DIR};

/// Hidden folder (in the data directory) holding the real mount.
const TARGET_DIR: &str = ".mount";

pub struct Mountpoint {
    /// Absolute path exposed to clients; a link to `target`, or the target itself as a fallback.
    pub public: PathBuf,
    /// Where the file system is mounted.
    pub target: PathBuf,
}

/// Prepares the hidden mount folder and makes `requested` a link to it. A mount point that is a
/// folder with files (or anything else than an empty folder or a link) is never touched: it stays
/// the real mount point, and GIO will show it as a second volume.
pub fn resolve(requested: &Path) -> Result<Mountpoint> {
    let public = std::path::absolute(requested).with_context(|| format!("invalid mount point {}", requested.display()))?;
    let target = config::xdg_dir("XDG_DATA_HOME", ".local/share")?.join(APP_DIR).join(TARGET_DIR);
    prepare_dir(&target)?;
    match link(&public, &target) {
        Ok(()) => Ok(Mountpoint { public, target }),
        Err(e) => {
            eprintln!("cannot use {} as a link to the hidden mount ({e:#}): mounting on it directly", public.display());
            prepare_dir(&public)?;
            let target = public.canonicalize().with_context(|| format!("cannot access {}", public.display()))?;
            Ok(Mountpoint { public, target })
        }
    }
}

/// Makes `public` a link to `target` if it is absent, an empty folder, or already that link.
fn link(public: &Path, target: &Path) -> Result<()> {
    clear_dead_mount(public);
    match std::fs::symlink_metadata(public) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if std::fs::read_link(public)? == target {
                return Ok(());
            }
            bail!("it is a link to something else");
        }
        Ok(meta) if meta.is_dir() => {
            let parent_dev = public.parent().map(std::fs::metadata).transpose()?.map(|m| m.dev());
            if parent_dev != Some(meta.dev()) {
                bail!("something is already mounted on it");
            }
            if std::fs::read_dir(public)?.next().is_some() {
                bail!("it contains files, which are left in place");
            }
            std::fs::remove_dir(public)?;
        }
        Ok(_) => bail!("it is not a folder"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = public.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Err(e) => return Err(e.into()),
    }
    std::os::unix::fs::symlink(target, public)?;
    Ok(())
}

/// Creates a mount folder if needed, and clears a dead mount left by a crashed run.
pub fn prepare_dir(path: &Path) -> Result<()> {
    clear_dead_mount(path);
    std::fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))
}

fn clear_dead_mount(path: &Path) {
    if std::fs::symlink_metadata(path).is_err_and(|e| e.raw_os_error() == Some(libc::ENOTCONN)) {
        eprintln!("clearing a dead mount on {}", path.display());
        lazy_unmount(path);
    }
}

pub fn lazy_unmount(path: &Path) {
    match std::process::Command::new("fusermount3").arg("-u").arg("-z").arg(path).status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("fusermount3 -uz {} failed: {status}", path.display()),
        Err(e) => eprintln!("cannot run fusermount3: {e}"),
    }
}
