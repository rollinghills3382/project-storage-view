//! Finding items on a scanned drive by name.
//!
//! Every app, folder and file over the small-file limit can be found. Files under it are
//! bundled per folder during the scan and keep no names, so they cannot. Each result comes
//! with the items it sits inside, exactly as the list and the map show them (through the app
//! that claimed it, and through any "smaller items" tile it is bundled into), so the UI can
//! open the way to it without asking again.

use super::{short_location, Id, View, ViewNode, LIMIT_TOP, UNOWNED};
use crate::scan::{NodeKind, Tree};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;

/// Results returned at most, largest first.
pub const MAX_RESULTS: usize = 200;

#[derive(Debug, Serialize)]
pub struct Found {
    pub node: ViewNode,
    /// What it sits inside, from the drive down to its parent, as the list and the map show it.
    pub chain: Vec<ViewNode>,
}

#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub query: String,
    /// The largest matches, at most `MAX_RESULTS`.
    pub found: Vec<Found>,
    /// Every match, including those past `found`.
    pub total: usize,
}

/// Every word of the query has to appear in the name, ignoring case.
struct Query {
    words: Vec<String>,
}

impl Query {
    fn new(text: &str) -> Query {
        Query { words: text.split_whitespace().map(str::to_lowercase).collect() }
    }

    fn matches(&self, name: &str) -> bool {
        self.words.iter().all(|w| contains_folded(name, w))
    }
}

/// Whether `name` contains `word`, which is already lower case, ignoring case. Most names and
/// queries are ASCII, which is compared byte by byte without building a lower-case copy.
fn contains_folded(name: &str, word: &str) -> bool {
    if word.is_ascii() {
        name.as_bytes().windows(word.len()).any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
    } else {
        name.to_lowercase().contains(word)
    }
}

impl View {
    /// Apps, folders and files whose names contain every word of `text`, largest first.
    pub fn search(&self, text: &str) -> SearchResults {
        let query = Query::new(text);
        let mut hits: Vec<Id> = Vec::new();
        if !query.words.is_empty() {
            let groups = (0..self.groups.len() as u32).filter(|&g| {
                let group = &self.groups[g as usize];
                group.size > 0 && query.matches(&group.name)
            });
            let nodes: Vec<Id> = (1..self.tree.nodes.len() as u32)
                .into_par_iter()
                .filter(|&n| {
                    let node = self.tree.node(n);
                    // Nothing to show for an item that counts 0 bytes: the view leaves it out.
                    !matches!(node.kind, NodeKind::SmallFiles { .. }) && self.eff[n as usize] > 0 && query.matches(&node.name)
                })
                .map(|n| self.shown_as(n))
                .collect();
            // An app is found by its name and by the name of the folder it stands for.
            let mut seen = vec![false; self.groups.len()];
            for id in groups.map(Id::Group).chain(nodes) {
                if let Id::Group(g) = id {
                    if std::mem::replace(&mut seen[g as usize], true) {
                        continue;
                    }
                }
                hits.push(id);
            }
        }
        let mut sized: Vec<(u64, Id)> = hits.into_iter().map(|id| (self.size(id.clone()), id)).collect();
        sized.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
        let total = sized.len();
        let mut kids = HashMap::new();
        let found = sized.into_iter().take(MAX_RESULTS).map(|(_, id)| self.found(id, &mut kids)).collect();
        SearchResults { query: text.to_string(), found, total }
    }

    /// The id a node is shown under. An app with a single location is shown in its place.
    fn shown_as(&self, n: u32) -> Id {
        match self.owner[n as usize] {
            UNOWNED => Id::Node(n),
            g if self.groups[g as usize].locations.len() == 1 => Id::Group(g),
            _ => Id::Node(n),
        }
    }

