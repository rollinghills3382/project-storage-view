//! Counts a file with several hard links once.
//!
//! The walk keeps a compact key (the file's ID) next to wherever it counted a file's bytes.
//! Afterwards, keys seen more than once are found by sorting, and one pass in a fixed order
//! gives each such file's bytes to the first of its names. Every other name counts 0 bytes and
//! its folder records what it would have added, so the UI can say where those bytes went.
//! Link counts are never needed: a file is shared exactly when its key turns up twice.

use super::{TmpDir, SMALL_FILE_LIMIT};
use rayon::prelude::*;
use serde::Serialize;
use std::cmp::Ordering;

/// Small files are under 1 MiB, so their size fits in the low 20 bits of a packed key.
const SIZE_BITS: u32 = 20;
const _: () = assert!(SMALL_FILE_LIMIT == 1 << SIZE_BITS);
const KEY_BITS: u32 = 64 - SIZE_BITS;
const KEY_MASK: u64 = (1 << KEY_BITS) - 1;
/// No identity. A non-empty file without one is counted as listed and reported as unchecked.
pub const NO_KEY: u64 = 0;

/// Keys kept at once, 8 bytes each: the hard cap on what hard-link detection adds to a scan's
/// memory. A drive with more non-empty files than this is scanned without it.
pub const MAX_KEYS: u64 = 8 << 20;
/// Bytes of keys sorted at a time while looking for shared ones.
const SHARD_BYTES: u64 = 8 << 20;

/// NTFS file reference number to key: the 32-bit MFT record number and the low 12 bits of
/// the sequence number, which changes when a record is reused. NTFS holds fewer than 2^32
/// files, so the record number fits; a reference that does not is left without a key.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn ntfs_key(frn: u64) -> u64 {
    let record = frn & 0x0000_FFFF_FFFF_FFFF;
    if record >> 32 != 0 {
        return NO_KEY;
    }
    (((frn >> 48) & 0xFFF) << 32) | record
}

/// An inode number as a key, for tests on Unix: exact while it fits, which it does on the
/// file systems tests use.
#[cfg(unix)]
pub fn inode_key(ino: u64) -> u64 {
    if ino <= KEY_MASK { ino } else { NO_KEY }
}

pub fn pack(key: u64, size: u64) -> u64 {
    debug_assert!(size < SMALL_FILE_LIMIT && key <= KEY_MASK);
    (key << SIZE_BITS) | size
}

fn unpack(packed: u64) -> (u64, u64) {
    (packed >> SIZE_BITS, packed & (SMALL_FILE_LIMIT - 1))
}

/// What hard-link detection found, for the scan summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LinkReport {
    /// Whether hard links were looked for at all.
    pub checked: bool,
    /// Why not, when they were not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_because: Option<&'static str>,
    /// Files found under more than one name.
    pub files: u64,
    /// Names whose bytes are counted under another name of the same file.
    pub links: u64,
    /// What those names would have added.
    pub bytes: u64,
    /// Non-empty files that could not be checked, and so may be counted under several names.
    pub unchecked: u64,
}

/// Visits every kept key.
fn each_key(d: &TmpDir, f: &mut impl FnMut(u64)) {
    for file in &d.files {
        if file.key != NO_KEY {
            f(file.key);
        }
    }
    for &packed in d.small_keys.iter() {
        f(unpack(packed).0);
    }
    for c in &d.dirs {
        each_key(c, f);
    }
}

/// Splits keys evenly between shards whatever their distribution.
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Keys that occur more than once, sorted. Keys are gathered and sorted in shards of about
/// `SHARD_BYTES`, so only a slice of them is copied at once. Returns None if cancelled.
fn shared_keys(root: &TmpDir, kept: u64, cancelled: &dyn Fn() -> bool) -> Option<Vec<u64>> {
    let shards = (kept * 8).div_ceil(SHARD_BYTES).max(1);
    let mut shared = Vec::new();
    let mut buf: Vec<u64> = Vec::new();
    for s in 0..shards {
        if cancelled() {
            return None;
        }
        buf.clear();
        each_key(root, &mut |k| {
            if mix(k) % shards == s {
                buf.push(k);
            }
        });
        buf.par_sort_unstable();
        shared.extend(buf.chunk_by(|a, b| a == b).filter(|run| run.len() > 1).map(|run| run[0]));
    }
    shared.sort_unstable();
    Some(shared)
}

