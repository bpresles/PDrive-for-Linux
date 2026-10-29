// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Keeps the mounted tree up to date from the Drive event stream of the volume. Changes that
//! affect loaded folders are replayed through the mount (see `replay.rs`).

use std::sync::Arc;
use std::time::Duration;

use pdsdk::proto::{DriveEvent, drive_event::Event};

use crate::activity::Kind;
use crate::drive::{Change, Drive};
use crate::replay::{self, Replay};
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
    let size = match &change {
        Change::Update { node: NodeInfo { kind: NodeKind::File { size, .. }, .. }, .. } => *size,
        _ => 0,
    };
    match &replay {
        Some(Replay::CreateFile(path) | Replay::CreateFolder(path)) => record(drive, Kind::RemoteCreated, path, size),
        Some(Replay::Rename(_, path)) => record(drive, Kind::RemoteRenamed, path, size),
        Some(Replay::Touch(path, _)) => record(drive, Kind::RemoteUpdated, path, size),
        Some(Replay::RemoveFile(path) | Replay::RemoveFolder(path)) => record(drive, Kind::RemoteRemoved, path, 0),
        None => {}
    }
    if replay.is_some() {
        drive.quota_changed();
    }
    // A new or updated file needs its thumbnail in the desktop cache (the old one no longer
    // matches the modification time).
    let updated_file = match &change {
        Change::Update { node, .. } if matches!(node.kind, NodeKind::File { .. }) => Some(node.uid.clone()),
        _ => None,
    };
    let thumbnail_of = match &change {
        Change::Update { node, .. } if matches!(node.kind, NodeKind::File { has_thumbnail: true, .. }) => Some(node.clone()),
        _ => None,
    };

    let replayed = match replay {
        Some(replay) => replay::run(drive, change, replay).await,
        None => {
            let invalidations = drive.apply(change);
            drive.invalidate(invalidations);
            true
        }
    };
    if let Some(node) = thumbnail_of {
        drive.ensure_thumbnails(vec![node]).await;
    }
    if let Some(ino) = updated_file.and_then(|uid| drive.tree().ino_of(&uid)) {
        // A pinned file with a new revision, or a new file in a pinned folder: keep it offline.
        let in_pinned_folder = {
            let tree = drive.tree();
            tree.get(ino).and_then(|e| tree.get(e.parent)).is_some_and(|p| drive.pins.is_folder_pinned(&p.uid))
        };
        if in_pinned_folder {
            drive.keep_offline(ino).await;
        } else {
            drive.refresh_pinned(ino).await;
        }
    }
    if drive.verbose {
        eprintln!("[event] {} applied{}", event.id, if replayed { "" } else { " (without live notification)" });
    }
}

fn record(drive: &Drive, kind: Kind, path: &std::path::Path, size: u64) {
    drive.activity_log.record(kind, path.to_string_lossy(), size);
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
