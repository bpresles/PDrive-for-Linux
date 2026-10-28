// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
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
    let target = config::xdg_dir("XDG_DATA_HOME", ".local/share")?.join(APP_DIR).join(TARGET_DIR);
    resolve_with_target(requested, target)
}

fn resolve_with_target(requested: &Path, target: PathBuf) -> Result<Mountpoint> {
    let public = std::path::absolute(requested).with_context(|| format!("invalid mount point {}", requested.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_mount_point_becomes_a_link_and_parents_are_created() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("data/.mount");
        let public = root.path().join("home/sub/ProtonDrive");
        let mount = resolve_with_target(&public, target.clone()).unwrap();
        assert_eq!(mount.public, public);
        assert_eq!(mount.target, target);
        assert!(target.is_dir());
        assert_eq!(std::fs::read_link(&public).unwrap(), target);
    }

    #[test]
    fn empty_folder_is_replaced_by_a_link() {
        let root = tempfile::tempdir().unwrap();
        let (target, public) = (root.path().join(".mount"), root.path().join("ProtonDrive"));
        std::fs::create_dir(&public).unwrap();
        let mount = resolve_with_target(&public, target.clone()).unwrap();
        assert_eq!(mount.target, target);
        assert!(std::fs::symlink_metadata(&public).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&public).unwrap(), target);
    }

    #[test]
    fn folder_with_files_stays_the_mount_point() {
        let root = tempfile::tempdir().unwrap();
        let (target, public) = (root.path().join(".mount"), root.path().join("ProtonDrive"));
        std::fs::create_dir(&public).unwrap();
        std::fs::write(public.join("mine.txt"), b"keep").unwrap();
        let mount = resolve_with_target(&public, target).unwrap();
        assert_eq!(mount.public, public);
        assert_eq!(mount.target, public.canonicalize().unwrap());
        assert!(std::fs::symlink_metadata(&public).unwrap().is_dir());
        assert_eq!(std::fs::read(public.join("mine.txt")).unwrap(), b"keep");
    }

    #[test]
    fn correct_link_is_left_as_is() {
        let root = tempfile::tempdir().unwrap();
        let (target, public) = (root.path().join(".mount"), root.path().join("ProtonDrive"));
        resolve_with_target(&public, target.clone()).unwrap();
        let mount = resolve_with_target(&public, target.clone()).unwrap();
        assert_eq!(mount.target, target);
        assert_eq!(std::fs::read_link(&public).unwrap(), target);
    }

    #[test]
    fn link_to_something_else_is_refused_by_link() {
        let root = tempfile::tempdir().unwrap();
        let (target, other, public) = (root.path().join(".mount"), root.path().join("other"), root.path().join("ProtonDrive"));
        std::fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &public).unwrap();
        assert!(link(&public, &target).is_err());
        assert_eq!(std::fs::read_link(&public).unwrap(), other);
    }

    #[test]
    fn regular_file_is_refused_by_link() {
        let root = tempfile::tempdir().unwrap();
        let public = root.path().join("ProtonDrive");
        std::fs::write(&public, b"x").unwrap();
        assert!(link(&public, &root.path().join(".mount")).is_err());
        assert_eq!(std::fs::read(&public).unwrap(), b"x");
    }

    #[test]
    fn prepare_dir_creates_nested_folders() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a/b/c");
        prepare_dir(&path).unwrap();
        assert!(path.is_dir());
        prepare_dir(&path).unwrap();
    }
}