/// Case-insensitive name order, ties broken by the exact name, so the result does not depend
/// on the order a file system lists entries in. NTFS already lists in about this order, so the
/// sorts mostly find sorted input.
fn by_name(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_uppercase).cmp(b.chars().flat_map(char::to_uppercase)).then_with(|| a.cmp(b))
}

struct Resolve<'a> {
    shared: &'a [u64],
    /// One bit per shared key: whether its bytes are already counted.
    counted: Vec<u64>,
    report: &'a mut LinkReport,
}

impl Resolve<'_> {
    /// Whether this name is the one that keeps the file's bytes: false only for the second and
    /// later names of a shared file.
    fn keeps(&mut self, key: u64) -> bool {
        let Ok(i) = self.shared.binary_search(&key) else { return true };
        let (word, bit) = (i / 64, 1u64 << (i % 64));
        let first = self.counted[word] & bit == 0;
        self.counted[word] |= bit;
        first
    }

    fn lose(&mut self, bytes: u64) {
        self.report.links += 1;
        self.report.bytes += bytes;
    }

    /// A folder's own files come before its subfolders, and both go by name. Each name that
    /// loses gives up exactly the bytes it added, so a folder's sizes stay consistent even when
    /// the listing reports different lengths for two names of one file.
    fn visit(&mut self, d: &mut TmpDir) {
        d.files.sort_by(|a, b| by_name(&a.name, &b.name));
        for i in 0..d.files.len() {
            let file = &d.files[i];
            if file.key != NO_KEY && !self.keeps(file.key) {
                let bytes = file.size;
                self.lose(bytes);
                let file = &mut d.files[i];
                file.linked = bytes;
                file.size = 0;
            }
        }
        // Two names in one bundle cannot be told apart, so the order within it does not matter.
        for packed in std::mem::take(&mut d.small_keys).iter() {
            let (key, bytes) = unpack(*packed);
            if !self.keeps(key) {
                self.lose(bytes);
                d.small_size -= bytes;
                d.small_linked += bytes;
            }
        }
        d.dirs.sort_by(|a, b| by_name(&a.name, &b.name));
        for c in &mut d.dirs {
            self.visit(c);
        }
    }
}

