//! Parallel directory walk that turns a drive into a compact in-memory tree.

mod links;
#[cfg(windows)]
mod winlist;

pub use links::LinkReport;
use links::NO_KEY;
use rayon::prelude::*;
use serde::Serialize;
use std::fs;
use std::io;
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
    /// Bytes of hard links in here that are counted under another name of the same file.
    pub linked: u64,
}

/// Nodes are stored in pre-order, so a child's id is always greater than its parent's.
#[derive(Debug)]
pub struct Tree {
    pub root_path: PathBuf,
    pub nodes: Vec<Node>,
    pub links: LinkReport,
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
    pub errors: ErrorLog,
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

/// The step of the walk that failed, which decides how an error is described when its
/// cause does not say more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Opening a folder to list it.
    OpenDir,
    /// Reading the next entry of a folder that did open.
    ListDir,
    FileType,
    Metadata,
    /// Measuring how much of a cloud file is stored locally.
    CloudSize,
}

/// What went wrong reading one path, as the user is told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// Windows refused access. The only kind that running as administrator can help with.
    Denied,
    /// The path was deleted or moved while the scan ran, so there is nothing to count.
    Vanished,
    OpenDir,
    ListDir,
    FileType,
    Metadata,
    CloudSize,
}

impl Problem {
    pub fn classify(stage: Stage, err: &io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::PermissionDenied => return Problem::Denied,
            io::ErrorKind::NotFound => return Problem::Vanished,
            _ => {}
        }
        // ERROR_DELETE_PENDING: deleted, but another program still has it open.
        #[cfg(windows)]
        if err.raw_os_error() == Some(303) {
            return Problem::Vanished;
        }
        match stage {
            Stage::OpenDir => Problem::OpenDir,
            Stage::ListDir => Problem::ListDir,
            Stage::FileType => Problem::FileType,
            Stage::Metadata => Problem::Metadata,
            Stage::CloudSize => Problem::CloudSize,
        }
    }
}

/// Paths kept per kind of problem. A drive that fails everywhere still yields a short report,
/// and one kind cannot crowd out the examples of another.
pub const ERROR_SAMPLES_PER_KIND: usize = 20;
const ERROR_MESSAGE_LIMIT: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ErrorCounts {
    pub denied: u64,
    pub vanished: u64,
    pub open_dir: u64,
    pub list_dir: u64,
    pub file_type: u64,
    pub metadata: u64,
    pub cloud_size: u64,
}

impl ErrorCounts {
    fn slot(&mut self, kind: Problem) -> &mut u64 {
        match kind {
            Problem::Denied => &mut self.denied,
            Problem::Vanished => &mut self.vanished,
            Problem::OpenDir => &mut self.open_dir,
            Problem::ListDir => &mut self.list_dir,
            Problem::FileType => &mut self.file_type,
            Problem::Metadata => &mut self.metadata,
            Problem::CloudSize => &mut self.cloud_size,
        }
    }

