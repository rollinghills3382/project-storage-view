pub mod apps;
pub mod drives;
mod elevation;
pub mod grouping;
pub mod scan;

use grouping::{Id, View, ViewNode};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
struct AppState {
    /// Finished scans, keyed by drive root (`C:\`).
    views: Mutex<HashMap<String, Arc<View>>>,
    running: Mutex<Option<Arc<scan::Progress>>>,
}

impl AppState {
    /// Claims the result slot for `progress` and clears it.
    ///
    /// False means a newer scan took over while this one was running, so this scan is
    /// superseded: it must not store its tree or report anything to the UI, otherwise it
    /// would overwrite fresher data and reset the progress of the scan the user is watching.
    fn retire(&self, progress: &Arc<scan::Progress>) -> bool {
        let mut running = self.running.lock().unwrap();
        if running.as_ref().is_some_and(|p| Arc::ptr_eq(p, progress)) {
            *running = None;
            true
        } else {
            false
        }
    }

    /// Asks the running scan to stop. The slot stays taken until it actually stops, so a
    /// scan started right afterwards is not mistaken for the cancelled one.
    fn cancel(&self) {
        if let Some(p) = self.running.lock().unwrap().as_ref() {
            p.cancel.store(true, Relaxed);
        }
    }
}

#[derive(Clone, Serialize)]
struct ProgressEvent {
    drive: String,
    files: u64,
    dirs: u64,
    bytes: u64,
    current: String,
}

#[derive(Clone, Serialize)]
struct ScanSummary {
    drive: String,
    files: u64,
    dirs: u64,
    /// Folders Windows would not let us read (usually needs administrator rights).
    denied: u64,
    elapsed_ms: u64,
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

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>, drive: String) -> Result<(), String> {
    let root = PathBuf::from(&drive);
    if !root.is_dir() {
        return Err(format!("{drive} is not available."));
    }
    let progress = Arc::new(scan::Progress::default());
    if let Some(old) = state.running.lock().unwrap().replace(progress.clone()) {
        old.cancel.store(true, Relaxed);
    }

    std::thread::Builder::new()
        .name("scan".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let started = Instant::now();
            let finished = Arc::new(AtomicBool::new(false));
            let ticker = {
                let (app, progress, finished, drive) = (app.clone(), progress.clone(), finished.clone(), drive.clone());
                std::thread::spawn(move || {
                    while !finished.load(Relaxed) {
                        let _ = app.emit(
                            "scan-progress",
                            ProgressEvent {
                                drive: drive.clone(),
                                files: progress.files.load(Relaxed),
                                dirs: progress.dirs.load(Relaxed),
                                bytes: progress.bytes.load(Relaxed),
                                current: progress.current.lock().map(|c| c.clone()).unwrap_or_default(),
                            },
                        );
                        std::thread::sleep(Duration::from_millis(120));
                    }
                })
            };

            let tree = scan::scan(&root, &progress);
            let view = (!progress.cancelled()).then(|| View::build(tree, &apps::installed_apps()));
            finished.store(true, Relaxed);
            let _ = ticker.join();

            let state = app.state::<AppState>();
            // A scan that was replaced while walking is dropped whole, so the UI never sees
            // stale results or a stray "cancelled" for a scan the user never cancelled.
            if !state.retire(&progress) {
                return;
            }
            let Some(view) = view else {
                let _ = app.emit("scan-cancelled", &drive);
                return;
            };
            let summary = ScanSummary {
                drive: drive.clone(),
                files: view.tree.file_count(),
                dirs: progress.dirs.load(Relaxed),
                denied: progress.denied.load(Relaxed),
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
            state.views.lock().unwrap().insert(drive, Arc::new(view));
            let _ = app.emit("scan-done", summary);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    state.cancel();
}

fn lookup(state: &State<AppState>, drive: &str, id: &str) -> Result<(Arc<View>, Id), String> {
    let view = state.views.lock().unwrap().get(drive).cloned().ok_or_else(|| format!("{drive} has not been scanned yet."))?;
    let id = Id::parse(id).filter(|&i| view.contains(i)).ok_or_else(|| format!("Unknown item {id}."))?;
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

    fn slot(state: &AppState) -> Option<Arc<scan::Progress>> {
        state.running.lock().unwrap().clone()
    }

    /// A scan that a newer one replaced must report nothing, so it cannot reset the
    /// progress of the scan the user is actually watching.
    #[test]
    fn superseded_scans_report_nothing() {
        let state = AppState::default();
        let first = Arc::new(scan::Progress::default());
        *state.running.lock().unwrap() = Some(first.clone());
        let second = Arc::new(scan::Progress::default());
        state.running.lock().unwrap().replace(second.clone());

        assert!(!state.retire(&first), "the replaced scan must not claim the result");
        assert!(slot(&state).is_some(), "the newer scan still owns the slot");
        assert!(state.retire(&second));
        assert!(slot(&state).is_none());
    }

    /// Cancelling then restarting must not let the old scan's "cancelled" land on the new one.
    #[test]
    fn cancelling_keeps_the_slot_so_a_restart_is_not_confused() {
        let state = AppState::default();
        let cancelled = Arc::new(scan::Progress::default());
        *state.running.lock().unwrap() = Some(cancelled.clone());

        state.cancel();
        assert!(cancelled.cancelled());
        assert!(state.retire(&cancelled), "the cancelled scan still reports that it stopped");

        let restarted = Arc::new(scan::Progress::default());
        *state.running.lock().unwrap() = Some(restarted.clone());
        assert!(state.retire(&restarted));
        assert!(!restarted.cancelled());
    }

    #[test]
    fn cancelling_without_a_scan_does_nothing() {
        let state = AppState::default();
        state.cancel();
        assert!(slot(&state).is_none());
    }
}
