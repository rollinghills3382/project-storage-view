pub mod apps;
pub mod drives;
mod elevation;
pub mod grouping;
pub mod scan;

use grouping::{Id, View, ViewNode};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// One scan, from `start_scan` until it publishes, is cancelled or is replaced.
struct Run {
    /// Sent with every event the scan emits, so the UI can tell this run from an
    /// earlier scan of the same drive whose events are still arriving.
    id: u64,
    progress: scan::Progress,
}

/// How a scan ended, which decides what it may tell the UI.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Its view is stored and it reports `scan-done`.
    Published,
    /// It was cancelled before it could publish. It stored nothing and reports `scan-cancelled`.
    Cancelled,
    /// A newer scan replaced it. It stored nothing and reports nothing, so it cannot
    /// overwrite fresher data or reset the progress of the scan the user is watching.
    Superseded,
}

#[derive(Default)]
struct AppState {
    /// Finished scans, keyed by drive root (`C:\`).
    views: Mutex<HashMap<String, Arc<View>>>,
    running: Mutex<Option<Arc<Run>>>,
    last_id: AtomicU64,
}

impl AppState {
    /// Makes a new scan the running one and cancels whichever it replaces.
    fn begin(&self) -> Arc<Run> {
        let run = Arc::new(Run { id: self.last_id.fetch_add(1, Relaxed) + 1, progress: scan::Progress::default() });
        if let Some(old) = self.running.lock().unwrap().replace(run.clone()) {
            old.progress.cancel.store(true, Relaxed);
        }
        run
    }

    /// Asks the running scan to stop, if it is `scan` (or any scan, for `None`). The slot
    /// stays taken until it actually stops, so a scan started right afterwards is not
    /// mistaken for the cancelled one.
    fn cancel(&self, scan: Option<u64>) {
        if let Some(run) = self.running.lock().unwrap().as_ref().filter(|r| scan.is_none_or(|id| id == r.id)) {
            run.progress.cancel.store(true, Relaxed);
        }
    }

    /// Ends `run`, storing `view` only if `run` is still the running scan and was not
    /// cancelled. Both are checked under the lock `cancel` takes, so a cancel either lands
    /// before this and wins, or finds the slot already empty.
    fn publish(&self, run: &Arc<Run>, drive: &str, view: Option<View>) -> Outcome {
        let mut running = self.running.lock().unwrap();
        if !running.as_ref().is_some_and(|r| Arc::ptr_eq(r, run)) {
            return Outcome::Superseded;
        }
        *running = None;
        match view {
            Some(view) if !run.progress.cancelled() => {
                self.views.lock().unwrap().insert(drive.to_string(), Arc::new(view));
                Outcome::Published
            }
            _ => Outcome::Cancelled,
        }
    }

    /// Scans `root`, groups it by app and publishes the result. Cancellation is checked
    /// again after each slow step, so a Stop pressed while apps are being detected or the
    /// tree is being grouped still ends the run without results.
    fn complete(&self, run: &Arc<Run>, root: &Path, drive: &str, detect_apps: impl FnOnce() -> Vec<apps::InstalledApp>) -> (Outcome, u64) {
        let p = &run.progress;
        let tree = scan::scan(root, p);
        let view = (|| {
            if p.cancelled() {
                return None;
            }
            let apps = detect_apps();
            if p.cancelled() {
                return None;
            }
            let view = View::build(tree, &apps);
            (!p.cancelled()).then_some(view)
        })();
        let files = view.as_ref().map_or(0, |v| v.tree.file_count());
        (self.publish(run, drive, view), files)
    }
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    scan: u64,
    drive: String,
    files: u64,
    dirs: u64,
    bytes: u64,
    current: String,
}

#[derive(Clone, Serialize)]
struct ScanSummary {
    scan: u64,
    drive: String,
    files: u64,
    dirs: u64,
    /// Paths that could not be read, by cause, with a few examples of each.
    errors: scan::ErrorReport,
    elapsed_ms: u64,
}

#[derive(Clone, Serialize)]
struct ScanCancelled {
    scan: u64,
    drive: String,
}

#[derive(Serialize)]
struct AppStatus {
    elevated: bool,
    /// Drive to scan right away, set when the app restarted itself as administrator.
    startup_scan: Option<String>,
}

#[tauri::command]
fn app_status() -> AppStatus {
    AppStatus { elevated: elevation::is_elevated(), startup_scan: elevation::startup_scan(std::env::args()) }
}

