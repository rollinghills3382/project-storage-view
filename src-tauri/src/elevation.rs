//! Detecting administrator rights and relaunching with them through the UAC prompt.

/// Command-line flag an elevated relaunch uses to resume scanning the same drive.
pub const SCAN_FLAG: &str = "--scan";

#[derive(Debug, PartialEq, Eq)]
pub enum RelaunchError {
    /// The user said no at the UAC prompt.
    Cancelled,
    Failed(String),
}

/// The drive passed as `--scan C:` on the command line, as a root path (`C:\`).
/// Only a bare drive letter is accepted.
pub fn startup_scan(args: impl IntoIterator<Item = String>) -> Option<String> {
    let mut args = args.into_iter();
    args.by_ref().find(|a| a == SCAN_FLAG)?;
    let drive = args.next()?;
    let b = drive.as_bytes();
    (b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| format!("{}\\", drive.to_ascii_uppercase()))
}

/// `C:\` -> `--scan C:`. The trailing backslash is dropped because `"C:\"` would escape the quote.
pub fn scan_args(drive: &str) -> String {
    format!("{SCAN_FLAG} {}", drive.trim_end_matches(['\\', '/']))
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: the token handle is checked before use and closed afterwards; the output
    // buffer is a TOKEN_ELEVATION whose size is passed alongside it.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Starts a new copy of this app with administrator rights. Windows shows the UAC prompt.
#[cfg(windows)]
pub fn relaunch_elevated(args: &str) -> Result<(), RelaunchError> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &OsStr| s.encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let exe = std::env::current_exe().map_err(|e| RelaunchError::Failed(e.to_string()))?;
    let (verb, file, params) = (wide(OsStr::new("runas")), wide(exe.as_os_str()), wide(OsStr::new(args)));

    // SAFETY: every pointer refers to a NUL-terminated UTF-16 buffer that outlives the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), SW_SHOWNORMAL)
    };
    // ShellExecuteW returns a value greater than 32 on success.
    if result as isize > 32 {
        return Ok(());
    }
    // SAFETY: reads the calling thread's last-error value; no preconditions.
    match unsafe { GetLastError() } {
        ERROR_CANCELLED => Err(RelaunchError::Cancelled),
        code => Err(RelaunchError::Failed(format!("Windows error {code}"))),
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(not(windows))]
pub fn relaunch_elevated(_args: &str) -> Result<(), RelaunchError> {
    Err(RelaunchError::Failed("Only supported on Windows.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn startup_scan_reads_a_drive_letter() {
        assert_eq!(startup_scan(args(&["app.exe", "--scan", "d:"])), Some(r"D:\".into()));
        assert_eq!(startup_scan(args(&["app.exe"])), None);
        assert_eq!(startup_scan(args(&["app.exe", "--scan"])), None);
        assert_eq!(startup_scan(args(&["app.exe", "--scan", r"C:\Windows"])), None);
        assert_eq!(startup_scan(args(&["app.exe", "--scan", r"\\server\share"])), None);
    }

    #[test]
    fn scan_args_round_trip() {
        let line = scan_args(r"C:\");
        assert_eq!(line, "--scan C:");
        let parsed = std::iter::once("app.exe".to_string()).chain(line.split(' ').map(String::from));
        assert_eq!(startup_scan(parsed), Some(r"C:\".into()));
    }
}