    /// Where a node sits in the view, from the drive down to its parent, before any
    /// "smaller items" tiles. The nearest folder an app claimed decides it: claimed folders
    /// are shown under their app, not where they are on disk.
    fn parents(&self, id: &Id) -> Vec<Id> {
        let mut up = Vec::new();
        if let Id::Node(n) = *id {
            let mut cur = n;
            loop {
                let owner = self.owner[cur as usize];
                if owner != UNOWNED {
                    // An app with one location lists that folder's contents directly.
                    if cur != n && self.groups[owner as usize].locations.len() > 1 {
                        up.push(Id::Node(cur));
                    }
                    up.push(Id::Group(owner));
                    break;
                }
                if cur != n {
                    if cur == Tree::ROOT {
                        break;
                    }
                    up.push(Id::Node(cur));
                }
                cur = self.tree.node(cur).parent;
            }
        }
        up.push(Id::Root);
        up.reverse();
        up
    }

    fn found(&self, id: Id, kids: &mut HashMap<String, Vec<Id>>) -> Found {
        let mut steps = self.parents(&id);
        steps.push(id);
        // A child past the display limit sits in its parent's "smaller items" tile, which
        // lists the rest of the parent and caps again, so it can take several tiles to reach.
        let mut chain: Vec<Id> = Vec::new();
        for pair in steps.windows(2) {
            let (parent, child) = (&pair[0], &pair[1]);
            chain.push(parent.clone());
            let siblings = kids.entry(parent.key()).or_insert_with(|| self.children(parent.clone()));
            let mut at = siblings.iter().position(|s| s == child).unwrap_or(0);
            let mut holder = parent.clone();
            while at >= LIMIT_TOP {
                holder = Id::Tail(Box::new(holder), LIMIT_TOP);
                chain.push(holder.clone());
                at -= LIMIT_TOP;
            }
        }
        let mut node = self.describe(steps.pop().unwrap());
        let mut chain: Vec<ViewNode> = chain.into_iter().map(|id| self.describe(id)).collect();
        // The view labels an app's locations by path, since they often share a folder name.
        let mut after_app = false;
        for v in chain.iter_mut().chain(std::iter::once(&mut node)) {
            if after_app {
                if let Some(path) = &v.path {
                    v.name = short_location(path);
                }
            }
            after_app = v.kind == "app" && v.locations.len() > 1;
        }
        Found { node, chain }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::InstalledApp;
    use crate::scan::{scan, Progress, SMALL_FILE_LIMIT};
    use std::fs;
    use std::path::Path;

    const MB: u64 = SMALL_FILE_LIMIT;

    fn write(root: &Path, rel: &str, bytes: u64) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; bytes as usize]).unwrap();
    }

    fn app(root: &Path, name: &str, publisher: &str, rel: &str) -> InstalledApp {
        InstalledApp { name: name.into(), publisher: publisher.into(), location: root.join(rel), steam: false }
    }

    fn names(v: &[ViewNode]) -> Vec<&str> {
        v.iter().map(|n| n.name.as_str()).collect()
    }

    /// Forward slashes, so these run on every platform.
    fn sample() -> (tempfile::TempDir, View) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "Program Files/Docker/Docker/resources/docker.exe", 4 * MB);
        write(r, "Users/me/AppData/Local/Docker/wsl/disk.vhdx", 50 * MB);
        write(r, "Users/me/Videos/Holiday.mp4", 9 * MB);
        write(r, "Users/me/Videos/holiday notes.txt", 2 * MB);
        write(r, "Users/me/Videos/tiny holiday.txt", 10);
        write(r, "Users/me/Documents/empty holiday.doc", 0);
        write(r, "Program Files/Notepad++/notepad++.exe", 3 * MB);
        let apps = vec![app(r, "Docker Desktop", "Docker Inc.", "Program Files/Docker/Docker"), app(r, "Notepad++", "Notepad++ Team", "Program Files/Notepad++")];
        let v = View::build(scan(r, &Progress::default()), &apps);
        (dir, v)
    }

    #[test]
    fn names_match_ignoring_case_and_every_word_counts() {
        let (_d, v) = sample();
        let r = v.search("HOLIDAY");
        let found: Vec<&str> = r.found.iter().map(|f| f.node.name.as_str()).collect();
        assert_eq!(found, ["Holiday.mp4", "holiday notes.txt"]);
        assert_eq!(r.total, 2, "small files are bundled and empty ones take no space, so neither is found");
        assert_eq!(v.search("notes holiday").total, 1);
        assert_eq!(v.search("holiday nowhere").total, 0);
        assert_eq!(v.search("   ").total, 0);
    }

    #[test]
    fn non_ascii_names_match_ignoring_case() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Über Projekt/Größe.bin", 2 * MB);
        let v = View::build(scan(dir.path(), &Progress::default()), &[]);
        assert_eq!(v.search("über").total, 1);
        assert_eq!(v.search("GRÖẞE").total, 1);
        assert_eq!(v.search("GRÖSSE").total, 0, "letters are folded one by one, so ß is not ss");
    }

    /// A file in an app's data is shown under the app, not under the user's folder where it
    /// sits on disk, so that is the way to it.
    #[test]
    fn the_way_to_a_result_goes_through_the_app_that_claimed_it() {
        let (_d, v) = sample();
        let r = v.search("disk.vhdx");
        let f = &r.found[0];
        assert_eq!(f.node.name, "disk.vhdx");
        assert_eq!(f.chain[0].kind, "drive");
        assert_eq!(f.chain[1].name, "Docker Desktop");
        // Docker has two locations, which the view labels by path.
        assert!(f.chain[2].name.ends_with("Local/Docker") || f.chain[2].name.ends_with(r"Local\Docker"), "{:?}", f.chain[2].name);
        assert_eq!(names(&f.chain[3..]), ["wsl"]);
        assert_reachable(&v, f);
    }

    /// An app with one location stands for its folder, which the view never shows on its own.
    #[test]
    fn a_folder_an_app_stands_for_is_found_as_the_app() {
        let (_d, v) = sample();
        let r = v.search("notepad++");
        let shown: Vec<(&str, &str)> = r.found.iter().map(|f| (f.node.name.as_str(), f.node.kind)).collect();
        assert_eq!(shown, [("Notepad++", "app"), ("notepad++.exe", "file")], "the app once, though its name and its folder's both match");
        assert_eq!(names(&r.found[1].chain), [v.tree.root_path.display().to_string().as_str(), "Notepad++"]);
        for f in &r.found {
            assert_reachable(&v, f);
        }
    }

    /// Past the display limit an item sits in a "smaller items" tile, or in a tile inside one.
    #[test]
    fn a_result_past_the_display_limit_is_reached_through_smaller_items_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let n = LIMIT_TOP * 2 + 5;
        for i in 0..n {
            // Largest first, so d000 is listed first and the last few are two tiles deep.
            write(r, &format!("big/d{i:03}/f.bin"), MB + (n - i) as u64 * 1000);
        }
        let v = View::build(scan(r, &Progress::default()), &[]);
        for (name, tiles) in [("d000", 0), (&format!("d{:03}", LIMIT_TOP), 1), (&format!("d{:03}", n - 1), 2)] {
            let res = v.search(name);
            let f = res.found.iter().find(|f| f.node.name == *name).unwrap();
            assert_eq!(f.chain.iter().filter(|c| c.kind == "more").count(), tiles, "{name}");
            assert_reachable(&v, f);
        }
    }

    #[test]
    fn results_are_largest_first_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let n = MAX_RESULTS + 10;
        for i in 0..n {
            write(r, &format!("logs/run{i:03}.log"), MB + i as u64 * 1000);
        }
        let v = View::build(scan(r, &Progress::default()), &[]);
        let res = v.search(".log");
        assert_eq!(res.total, n);
        assert_eq!(res.found.len(), MAX_RESULTS);
        assert!(res.found.windows(2).all(|w| w[0].node.size >= w[1].node.size));
        assert_eq!(res.found[0].node.name, format!("run{:03}.log", n - 1));
    }

    /// Opening each step of the chain the way the UI does lists the next one, ending at the result.
    fn assert_reachable(v: &View, f: &Found) {
        let mut steps = f.chain.iter().chain(std::iter::once(&f.node));
        let mut cur = steps.next().unwrap();
        assert_eq!(cur.id, "root");
        for next in steps {
            let opened = v.view(Id::parse(&cur.id).unwrap(), 1);
            let listed = opened.children.unwrap();
            assert!(listed.iter().any(|c| c.id == next.id), "{} is not listed in {}", next.name, cur.name);
            cur = next;
        }
    }
}
