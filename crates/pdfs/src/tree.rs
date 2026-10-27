//! In-memory view of the remote tree, indexed by inode number.
//!
//! Folder listings are loaded lazily and kept up to date from Drive events. Inode numbers are
//! never reused while mounted.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pdsdk::proto::{node, Node, Timestamp};

pub const ROOT_INO: u64 = 1;

#[derive(Clone, PartialEq)]
pub enum NodeKind {
    Folder,
    File { size: u64, revision_uid: String, has_thumbnail: bool },
}

/// Decrypted metadata of a remote node.
#[derive(Clone)]
pub struct NodeInfo {
    pub uid: String,
    pub parent_uid: Option<String>,
    pub name: String,
    pub kind: NodeKind,
    pub mtime: SystemTime,
}

impl NodeInfo {
    /// Converts an SDK node. Trashed nodes, nodes without a usable active revision and nodes
    /// whose name could not be decrypted are not exposed.
    pub fn from_node(node: Node) -> Option<Self> {
        match node.kind? {
            node::Kind::Folder(f) => {
                if f.trash_time.is_some() {
                    return None;
                }
                Some(Self {
                    name: valid_name(f.name?.result?)?,
                    uid: f.uid,
                    parent_uid: f.parent_uid,
                    kind: NodeKind::Folder,
                    mtime: to_system_time(f.creation_time),
                })
            }
            node::Kind::File(f) => {
                if f.trash_time.is_some() {
                    return None;
                }
                let revision = f.active_revision?;
                let mtime = revision.claimed_modification_time.or(revision.creation_time).or(f.creation_time);
                Some(Self {
                    name: valid_name(f.name?.result?)?,
                    uid: f.uid,
                    parent_uid: Some(f.parent_uid).filter(|p| !p.is_empty()),
                    kind: NodeKind::File {
                        size: revision.claimed_size.unwrap_or(revision.storage_size).max(0) as u64,
                        has_thumbnail: revision.thumbnails.iter().any(|t| t.r#type == pdsdk::proto::ThumbnailType::Thumbnail as i32),
                        revision_uid: revision.uid,
                    },
                    mtime: to_system_time(mtime),
                })
            }
        }
    }
}

fn valid_name(name: pdsdk::proto::string_result::Result) -> Option<String> {
    match name {
        pdsdk::proto::string_result::Result::Value(n) if !n.is_empty() && n != "." && n != ".." && !n.contains('/') => Some(n),
        _ => None,
    }
}

fn to_system_time(t: Option<Timestamp>) -> SystemTime {
    t.map(|t| UNIX_EPOCH + Duration::new(t.seconds.max(0) as u64, t.nanos.max(0) as u32)).unwrap_or(UNIX_EPOCH)
}

pub enum EntryKind {
    /// `None` until the listing has been loaded.
    Folder { children: Option<Vec<u64>> },
    File { size: u64, revision_uid: String, has_thumbnail: bool },
}

pub struct Entry {
    pub uid: String,
    pub parent: u64,
    pub name: String,
    pub kind: EntryKind,
    pub mtime: SystemTime,
}

/// Kernel caches to invalidate after a change.
pub enum Invalidation {
    /// A name in a directory (dentry).
    Entry(u64, String),
    /// Attributes and content of an inode (for a directory: its listing).
    Inode(u64),
}

pub struct Tree {
    entries: HashMap<u64, Entry>,
    by_uid: HashMap<String, u64>,
    next_ino: u64,
}

impl Tree {
    pub fn new(root_uid: String) -> Self {
        let root = Entry {
            uid: root_uid.clone(),
            parent: ROOT_INO,
            name: String::new(),
            kind: EntryKind::Folder { children: None },
            mtime: UNIX_EPOCH,
        };
        Self { entries: HashMap::from([(ROOT_INO, root)]), by_uid: HashMap::from([(root_uid, ROOT_INO)]), next_ino: ROOT_INO + 1 }
    }

    pub fn get(&self, ino: u64) -> Option<&Entry> {
        self.entries.get(&ino)
    }

    pub fn ino_of(&self, uid: &str) -> Option<u64> {
        self.by_uid.get(uid).copied()
    }

    pub fn children(&self, ino: u64) -> Option<&[u64]> {
        match &self.entries.get(&ino)?.kind {
            EntryKind::Folder { children } => children.as_deref(),
            EntryKind::File { .. } => None,
        }
    }

    pub fn is_loaded(&self, ino: u64) -> bool {
        self.children(ino).is_some()
    }

    pub fn child(&self, parent: u64, name: &str) -> Option<u64> {
        self.children(parent)?.iter().copied().find(|c| self.entries[c].name == name)
    }

    /// Path of a node relative to the mount root.
    pub fn path_of(&self, mut ino: u64) -> Option<std::path::PathBuf> {
        let mut components = Vec::new();
        while ino != ROOT_INO {
            let entry = self.entries.get(&ino)?;
            components.push(entry.name.as_str());
            ino = entry.parent;
        }
        Some(components.into_iter().rev().collect())
    }

    pub fn set_file_size(&mut self, ino: u64, new_size: u64) {
        if let Some(Entry { kind: EntryKind::File { size, .. }, .. }) = self.entries.get_mut(&ino) {
            *size = new_size;
        }
    }