    pub fn get(&self, kind: Problem) -> u64 {
        match kind {
            Problem::Denied => self.denied,
            Problem::Vanished => self.vanished,
            Problem::OpenDir => self.open_dir,
            Problem::ListDir => self.list_dir,
            Problem::FileType => self.file_type,
            Problem::Metadata => self.metadata,
            Problem::CloudSize => self.cloud_size,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorSample {
    pub kind: Problem,
    pub path: String,
    pub message: String,
}

/// Everything that could not be read, for the scan summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ErrorReport {
    pub counts: ErrorCounts,
    pub samples: Vec<ErrorSample>,
}

/// Collects read failures while the scan runs. Failures are rare enough that one lock is
/// cheaper than it looks, and it keeps each count and its samples consistent.
#[derive(Default)]
pub struct ErrorLog {
    report: Mutex<ErrorReport>,
}

impl ErrorLog {
    pub fn record(&self, stage: Stage, path: &Path, err: &io::Error) -> Problem {
        let kind = Problem::classify(stage, err);
        let mut r = self.report.lock().unwrap_or_else(|e| e.into_inner());
        let n = r.counts.slot(kind);
        *n += 1;
        if *n as usize <= ERROR_SAMPLES_PER_KIND {
            let mut message = err.to_string();
            if message.len() > ERROR_MESSAGE_LIMIT {
                let mut end = ERROR_MESSAGE_LIMIT;
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
                message.push('…');
            }
            r.samples.push(ErrorSample { kind, path: path.display().to_string(), message });
        }
        kind
    }

    pub fn count(&self, kind: Problem) -> u64 {
        self.report.lock().unwrap_or_else(|e| e.into_inner()).counts.get(kind)
    }

    pub fn report(&self) -> ErrorReport {
        self.report.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[derive(Default)]
struct TmpDir {
    name: String,
    files: Vec<TmpFile>,
    small_count: u32,
    small_size: u64,
    /// A packed key and size for each small file with a key, until hard links are resolved.
    small_keys: Box<[u64]>,
    /// Bytes of small files here that are counted under another name of the same file.
    small_linked: u64,
    dirs: Vec<TmpDir>,
}

/// A file of at least `SMALL_FILE_LIMIT` bytes.
struct TmpFile {
    name: String,
    size: u64,
    /// The file's ID, or `NO_KEY`, until hard links are resolved.
    key: u64,
    /// Bytes counted under another name of the same file instead of this one.
    linked: u64,
}

/// How a folder is listed.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// `std::fs::read_dir`. Gives file IDs only on Unix, where tests run.
    Std,
    /// Windows directory queries that return each entry's file ID, so hard links of one file
    /// can be recognized without opening anything.
    #[cfg(windows)]
    Ids(winlist::IdClass),
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub listing: Listing,
    /// Count a file with several hard links once.
    pub count_links_once: bool,
    /// Why hard links are not looked for, when they are not.
    pub skipped_because: Option<&'static str>,
    /// Most file IDs kept at once.
    pub max_keys: u64,
}

impl ScanOptions {
    /// What the drive or folder at `root` supports.
    pub fn for_root(root: &Path) -> Self {
        #[cfg(windows)]
        let (listing, skipped_because) = match winlist::id_class(root) {
            Ok(class) => (Listing::Ids(class), None),
            Err(why) => (Listing::Std, Some(why)),
        };
        #[cfg(not(windows))]
        let (listing, skipped_because) = {
            let _ = root;
            (Listing::Std, None)
        };
        ScanOptions { listing, count_links_once: skipped_because.is_none(), skipped_because, max_keys: links::MAX_KEYS }
    }
}

/// What every folder of one walk shares.
struct Walk<'a> {
    p: &'a Progress,
    listing: Listing,
    keys: bool,
    max_keys: u64,
    kept: AtomicU64,
    /// Set once more keys were found than `max_keys`. From then on none are kept, and the
    /// scan does not look for hard links at all, so which files are checked never depends on
    /// which thread got there first.
    too_many: AtomicBool,
    unchecked: AtomicU64,
}

pub fn scan(root: &Path, progress: &Progress) -> Tree {
    scan_with(root, progress, ScanOptions::for_root(root))
}

#[doc(hidden)]
pub fn scan_with(root: &Path, progress: &Progress, options: ScanOptions) -> Tree {
    let pool = rayon::ThreadPoolBuilder::new()
        .stack_size(16 << 20)
        .build()
        .expect("failed to start scan threads");
    let w = Walk {
        p: progress,
        listing: options.listing,
        keys: options.count_links_once,
        max_keys: options.max_keys,
        kept: AtomicU64::new(0),
        too_many: AtomicBool::new(false),
        unchecked: AtomicU64::new(0),
    };
    let mut tmp = pool.install(|| walk(root, root.display().to_string(), &w));
    let too_many = w.too_many.into_inner();
    let mut links = match (options.count_links_once, too_many) {
        (true, false) => LinkReport { checked: true, unchecked: w.unchecked.into_inner(), ..Default::default() },
        (true, true) => LinkReport { skipped_because: Some("too many files"), ..Default::default() },
        (false, _) => LinkReport { skipped_because: options.skipped_because, ..Default::default() },
    };
    if links.checked && !progress.cancelled() {
        let kept = w.kept.into_inner();
        pool.install(|| links::count_once(&mut tmp, kept, &mut links, &|| progress.cancelled()));
    }
    let mut nodes = Vec::new();
    flatten(tmp, NO_PARENT, &mut nodes);
    Tree { root_path: root.to_path_buf(), nodes, links }
}

/// What one folder's listing has found so far.
struct Folder {
    out: TmpDir,
    subdirs: Vec<(PathBuf, String)>,
    small_keys: Vec<u64>,
}

impl Folder {
    fn add_file(&mut self, w: &Walk, name: String, size: u64, key: u64) {
        w.p.files.fetch_add(1, Relaxed);
        w.p.bytes.fetch_add(size, Relaxed);
        // An empty file adds nothing, whatever names it has.
        let key = if size == 0 { NO_KEY } else { key };
        if w.keys && size > 0 && key == NO_KEY {
            w.unchecked.fetch_add(1, Relaxed);
        }
        if size < SMALL_FILE_LIMIT {
            self.out.small_count += 1;
            self.out.small_size += size;
            if key != NO_KEY {
                self.small_keys.push(links::pack(key, size));
            }
        } else {
            self.out.files.push(TmpFile { name, size, key, linked: 0 });
        }
    }

    /// Keeps this folder's keys while the walk is under the cap. Counted per folder rather
    /// than per file, so threads rarely touch the shared counter.
    fn keep_keys(&mut self, w: &Walk) {
        let n = self.small_keys.len() as u64 + self.out.files.iter().filter(|f| f.key != NO_KEY).count() as u64;
        if n == 0 {
            return;
        }
        if w.too_many.load(Relaxed) || w.kept.fetch_add(n, Relaxed) + n > w.max_keys {
            w.too_many.store(true, Relaxed);
            self.small_keys = Vec::new();
            self.out.files.iter_mut().for_each(|f| f.key = NO_KEY);
            return;
        }
        self.out.small_keys = std::mem::take(&mut self.small_keys).into_boxed_slice();
    }
}

/// How a listing ended.
enum Listed {
    Done,
    Cancelled,
    /// The file system does not answer the query that returns file IDs.
    #[cfg(windows)]
    Unsupported,
}

fn walk(path: &Path, name: String, w: &Walk) -> TmpDir {
    let mut f = Folder { out: TmpDir { name, ..Default::default() }, subdirs: Vec::new(), small_keys: Vec::new() };
    if w.p.cancelled() {
        return f.out;
    }
    let listed = match w.listing {
        Listing::Std => list_std(path, w, &mut f),
        #[cfg(windows)]
        Listing::Ids(class) => match list_ids(path, class, w, &mut f) {
            Listed::Unsupported => list_std(path, w, &mut f),
            done => done,
        },
    };
    if let Listed::Cancelled = listed {
        return f.out;
    }
    f.keep_keys(w);
    if w.p.dirs.fetch_add(1, Relaxed).is_multiple_of(256) {
        if let Ok(mut cur) = w.p.current.try_lock() {
            *cur = path.display().to_string();
        }
    }
    f.out.dirs = f.subdirs.into_par_iter().map(|(sp, sn)| walk(&sp, sn, w)).collect();
    f.out
}

fn list_std(path: &Path, w: &Walk, f: &mut Folder) -> Listed {
    let p = w.p;
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            p.errors.record(Stage::OpenDir, path, &e);
            return Listed::Done;
        }
    };
    for entry in entries {
        // Checked per entry, not just per folder: one folder can hold hundreds of
        // thousands of files, and Stop should not wait for all of them.
        if p.cancelled() {
            return Listed::Cancelled;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // The rest of the listing is lost, but what was read so far still counts.
                // Stop here: a listing that keeps failing could otherwise repeat forever.
                p.errors.record(Stage::ListDir, path, &e);
                break;
            }
        };
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                p.errors.record(Stage::FileType, &entry.path(), &e);
                continue;
            }
        };
        // Junctions and symlinks point at data that is counted where it really lives.
        if ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            f.subdirs.push((entry.path(), name));
            continue;
        }
        // On Windows this comes from the directory listing itself, with no extra file open.
        let md = match entry.metadata() {
            Ok(md) => md,
            Err(e) => {
                p.errors.record(Stage::Metadata, &entry.path(), &e);
                continue;
            }
        };
        let size = match size_on_disk(&entry.path(), &md) {
            Ok(size) => size,
            Err(e) => {
                // The file is listed but its local size is unknown, so it adds nothing rather
                // than a guess, and the summary says how many files that was.
                p.errors.record(Stage::CloudSize, &entry.path(), &e);
                0
            }
        };
        let key = if w.keys { std_key(&md) } else { NO_KEY };
        f.add_file(w, name, size, key);
    }
    Listed::Done
}

