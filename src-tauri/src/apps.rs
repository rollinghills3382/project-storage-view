//! Installed applications (from the Windows uninstall registry) and category heuristics.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct InstalledApp {
    pub name: String,
    pub publisher: String,
    pub location: PathBuf,
    /// Registered by Steam ("Steam App 123" keys).
    pub steam: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Games,
    Creative,
    Dev,
    Productivity,
    Media,
    User,
    System,
    Other,
}

/// Folder names too generic to identify an application on their own.
const GENERIC: &[&str] = &[
    "app", "application", "applications", "bin", "cache", "common", "commonfiles", "current", "data", "files",
    "installer", "local", "locallow", "microsoft", "packages", "program", "programfiles", "programfilesx86",
    "programs", "roaming", "setup", "software", "system", "temp", "tools", "update", "updater", "users", "windows",
    "x64", "x86",
];

/// Lowercase letters and digits only, so "Docker Desktop" and "docker-desktop" compare equal.
pub fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Display name without versions and architecture suffixes: "Python 3.12.1 (64-bit)" -> "python".
pub fn base_name(name: &str) -> String {
    let mut words = name.split_whitespace();
    // The first word is always kept so names like "7-Zip" survive.
    let first = words.next().unwrap_or_default();
    let rest = words.take_while(|w| !w.chars().any(|c| c.is_ascii_digit()) && !w.starts_with('(') && *w != "-");
    norm(&std::iter::once(first).chain(rest).collect::<Vec<_>>().join(" "))
}

/// Name for display, without versions and suffixes:
/// "NVIDIA App 11.0.8.299" -> "NVIDIA App", "Microsoft 365 - en-us" -> "Microsoft 365", "Cursor (User)" -> "Cursor".
pub fn display_name(name: &str) -> String {
    let mut words: Vec<&str> = name.split_whitespace().collect();
    let is_locale = |w: &str| w.len() == 5 && w.as_bytes()[2] == b'-' && w.chars().filter(|c| *c != '-').all(|c| c.is_ascii_alphabetic());
    if words.len() >= 3 && words[words.len() - 2] == "-" && is_locale(words[words.len() - 1]) {
        words.truncate(words.len() - 2);
    }
    while words.len() > 1 {
        let w = words[words.len() - 1];
        let version = w.contains('.') && w.trim_start_matches(['v', 'V']).chars().all(|c| c.is_ascii_digit() || c == '.');
        let note = w.starts_with('(') && w.ends_with(')');
        if !(version || note || w == "-") {
            break;
        }
        words.pop();
    }
    words.join(" ")
}

pub fn is_generic(token: &str) -> bool {
    token.len() < 3 || GENERIC.contains(&token)
}

/// Last folder name of an install location, skipping generic ones like `Application` or `bin`.
pub fn location_token(location: &Path) -> Option<String> {
    location
        .components()
        .rev()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(norm(&s.to_string_lossy())),
            _ => None,
        })
        .find(|t| !is_generic(t))
}

pub fn categorize(name: &str, publisher: &str, location: &Path, steam: bool) -> Category {
    let hay = format!("{} {} {}", name, publisher, location.display()).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| hay.contains(w));
    if steam
        || has(&[
            "steamapps", "xboxgames", "epic games", "gog galaxy", "gog games", "riot games", "battle.net", "blizzard",
            "ubisoft", "ea games", "electronic arts", "rockstar games", "minecraft", "valve", "bethesda", "game",
        ])
    {
        Category::Games
    } else if has(&[
        "visual studio", "docker", r"\git", "github", "python", "node.js", "nodejs", "jetbrains", "android studio", "sdk", ".net",
        "rustup", "wsl", "sql server", "postman", "unity hub", r"\unity", "unreal", "cmake", "llvm", "java", "powershell", "terminal",
        "windows kits", "cursor", "wireshark",
    ]) {
        Category::Dev
    } else if has(&[
        "adobe", "blender", "autodesk", "davinci", "affinity", "obs studio", "gimp", "figma", "krita", "ableton",
        "fl studio", "audacity", "inkscape", "capture one", "corel", "maxon",
    ]) {
        Category::Creative
    } else if has(&["spotify", "vlc", "itunes", "plex", "netflix", "music", "media player", "kodi", "foobar"]) {
        Category::Media
    } else {
        Category::Productivity
    }
}

/// Category for a top-level folder or file that no application owns.
pub fn root_entry_category(name: &str) -> Category {
    match name.to_lowercase().as_str() {
        "windows" | "$recycle.bin" | "system volume information" | "recovery" | "perflogs" | "boot" | "config.msi"
        | "$windows.~bt" | "$windows.~ws" | "$winreagent" | "$sysreset" | "windows.old" | "pagefile.sys"
        | "hiberfil.sys" | "swapfile.sys" | "dumpstack.log" | "dumpstack.log.tmp" | "bootmgr" | "msocache"
        | "onedrivetemp" => Category::System,
        "program files" | "program files (x86)" | "programdata" => Category::Other,
        "xboxgames" | "steamlibrary" | "epic games" | "games" | "gog games" | "riot games" => Category::Games,
        _ => Category::User,
    }
}

fn clean_path(raw: &str) -> Option<PathBuf> {
    let s = raw.trim().trim_matches('"').trim().trim_end_matches(['\\', '/']);
    let bytes = s.as_bytes();
    // Absolute drive paths only ("C:\...").
    if bytes.len() < 3 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    Some(PathBuf::from(s))
}

/// Folder of the executable in a `DisplayIcon` value such as `"C:\App\app.exe",0`.
fn exe_dir(raw: &str) -> Option<PathBuf> {
    let lower = raw.to_lowercase();
    let end = lower.find(".exe")? + 4;
    clean_path(&raw[..end])?.parent().map(Path::to_path_buf)
}

