//! Lists a folder from its handle, getting each entry's file ID from the same directory query
//! that returns its name, size and attributes. No file is opened: hard links of one file share
//! its ID, so the walk can tell them apart without asking each file for its link count.

use std::cell::RefCell;
use std::io;
use std::mem::offset_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED,
    ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdBothDirectoryInfo, FileIdExtdDirectoryInfo, GetDriveTypeW, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_ID_EXTD_DIR_INFO,
    FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, SYNCHRONIZE,
};

/// In `Win32_System_SystemServices`, a feature the app does not otherwise need.
const FILE_SUPPORTS_HARD_LINKS: u32 = 0x0040_0000;
/// In `Win32_System_WindowsProgramming`.
const DRIVE_REMOTE: u32 = 4;

/// Which directory query returns the IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdClass {
    /// `FILE_ID_EXTD_DIR_INFO` (Windows 8 and later): no short names to look up, and a 128-bit
    /// ID whose low 64 bits are the NTFS file reference number.
    Extd,
    /// `FILE_ID_BOTH_DIR_INFO`: the 64-bit file reference number, for file systems that refuse
    /// the newer query.
    Both,
}

pub struct Listed<'a> {
    pub name: &'a [u16],
    pub attributes: u32,
    pub reparse_tag: u32,
    pub len: u64,
    pub id: u128,
}

impl Listed<'_> {
    fn is_dot(&self) -> bool {
        matches!(self.name, [0x2e] | [0x2e, 0x2e])
    }

    /// The test std's `FileType::is_symlink` makes: a name-surrogate reparse point, which covers
    /// symbolic links, junctions and volume mount points.
    pub fn is_link(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && self.reparse_tag & 0x2000_0000 != 0
    }

    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// Which step failed, so the walk can describe the error the way it does for other listings.
#[derive(Debug)]
pub enum Failed {
    Open(io::Error),
    List(io::Error),
    /// The folder opened but its file system does not answer this query. The caller lists it
    /// another way.
    Unsupported,
}

const BUF_BYTES: usize = 64 * 1024;

thread_local! {
    /// One 64 KiB buffer per scan thread, 8-byte aligned as these structures require. A listing
    /// never recurses while it holds it: subfolders are walked after the listing finishes.
    static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0u64; BUF_BYTES / 8]);
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// `\\?\` form of an absolute path, as std uses for long paths, so folders deeper than
/// MAX_PATH open too.
fn verbatim(path: &Path) -> Vec<u16> {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let starts = |p: &str| raw.starts_with(&p.encode_utf16().collect::<Vec<u16>>());
    let mut out: Vec<u16> = if !path.is_absolute() || starts(r"\\?\") || starts(r"\\.\") {
        raw
    } else if starts(r"\\") {
        r"\\?\UNC".encode_utf16().chain(raw[1..].iter().copied()).collect()
    } else {
        r"\\?\".encode_utf16().chain(raw).collect()
    };
    out.push(0);
    out
}

fn open_dir(path: &Path) -> io::Result<Handle> {
    let wide = verbatim(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            // Needed to open a folder. There is no FILE_FLAG_OPEN_REPARSE_POINT: the walk never
            // enters name-surrogate reparse points, and other folders with reparse points (cloud
            // folders) must open the way FindFirstFileExW opens them.
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(h))
}

/// Calls `f` for every entry except `.` and `..`, until `f` returns false.
pub fn list_dir(path: &Path, class: IdClass, mut f: impl FnMut(&Listed<'_>) -> bool) -> Result<(), Failed> {
    let h = open_dir(path).map_err(Failed::Open)?;
    let info_class = match class {
        IdClass::Extd => FileIdExtdDirectoryInfo,
        IdClass::Both => FileIdBothDirectoryInfo,
    };
    BUF.with_borrow_mut(|buf| {
        let mut first = true;
        loop {
            // SAFETY: the buffer is BUF_BYTES long and 8-byte aligned.
            let ok = unsafe { GetFileInformationByHandleEx(h.0, info_class, buf.as_mut_ptr().cast(), BUF_BYTES as u32) };
            if ok == 0 {
                let e = io::Error::last_os_error();
                return match e.raw_os_error().map(|c| c as u32) {
                    // The documented end of a listing, and the one GetFileInformationByHandleEx
                    // documents for its other enumerations.
                    Some(ERROR_NO_MORE_FILES | ERROR_HANDLE_EOF) => Ok(()),
                    // A folder with no entries at all.
                    Some(ERROR_FILE_NOT_FOUND) if first => Ok(()),
                    Some(ERROR_INVALID_PARAMETER | ERROR_INVALID_LEVEL | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION) if first => {
                        Err(Failed::Unsupported)
                    }
                    // The rest of the listing is lost, but what was read so far still counts.
                    _ if !first => Err(Failed::List(e)),
                    _ => Err(Failed::Open(e)),
                };
            }
            first = false;
            let base = buf.as_ptr() as *const u8;
            let mut off = 0usize;
            loop {
                if !fits(base, off, class) {
                    return Err(Failed::List(io::Error::new(io::ErrorKind::InvalidData, "malformed directory listing")));
                }
                // SAFETY: `fits` checked that the entry, name included, lies inside the buffer.
                let (next, entry) = unsafe { parse(base.add(off), class) };
                if !entry.is_dot() && !f(&entry) {
                    return Ok(());
                }
                if next == 0 {
                    break;
                }
                off += next as usize;
            }
        }
    })
}

/// Whether the entry at `off`, with its name, lies inside the buffer, so a malformed listing
/// stops the listing instead of reading past it.
fn fits(base: *const u8, off: usize, class: IdClass) -> bool {
    let (len_at, name_at) = match class {
        IdClass::Extd => (offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength), offset_of!(FILE_ID_EXTD_DIR_INFO, FileName)),
        IdClass::Both => (offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength), offset_of!(FILE_ID_BOTH_DIR_INFO, FileName)),
    };
    if off + name_at > BUF_BYTES {
        return false;
    }
    // SAFETY: the length field lies inside the buffer, checked just above.
    let name_len = unsafe { std::ptr::read_unaligned(base.add(off + len_at) as *const u32) } as usize;
    off + name_at + name_len <= BUF_BYTES
}

