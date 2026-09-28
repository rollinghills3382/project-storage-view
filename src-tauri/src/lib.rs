pub mod apps;
pub mod drives;
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
            let state = app.state::<AppState>();
            state.views.lock().unwrap().insert(drive, Arc::new(view));
            let mut running = state.running.lock().unwrap();
            if running.as_ref().is_some_and(|p| Arc::ptr_eq(p, &progress)) {
                *running = None;
            }
            drop(running);
            let _ = app.emit("scan-done", summary);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn cancel_scan(state: State<AppState>) {
    if let Some(p) = state.running.lock().unwrap().take() {
        p.cancel.store(true, Relaxed);
    }
}

/// One node of a scanned drive with two levels of children, which is what the treemap draws.
#[tauri::command]
fn get_node(state: State<AppState>, drive: String, id: String) -> Result<ViewNode, String> {
    let view = state.views.lock().unwrap().get(&drive).cloned().ok_or_else(|| format!("{drive} has not been scanned yet."))?;
    let id = Id::parse(&id).filter(|&i| view.contains(i)).ok_or_else(|| format!("Unknown item {id}."))?;
    Ok(view.view(id, 2))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![list_drives, start_scan, cancel_scan, get_node])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
