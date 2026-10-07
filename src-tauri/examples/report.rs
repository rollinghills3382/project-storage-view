//! Prints the app-grouped breakdown of a folder or drive, for checking the grouping rules
//! without the UI: `cargo run --release --example report -- C:\`
//!
//! `--json <file>` also writes the drive root and its largest folders in the shape the UI
//! requests them, so the browser preview (`npm run dev`) can replay a real scan.
use std::path::PathBuf;
use std::time::Instant;
use storage_view_lib::{apps, drives, grouping::{Id, View}, scan};

fn gb(bytes: u64) -> String {
    format!("{:>8.2} GB", bytes as f64 / (1u64 << 30) as f64)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_out = args.iter().position(|a| a == "--json").and_then(|i| args.get(i + 1)).cloned();
    let root = PathBuf::from(args.first().filter(|a| *a != "--json").cloned().unwrap_or_else(|| r"C:\".into()));
    let started = Instant::now();
    let progress = scan::Progress::default();
    let tree = scan::scan(&root, &progress);
    let installed = apps::installed_apps();
    let view = View::build(tree, &installed);
    println!(
        "{} · {} files · {} unreadable folders · {} registered apps · {:.1} s\n",
        root.display(),
        view.tree.file_count(),
        progress.denied.load(std::sync::atomic::Ordering::Relaxed),
        installed.len(),
        started.elapsed().as_secs_f64()
    );
    let top = view.view(Id::Root, 2);
    for c in top.children.unwrap_or_default().iter().take(40) {
        let cat = c.category.map(|c| format!("{c:?}")).unwrap_or_default();
        println!("{}  {:<12} {:<6} {}", gb(c.size), cat, c.kind, c.name);
        for l in c.locations.iter().filter(|_| c.locations.len() > 1) {
            println!("{}  {:<12} {:<6}   └ {}", gb(l.size), "", "", l.path);
        }
    }

    if let Some(out) = json_out {
        let count = |n: &std::sync::atomic::AtomicU64| n.load(std::sync::atomic::Ordering::Relaxed);
        write_json(&view, &root, started.elapsed().as_millis() as u64, count(&progress.dirs), count(&progress.denied), &out);
    }
}

fn write_json(view: &View, root: &std::path::Path, elapsed_ms: u64, dirs: u64, denied: u64, out: &str) {
    // `maps` is what `get_map` returns for an id and `nodes` what `get_node` returns.
    let mut maps = serde_json::Map::new();
    let mut nodes = serde_json::Map::new();
    let mut add = |id: Id| {
        let v = view.view(id.clone(), 1);
        if v.has_children {
            maps.insert(v.id.clone(), serde_json::to_value(view.map(id)).unwrap());
            nodes.insert(v.id.clone(), serde_json::to_value(&v).unwrap());
        }
    };
    add(Id::Root);
    // Enough to zoom in twice: every top-level block, and the largest folders inside the big ones.
    for (i, child) in view.children(Id::Root).into_iter().enumerate() {
        add(child.clone());
        if i < 12 {
            for grandchild in view.children(child).into_iter().take(8) {
                add(grandchild);
            }
        }
    }
    let drive = drives::list().into_iter().find(|d| d.mount.eq_ignore_ascii_case(&root.display().to_string()));
    let doc = serde_json::json!({
        "drive": drive,
        "summary": { "drive": root.display().to_string(), "files": view.tree.file_count(), "dirs": dirs, "denied": denied, "elapsed_ms": elapsed_ms },
        "maps": maps,
        "nodes": nodes,
    });
    std::fs::write(out, serde_json::to_string(&doc).unwrap()).unwrap();
    println!("
wrote {out}");
}