/// SAFETY: `p` points at a complete entry of the given class inside a live buffer.
unsafe fn parse<'a>(p: *const u8, class: IdClass) -> (u32, Listed<'a>) {
    use std::ptr::read_unaligned as rd;
    macro_rules! field {
        ($t:ty, $f:ident, $ty:ty) => {
            rd(p.add(offset_of!($t, $f)) as *const $ty)
        };
    }
    match class {
        IdClass::Extd => {
            let next: u32 = field!(FILE_ID_EXTD_DIR_INFO, NextEntryOffset, u32);
            let attributes: u32 = field!(FILE_ID_EXTD_DIR_INFO, FileAttributes, u32);
            let tag: u32 = field!(FILE_ID_EXTD_DIR_INFO, ReparsePointTag, u32);
            let len: i64 = field!(FILE_ID_EXTD_DIR_INFO, EndOfFile, i64);
            let id: [u8; 16] = field!(FILE_ID_EXTD_DIR_INFO, FileId, [u8; 16]);
            let name_len: u32 = field!(FILE_ID_EXTD_DIR_INFO, FileNameLength, u32);
            let name = std::slice::from_raw_parts(p.add(offset_of!(FILE_ID_EXTD_DIR_INFO, FileName)) as *const u16, name_len as usize / 2);
            let reparse_tag = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 { tag } else { 0 };
            (next, Listed { name, attributes, reparse_tag, len: len.max(0) as u64, id: u128::from_le_bytes(id) })
        }
        IdClass::Both => {
            let next: u32 = field!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset, u32);
            let attributes: u32 = field!(FILE_ID_BOTH_DIR_INFO, FileAttributes, u32);
            // For a reparse point the directory query puts its tag in EaSize (MS-FSA
            // 2.1.5.6.3), which FindFirstFileExW hands out as dwReserved0.
            let ea: u32 = field!(FILE_ID_BOTH_DIR_INFO, EaSize, u32);
            let len: i64 = field!(FILE_ID_BOTH_DIR_INFO, EndOfFile, i64);
            let id: i64 = field!(FILE_ID_BOTH_DIR_INFO, FileId, i64);
            let name_len: u32 = field!(FILE_ID_BOTH_DIR_INFO, FileNameLength, u32);
            let name = std::slice::from_raw_parts(p.add(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName)) as *const u16, name_len as usize / 2);
            let reparse_tag = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 { ea } else { 0 };
            (next, Listed { name, attributes, reparse_tag, len: len.max(0) as u64, id: id as u64 as u128 })
        }
    }
}

// The offsets the parser relies on, checked against the SDK layout.
const _: () = assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, ReparsePointTag) == 68);
const _: () = assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, FileId) == 72);
const _: () = assert!(offset_of!(FILE_ID_EXTD_DIR_INFO, FileName) == 88);
const _: () = assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, EaSize) == 64);
const _: () = assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileId) == 96);
const _: () = assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileName) == 104);

/// How to list the volume `root` is on, or why hard links are not looked for there.
pub fn id_class(root: &Path) -> Result<IdClass, &'static str> {
    let plain: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    // GetDriveTypeW only knows drive roots, so a folder on a share is caught by its UNC path.
    let unc = root.as_os_str().to_string_lossy().starts_with(r"\\") && !root.as_os_str().to_string_lossy().starts_with(r"\\?\");
    // SAFETY: `plain` is NUL-terminated and outlives the call.
    if unc || unsafe { GetDriveTypeW(plain.as_ptr()) } == DRIVE_REMOTE {
        return Err("network drive");
    }
    let h = open_dir(root).map_err(|_| "the drive could not be opened")?;
    let mut flags = 0u32;
    let mut fs = [0u16; 32];
    // SAFETY: every out pointer is valid for the call, and the name buffer's length is passed.
    let ok = unsafe {
        GetVolumeInformationByHandleW(h.0, std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut flags, fs.as_mut_ptr(), fs.len() as u32)
    };
    if ok == 0 {
        return Err("the drive's file system could not be read");
    }
    if flags & FILE_SUPPORTS_HARD_LINKS == 0 {
        return Err("no hard links on this file system");
    }
    let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    // Only NTFS IDs are known to fit the walk's compact keys. ReFS IDs are 128 bits and would
    // have to be hashed, and a collision would count a file as a link of another.
    if String::from_utf16_lossy(&fs[..end]) != "NTFS" {
        return Err("file system not checked for hard links");
    }
    drop(h);
    for class in [IdClass::Extd, IdClass::Both] {
        match list_dir(root, class, |_| false) {
            Ok(()) => return Ok(class),
            Err(Failed::Unsupported) => continue,
            Err(_) => return Err("the drive could not be listed"),
        }
    }
    Err("the drive does not list file IDs")
}

