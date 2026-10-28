//! In-memory journal of recent activity (transfers and changes), shown by the status window.
//! Exposed by D-Bus through `GetRecentActivity`; `ActivityChanged` is emitted on each change.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Entries kept; the oldest finished ones are dropped first.
const CAPACITY: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Upload,
    Download,
    Rename,
    Move,
    Trash,
    RemoteCreated,
    RemoteRenamed,
    RemoteUpdated,
    RemoteRemoved,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Upload => "upload",
            Kind::Download => "download",
            Kind::Rename => "rename",
            Kind::Move => "move",
            Kind::Trash => "trash",
            Kind::RemoteCreated => "remote-created",
            Kind::RemoteRenamed => "remote-renamed",
            Kind::RemoteUpdated => "remote-updated",
            Kind::RemoteRemoved => "remote-removed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Running,
    Done,
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
        }
    }
}

#[derive(Clone)]
pub struct Entry {
    pub id: u64,
    /// Path inside the mount.
    pub path: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the epoch, of the last change of the entry.
    pub timestamp: i64,
    pub state: State,
}

#[derive(Default)]
pub struct ActivityLog {
    entries: Mutex<(u64, VecDeque<Entry>)>,
    /// Session bus connection, for `ActivityChanged`.
    pub dbus: OnceLock<gio::DBusConnection>,
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl ActivityLog {
    fn lock(&self) -> std::sync::MutexGuard<'_, (u64, VecDeque<Entry>)> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        if let Some(connection) = self.dbus.get() {
            crate::dbus::emit_activity_changed(connection);
        }
    }

    /// Adds an operation in progress; returns its id for [`ActivityLog::finish`].
    pub fn begin(&self, kind: Kind, path: impl Into<String>, size: u64) -> u64 {
        let id = self.push(kind, path.into(), size, State::Running);
        self.changed();
        id
    }

    /// Adds an operation that is already done.
    pub fn record(&self, kind: Kind, path: impl Into<String>, size: u64) {
        self.push(kind, path.into(), size, State::Done);
        self.changed();
    }

    pub fn finish(&self, id: u64, state: State) {
        if let Some(entry) = self.lock().1.iter_mut().find(|e| e.id == id) {
            entry.state = state;
            entry.timestamp = now();
        }
        self.changed();
    }

    /// Removes an entry (e.g. a download cancelled because nobody reads it any more).
    pub fn discard(&self, id: u64) {
        self.lock().1.retain(|e| e.id != id);
        self.changed();
    }

    fn push(&self, kind: Kind, path: String, size: u64, state: State) -> u64 {
        let mut guard = self.lock();
        guard.0 += 1;
        let id = guard.0;
        let entries = &mut guard.1;
        entries.push_front(Entry { id, path, kind, size, timestamp: now(), state });
        while entries.len() > CAPACITY {
            // Keep what is still running, drop the oldest finished entry.
            match entries.iter().rposition(|e| e.state != State::Running) {
                Some(position) => entries.remove(position),
                None => entries.pop_back(),
            };
        }
        id
    }

    /// Most recent first.
    pub fn recent(&self) -> Vec<Entry> {
        self.lock().1.iter().cloned().collect()
    }
}