#[cfg(unix)]
fn std_key(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    links::inode_key(md.ino())
}

/// std's listing on Windows has no file IDs.
#[cfg(not(unix))]
fn std_key(_md: &fs::Metadata) -> u64 {
    NO_KEY
}

/// Lists through the directory query that also returns file IDs. Everything else the walk
/// uses comes from the same fields FindFirstFileExW reads for std's listing.
#[cfg(windows)]
fn list_ids(path: &Path, class: winlist::IdClass, w: &Walk, f: &mut Folder) -> Listed {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let p = w.p;
    let mut cancelled = false;
    let result = winlist::list_dir(path, class, |e| {
        if p.cancelled() {
            cancelled = true;
            return false;
        }
        // Junctions and symlinks point at data that is counted where it really lives.
        if e.is_link() {
            return true;
        }
        let entry_path = || path.join(OsString::from_wide(e.name));
        let name = String::from_utf16_lossy(e.name);
        if e.is_dir() {
            f.subdirs.push((entry_path(), name));
            return true;
        }
        let size = match resident_size(e.attributes, e.len, || allocated_without_recall(&entry_path())) {
            Ok(size) => size,
            Err(err) => {
                p.errors.record(Stage::CloudSize, &entry_path(), &err);
                0
            }
        };
        // On NTFS the ID's high half is zero and the low half is the file reference number.
        let key = if w.keys && e.id >> 64 == 0 { links::ntfs_key(e.id as u64) } else { NO_KEY };
        f.add_file(w, name, size, key);
        true
    });
    match result {
        _ if cancelled => Listed::Cancelled,
        Ok(()) => Listed::Done,
        Err(winlist::Failed::Open(e)) => {
            p.errors.record(Stage::OpenDir, path, &e);
            Listed::Done
        }
        Err(winlist::Failed::List(e)) => {
            p.errors.record(Stage::ListDir, path, &e);
            Listed::Done
        }
        Err(winlist::Failed::Unsupported) => Listed::Unsupported,
    }
}