/// Relaunches with administrator rights (Windows shows the UAC prompt) and closes this window.
/// `scan` is the drive the new window should scan as soon as it opens.
#[tauri::command]
fn restart_as_admin(app: AppHandle, scan: Option<String>) -> Result<(), String> {
    let args = scan.as_deref().map(elevation::scan_args).unwrap_or_default();
    match elevation::relaunch_elevated(&args) {
        Ok(()) => {
            // Under `tauri dev`, exiting would also stop the dev server the new window loads from.
            if !cfg!(debug_assertions) {
                app.exit(0);
            }
            Ok(())
        }
        Err(elevation::RelaunchError::Cancelled) => Err("cancelled".into()),
        Err(elevation::RelaunchError::Failed(e)) => Err(format!("Couldn't restart as administrator: {e}")),
    }
}

#[tauri::command]
fn list_drives() -> Vec<drives::DriveInfo> {
    drives::list()
}

/// Starts scanning `drive` and returns the id its events will carry.
#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>, drive: String) -> Result<u64, String> {
    let root = PathBuf::from(&drive);
    if !root.is_dir() {
        return Err(format!("{drive} is not available."));
    }
    let run = state.begin();
    let scan = run.id;

    std::thread::Builder::new()
        .name("scan".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let started = Instant::now();
            let finished = Arc::new(AtomicBool::new(false));
            let ticker = {
                let (app, run, finished, drive) = (app.clone(), run.clone(), finished.clone(), drive.clone());
                std::thread::spawn(move || {
                    let p = &run.progress;
                    while !finished.load(Relaxed) {
                        let _ = app.emit(
                            "scan-progress",
                            ProgressEvent {
                                scan: run.id,
                                drive: drive.clone(),
                                files: p.files.load(Relaxed),
                                dirs: p.dirs.load(Relaxed),
                                bytes: p.bytes.load(Relaxed),
                                current: p.current.lock().map(|c| c.clone()).unwrap_or_default(),
                            },
                        );
                        std::thread::sleep(Duration::from_millis(120));
                    }
                })
            };

            let state = app.state::<AppState>();
            let (outcome, files) = state.complete(&run, &root, &drive, apps::installed_apps);
            // Progress stops before the outcome is reported, so no progress event for this
            // run can arrive after it.
            finished.store(true, Relaxed);
            let _ = ticker.join();

            match outcome {
                Outcome::Published => {
                    let summary = ScanSummary {
                        scan: run.id,
                        drive,
                        files,
                        dirs: run.progress.dirs.load(Relaxed),
                        errors: run.progress.errors.report(),
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    };
                    let _ = app.emit("scan-done", summary);
                }
                Outcome::Cancelled => {
                    let _ = app.emit("scan-cancelled", ScanCancelled { scan: run.id, drive });
                }
                Outcome::Superseded => {}
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(scan)
}

/// Stops scan `scan`, or whichever scan is running when no id is given.
#[tauri::command]
fn cancel_scan(state: State<AppState>, scan: Option<u64>) {
    state.cancel(scan);
}

fn lookup(state: &State<AppState>, drive: &str, id: &str) -> Result<(Arc<View>, Id), String> {
    let view = state.views.lock().unwrap().get(drive).cloned().ok_or_else(|| format!("{drive} has not been scanned yet."))?;
    let id = Id::parse(id).filter(|i| view.contains(i.clone())).ok_or_else(|| format!("Unknown item {id}."))?;
    Ok((view, id))
}

/// One node of a scanned drive and its children, which is what a row in the list expands to.
#[tauri::command]
fn get_node(state: State<AppState>, drive: String, id: String) -> Result<ViewNode, String> {
    let (view, id) = lookup(&state, &drive, &id)?;
    Ok(view.view(id, 1))
}

/// One node of a scanned drive with the nested levels the treemap draws.
#[tauri::command]
fn get_map(state: State<AppState>, drive: String, id: String) -> Result<ViewNode, String> {
    let (view, id) = lookup(&state, &drive, &id)?;
    Ok(view.map(id))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![app_status, restart_as_admin, list_drives, start_scan, cancel_scan, get_node, get_map])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(state: &AppState) -> Option<u64> {
        state.running.lock().unwrap().as_ref().map(|r| r.id)
    }

    fn drive_with_a_file() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.bin"), [0u8; 10]).unwrap();
        dir
    }

    fn stored(state: &AppState, drive: &str) -> Option<Arc<View>> {
        state.views.lock().unwrap().get(drive).cloned()
    }

    #[test]
    fn each_scan_gets_a_new_id() {
        let state = AppState::default();
        let a = state.begin();
        let b = state.begin();
        assert!(b.id > a.id);
        assert!(a.progress.cancelled(), "starting a scan cancels the one it replaces");
        assert_eq!(slot(&state), Some(b.id));
    }

    #[test]
    fn a_finished_scan_publishes_its_view() {
        let dir = drive_with_a_file();
        let state = AppState::default();
        let run = state.begin();
        let (outcome, files) = state.complete(&run, dir.path(), "C:\\", Vec::new);
        assert_eq!(outcome, Outcome::Published);
        assert_eq!(files, 1);
        assert!(stored(&state, "C:\\").is_some());
        assert_eq!(slot(&state), None);
    }

    /// Stop pressed during traversal: the run ends without results and the previous scan
    /// of the drive stays on screen.
    #[test]
    fn cancel_during_traversal_publishes_nothing() {
        let dir = drive_with_a_file();
        let state = AppState::default();
        let first = state.begin();
        assert_eq!(state.complete(&first, dir.path(), "C:\\", Vec::new).0, Outcome::Published);
        let before = stored(&state, "C:\\").unwrap();

        let run = state.begin();
        state.cancel(Some(run.id));
        let (outcome, _) = state.complete(&run, dir.path(), "C:\\", || panic!("a cancelled scan must not go on to detect apps"));
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(Arc::ptr_eq(&stored(&state, "C:\\").unwrap(), &before), "the earlier view is kept");
    }

    /// Stop pressed while installed apps are being read or the tree grouped. This used to
    /// publish the view and report a successful scan anyway.
    #[test]
    fn cancel_during_grouping_publishes_nothing() {
        let dir = drive_with_a_file();
        let state = AppState::default();
        let run = state.begin();
        let (outcome, _) = state.complete(&run, dir.path(), "C:\\", || {
            state.cancel(None);
            Vec::new()
        });
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(stored(&state, "C:\\").is_none());
        assert_eq!(slot(&state), None, "the cancelled run frees the slot");
    }

    /// A cancel that lands after grouping but before the view is stored still wins.
    #[test]
    fn cancel_just_before_publishing_wins() {
        let dir = drive_with_a_file();
        let state = AppState::default();
        let run = state.begin();
        let view = View::build(scan::scan(dir.path(), &run.progress), &[]);
        state.cancel(Some(run.id));
        assert_eq!(state.publish(&run, "C:\\", Some(view)), Outcome::Cancelled);
        assert!(stored(&state, "C:\\").is_none());
    }

    /// Rescanning the same drive: the older run, finishing late, must neither replace the
    /// newer scan's view nor report anything that would end the newer scan in the UI.
    #[test]
    fn a_superseded_same_drive_scan_cannot_overwrite_the_newer_one() {
        let dir = drive_with_a_file();
        let state = AppState::default();
        let old = state.begin();
        let new = state.begin();

        assert_eq!(state.complete(&new, dir.path(), "C:\\", Vec::new).0, Outcome::Published);
        let newer = stored(&state, "C:\\").unwrap();
        assert_eq!(state.complete(&old, dir.path(), "C:\\", Vec::new).0, Outcome::Superseded);
        assert!(Arc::ptr_eq(&stored(&state, "C:\\").unwrap(), &newer));
    }

    /// The old run can also finish while the new one is still going; that must leave the
    /// new run in place.
    #[test]
    fn a_superseded_scan_leaves_the_running_one_alone() {
        let dir = drive_with_a_file();
        let state = AppState::default();
        let old = state.begin();
        let new = state.begin();
        assert_eq!(state.complete(&old, dir.path(), "C:\\", Vec::new).0, Outcome::Superseded);
        assert_eq!(slot(&state), Some(new.id));
        assert!(!new.progress.cancelled());
    }

    /// Stop for a run that already ended must not cancel the run that replaced it.
    #[test]
    fn cancel_names_the_run_it_stops() {
        let state = AppState::default();
        let old = state.begin();
        let new = state.begin();
        state.cancel(Some(old.id));
        assert!(!new.progress.cancelled());
        state.cancel(Some(new.id));
        assert!(new.progress.cancelled());
    }

    /// Cancelling then restarting must not let the old scan's "cancelled" land on the new one.
    #[test]
    fn cancelling_keeps_the_slot_so_a_restart_is_not_confused() {
        let state = AppState::default();
        let cancelled = state.begin();
        state.cancel(None);
        assert_eq!(slot(&state), Some(cancelled.id), "the slot is held until the scan stops");
        assert_eq!(state.publish(&cancelled, "C:\\", None), Outcome::Cancelled);

        let restarted = state.begin();
        assert!(!restarted.progress.cancelled());
        assert_ne!(restarted.id, cancelled.id);
    }

    #[test]
    fn cancelling_without_a_scan_does_nothing() {
        let state = AppState::default();
        state.cancel(None);
        assert_eq!(slot(&state), None);
    }
}