/// Counts each shared file once, at its first name in this order: a folder's own files before
/// its subfolders, each by name. Drops every key on the way. Returns false if cancelled, in
/// which case sizes are left as listed.
pub fn count_once(root: &mut TmpDir, kept: u64, report: &mut LinkReport, cancelled: &dyn Fn() -> bool) -> bool {
    let Some(shared) = shared_keys(root, kept, cancelled) else { return false };
    report.files = shared.len() as u64;
    let mut r = Resolve { counted: vec![0; shared.len().div_ceil(64)], shared: &shared, report };
    r.visit(root);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::TmpFile;

    #[test]
    fn ntfs_keys_keep_the_record_and_part_of_the_sequence() {
        // Record 0x1234 with sequence 0x0005.
        assert_eq!(ntfs_key(0x0005_0000_0000_1234), 0x5_0000_1234);
        // Only the low 12 bits of the sequence are kept.
        assert_eq!(ntfs_key(0xF005_0000_0000_1234), 0x5_0000_1234);
        // A record number past 32 bits is not an NTFS one: no key, rather than a truncated one.
        assert_eq!(ntfs_key(0x0001_0001_0000_0000), NO_KEY);
    }

    #[test]
    fn packed_keys_round_trip() {
        for (key, size) in [(1, 0), (KEY_MASK, SMALL_FILE_LIMIT - 1), (0x5_0000_1234, 4096)] {
            assert_eq!(unpack(pack(key, size)), (key, size));
        }
    }

    fn dir(name: &str, files: &[(&str, u64, u64)], small: &[(u64, u64)], dirs: Vec<TmpDir>) -> TmpDir {
        TmpDir {
            name: name.into(),
            files: files.iter().map(|&(n, size, key)| TmpFile { name: n.into(), size, key, linked: 0 }).collect(),
            small_count: small.len() as u32,
            small_size: small.iter().map(|s| s.1).sum(),
            small_keys: small.iter().map(|&(k, s)| pack(k, s)).collect(),
            small_linked: 0,
            dirs,
        }
    }

    fn kept(d: &TmpDir) -> u64 {
        let mut n = 0;
        each_key(d, &mut |_| n += 1);
        n
    }

    /// The listing can report different lengths for two names of one file (NTFS updates the
    /// length only in the folder the file was written through). Each name that loses takes
    /// back what it added, so no size goes negative and every folder still adds up.
    #[test]
    fn a_losing_name_gives_back_exactly_what_it_added() {
        const MB: u64 = 1 << 20;
        let mut root = dir(
            "root",
            &[],
            &[],
            vec![
                dir("a", &[("x.dll", 3 * MB, 7)], &[(9, 100)], vec![]),
                dir("b", &[("x.dll", 2 * MB, 7)], &[(9, 80), (10, 5)], vec![]),
            ],
        );
        let mut report = LinkReport::default();
        let n = kept(&root);
        assert!(count_once(&mut root, n, &mut report, &|| false));
        let [a, b] = [&root.dirs[0], &root.dirs[1]];
        assert_eq!((a.files[0].size, a.small_size), (3 * MB, 100));
        assert_eq!((b.files[0].size, b.files[0].linked), (0, 2 * MB));
        assert_eq!((b.small_size, b.small_linked), (5, 80));
        assert_eq!(report, LinkReport { files: 2, links: 2, bytes: 2 * MB + 80, ..Default::default() });
        assert!(a.small_keys.is_empty() && b.small_keys.is_empty(), "keys are dropped as they are resolved");
    }

    #[test]
    fn the_first_name_wins_whatever_order_folders_were_listed_in() {
        let build = |order: [&str; 3]| {
            dir("root", &[], &[], order.iter().map(|&n| dir(n, &[("f.bin", 2 << 20, 42)], &[], vec![])).collect())
        };
        for order in [["b", "A", "c"], ["c", "b", "A"], ["A", "c", "b"]] {
            let mut root = build(order);
            count_once(&mut root, 3, &mut LinkReport::default(), &|| false);
            let keeper: Vec<&str> = root.dirs.iter().filter(|d| d.files[0].size > 0).map(|d| d.name.as_str()).collect();
            assert_eq!(keeper, ["A"], "listed as {order:?}");
        }
    }

    #[test]
    fn a_cancelled_pass_changes_nothing() {
        let mut root = dir("root", &[("f", 2 << 20, 1), ("g", 2 << 20, 1)], &[], vec![]);
        let mut report = LinkReport::default();
        assert!(!count_once(&mut root, 2, &mut report, &|| true));
        assert!(root.files.iter().all(|f| f.size == 2 << 20));
        assert_eq!(report, LinkReport::default());
    }

    /// Shards only split the work: with more keys than one shard holds, the same keys are found.
    #[test]
    fn sharding_finds_the_same_shared_keys() {
        let n = (SHARD_BYTES / 8) * 3 + 17;
        let small: Vec<(u64, u64)> = (1..=n).map(|k| (k, 1)).chain([(5, 1), (n, 1), (n / 2, 1)]).collect();
        let root = dir("root", &[], &small, vec![]);
        let shared = shared_keys(&root, kept(&root), &|| false).unwrap();
        let mut expected = vec![5, n / 2, n];
        expected.sort();
        assert_eq!(shared, expected);
    }
}