const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
const NOT_FULLY_LOCAL: u32 = FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

/// Bytes a file takes on this disk.
///
/// A cloud file (OneDrive "files on demand" and other Cloud Files providers) reports its full
/// size whether or not its data is here. Those attributes only say the file is not *fully*
/// local: a partly downloaded file has some of its data on disk. So such a file is measured
/// with `allocated`, and a fully local one keeps its plain length.
fn resident_size(attributes: u32, len: u64, allocated: impl FnOnce() -> io::Result<u64>) -> io::Result<u64> {
    if attributes & NOT_FULLY_LOCAL == 0 {
        return Ok(len);
    }
    // Allocation is rounded up to whole clusters; a file never counts for more than its length.
    Ok(allocated()?.min(len))
}

#[cfg(windows)]
fn size_on_disk(path: &Path, md: &fs::Metadata) -> io::Result<u64> {
    use std::os::windows::fs::MetadataExt;
    resident_size(md.file_attributes(), md.len(), || allocated_without_recall(path))
}

#[cfg(not(windows))]
fn size_on_disk(_path: &Path, md: &fs::Metadata) -> io::Result<u64> {
    resident_size(0, md.len(), || unreachable!("only Windows has cloud placeholders"))
}

/// Disk space allocated to a cloud file's local data, read without downloading anything.
///
/// The file is opened for `FILE_READ_ATTRIBUTES` only, which grants no access to its data, with
/// `FILE_FLAG_OPEN_NO_RECALL` (Windows' documented flag for "leave the data in remote storage")
/// and `FILE_FLAG_OPEN_REPARSE_POINT`, which opens the placeholder itself so the cloud provider
/// is not asked to act. `GetFileInformationByHandleEx(FileStandardInfo)` then reads the
/// allocation size from file system metadata. None of these read file contents, so the
/// provider sees no data access and hydration and pinning stay as they were.
#[cfg(windows)]
fn allocated_without_recall(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileStandardInfo, GetFileInformationByHandleEx, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives the call, and the handle is
    // closed before returning on every path.
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_NO_RECALL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut info: FILE_STANDARD_INFO = std::mem::zeroed();
        let ok = GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        );
        let err = (ok == 0).then(io::Error::last_os_error);
        CloseHandle(handle);
        match err {
            Some(e) => Err(e),
            None => Ok(info.AllocationSize.max(0) as u64),
        }
    }
}

