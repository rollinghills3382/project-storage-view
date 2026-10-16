//! Measures what counting hard links once costs a scan: `scan_bench <root> --mode <mode>`
//! scans once and prints one JSON line. Run each mode in its own process so its memory peak
//! is its own.
//!
//! - `std`: folders listed with `std::fs::read_dir`, every hard link counted (the old scan).
//! - `ids`: folders listed with the query that returns file IDs, every hard link counted.
//! - `links`: the same listing, each hard-linked file counted once (what the app does).
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Instant;
use storage_view_lib::scan::{self, Listing, Progress, ScanOptions, Tree};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(args.first().expect("usage: scan_bench <root> --mode std|ids|links"));
    let mode = args.iter().position(|a| a == "--mode").and_then(|i| args.get(i + 1)).map_or("links", String::as_str);
    let detected = ScanOptions::for_root(&root);
    let options = match mode {
        "std" => ScanOptions { listing: Listing::Std, count_links_once: false, ..detected },
        "ids" => ScanOptions { count_links_once: false, ..detected },
        "links" => detected,
        other => panic!("unknown mode {other}"),
    };
    let progress = Progress::default();
    let started = Instant::now();
    let tree = scan::scan_with(&root, &progress, options);
    let ms = started.elapsed().as_millis() as u64;
    let (peak_working_set, peak_private) = peaks();
    let folder = |rel: &str| tree.find(&root.join(rel)).map(|id| (tree.node(id).size, tree.node(id).linked));
    let line = serde_json::json!({
        "mode": mode,
        "listing": format!("{:?}", options.listing),
        "ms": ms,
        "files": tree.file_count(),
        "dirs": progress.dirs.load(Relaxed),
        "listed_bytes": progress.bytes.load(Relaxed),
        "counted_bytes": tree.node(Tree::ROOT).size,
        "errors": progress.errors.report().counts,
        "links": tree.links,
        "winsxs": folder(r"Windows\WinSxS"),
        "system32": folder(r"Windows\System32"),
        "peak_working_set": peak_working_set,
        "peak_private": peak_private,
    });
    println!("{line}");
}

/// Peak working set and peak private bytes of this process, in bytes.
#[cfg(windows)]
fn peaks() -> (u64, u64) {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: the counters struct is plain data, sized by `cb`, and the pseudo-handle needs no closing.
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) == 0 {
            return (0, 0);
        }
        (c.PeakWorkingSetSize as u64, c.PeakPagefileUsage as u64)
    }
}

#[cfg(not(windows))]
fn peaks() -> (u64, u64) {
    (0, 0)
}
