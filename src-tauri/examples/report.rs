//! Prints the app-grouped breakdown of a folder or drive, for checking the grouping rules
//! without the UI: `cargo run --release --example report -- C:\`
//!
//! `--json <file>` also writes the drive root and its largest folders in the shape the UI
//! requests them, so the browser preview (`npm run dev`) can replay a real scan.
use std::path::PathBuf;
use std::time::Instant;
use storage_view_lib::{apps, drives, grouping::{Id, View, ViewNode}, scan};

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
        "{} · {} files · {} paths denied · {} registered apps · {:.1} s\n",
        root.display(),
        view.tree.file_count(),
        progress.errors.count(scan::Problem::Denied),
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
        write_json(&view, &root, started.elapsed().as_millis() as u64, count(&progress.dirs), &progress.errors.report(), &out);
    }
}

fn collect_tails(n: &ViewNode, out: &mut Vec<String>) {
    for c in n.children.iter().flatten() {
        // A "smaller items" tile is saved as a childless placeholder inside its parent's map,
        // so it needs saving in its own right or the preview cannot open it.
        if c.kind == "more" {
            out.push(c.id.clone());
        }
        collect_tails(c, out);
    }
}

/// Saves one id and returns the map that was saved, so its "smaller items" tiles can be found.
fn save(
    view: &View,
    id: Id,
    maps: &mut serde_json::Map<String, serde_json::Value>,
    nodes: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<ViewNode> {
    let v = view.view(id.clone(), 1);
    if !v.has_children {
        return None;
    }
    let m = view.map(id);
    maps.insert(v.id.clone(), serde_json::to_value(&m).unwrap());
    nodes.insert(v.id.clone(), serde_json::to_value(&v).unwrap());
    Some(m)
}

fn write_json(view: &View, root: &std::path::Path, elapsed_ms: u64, dirs: u64, errors: &scan::ErrorReport, out: &str) {
    // `maps` is what `get_map` returns for an id and `nodes` what `get_node` returns.
    let mut maps = serde_json::Map::new();
    let mut nodes = serde_json::Map::new();

    // Enough to zoom in twice: the drive, every top-level block, and the largest folders
    // inside the big ones.
    let mut batch: Vec<Id> = vec![Id::Root];
    for (i, child) in view.children(Id::Root).into_iter().enumerate() {
        batch.push(child.clone());
        if i < 12 {
            batch.extend(view.children(child).into_iter().take(8));
        }
    }
    let mut all: Vec<ViewNode> = batch.iter().filter_map(|id| save(view, id.clone(), &mut maps, &mut nodes)).collect();

    // A "smaller items" tile is written as a childless placeholder inside its parent's map, so
    // it has to be saved in its own right or the preview throws when it is opened. Each round
    // looks for tiles across everything saved so far: a block saved on its own expands further
    // than the same block inside the drive's map, so the drive's map alone misses most of them.
    // Tiles nested several rounds deep are left out: saving every one of them roughly doubles
    // the fixture, and the app itself opens all of them regardless.
    for _ in 0..3 {
        let mut tails: Vec<String> = Vec::new();
        for m in &all {
            collect_tails(m, &mut tails);
        }
        tails.sort();
        tails.dedup();
        let next: Vec<Id> = tails
            .into_iter()
            .filter(|s| !maps.contains_key(s))
            .take(200)
            .filter_map(|s| Id::parse(&s))
            .collect();
        if next.is_empty() {
            break;
        }
        let added: Vec<ViewNode> = next.iter().filter_map(|id| save(view, id.clone(), &mut maps, &mut nodes)).collect();
        if added.is_empty() {
            break;
        }
        all.extend(added);
    }
    let drive = drives::list().into_iter().find(|d| d.mount.eq_ignore_ascii_case(&root.display().to_string()));
    let doc = serde_json::json!({
        "drive": drive,
        "summary": { "drive": root.display().to_string(), "files": view.tree.file_count(), "dirs": dirs, "errors": errors, "elapsed_ms": elapsed_ms },
        "maps": maps,
        "nodes": nodes,
    });
    std::fs::write(out, serde_json::to_string(&doc).unwrap()).unwrap();
    println!("
wrote {out}");
}