fn flatten(tmp: TmpDir, parent: u32, nodes: &mut Vec<Node>) -> u32 {
    let id = nodes.len() as u32;
    nodes.push(Node { name: tmp.name.into(), size: 0, parent, children: Vec::new(), kind: NodeKind::Dir, linked: 0 });
    let mut children = Vec::with_capacity(tmp.dirs.len() + tmp.files.len() + 1);
    for d in tmp.dirs {
        children.push(flatten(d, id, nodes));
    }
    for f in tmp.files {
        children.push(push_leaf(nodes, id, f.name, f.size, NodeKind::File, f.linked));
    }
    if tmp.small_count > 0 {
        let name = match tmp.small_count {
            1 => "1 small file".to_string(),
            n => format!("{n} small files"),
        };
        children.push(push_leaf(nodes, id, name, tmp.small_size, NodeKind::SmallFiles { count: tmp.small_count }, tmp.small_linked));
    }
    children.sort_by(|&a, &b| nodes[b as usize].size.cmp(&nodes[a as usize].size));
    nodes[id as usize].size = children.iter().map(|&c| nodes[c as usize].size).sum();
    nodes[id as usize].linked = children.iter().map(|&c| nodes[c as usize].linked).sum();
    nodes[id as usize].children = children;
    id
}

