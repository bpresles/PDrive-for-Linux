// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Keeps the logs when the application is started from a launcher (standard error on
//! `/dev/null`). The daemon writes to the standard error output of this program, so redirecting
//! this one is enough. Nothing changes when it is a terminal or already a stream of the journal.
//! Otherwise it goes to the user journal, as the identifier `pdrive`
//! (`journalctl --user -t pdrive`), or to `$XDG_STATE_HOME/pdrive/pdrive.log` without journal.

use std::fs::OpenOptions;
use std::io::{IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

const IDENTIFIER: &str = "pdrive";
const JOURNAL_SOCKET: &str = "/run/systemd/journal/stdout";
/// The file is moved to `pdrive.log.1` at the start when it is larger than this.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub fn init() {
    if std::io::stderr().is_terminal() || stderr_is_journal() {
        return;
    }
    let redirected = journal_stream().or_else(log_file);
    if let Some(fd) = redirected {
        unsafe { libc::dup2(fd, libc::STDERR_FILENO) };
    }
}

/// Whether standard error is the journal stream named by `JOURNAL_STREAM` (`device:inode`). The
/// variable alone proves nothing: it is inherited by the programs a launcher starts.
fn stderr_is_journal() -> bool {
    let Some(stream) = std::env::var("JOURNAL_STREAM").ok() else { return false };
    let Ok(stat) = std::fs::metadata("/proc/self/fd/2") else { return false };
    use std::os::unix::fs::MetadataExt;
    stream == format!("{}:{}", stat.dev(), stat.ino())
}

/// A connection to the stdout stream service of journald, as `systemd-cat` makes it. The
/// returned descriptor stays open for the whole life of the process.
fn journal_stream() -> Option<i32> {
    let mut stream = UnixStream::connect(JOURNAL_SOCKET).ok()?;
    // identifier, unit, priority, level prefix, and forwarding to syslog, kmsg, console
    write!(stream, "{IDENTIFIER}\n\n6\n0\n0\n0\n0\n").ok()?;
    let _ = stream.shutdown(std::net::Shutdown::Read);
    let fd = stream.as_raw_fd();
    std::mem::forget(stream);
    Some(fd)
}

fn log_file() -> Option<i32> {
    let state_home = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() && std::path::Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    let dir = state_home.join("pdrive");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("pdrive.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, dir.join("pdrive.log.1"));
    }
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let fd = file.as_raw_fd();
    std::mem::forget(file);
    Some(fd)
}