/// Rejects locations that would claim far more than the app, such as a drive root or `Program Files` itself.
pub fn is_usable_location(p: &Path) -> bool {
    let parts: Vec<String> = p
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => false,
        [top] => top != "users" && root_entry_category(top) == Category::User,
        [top, ..] if top == "windows" => false,
        [top, _] if top == "users" => false,
        _ => {
            let last = parts.last().map(String::as_str).unwrap_or_default();
            let tail = parts[parts.len().saturating_sub(3)..].join("\\");
            !parts.iter().any(|c| c.starts_with("installer") || c == "package cache" || c.contains('{'))
                && !tail.ends_with(r"appdata\local\temp")
                && !["appdata", "local", "roaming", "locallow", "programs", "common files"].contains(&last)
        }
    }
}

#[cfg(windows)]
pub fn installed_apps() -> Vec<InstalledApp> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let sources = [
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_64KEY),
        (HKEY_LOCAL_MACHINE, KEY_READ | KEY_WOW64_32KEY),
        (HKEY_CURRENT_USER, KEY_READ),
    ];
    let mut apps: Vec<InstalledApp> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (hive, flags) in sources {
        let Ok(list) = RegKey::predef(hive).open_subkey_with_flags(UNINSTALL, flags) else { continue };
        for sub in list.enum_keys().flatten() {
            let Ok(k) = list.open_subkey_with_flags(&sub, flags) else { continue };
            let Ok(name) = k.get_value::<String, _>("DisplayName") else { continue };
            if k.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1 || k.get_value::<String, _>("ParentKeyName").is_ok() {
                continue;
            }
            let location = k
                .get_value::<String, _>("InstallLocation")
                .ok()
                .and_then(|s| clean_path(&s))
                .filter(|p| is_usable_location(p))
                .or_else(|| k.get_value::<String, _>("DisplayIcon").ok().and_then(|s| exe_dir(&s)).filter(|p| is_usable_location(p)));
            let Some(location) = location else { continue };
            if !location.is_dir() || !seen.insert(location.to_string_lossy().to_lowercase()) {
                continue;
            }
            apps.push(InstalledApp {
                name: name.trim().to_string(),
                publisher: k.get_value::<String, _>("Publisher").unwrap_or_default().trim().to_string(),
                location,
                steam: sub.starts_with("Steam App "),
            });
        }
    }
    apps
}

#[cfg(not(windows))]
pub fn installed_apps() -> Vec<InstalledApp> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalize_for_matching() {
        assert_eq!(norm("Docker Desktop"), "dockerdesktop");
        assert_eq!(base_name("Python 3.12.1 (64-bit)"), "python");
        assert_eq!(base_name("Visual Studio Community 2022"), "visualstudiocommunity");
        assert_eq!(base_name("7-Zip 24.08 (x64)"), "7zip");
        assert_eq!(display_name("NVIDIA App 11.0.8.299"), "NVIDIA App");
        assert_eq!(display_name("Microsoft 365 - en-us"), "Microsoft 365");
        assert_eq!(display_name("Cursor (User)"), "Cursor");
        assert_eq!(display_name("Python 3.12.1 (64-bit)"), "Python");
        assert_eq!(display_name("Visual Studio Community 2026"), "Visual Studio Community 2026");
    }

    #[test]
    fn location_token_skips_generic_folders() {
        assert_eq!(location_token(Path::new(r"C:\Program Files\Google\Chrome\Application")).as_deref(), Some("chrome"));
        assert_eq!(location_token(Path::new(r"C:\Program Files\Docker\Docker")).as_deref(), Some("docker"));
    }

    #[test]
    fn display_icon_paths_resolve_to_folder() {
        assert_eq!(exe_dir(r#""C:\Program Files\App\app.exe",0"#), Some(PathBuf::from(r"C:\Program Files\App")));
        assert_eq!(exe_dir("not a path"), None);
    }

    #[test]
    fn overly_broad_locations_are_rejected() {
        assert!(!is_usable_location(Path::new(r"C:\Program Files")));
        assert!(!is_usable_location(Path::new(r"C:\Windows\System32")));
        assert!(!is_usable_location(Path::new(r"C:\ProgramData\Package Cache\{ABC}")));
        assert!(!is_usable_location(Path::new(r"C:\Program Files\NVIDIA Corporation\Installer2\Display.NvApp.{2BE7}")));
        assert!(!is_usable_location(Path::new(r"C:\Users\me\AppData\Local\Programs")));
        assert!(is_usable_location(Path::new(r"C:\Program Files\Docker\Docker")));
        assert!(is_usable_location(Path::new(r"D:\SteamLibrary\steamapps\common\Elden Ring")));
    }

    #[test]
    fn categories() {
        assert_eq!(categorize("Elden Ring", "", Path::new(r"D:\SteamLibrary\steamapps\common\ELDEN RING"), true), Category::Games);
        assert_eq!(categorize("Docker Desktop", "Docker Inc.", Path::new(r"C:\Program Files\Docker"), false), Category::Dev);
        assert_eq!(categorize("Adobe Photoshop 2025", "Adobe Inc.", Path::new(r"C:\Program Files\Adobe"), false), Category::Creative);
        assert_eq!(categorize("Spotify", "Spotify AB", Path::new(r"C:\Users\me\AppData\Roaming\Spotify"), false), Category::Media);
        assert_eq!(categorize("Discord", "Discord Inc.", Path::new(r"C:\Users\me\AppData\Local\Discord"), false), Category::Productivity);
        assert_eq!(root_entry_category("Windows"), Category::System);
        assert_eq!(root_entry_category("Photos"), Category::User);
    }
}
