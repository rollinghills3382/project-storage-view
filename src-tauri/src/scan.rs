//! Parallel directory walk that turns a drive into a compact in-memory tree.

use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// Files smaller than this are folded into one "small files" entry per folder.
/// Keeps the tree to a few hundred thousand nodes even on drives with millions of files.
pub const SMALL_FILE_LIMIT: u64 = 1024 * 1024;

pub const NO_PARENT: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    SmallFiles { count: u32 },
}

#[derive(Debug)]
pub struct Node {
    pub name: Box<str>,
    pub size: u64,
    pub parent: u32,
    /// Sorted by size, largest first.
    pub children: Vec<u32>,
    pub kind: NodeKind,
}

/// Nodes are stored in pre-order, so a child's id is always greater than its parent's.
#[derive(Debug)]
pub struct Tree {
    pub root_path: PathBuf,
    pub nodes: Vec<Node>,
}

impl Tree {
    pub const ROOT: u32 = 0;

    pub fn node(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn path(&self, id: u32) -> PathBuf {
        let mut names = Vec::new();
        let mut cur = id;
        while cur != Self::ROOT {
            let n = self.node(cur);
            names.push(&*n.name);
            cur = n.parent;
        }
        let mut p = self.root_path.clone();
        for name in names.iter().rev() {
            p.push(name);
        }
        p
    }

    /// Case-insensitive lookup of a directory or file by absolute path.
    pub fn find(&self, path: &Path) -> Option<u32> {
        let root = self.root_path.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
        let full = path.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
        let rest = full.strip_prefix(&root)?;
        if !(rest.is_empty() || rest.starts_with(['\\', '/'])) {
            return None;
        }
        let mut cur = Self::ROOT;
        for part in rest.split(['\\', '/']).filter(|s| !s.is_empty()) {
            cur = *self
                .node(cur)
                .children
                .iter()
                .find(|&&c| {
                    let n = self.node(c);
                    // `part` is already lower case, so the name is folded as it is compared.
                    // This runs once per sibling per lookup, and a drive's top-level folders
                    // have hundreds of siblings, so no string is built here.
                    !matches!(n.kind, NodeKind::SmallFiles { .. })
                        && n.name.chars().flat_map(char::to_lowercase).eq(part.chars())
                })?;
        }
        Some(cur)
    }

    pub fn file_count(&self) -> u64 {
        self.nodes
            .iter()
            .map(|n| match n.kind {
                NodeKind::File => 1,
                NodeKind::SmallFiles { count } => count as u64,
                NodeKind::Dir => 0,
            })
            .sum()
    }
}

/// Live counters shared with the UI while a scan runs.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub denied: AtomicU64,
    pub current: Mutex<String>,
    pub cancel: AtomicBool,
    /// Tests cancel deterministically once this many files have been counted, which is
    /// how they land a cancellation in the middle of one folder's listing.
    #[cfg(test)]
    pub cancel_after_files: Option<u64>,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        #[cfg(test)]
        if self.cancel_after_files.is_some_and(|n| self.files.load(Relaxed) >= n) {
            return true;
        }
        self.cancel.load(Relaxed)
    }
}

#[derive(Default)]
struct TmpDir {
    name: String,
    files: Vec<(String, u64)>,
    small_count: u32,
    small_size: u64,
    dirs: Vec<TmpDir>,
}

pub fn scan(root: &Path, progress: &Progress) -> Tree {
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(16 << 20)
        .build()
        .expect("failed to start scan threads");
    let tmp = pool.install(|| walk(root, root.display().to_string(), progress));
    let mut nodes = Vec::new();
    flatten(tmp, NO_PARENT, &mut nodes);
    Tree { root_path: root.to_path_buf(), nodes }
}

fn walk(path: &Path, name: String, p: &Progress) -> TmpDir {
    let mut out = TmpDir { name, ..Default::default() };
    if p.cancelled() {
        return out;
    }
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => {
            p.denied.fetch_add(1, Relaxed);
            return out;
        }
    };
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        // Checked per entry, not just per folder: one folder can hold hundreds of
        // thousands of files, and Stop should not wait for all of them.
        if p.cancelled() {
            return out;
        }
        let Ok(ft) = entry.file_type() else { continue };
        // Junctions and symlinks point at data that is counted where it really lives.
        if ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            subdirs.push((entry.path(), name));
            continue;
        }
        // On Windows this comes from the directory listing itself, with no extra file open.
        let Ok(md) = entry.metadata() else { continue };
        let size = size_on_disk(&md);
        p.files.fetch_add(1, Relaxed);
        p.bytes.fetch_add(size, Relaxed);
        if size < SMALL_FILE_LIMIT {
            out.small_count += 1;
            out.small_size += size;
        } else {
            out.files.push((name, size));
        }
    }
    if p.dirs.fetch_add(1, Relaxed).is_multiple_of(256) {
        if let Ok(mut cur) = p.current.try_lock() {
            *cur = path.display().to_string();
        }
    }
    out.dirs = subdirs.into_par_iter().map(|(sp, sn)| walk(&sp, sn, p)).collect();
    out
}