    /// Replaces the listing of a folder.
    pub fn set_children(&mut self, parent: u64, nodes: Vec<NodeInfo>) {
        let previous = self.children(parent).map(<[u64]>::to_vec).unwrap_or_default();
        let mut children = Vec::with_capacity(nodes.len());
        for node in nodes {
            // A node moved here from another loaded folder must leave that listing.
            if let Some(old_parent) = self.ino_of(&node.uid).map(|ino| self.entries[&ino].parent).filter(|p| *p != parent) {
                let ino = self.by_uid[&node.uid];
                self.detach(old_parent, ino);
            }
            let ino = self.insert_or_update(parent, node);
            children.push(ino);
        }
        for gone in previous.into_iter().filter(|ino| !children.contains(ino)) {
            if self.entries.get(&gone).is_some_and(|e| e.parent == parent) {
                self.remove(gone);
            }
        }
        if let Some(Entry { kind: EntryKind::Folder { children: slot }, .. }) = self.entries.get_mut(&parent) {
            *slot = Some(children);
        }
    }

    /// Applies an updated node located in `parent` (a rename, a move, a new revision or a new node).
    pub fn apply_update(&mut self, parent: u64, node: NodeInfo) -> Vec<Invalidation> {
        let mut invalidations = Vec::new();
        let previous = self.ino_of(&node.uid).map(|ino| {
            let e = &self.entries[&ino];
            let revision = match &e.kind {
                EntryKind::File { revision_uid, .. } => Some(revision_uid.clone()),
                EntryKind::Folder { .. } => None,
            };
            (ino, e.parent, e.name.clone(), revision)
        });
        let (name, revision) = (node.name.clone(), match &node.kind {
            NodeKind::File { revision_uid, .. } => Some(revision_uid.clone()),
            NodeKind::Folder => None,
        });

        // A node with the same name in the target folder is replaced.
        if let Some(replaced) = self.child(parent, &name).filter(|c| Some(*c) != previous.as_ref().map(|p| p.0)) {
            self.detach(parent, replaced);
            self.remove(replaced);
        }

        let ino = self.insert_or_update(parent, node);
        match previous {
            Some((_, old_parent, old_name, old_revision)) => {
                if old_parent != parent || old_name != name {
                    self.detach(old_parent, ino);
                    self.attach(parent, ino);
                    invalidations.push(Invalidation::Entry(old_parent, old_name));
                    invalidations.push(Invalidation::Inode(old_parent));
                    invalidations.push(Invalidation::Entry(parent, name));
                    invalidations.push(Invalidation::Inode(parent));
                }
                if old_revision != revision {
                    invalidations.push(Invalidation::Inode(ino));
                }
            }
            None => {
                self.attach(parent, ino);
                invalidations.push(Invalidation::Entry(parent, name));
                invalidations.push(Invalidation::Inode(parent));
            }
        }
        invalidations
    }

    /// Removes a node and its descendants.
    pub fn remove_uid(&mut self, uid: &str) -> Vec<Invalidation> {
        let Some(ino) = self.ino_of(uid) else { return Vec::new() };
        if ino == ROOT_INO {
            return Vec::new();
        }
        let (parent, name) = {
            let e = &self.entries[&ino];
            (e.parent, e.name.clone())
        };
        self.detach(parent, ino);
        self.remove(ino);
        vec![Invalidation::Entry(parent, name), Invalidation::Inode(parent)]
    }

    /// Forgets every listing, e.g. after the event stream lost continuity.
    pub fn reset_listings(&mut self) -> Vec<Invalidation> {
        let mut invalidations = Vec::new();
        for (ino, entry) in &mut self.entries {
            if let EntryKind::Folder { children } = &mut entry.kind
                && children.take().is_some() {
                    invalidations.push(Invalidation::Inode(*ino));
                }
        }
        invalidations
    }

    fn insert_or_update(&mut self, parent: u64, node: NodeInfo) -> u64 {
        let kind = match node.kind {
            NodeKind::Folder => EntryKind::Folder { children: None },
            NodeKind::File { size, revision_uid, has_thumbnail } => EntryKind::File { size, revision_uid, has_thumbnail },
        };
        if let Some(ino) = self.ino_of(&node.uid) {
            let entry = self.entries.get_mut(&ino).expect("indexed inode exists");
            entry.parent = parent;
            entry.name = node.name;
            entry.mtime = node.mtime;
            // Keep a loaded folder listing: it is maintained separately.
            if !matches!((&entry.kind, &kind), (EntryKind::Folder { .. }, EntryKind::Folder { .. })) {
                entry.kind = kind;
            }
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.by_uid.insert(node.uid.clone(), ino);
        self.entries.insert(ino, Entry { uid: node.uid, parent, name: node.name, kind, mtime: node.mtime });
        ino
    }

    fn attach(&mut self, parent: u64, ino: u64) {
        if let Some(Entry { kind: EntryKind::Folder { children: Some(children) }, .. }) = self.entries.get_mut(&parent)
            && !children.contains(&ino) {
                children.push(ino);
            }
    }

    fn detach(&mut self, parent: u64, ino: u64) {
        if let Some(Entry { kind: EntryKind::Folder { children: Some(children) }, .. }) = self.entries.get_mut(&parent) {
            children.retain(|c| *c != ino);
        }
    }

    fn remove(&mut self, ino: u64) {
        let Some(entry) = self.entries.remove(&ino) else { return };
        self.by_uid.remove(&entry.uid);
        if let EntryKind::Folder { children: Some(children) } = entry.kind {
            for child in children {
                self.remove(child);
            }
        }
    }
}