fn push_leaf(nodes: &mut Vec<Node>, parent: u32, name: String, size: u64, kind: NodeKind, linked: u64) -> u32 {
    nodes.push(Node { name: name.into(), size, parent, children: Vec::new(), kind, linked });
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
        write(&r.join("c/only.txt"), 5);

        let tree = scan(r, &Progress::default());
        let root = tree.node(Tree::ROOT);
        assert_eq!(root.size, 5 * SMALL_FILE_LIMIT + 35);
        assert_eq!(tree.file_count(), 5);

        // Largest child first.
        assert_eq!(&*tree.node(root.children[0]).name, "big.bin");

        let a = tree.find(&r.join("A")).expect("case-insensitive lookup");
        let small = tree.node(a).children.iter().map(|&c| tree.node(c)).find(|n| matches!(n.kind, NodeKind::SmallFiles { .. })).unwrap();
        assert_eq!(small.kind, NodeKind::SmallFiles { count: 2 });
        assert_eq!(small.size, 30);
        assert_eq!(&*small.name, "2 small files");

        let c = tree.find(&r.join("c")).unwrap();
        let one = tree.node(tree.node(c).children[0]);
        assert_eq!((one.kind, &*one.name), (NodeKind::SmallFiles { count: 1 }, "1 small file"));

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

    fn plain_walk(p: &Progress) -> Walk<'_> {
        Walk { p, listing: Listing::Std, keys: false, max_keys: 0, kept: AtomicU64::new(0), too_many: AtomicBool::new(false), unchecked: AtomicU64::new(0) }
    }

    fn err(kind: io::ErrorKind) -> io::Error {
        io::Error::new(kind, "test failure")
    }

    #[test]
    fn errors_are_classified_by_cause_before_stage() {
        use Stage::*;
        for stage in [OpenDir, ListDir, FileType, Metadata, CloudSize] {
            assert_eq!(Problem::classify(stage, &err(io::ErrorKind::PermissionDenied)), Problem::Denied);
            assert_eq!(Problem::classify(stage, &err(io::ErrorKind::NotFound)), Problem::Vanished);
        }
        // Anything else is an I/O failure of the step that hit it, never an access denial.
        assert_eq!(Problem::classify(OpenDir, &err(io::ErrorKind::Other)), Problem::OpenDir);
        assert_eq!(Problem::classify(ListDir, &err(io::ErrorKind::UnexpectedEof)), Problem::ListDir);
        assert_eq!(Problem::classify(FileType, &err(io::ErrorKind::InvalidData)), Problem::FileType);
        assert_eq!(Problem::classify(Metadata, &err(io::ErrorKind::TimedOut)), Problem::Metadata);
        assert_eq!(Problem::classify(CloudSize, &err(io::ErrorKind::Unsupported)), Problem::CloudSize);
    }

    #[test]
    fn error_samples_stay_bounded_per_kind() {
        let log = ErrorLog::default();
        for i in 0..1000 {
            log.record(Stage::OpenDir, &PathBuf::from(format!("/denied/{i}")), &err(io::ErrorKind::PermissionDenied));
        }
        log.record(Stage::Metadata, Path::new("/io/file"), &io::Error::other("x".repeat(10_000)));
        let r = log.report();
        assert_eq!(r.counts.denied, 1000);
        assert_eq!(r.counts.metadata, 1);
        assert_eq!(r.samples.len(), ERROR_SAMPLES_PER_KIND + 1);
        // A flood of one kind does not hide the other.
        let io = r.samples.iter().find(|s| s.kind == Problem::Metadata).unwrap();
        assert_eq!(io.path, "/io/file");
        assert!(io.message.len() <= ERROR_MESSAGE_LIMIT + '…'.len_utf8());
    }

    #[test]
    fn a_folder_that_vanished_mid_scan_is_reported_as_vanished() {
        let dir = tempfile::tempdir().unwrap();
        let p = Progress::default();
        let out = walk(&dir.path().join("deleted meanwhile"), "x".into(), &plain_walk(&p));
        assert!(out.files.is_empty() && out.dirs.is_empty() && out.small_count == 0);
        let r = p.errors.report();
        assert_eq!(r.counts, ErrorCounts { vanished: 1, ..Default::default() });
        assert!(r.samples[0].path.ends_with("deleted meanwhile"));
    }

    #[test]
    fn a_folder_that_fails_to_open_for_other_reasons_is_not_called_denied() {
        // Listing a file as a folder fails with "not a directory": a real I/O failure that has
        // nothing to do with permissions.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not a folder");
        write(&file, 10);
        let p = Progress::default();
        walk(&file, "x".into(), &plain_walk(&p));
        let c = p.errors.report().counts;
        assert_eq!(c, ErrorCounts { open_dir: 1, ..Default::default() });
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_leaves_the_rest_of_the_scan_usable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("ok/big.bin"), 2 * SMALL_FILE_LIMIT as usize);
        write(&r.join("locked/hidden.bin"), 10);
        let locked = r.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads through permissions, so the denial only happens for other users.
        let denied = fs::read_dir(&locked).is_err();

        let p = Progress::default();
        let tree = scan(r, &p);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(tree.node(Tree::ROOT).size, 2 * SMALL_FILE_LIMIT + if denied { 0 } else { 10 });
        assert!(tree.find(&r.join("ok/big.bin")).is_some());
        let report = p.errors.report();
        let expected = if denied { ErrorCounts { denied: 1, ..Default::default() } } else { ErrorCounts::default() };
        assert_eq!(report.counts, expected);
        if denied {
            assert_eq!(report.samples[0].path, locked.display().to_string());
        }
    }

    const PLACEHOLDER: u32 = FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

    #[test]
    fn a_fully_remote_cloud_file_counts_nothing() {
        assert_eq!(resident_size(PLACEHOLDER, 5_000_000, || Ok(0)).unwrap(), 0);
        assert_eq!(resident_size(FILE_ATTRIBUTE_OFFLINE, 5_000_000, || Ok(0)).unwrap(), 0);
    }

    #[test]
    fn a_partly_downloaded_cloud_file_counts_its_local_bytes() {
        assert_eq!(resident_size(PLACEHOLDER, 5_000_000, || Ok(1_048_576)).unwrap(), 1_048_576);
        assert_eq!(resident_size(FILE_ATTRIBUTE_RECALL_ON_OPEN, 5_000_000, || Ok(4096)).unwrap(), 4096);
        // Cluster rounding never makes a file count for more than its length.
        assert_eq!(resident_size(PLACEHOLDER, 1000, || Ok(4096)).unwrap(), 1000);
    }

    #[test]
    fn a_fully_local_file_keeps_its_length_without_a_query() {
        let size = resident_size(0x20 /* ARCHIVE */, 5_000_000, || panic!("local files need no extra query"));
        assert_eq!(size.unwrap(), 5_000_000);
    }

    #[test]
    fn a_failed_size_query_is_an_error_not_zero() {
        let e = resident_size(PLACEHOLDER, 5_000_000, || Err(io::Error::other("provider not running"))).unwrap_err();
        assert_eq!(Problem::classify(Stage::CloudSize, &e), Problem::CloudSize);
    }

    fn size_of(tree: &Tree, path: &Path) -> u64 {
        tree.node(tree.find(path).unwrap()).size
    }

    fn linked_of(tree: &Tree, path: &Path) -> u64 {
        tree.node(tree.find(path).unwrap()).linked
    }

    /// One file under three names (WinSxS keeps a name for nearly every file in System32)
    /// takes its space once, in the folder that comes first by name.
    #[test]
    fn a_hard_linked_file_counts_once_in_the_first_folder() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let big = 3 * SMALL_FILE_LIMIT;
        write(&r.join("Windows/WinSxS/amd64_x/shell32.dll"), big as usize);
        fs::create_dir_all(r.join("Windows/System32")).unwrap();
        fs::hard_link(r.join("Windows/WinSxS/amd64_x/shell32.dll"), r.join("Windows/System32/shell32.dll")).unwrap();
        fs::hard_link(r.join("Windows/WinSxS/amd64_x/shell32.dll"), r.join("Windows/WinSxS/amd64_x/copy.dll")).unwrap();
        write(&r.join("Windows/WinSxS/own.bin"), 2 * SMALL_FILE_LIMIT as usize);

        let tree = scan(r, &Progress::default());
        assert_eq!(size_of(&tree, r), big + 2 * SMALL_FILE_LIMIT, "the drive total counts the file once");
        assert_eq!(size_of(&tree, &r.join("Windows/System32")), big);
        assert_eq!(size_of(&tree, &r.join("Windows/WinSxS")), 2 * SMALL_FILE_LIMIT, "only what WinSxS alone holds");
        assert_eq!(linked_of(&tree, &r.join("Windows/WinSxS")), 2 * big, "and what it shares, for the explanation");
        assert_eq!(linked_of(&tree, &r.join("Windows/System32")), 0);
        assert_eq!(linked_of(&tree, r), 2 * big);
        // Every name is still a file of its folder, so file counts do not change.
        assert_eq!(tree.file_count(), 4);
        assert_eq!(tree.links, LinkReport { checked: true, files: 1, links: 2, bytes: 2 * big, ..Default::default() });
    }

    #[test]
    fn small_hard_linked_files_count_once_too() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("a/one.txt"), 100);
        fs::hard_link(r.join("a/one.txt"), r.join("a/two.txt")).unwrap();
        fs::create_dir_all(r.join("b")).unwrap();
        fs::hard_link(r.join("a/one.txt"), r.join("b/three.txt")).unwrap();
        write(&r.join("b/own.txt"), 7);

        let tree = scan(r, &Progress::default());
        assert_eq!(size_of(&tree, r), 107);
        let bundle = |folder: &str| {
            let id = tree.find(&r.join(folder)).unwrap();
            let n = tree.node(tree.node(id).children[0]);
            (n.kind, n.size, n.linked)
        };
        assert_eq!(bundle("a"), (NodeKind::SmallFiles { count: 2 }, 100, 100));
        assert_eq!(bundle("b"), (NodeKind::SmallFiles { count: 2 }, 7, 100));
        assert_eq!((tree.links.files, tree.links.links, tree.links.bytes), (1, 2, 200));
    }

    /// A folder's own files come before its subfolders, and names compare without case.
    #[test]
    fn the_first_name_is_found_the_same_way_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let big = 2 * SMALL_FILE_LIMIT as usize;
        write(&r.join("z/deep/x.bin"), big);
        for link in ["b/x.bin", "A/x.bin", "A/sub/x.bin", "c/x.bin"] {
            fs::create_dir_all(r.join(link).parent().unwrap()).unwrap();
            fs::hard_link(r.join("z/deep/x.bin"), r.join(link)).unwrap();
        }
        for _ in 0..10 {
            let tree = scan(r, &Progress::default());
            let sizes: Vec<u64> = ["A", "A/sub", "b", "c", "z"].iter().map(|f| size_of(&tree, &r.join(f))).collect();
            assert_eq!(sizes, [big as u64, 0, 0, 0, 0]);
        }
    }

    #[test]
    fn without_link_detection_every_name_counts() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("a/x.bin"), 2 * SMALL_FILE_LIMIT as usize);
        fs::hard_link(r.join("a/x.bin"), r.join("b.bin")).unwrap();
        let options = ScanOptions { count_links_once: false, skipped_because: Some("test"), ..ScanOptions::for_root(r) };
        let tree = scan_with(r, &Progress::default(), options);
        assert_eq!(size_of(&tree, r), 4 * SMALL_FILE_LIMIT);
        assert_eq!(tree.links, LinkReport { checked: false, skipped_because: Some("test"), ..Default::default() });
    }

    /// Past the cap no hard links are looked for at all, rather than some, which would depend
    /// on which thread reached the cap first. Empty files never count towards it.
    #[test]
    fn past_the_key_cap_hard_links_are_not_looked_for() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(&r.join("a/x.txt"), 10);
        fs::hard_link(r.join("a/x.txt"), r.join("a/y.txt")).unwrap();
        write(&r.join("b/empty.txt"), 0);
        write(&r.join("b/other.txt"), 5);

        let under = scan_with(r, &Progress::default(), ScanOptions { max_keys: 3, ..ScanOptions::for_root(r) });
        assert_eq!(size_of(&under, r), 15);
        assert!(under.links.checked);

        let over = scan_with(r, &Progress::default(), ScanOptions { max_keys: 2, ..ScanOptions::for_root(r) });
        assert_eq!(size_of(&over, r), 25);
        assert_eq!(over.links, LinkReport { skipped_because: Some("too many files"), ..Default::default() });
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
