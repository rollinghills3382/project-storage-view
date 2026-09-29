use serde::Serialize;
use sysinfo::{DiskKind, Disks};

#[derive(Debug, Serialize)]
pub struct DriveInfo {
    /// Root path, e.g. `C:\`.
    pub mount: String,
    pub label: String,
    pub kind: String,
    pub total: u64,
    pub free: u64,
    pub removable: bool,
    pub file_system: String,
}

pub fn list() -> Vec<DriveInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut out: Vec<DriveInfo> = disks
        .list()
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| DriveInfo {
            mount: d.mount_point().display().to_string(),
            label: d.name().to_string_lossy().trim().to_string(),
            kind: match (d.is_removable(), d.kind()) {
                (true, _) => "Removable drive".into(),
                (_, DiskKind::SSD) => "SSD".into(),
                (_, DiskKind::HDD) => "Hard drive".into(),
                _ => "Drive".into(),
            },
            total: d.total_space(),
            free: d.available_space(),
            removable: d.is_removable(),
            file_system: d.file_system().to_string_lossy().into_owned(),
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out.dedup_by(|a, b| a.mount == b.mount);
    out
}
