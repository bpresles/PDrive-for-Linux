//! Integration tests of the FUSE file system against a real Proton test account.
//!
//! Skeleton, ignored by default and never part of the CI. It starts the `pdrived` binary with a
//! throwaway HOME and XDG folders and checks basic file operations on a mount point inside them.
//! It mounts and writes to a real Drive account, so use a dedicated test account only.
//!
//! Requirements:
//! - `fusermount3` and `/dev/fuse` available to the current user;
//! - a session bus and an unlocked Secret Service holding a session of the TEST account
//!   (`pdrive login`). Run everything in its own session so that the keyring of the real user
//!   is never involved, for example:
//!
//!       PDRIVE_FUSE_TEST=1 dbus-run-session -- sh -c \
//!         'echo -n "" | gnome-keyring-daemon --unlock >/dev/null; \
//!          target/debug/pdrive login && cargo test -p pdrived --test fuse_integration -- --ignored --nocapture'
//!
//! - the environment variable `PDRIVE_FUSE_TEST=1`, an explicit confirmation that the above is
//!   in place; without it the tests do nothing.
//!
//! Everything is created in a folder named after the test run, inside the Drive of the account.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var_os("PDRIVE_FUSE_TEST").is_some_and(|v| v == "1") && std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
}

/// A running daemon with its isolated environment; unmounts and stops it on drop.
struct Daemon {
    child: Child,
    mountpoint: PathBuf,
    _home: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mountpoint = home.path().join("ProtonDrive");
        let child = Command::new(env!("CARGO_BIN_EXE_pdrived"))
            .arg(&mountpoint)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .stdout(Stdio::null())
            .spawn()
            .expect("cannot start pdrived");
        let daemon = Self { child, mountpoint, _home: home };
        daemon.wait_mounted();
        daemon
    }

    fn wait_mounted(&self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            // The root of the file system lists without error once mounted and signed in.
            if std::fs::read_dir(&self.mountpoint).is_ok_and(|mut d| d.next().is_some() || mount_is_fuse(&self.mountpoint)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        panic!("the file system did not get mounted on {}", self.mountpoint.display());
    }
}

fn mount_is_fuse(path: &Path) -> bool {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|m| m.lines().any(|l| l.contains(" - fuse") && l.contains(target.to_str().unwrap_or("\0"))))
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("fusermount3").arg("-u").arg("-z").arg(&self.mountpoint).status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "needs a Proton test account and FUSE; see the module documentation"]
fn create_read_rename_and_delete_a_file() {
    if !enabled() {
        eprintln!("PDRIVE_FUSE_TEST=1 and a dedicated session bus are required: skipping");
        return;
    }
    let daemon = Daemon::start();
    let dir = daemon.mountpoint.join(format!("pdrive-test-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();

    let file = dir.join("hello.txt");
    std::fs::write(&file, b"hello").unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"hello");

    let renamed = dir.join("renamed.txt");
    std::fs::rename(&file, &renamed).unwrap();
    assert!(!file.exists());
    assert_eq!(std::fs::read(&renamed).unwrap(), b"hello");

    // TODO: wait for the upload (a few seconds after the close), restart the daemon and check
    // that the content is served again from Drive; check conflict copies after a crash.

    std::fs::remove_file(&renamed).unwrap();
    std::fs::remove_dir(&dir).unwrap();
}
