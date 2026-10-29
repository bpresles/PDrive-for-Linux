// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! In-memory view of the remote tree, indexed by inode number.
//!
//! Folder listings are loaded lazily and kept up to date from Drive events. Inode numbers are
//! never reused while mounted.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pdsdk::proto::{Node, Timestamp, node};

pub const ROOT_INO: u64 = 1;

/// UID prefix of files created locally and not uploaded yet.
const LOCAL_UID_PREFIX: &str = "local:";

pub fn is_local(uid: &str) -> bool {
    uid.starts_with(LOCAL_UID_PREFIX)
}

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
    Folder {
        children: Option<Vec<u64>>,
    },
    File {
        size: u64,
        revision_uid: String,
        has_thumbnail: bool,
    },
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
        let root = Entry { uid: root_uid.clone(), parent: ROOT_INO, name: String::new(), kind: EntryKind::Folder { children: None }, mtime: UNIX_EPOCH };
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

    /// Inode of a path relative to the mount root, among the loaded entries.
    pub fn lookup_path(&self, path: &std::path::Path) -> Option<u64> {
        let mut ino = ROOT_INO;
        for component in path.components() {
            ino = self.child(ino, component.as_os_str().to_str()?)?;
        }
        Some(ino)
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

    pub fn set_mtime(&mut self, ino: u64, mtime: SystemTime) {
        if let Some(entry) = self.entries.get_mut(&ino) {
            entry.mtime = mtime;
        }
    }

    /// Adds an empty file created locally, not uploaded yet.
    pub fn insert_local_file(&mut self, parent: u64, name: String, mtime: SystemTime) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        let uid = format!("{LOCAL_UID_PREFIX}{ino}");
        let kind = EntryKind::File { size: 0, revision_uid: String::new(), has_thumbnail: false };
        self.by_uid.insert(uid.clone(), ino);
        self.entries.insert(ino, Entry { uid, parent, name, kind, mtime });
        self.attach(parent, ino);
        ino
    }

    /// Records the remote identity of a file after an upload.
    pub fn set_uploaded(&mut self, ino: u64, uid: String, revision: String) {
        let Some(entry) = self.entries.get_mut(&ino) else { return };
        if entry.uid != uid {
            self.by_uid.remove(&entry.uid);
            self.by_uid.insert(uid.clone(), ino);
            entry.uid = uid;
        }
        if let EntryKind::File { revision_uid, .. } = &mut entry.kind {
            *revision_uid = revision;
        }
    }

    /// Moves a node to another place. The target name must be free.
    pub fn move_entry(&mut self, ino: u64, parent: u64, name: String) {
        let Some(old_parent) = self.entries.get(&ino).map(|e| e.parent) else { return };
        self.detach(old_parent, ino);
        if let Some(entry) = self.entries.get_mut(&ino) {
            entry.parent = parent;
            entry.name = name;
        }
        self.attach(parent, ino);
    }

    /// Removes a node and its descendants.
    pub fn remove_ino(&mut self, ino: u64) {
        let Some(parent) = self.entries.get(&ino).map(|e| e.parent) else { return };
        self.detach(parent, ino);
        self.remove(ino);
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
        let gone: Vec<u64> = previous.into_iter().filter(|ino| !children.contains(ino)).collect();
        for gone in gone {
            match self.entries.get(&gone) {
                // Not uploaded yet: unknown remotely but still there.
                Some(e) if e.parent == parent && is_local(&e.uid) => children.push(gone),
                Some(e) if e.parent == parent => self.remove(gone),
                _ => {}
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
        let (name, revision) = (
            node.name.clone(),
            match &node.kind {
                NodeKind::File { revision_uid, .. } => Some(revision_uid.clone()),
                NodeKind::Folder => None,
            },
        );

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
                && children.take().is_some()
            {
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
            && !children.contains(&ino)
        {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use pdsdk::proto::{FileNode, FileRevision, FolderNode, StringResult, ThumbnailHeader, ThumbnailType, string_result};

    fn mtime(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn folder(uid: &str, name: &str) -> NodeInfo {
        NodeInfo { uid: uid.into(), parent_uid: Some("root".into()), name: name.into(), kind: NodeKind::Folder, mtime: mtime(1) }
    }

    fn file(uid: &str, name: &str, revision: &str) -> NodeInfo {
        NodeInfo {
            uid: uid.into(),
            parent_uid: Some("root".into()),
            name: name.into(),
            kind: NodeKind::File { size: 10, revision_uid: revision.into(), has_thumbnail: false },
            mtime: mtime(2),
        }
    }

    fn tree_with(nodes: Vec<NodeInfo>) -> Tree {
        let mut tree = Tree::new("root".into());
        tree.set_children(ROOT_INO, nodes);
        tree
    }

    fn names(tree: &Tree, ino: u64) -> Vec<String> {
        let mut names: Vec<String> = tree.children(ino).unwrap().iter().map(|c| tree.get(*c).unwrap().name.clone()).collect();
        names.sort();
        names
    }

    fn is_entry(i: &Invalidation, parent: u64, name: &str) -> bool {
        matches!(i, Invalidation::Entry(p, n) if *p == parent && n == name)
    }

    fn is_inode(i: &Invalidation, ino: u64) -> bool {
        matches!(i, Invalidation::Inode(n) if *n == ino)
    }

    // --- Conversion of SDK nodes -----------------------------------------------------------

    fn name(value: &str) -> Option<StringResult> {
        Some(StringResult { result: Some(string_result::Result::Value(value.into())) })
    }

    fn sdk_file(name_value: Option<StringResult>, revision: Option<FileRevision>) -> Node {
        Node {
            kind: Some(node::Kind::File(FileNode {
                uid: "f".into(),
                parent_uid: "p".into(),
                name: name_value,
                media_type: String::new(),
                creation_time: Some(Timestamp { seconds: 10, nanos: 0 }),
                trash_time: None,
                active_revision: revision,
                total_storage_size: 99,
            })),
        }
    }

    fn revision() -> FileRevision {
        FileRevision {
            uid: "rev".into(),
            creation_time: Some(Timestamp { seconds: 20, nanos: 0 }),
            storage_size: 50,
            claimed_size: Some(42),
            claimed_modification_time: Some(Timestamp { seconds: 30, nanos: 500 }),
            thumbnails: vec![],
        }
    }

    #[test]
    fn file_conversion_prefers_claimed_values() {
        let info = NodeInfo::from_node(sdk_file(name("a.txt"), Some(revision()))).unwrap();
        assert_eq!((info.uid.as_str(), info.name.as_str(), info.parent_uid.as_deref()), ("f", "a.txt", Some("p")));
        assert_eq!(info.mtime, UNIX_EPOCH + Duration::new(30, 500));
        match info.kind {
            NodeKind::File { size, revision_uid, has_thumbnail } => assert_eq!((size, revision_uid.as_str(), has_thumbnail), (42, "rev", false)),
            NodeKind::Folder => panic!("a file"),
        }
    }

    #[test]
    fn file_conversion_falls_back_on_storage_size_and_creation_times() {
        let mut rev = revision();
        rev.claimed_size = None;
        rev.claimed_modification_time = None;
        let info = NodeInfo::from_node(sdk_file(name("a"), Some(rev.clone()))).unwrap();
        assert_eq!(info.mtime, mtime(20));
        assert!(matches!(info.kind, NodeKind::File { size: 50, .. }));
        rev.creation_time = None;
        assert_eq!(NodeInfo::from_node(sdk_file(name("a"), Some(rev))).unwrap().mtime, mtime(10));
    }

    #[test]
    fn negative_sizes_and_times_are_clamped() {
        let mut rev = revision();
        rev.claimed_size = Some(-5);
        rev.claimed_modification_time = Some(Timestamp { seconds: -1, nanos: -1 });
        let info = NodeInfo::from_node(sdk_file(name("a"), Some(rev))).unwrap();
        assert!(matches!(info.kind, NodeKind::File { size: 0, .. }));
        assert_eq!(info.mtime, UNIX_EPOCH);
    }

    #[test]
    fn thumbnail_flag_only_counts_small_thumbnails() {
        let header = |kind: ThumbnailType| ThumbnailHeader { id: "t".into(), r#type: kind as i32 };
        let mut rev = revision();
        rev.thumbnails = vec![header(ThumbnailType::Preview)];
        let info = NodeInfo::from_node(sdk_file(name("a"), Some(rev.clone()))).unwrap();
        assert!(matches!(info.kind, NodeKind::File { has_thumbnail: false, .. }));
        rev.thumbnails.push(header(ThumbnailType::Thumbnail));
        let info = NodeInfo::from_node(sdk_file(name("a"), Some(rev))).unwrap();
        assert!(matches!(info.kind, NodeKind::File { has_thumbnail: true, .. }));
    }

    #[test]
    fn unusable_nodes_are_not_exposed() {
        // No active revision.
        assert!(NodeInfo::from_node(sdk_file(name("a"), None)).is_none());
        // Undecryptable or missing name.
        assert!(NodeInfo::from_node(sdk_file(None, Some(revision()))).is_none());
        assert!(NodeInfo::from_node(sdk_file(Some(StringResult { result: None }), Some(revision()))).is_none());
        // Names that cannot exist in a POSIX directory.
        for bad in ["", ".", "..", "a/b"] {
            assert!(NodeInfo::from_node(sdk_file(name(bad), Some(revision()))).is_none(), "{bad:?}");
        }
        // Trashed.
        let mut trashed = sdk_file(name("a"), Some(revision()));
        if let Some(node::Kind::File(f)) = &mut trashed.kind {
            f.trash_time = Some(Timestamp { seconds: 1, nanos: 0 });
        }
        assert!(NodeInfo::from_node(trashed).is_none());
        assert!(NodeInfo::from_node(Node { kind: None }).is_none());
    }

    #[test]
    fn folder_conversion() {
        let sdk = |trash: Option<Timestamp>, parent: Option<&str>| Node {
            kind: Some(node::Kind::Folder(FolderNode {
                uid: "d".into(),
                parent_uid: parent.map(Into::into),
                tree_event_scope_id: String::new(),
                name: name("docs"),
                creation_time: Some(Timestamp { seconds: 7, nanos: 0 }),
                trash_time: trash,
            })),
        };
        let info = NodeInfo::from_node(sdk(None, Some("p"))).unwrap();
        assert!(matches!(info.kind, NodeKind::Folder));
        assert_eq!((info.name.as_str(), info.parent_uid.as_deref(), info.mtime), ("docs", Some("p"), mtime(7)));
        assert!(NodeInfo::from_node(sdk(None, None)).unwrap().parent_uid.is_none());
        assert!(NodeInfo::from_node(sdk(Some(Timestamp { seconds: 1, nanos: 0 }), Some("p"))).is_none());
    }

    #[test]
    fn local_uids_are_recognised() {
        assert!(is_local("local:12"));
        assert!(!is_local("volume~node"));
    }

    // --- Tree -----------------------------------------------------------------------------

    #[test]
    fn new_tree_has_an_unloaded_root() {
        let tree = Tree::new("root".into());
        assert_eq!(tree.ino_of("root"), Some(ROOT_INO));
        assert!(!tree.is_loaded(ROOT_INO));
        assert!(tree.children(ROOT_INO).is_none());
        assert_eq!(tree.path_of(ROOT_INO), Some(PathBuf::new()));
    }

    #[test]
    fn listing_a_folder_assigns_inodes_and_allows_lookups() {
        let tree = tree_with(vec![folder("d", "docs"), file("f", "a.txt", "r1")]);
        assert!(tree.is_loaded(ROOT_INO));
        assert_eq!(names(&tree, ROOT_INO), ["a.txt", "docs"]);
        let ino = tree.child(ROOT_INO, "a.txt").unwrap();
        assert_ne!(ino, ROOT_INO);
        assert_eq!(tree.ino_of("f"), Some(ino));
        assert_eq!(tree.get(ino).unwrap().uid, "f");
        assert!(tree.child(ROOT_INO, "missing").is_none());
        // A file has no children.
        assert!(tree.children(ino).is_none() && !tree.is_loaded(ino));
    }

    #[test]
    fn relisting_keeps_inodes_and_drops_vanished_nodes() {
        let mut tree = tree_with(vec![file("f", "a", "r1"), file("g", "b", "r1")]);
        let a = tree.ino_of("f").unwrap();
        tree.set_children(ROOT_INO, vec![file("f", "a2", "r2"), file("h", "c", "r1")]);
        assert_eq!(tree.ino_of("f"), Some(a));
        assert_eq!(tree.get(a).unwrap().name, "a2");
        assert!(matches!(&tree.get(a).unwrap().kind, EntryKind::File { revision_uid, .. } if revision_uid == "r2"));
        assert!(tree.ino_of("g").is_none());
        assert_eq!(names(&tree, ROOT_INO), ["a2", "c"]);
    }

    #[test]
    fn relisting_keeps_local_files_not_uploaded_yet() {
        let mut tree = tree_with(vec![file("f", "a", "r1")]);
        let local = tree.insert_local_file(ROOT_INO, "new.txt".into(), mtime(5));
        assert!(is_local(&tree.get(local).unwrap().uid));
        assert_eq!(tree.ino_of(&tree.get(local).unwrap().uid.clone()), Some(local));
        tree.set_children(ROOT_INO, vec![file("f", "a", "r1")]);
        assert_eq!(names(&tree, ROOT_INO), ["a", "new.txt"]);
    }

    #[test]
    fn relisting_removes_descendants_of_vanished_folders() {
        let mut tree = tree_with(vec![folder("d", "docs")]);
        let docs = tree.ino_of("d").unwrap();
        tree.set_children(docs, vec![file("f", "a", "r"), folder("e", "sub")]);
        let sub = tree.ino_of("e").unwrap();
        tree.set_children(sub, vec![file("g", "deep", "r")]);
        tree.set_children(ROOT_INO, vec![]);
        for uid in ["d", "f", "e", "g"] {
            assert!(tree.ino_of(uid).is_none(), "{uid}");
        }
        assert!(tree.get(docs).is_none() && tree.get(sub).is_none());
    }

    #[test]
    fn node_moved_between_loaded_folders_leaves_the_old_listing() {
        let mut tree = tree_with(vec![folder("d1", "one"), folder("d2", "two")]);
        let (one, two) = (tree.ino_of("d1").unwrap(), tree.ino_of("d2").unwrap());
        tree.set_children(one, vec![file("f", "a", "r")]);
        let a = tree.ino_of("f").unwrap();
        tree.set_children(two, vec![file("f", "a", "r")]);
        assert!(tree.children(one).unwrap().is_empty());
        assert_eq!(tree.children(two).unwrap(), [a]);
        assert_eq!(tree.get(a).unwrap().parent, two);
    }

    #[test]
    fn relisting_keeps_the_listing_of_loaded_subfolders() {
        let mut tree = tree_with(vec![folder("d", "docs")]);
        let docs = tree.ino_of("d").unwrap();
        tree.set_children(docs, vec![file("f", "a", "r")]);
        tree.set_children(ROOT_INO, vec![folder("d", "docs")]);
        assert!(tree.is_loaded(docs));
    }

    #[test]
    fn paths_and_lookups_by_path() {
        let mut tree = tree_with(vec![folder("d", "docs")]);
        let docs = tree.ino_of("d").unwrap();
        tree.set_children(docs, vec![file("f", "a.txt", "r")]);
        let a = tree.ino_of("f").unwrap();
        assert_eq!(tree.path_of(a), Some(PathBuf::from("docs/a.txt")));
        assert_eq!(tree.lookup_path(std::path::Path::new("docs/a.txt")), Some(a));
        assert_eq!(tree.lookup_path(std::path::Path::new("")), Some(ROOT_INO));
        assert!(tree.lookup_path(std::path::Path::new("docs/none")).is_none());
        // Unloaded folders are not searched.
        let mut tree = tree_with(vec![folder("d", "docs")]);
        assert!(tree.lookup_path(std::path::Path::new("docs/a.txt")).is_none());
        let docs = tree.ino_of("d").unwrap();
        tree.remove_ino(docs);
        assert!(tree.path_of(docs).is_none());
    }

    #[test]
    fn size_and_mtime_setters() {
        let mut tree = tree_with(vec![file("f", "a", "r"), folder("d", "docs")]);
        let (a, d) = (tree.ino_of("f").unwrap(), tree.ino_of("d").unwrap());
        tree.set_file_size(a, 77);
        tree.set_mtime(a, mtime(9));
        assert!(matches!(tree.get(a).unwrap().kind, EntryKind::File { size: 77, .. }));
        assert_eq!(tree.get(a).unwrap().mtime, mtime(9));
        // Sizes only apply to files; unknown inodes are ignored.
        tree.set_file_size(d, 5);
        assert!(matches!(tree.get(d).unwrap().kind, EntryKind::Folder { .. }));
        tree.set_file_size(999, 5);
        tree.set_mtime(999, mtime(1));
    }

    #[test]
    fn local_file_becomes_remote_after_upload() {
        let mut tree = tree_with(vec![]);
        let ino = tree.insert_local_file(ROOT_INO, "new".into(), mtime(5));
        let local_uid = tree.get(ino).unwrap().uid.clone();
        assert!(matches!(&tree.get(ino).unwrap().kind, EntryKind::File { size: 0, revision_uid, .. } if revision_uid.is_empty()));
        tree.set_uploaded(ino, "vol~node".into(), "vol~node~rev".into());
        assert!(tree.ino_of(&local_uid).is_none());
        assert_eq!(tree.ino_of("vol~node"), Some(ino));
        assert!(!is_local(&tree.get(ino).unwrap().uid));
        assert!(matches!(&tree.get(ino).unwrap().kind, EntryKind::File { revision_uid, .. } if revision_uid == "vol~node~rev"));
        // A new revision of the same node only changes the revision.
        tree.set_uploaded(ino, "vol~node".into(), "rev2".into());
        assert!(matches!(&tree.get(ino).unwrap().kind, EntryKind::File { revision_uid, .. } if revision_uid == "rev2"));
        tree.set_uploaded(999, "x".into(), "y".into());
    }

    #[test]
    fn local_inodes_are_never_reused() {
        let mut tree = tree_with(vec![]);
        let a = tree.insert_local_file(ROOT_INO, "a".into(), mtime(1));
        tree.remove_ino(a);
        let b = tree.insert_local_file(ROOT_INO, "a".into(), mtime(1));
        assert_ne!(a, b);
    }

    #[test]
    fn move_entry_renames_and_reparents() {
        let mut tree = tree_with(vec![folder("d", "docs"), file("f", "a", "r")]);
        let (docs, a) = (tree.ino_of("d").unwrap(), tree.ino_of("f").unwrap());
        tree.set_children(docs, vec![]);
        tree.move_entry(a, docs, "b".into());
        assert_eq!(names(&tree, ROOT_INO), ["docs"]);
        assert_eq!(names(&tree, docs), ["b"]);
        assert_eq!(tree.get(a).unwrap().parent, docs);
        assert_eq!(tree.path_of(a), Some(PathBuf::from("docs/b")));
        // Rename within a folder.
        tree.move_entry(a, docs, "c".into());
        assert_eq!(names(&tree, docs), ["c"]);
        tree.move_entry(999, docs, "x".into());
    }

    #[test]
    fn remove_ino_removes_the_subtree_and_the_listing_entry() {
        let mut tree = tree_with(vec![folder("d", "docs")]);
        let docs = tree.ino_of("d").unwrap();
        tree.set_children(docs, vec![file("f", "a", "r")]);
        let a = tree.ino_of("f").unwrap();
        tree.remove_ino(docs);
        assert!(tree.children(ROOT_INO).unwrap().is_empty());
        assert!(tree.get(docs).is_none() && tree.get(a).is_none());
        assert!(tree.ino_of("d").is_none() && tree.ino_of("f").is_none());
        tree.remove_ino(docs);
    }

    #[test]
    fn apply_update_adds_a_new_node() {
        let mut tree = tree_with(vec![]);
        let invalidations = tree.apply_update(ROOT_INO, file("f", "a", "r"));
        let ino = tree.ino_of("f").unwrap();
        assert_eq!(tree.children(ROOT_INO).unwrap(), [ino]);
        assert_eq!(invalidations.len(), 2);
        assert!(is_entry(&invalidations[0], ROOT_INO, "a") && is_inode(&invalidations[1], ROOT_INO));
    }

    #[test]
    fn apply_update_into_an_unloaded_folder_does_not_invent_a_listing() {
        let mut tree = Tree::new("root".into());
        tree.apply_update(ROOT_INO, file("f", "a", "r"));
        assert!(!tree.is_loaded(ROOT_INO));
        assert!(tree.ino_of("f").is_some());
    }

    #[test]
    fn apply_update_new_revision_invalidates_the_inode_only() {
        let mut tree = tree_with(vec![file("f", "a", "r1")]);
        let a = tree.ino_of("f").unwrap();
        let invalidations = tree.apply_update(ROOT_INO, file("f", "a", "r2"));
        assert_eq!(invalidations.len(), 1);
        assert!(is_inode(&invalidations[0], a));
        // Same revision: nothing to invalidate.
        assert!(tree.apply_update(ROOT_INO, file("f", "a", "r2")).is_empty());
        assert_eq!(tree.children(ROOT_INO).unwrap(), [a]);
    }

    #[test]
    fn apply_update_rename_invalidates_old_and_new_names() {
        let mut tree = tree_with(vec![file("f", "old", "r")]);
        let a = tree.ino_of("f").unwrap();
        let invalidations = tree.apply_update(ROOT_INO, file("f", "new", "r"));
        assert_eq!(invalidations.len(), 4);
        assert!(is_entry(&invalidations[0], ROOT_INO, "old"));
        assert!(is_entry(&invalidations[2], ROOT_INO, "new"));
        assert_eq!(tree.child(ROOT_INO, "new"), Some(a));
        assert!(tree.child(ROOT_INO, "old").is_none());
        assert_eq!(tree.children(ROOT_INO).unwrap().len(), 1);
    }

    #[test]
    fn apply_update_move_between_folders() {
        let mut tree = tree_with(vec![folder("d1", "one"), folder("d2", "two")]);
        let (one, two) = (tree.ino_of("d1").unwrap(), tree.ino_of("d2").unwrap());
        tree.set_children(one, vec![file("f", "a", "r")]);
        tree.set_children(two, vec![]);
        let a = tree.ino_of("f").unwrap();
        let invalidations = tree.apply_update(two, file("f", "a", "r"));
        assert!(invalidations.iter().any(|i| is_entry(i, one, "a")) && invalidations.iter().any(|i| is_inode(i, one)));
        assert!(invalidations.iter().any(|i| is_entry(i, two, "a")) && invalidations.iter().any(|i| is_inode(i, two)));
        assert!(tree.children(one).unwrap().is_empty());
        assert_eq!(tree.children(two).unwrap(), [a]);
        assert_eq!(tree.ino_of("f"), Some(a));
    }

    #[test]
    fn apply_update_replaces_a_node_with_the_same_name() {
        let mut tree = tree_with(vec![file("f", "a", "r1")]);
        let old = tree.ino_of("f").unwrap();
        tree.apply_update(ROOT_INO, file("g", "a", "r2"));
        assert!(tree.get(old).is_none() && tree.ino_of("f").is_none());
        let new = tree.ino_of("g").unwrap();
        assert_eq!(tree.children(ROOT_INO).unwrap(), [new]);
    }

    #[test]
    fn apply_update_of_a_folder_keeps_its_loaded_listing() {
        let mut tree = tree_with(vec![folder("d", "docs")]);
        let docs = tree.ino_of("d").unwrap();
        tree.set_children(docs, vec![file("f", "a", "r")]);
        tree.apply_update(ROOT_INO, folder("d", "documents"));
        assert!(tree.is_loaded(docs));
        assert_eq!(tree.get(docs).unwrap().name, "documents");
        assert_eq!(names(&tree, docs), ["a"]);
    }

    #[test]
    fn remove_uid_invalidates_and_ignores_unknown_or_root() {
        let mut tree = tree_with(vec![file("f", "a", "r")]);
        let invalidations = tree.remove_uid("f");
        assert_eq!(invalidations.len(), 2);
        assert!(is_entry(&invalidations[0], ROOT_INO, "a") && is_inode(&invalidations[1], ROOT_INO));
        assert!(tree.children(ROOT_INO).unwrap().is_empty());
        assert!(tree.remove_uid("f").is_empty());
        assert!(tree.remove_uid("root").is_empty());
        assert!(tree.ino_of("root").is_some());
    }

    #[test]
    fn reset_listings_invalidates_loaded_folders_only() {
        let mut tree = tree_with(vec![folder("d", "docs"), folder("e", "other")]);
        let docs = tree.ino_of("d").unwrap();
        tree.set_children(docs, vec![]);
        let mut invalidated: Vec<u64> = tree.reset_listings().iter().map(|i| if let Invalidation::Inode(n) = i { *n } else { panic!("inode only") }).collect();
        invalidated.sort();
        assert_eq!(invalidated, [ROOT_INO, docs]);
        assert!(!tree.is_loaded(ROOT_INO) && !tree.is_loaded(docs));
        // Nodes themselves are kept, inodes stay stable.
        assert_eq!(tree.ino_of("d"), Some(docs));
        assert!(tree.reset_listings().is_empty());
    }
}
