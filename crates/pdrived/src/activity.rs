// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_recent_entry_comes_first() {
        let log = ActivityLog::default();
        log.record(Kind::Trash, "a", 0);
        log.record(Kind::Rename, "b", 0);
        let recent = log.recent();
        assert_eq!(recent.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert!(recent.iter().all(|e| e.state == State::Done));
    }

    #[test]
    fn begin_then_finish_updates_the_entry() {
        let log = ActivityLog::default();
        let id = log.begin(Kind::Upload, "f", 10);
        assert_eq!(log.recent()[0].state, State::Running);
        log.finish(id, State::Failed);
        let entry = &log.recent()[0];
        assert_eq!((entry.id, entry.state, entry.size, entry.kind), (id, State::Failed, 10, Kind::Upload));
        // Finishing an unknown id is harmless.
        log.finish(9999, State::Done);
        assert_eq!(log.recent().len(), 1);
    }

    #[test]
    fn ids_are_unique_and_discard_removes() {
        let log = ActivityLog::default();
        let a = log.begin(Kind::Download, "a", 0);
        let b = log.begin(Kind::Download, "b", 0);
        assert_ne!(a, b);
        log.discard(a);
        assert_eq!(log.recent().iter().map(|e| e.id).collect::<Vec<_>>(), [b]);
    }

    #[test]
    fn journal_is_bounded_and_drops_the_oldest_finished_entries() {
        let log = ActivityLog::default();
        for i in 0..CAPACITY + 10 {
            log.record(Kind::Upload, format!("f{i}"), 0);
        }
        let recent = log.recent();
        assert_eq!(recent.len(), CAPACITY);
        assert_eq!(recent[0].path, format!("f{}", CAPACITY + 9));
        assert_eq!(recent[CAPACITY - 1].path, "f10");
    }

    #[test]
    fn running_entries_survive_the_cleanup() {
        let log = ActivityLog::default();
        let running = log.begin(Kind::Download, "running", 0);
        for i in 0..CAPACITY * 2 {
            log.record(Kind::Upload, format!("f{i}"), 0);
        }
        let recent = log.recent();
        assert_eq!(recent.len(), CAPACITY);
        assert!(recent.iter().any(|e| e.id == running));
    }

    #[test]
    fn when_everything_runs_the_oldest_is_dropped() {
        let log = ActivityLog::default();
        for i in 0..CAPACITY + 1 {
            log.begin(Kind::Download, format!("f{i}"), 0);
        }
        let recent = log.recent();
        assert_eq!(recent.len(), CAPACITY);
        assert!(recent.iter().all(|e| e.path != "f0"));
    }

    #[test]
    fn names_are_stable_for_dbus_clients() {
        assert_eq!(Kind::RemoteCreated.as_str(), "remote-created");
        assert_eq!(Kind::Upload.as_str(), "upload");
        assert_eq!(State::Running.as_str(), "running");
        assert_eq!(State::Failed.as_str(), "failed");
    }
}