/// Cloud-only placeholders (OneDrive "files on demand") report their full size but use no disk space.
#[cfg(windows)]
fn size_on_disk(md: &fs::Metadata) -> u64 {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    if md.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0 {
        0
    } else {
        md.len()
    }
}

#[cfg(not(windows))]
fn size_on_disk(md: &fs::Metadata) -> u64 {
    md.len()
}

fn flatten(tmp: TmpDir, parent: u32, nodes: &mut Vec<Node>) -> u32 {
    let id = nodes.len() as u32;
    nodes.push(Node { name: tmp.name.into(), size: 0, parent, children: Vec::new(), kind: NodeKind::Dir });
    let mut children = Vec::with_capacity(tmp.dirs.len() + tmp.files.len() + 1);
    for d in tmp.dirs {
        children.push(flatten(d, id, nodes));
    }
    for (name, size) in tmp.files {
        children.push(push_leaf(nodes, id, name, size, NodeKind::File));
    }
    if tmp.small_count > 0 {
        let name = format!("{} small files", tmp.small_count);
        children.push(push_leaf(nodes, id, name, tmp.small_size, NodeKind::SmallFiles { count: tmp.small_count }));
    }
    children.sort_by(|&a, &b| nodes[b as usize].size.cmp(&nodes[a as usize].size));
    nodes[id as usize].size = children.iter().map(|&c| nodes[c as usize].size).sum();
    nodes[id as usize].children = children;
    id
}

fn push_leaf(nodes: &mut Vec<Node>, parent: u32, name: String, size: u64, kind: NodeKind) -> u32 {
    nodes.push(Node { name: name.into(), size, parent, children: Vec::new(), kind });
    (nodes.len() - 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn sizes_roll_up_and_small_files_are_folded() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("big.bin"), 3 * SMALL_FILE_LIMIT as usize);
        write(&r.join("a/one.txt"), 10);
        write(&r.join("a/two.txt"), 20);
        write(&r.join("a/b/large.bin"), 2 * SMALL_FILE_LIMIT as usize);

        let tree = scan(r, &Progress::default());
        let root = tree.node(Tree::ROOT);
        assert_eq!(root.size, 5 * SMALL_FILE_LIMIT + 30);
        assert_eq!(tree.file_count(), 4);

        // Largest child first.
        assert_eq!(&*tree.node(root.children[0]).name, "big.bin");

        let a = tree.find(&r.join("A")).expect("case-insensitive lookup");
        let small = tree.node(a).children.iter().map(|&c| tree.node(c)).find(|n| matches!(n.kind, NodeKind::SmallFiles { .. })).unwrap();
        assert_eq!(small.kind, NodeKind::SmallFiles { count: 2 });
        assert_eq!(small.size, 30);

        let large = tree.find(&r.join("a").join("b").join("large.bin")).unwrap();
        assert_eq!(tree.path(large), r.join("a").join("b").join("large.bin"));
    }

    #[test]
    fn find_rejects_paths_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let tree = scan(dir.path(), &Progress::default());
        assert_eq!(tree.find(&dir.path().join("missing")), None);
        let sibling = PathBuf::from(format!("{}x", dir.path().display()));
        assert_eq!(tree.find(&sibling), None);
    }

    /// Lookups fold case the way `to_lowercase` does, not just for ASCII: Windows treats
    /// these names as equal and so must we.
    #[test]
    fn find_folds_case_beyond_ascii() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("Ünïcode/ÄÖÜ/data.bin"), 4 * SMALL_FILE_LIMIT as usize);
        // Big enough to stay its own node rather than being folded into "small files".
        write(&r.join("MiXeD/CaSe.TxT"), 2 * SMALL_FILE_LIMIT as usize);
        let tree = scan(r, &Progress::default());

        let id = tree.find(&r.join("ünïcode").join("äöü")).expect("case-folded lookup");
        assert_eq!(&*tree.node(id).name, "ÄÖÜ");
        let mixed = tree.find(&r.join("mixed").join("case.txt")).expect("mixed-case lookup");
        assert_eq!(&*tree.node(mixed).name, "CaSe.TxT");
        assert_eq!(tree.node(mixed).size, 2 * SMALL_FILE_LIMIT);
    }

    #[test]
    fn cancelled_scan_returns_quickly() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/file.bin"), 10);
        let p = Progress::default();
        p.cancel.store(true, Relaxed);
        let tree = scan(dir.path(), &p);
        assert_eq!(tree.nodes.len(), 1);
    }

    /// A cancel that arrives while one large folder is being listed stops that listing,
    /// rather than waiting for the folder to finish.
    #[test]
    fn cancel_stops_inside_a_large_folder() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..200 {
            write(&dir.path().join(format!("big/{i}.txt")), 1);
        }
        let p = Progress { cancel_after_files: Some(5), ..Default::default() };
        scan(dir.path(), &p);
        assert_eq!(p.files.load(Relaxed), 5, "the listing stops at the next entry once cancelled");
    }
}