/// These run on the Windows CI runner, against its NTFS system drive and temp folder.
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::windows::fs::MetadataExt;

    /// (name, is a folder to walk, is skipped as a link, length of a file, attributes)
    type Row = (String, bool, bool, u64, u32);

    fn by_id(path: &Path, class: IdClass) -> Vec<Row> {
        let mut rows = Vec::new();
        list_dir(path, class, |e| {
            let file = !e.is_dir();
            rows.push((String::from_utf16_lossy(e.name), e.is_dir() && !e.is_link(), e.is_link(), if file { e.len } else { 0 }, e.attributes));
            true
        })
        .unwrap_or_else(|_| panic!("listing {}", path.display()));
        rows.sort();
        rows
    }

    fn by_std(path: &Path) -> Vec<Row> {
        let mut rows: Vec<Row> = fs::read_dir(path)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let (ft, md) = (e.file_type().unwrap(), e.metadata().unwrap());
                let file = !md.is_dir();
                (e.file_name().to_string_lossy().into_owned(), ft.is_dir(), ft.is_symlink(), if file { md.len() } else { 0 }, md.file_attributes())
            })
            .collect();
        rows.sort();
        rows
    }

    /// Everything the walk reads from a listing is what std's listing says, for both queries:
    /// names, which entries are folders, which are links it skips, file lengths and attributes.
    #[test]
    fn listing_with_ids_matches_std() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("sub/empty")).unwrap();
        fs::write(r.join("sub/data.bin"), vec![1u8; 3_000_000]).unwrap();
        fs::write(r.join("small.txt"), b"hello").unwrap();
        let junction = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(r.join("junction")).arg(r.join("sub")).status().unwrap();
        assert!(junction.success());
        let deep = (0..30).fold(r.to_path_buf(), |p, i| p.join(format!("deep folder {i:02}")));
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("far.bin"), b"x").unwrap();

        let system = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()));
        for path in [r.to_path_buf(), r.join("sub"), r.join("sub/empty"), deep.clone(), PathBuf::from(r"C:\"), system.join("System32")] {
            let expected = by_std(&path);
            for class in [IdClass::Extd, IdClass::Both] {
                assert_eq!(by_id(&path, class), expected, "{} listed with {class:?}", path.display());
            }
        }
        assert!(by_id(r, IdClass::Extd).iter().any(|row| row.0 == "junction" && row.2 && !row.1), "the junction is skipped, not walked");
        assert!(deep.as_os_str().len() > 260, "this folder is past MAX_PATH");
    }

    fn handle_id(path: &Path) -> u128 {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{FileIdInfo, FILE_ID_INFO};
        let f = fs::File::open(path).unwrap();
        let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is sized by the length passed, and the handle is open.
        let ok = unsafe { GetFileInformationByHandleEx(f.as_raw_handle(), FileIdInfo, (&mut info as *mut FILE_ID_INFO).cast(), std::mem::size_of::<FILE_ID_INFO>() as u32) };
        assert_ne!(ok, 0, "{}", io::Error::last_os_error());
        u128::from_le_bytes(info.FileId.Identifier)
    }

    /// Two names of one file list with the same ID, the file's own, and NTFS IDs fit in 64 bits.
    #[test]
    fn hard_links_list_with_the_files_own_id() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("a")).unwrap();
        fs::write(r.join("a/x.bin"), b"data").unwrap();
        fs::hard_link(r.join("a/x.bin"), r.join("y.bin")).unwrap();
        let id_of = |folder: &Path, name: &str, class: IdClass| {
            let mut id = None;
            list_dir(folder, class, |e| {
                if String::from_utf16_lossy(e.name) == name {
                    id = Some(e.id);
                }
                true
            })
            .unwrap();
            id.unwrap()
        };
        let own = handle_id(&r.join("a/x.bin"));
        assert_ne!(own, 0);
        assert_eq!(own >> 64, 0, "an NTFS ID is the 64-bit file reference number");
        for class in [IdClass::Extd, IdClass::Both] {
            assert_eq!(id_of(&r.join("a"), "x.bin", class), own, "{class:?}");
            assert_eq!(id_of(r, "y.bin", class), own, "{class:?}");
        }
    }

    #[test]
    fn the_system_drive_lists_with_ids() {
        let class = id_class(Path::new(r"C:\"));
        println!("C:\\ lists with {class:?}");
        assert!(class.is_ok(), "{class:?}");
    }
}
