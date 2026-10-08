//! Turns a raw folder tree into the app-grouped view shown in the treemap.
//!
//! Each installed app claims its install folder plus any folders under `AppData` and
//! `ProgramData` whose names match it. A claimed folder is removed from wherever it sits
//! in the folder tree and shown under its app instead, so nothing is counted twice.

use crate::apps::{self, Category, InstalledApp};
use crate::scan::{NodeKind, Tree};
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

const UNOWNED: u32 = u32::MAX;
/// Children returned for the level being viewed, and for each level nested inside it.
const LIMIT_TOP: usize = 60;
const LIMIT_PREVIEW: usize = 24;
/// The treemap nests up to this many levels, but only inside blocks that are at least
/// 1/`MAP_MIN_SHARE` of the folder being viewed. Anything smaller is too small to draw its contents.
const MAP_DEPTH: u32 = 6;
const MAP_MIN_SHARE: u64 = 300;

#[derive(Debug)]
pub struct Group {
    pub name: String,
    pub category: Category,
    /// Claimed folders, largest first.
    pub locations: Vec<u32>,
    pub size: u64,
}

pub struct View {
    pub tree: Tree,
    pub groups: Vec<Group>,
    /// Group that claimed each node, or `UNOWNED`.
    owner: Vec<u32>,
    /// Node size minus any claimed folders inside it.
    eff: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    Root,
    Group(u32),
    Node(u32),
    /// The children of another id past the first `usize` of them, reached through a
    /// "smaller items" tile. The count is carried so the id stays a plain string.
    Tail(Box<Id>, usize),
}

impl Id {
    pub fn parse(s: &str) -> Option<Id> {
        // Split from the right: the base id of a nested tail contains a `~` too.
        if let Some((base, offset)) = s.rsplit_once('~') {
            let offset: usize = offset.parse().ok()?;
            return Some(Id::Tail(Box::new(Id::parse(base)?), offset));
        }
        if s == "root" {
            return Some(Id::Root);
        }
        let (tag, num) = s.split_at_checked(1)?;
        let num = num.parse().ok()?;
        match tag {
            "g" => Some(Id::Group(num)),
            "n" => Some(Id::Node(num)),
            _ => None,
        }
    }

    fn key(&self) -> String {
        match self {
            Id::Root => "root".into(),
            Id::Group(g) => format!("g{g}"),
            Id::Node(n) => format!("n{n}"),
            Id::Tail(inner, offset) => format!("{}~{offset}", inner.key()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Location {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct ViewNode {
    pub id: String,
    pub name: String,
    pub size: u64,
    /// "drive", "app", "folder", "file", "files" (small files bundled together) or "more".
    pub kind: &'static str,
    pub category: Option<Category>,
    pub path: Option<String>,
    pub locations: Vec<Location>,
    pub has_children: bool,
    pub children: Option<Vec<ViewNode>>,
}

struct Candidate {
    name: String,
    suite_name: Option<String>,
    category: Category,
    apps: usize,
    path: PathBuf,
    tokens: Vec<String>,
}

/// One candidate per app, except apps installed side by side under a publisher folder
/// (`Program Files\Adobe\...`, `Program Files\JetBrains\...`) which become one suite.
fn candidates(apps: &[InstalledApp]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut by_path: HashMap<String, usize> = HashMap::new();
    for app in apps {
        let category = apps::categorize(&app.name, &app.publisher, &app.location, app.steam);
        let publisher = apps::norm(app.publisher.split_whitespace().next().unwrap_or(""));
        let suite = app.location.parent().filter(|dir| {
            let leaf = dir.file_name().map(|f| apps::norm(&f.to_string_lossy())).unwrap_or_default();
            !apps::is_generic(&publisher) && !apps::is_generic(&leaf) && leaf.starts_with(&publisher) && apps::is_usable_location(dir)
        });
        let (path, suite_name, mut tokens) = match suite {
            Some(dir) => {
                let leaf = dir.file_name().unwrap().to_string_lossy().into_owned();
                let tokens = vec![publisher.clone(), apps::norm(&leaf)];
                (dir.to_path_buf(), Some(leaf), tokens)
            }
            None => (app.location.clone(), None, apps::location_token(&app.location).into_iter().collect()),
        };
        tokens.push(apps::base_name(&app.name));
        let key = path.to_string_lossy().to_lowercase();
        match by_path.get(&key) {
            Some(&i) => {
                let c = &mut out[i];
                c.apps += 1;
                c.tokens.extend(tokens);
                if c.category != category {
                    c.category = Category::Productivity;
                }
            }
            None => {
                by_path.insert(key, out.len());
                out.push(Candidate { name: apps::display_name(&app.name), suite_name, category, apps: 1, path, tokens });
            }
        }
    }
    for c in &mut out {
        // A publisher folder holding a single app is just that app.
        if c.apps > 1 {
            if let Some(suite) = c.suite_name.take() {
                c.name = suite;
            }
        }
        c.tokens.retain(|t| !apps::is_generic(t));
        c.tokens.sort();
        c.tokens.dedup();
    }
    out
}

impl View {
    pub fn build(tree: Tree, apps: &[InstalledApp]) -> View {
        let cands = candidates(apps);
        let mut owner = vec![UNOWNED; tree.nodes.len()];
        let mut groups: Vec<Group> = cands
            .iter()
            .map(|c| Group { name: c.name.clone(), category: c.category, locations: Vec::new(), size: 0 })
            .collect();

        for (g, c) in cands.iter().enumerate() {
            if let Some(id) = tree.find(&c.path) {
                claim(&tree, &mut owner, &mut groups, id, g as u32);
            }
        }

        // Folders named after an app in AppData and ProgramData belong to that app.
        let mut tokens: HashMap<&str, Vec<u32>> = HashMap::new();
        for (g, c) in cands.iter().enumerate() {
            for t in &c.tokens {
                tokens.entry(t.as_str()).or_default().push(g as u32);
            }
        }
        let unique = |name: &str| match tokens.get(apps::norm(name).as_str()).map(Vec::as_slice) {
            Some([g]) => Some(*g),
            _ => None,
        };
        for container in data_folders(&tree) {
            for &child in &tree.node(container).children {
                if tree.node(child).kind != NodeKind::Dir || owner[child as usize] != UNOWNED {
                    continue;
                }
                if let Some(g) = unique(&tree.node(child).name) {
                    claim(&tree, &mut owner, &mut groups, child, g);
                    continue;
                }
                // Publisher folders: AppData\Local\Google\Chrome.
                for &grandchild in &tree.node(child).children {
                    if tree.node(grandchild).kind == NodeKind::Dir && owner[grandchild as usize] == UNOWNED {
                        if let Some(g) = unique(&tree.node(grandchild).name) {
                            claim(&tree, &mut owner, &mut groups, grandchild, g);
                        }
                    }
                }
            }
        }

        // Children always have larger ids than their parents, so one reverse pass works.
        let mut eff = vec![0u64; tree.nodes.len()];
        for id in (0..tree.nodes.len()).rev() {
            let node = &tree.nodes[id];
            eff[id] = match node.kind {
                NodeKind::Dir => node.children.iter().filter(|&&c| owner[c as usize] == UNOWNED).map(|&c| eff[c as usize]).sum(),
                _ => node.size,
            };
        }
        for g in &mut groups {
            g.locations.sort_by(|&a, &b| eff[b as usize].cmp(&eff[a as usize]));
            g.size = g.locations.iter().map(|&l| eff[l as usize]).sum();
        }
        View { tree, groups, owner, eff }
    }

    pub fn contains(&self, id: Id) -> bool {
        match id {
            Id::Root => true,
            Id::Group(g) => (g as usize) < self.groups.len(),
            Id::Node(n) => (n as usize) < self.tree.nodes.len(),
            Id::Tail(inner, _) => self.contains(*inner),
        }
    }

    pub fn size(&self, id: Id) -> u64 {
        match id {
            Id::Root => self.tree.node(Tree::ROOT).size,
            Id::Group(g) => self.groups[g as usize].size,
            Id::Node(n) => self.eff[n as usize],
            Id::Tail(inner, offset) => self.children(*inner).iter().skip(offset).map(|c| self.size(c.clone())).sum(),
        }
    }

    fn visible_children(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
        self.tree.node(n).children.iter().copied().filter(|&c| self.owner[c as usize] == UNOWNED && self.eff[c as usize] > 0)
    }

    pub fn children(&self, id: Id) -> Vec<Id> {
        let mut out: Vec<Id> = match id {
            Id::Root => (0..self.groups.len() as u32)
                .filter(|&g| self.groups[g as usize].size > 0)
                .map(Id::Group)
                .chain(self.visible_children(Tree::ROOT).map(Id::Node))
                .collect(),
            Id::Group(g) => match self.groups[g as usize].locations.as_slice() {
                [only] => return self.children(Id::Node(*only)),
                locs => locs.iter().copied().filter(|&l| self.eff[l as usize] > 0).map(Id::Node).collect(),
            },
            Id::Node(n) => self.visible_children(n).map(Id::Node).collect(),
            Id::Tail(inner, offset) => self.children(*inner).into_iter().skip(offset).collect(),
        };
        out.sort_by_key(|c| std::cmp::Reverse(self.size(c.clone())));
        out
    }

    fn category(&self, id: Id) -> Option<Category> {
        let mut cur = match id {
            Id::Root => return None,
            Id::Group(g) => return Some(self.groups[g as usize].category),
            // A "smaller items" tile is part of whatever it hangs off, so it takes that
            // category; for a folder that is the folder's own.
            Id::Tail(inner, _) => return self.category(*inner),
            Id::Node(n) => n,
        };
        loop {
            let owner = self.owner[cur as usize];
            if owner != UNOWNED {
                return Some(self.groups[owner as usize].category);
            }
            let parent = self.tree.node(cur).parent;
            if parent == Tree::ROOT || cur == Tree::ROOT {
                return Some(apps::root_entry_category(&self.tree.node(cur).name));
            }
            cur = parent;
        }
    }

    fn describe(&self, id: Id) -> ViewNode {
        let (name, kind, path, locations, has_children) = match id.clone() {
            Id::Root => (self.tree.root_path.display().to_string(), "drive", Some(self.tree.root_path.display().to_string()), Vec::new(), true),
            Id::Group(g) => {
                let group = &self.groups[g as usize];
                let locations: Vec<Location> = group
                    .locations
                    .iter()
                    .filter(|&&l| self.eff[l as usize] > 0)
                    .map(|&l| Location { path: self.tree.path(l).display().to_string(), size: self.eff[l as usize] })
                    .collect();
                (group.name.clone(), "app", locations.first().map(|l| l.path.clone()), locations, true)
            }
            Id::Node(n) => {
                let node = self.tree.node(n);
                let kind = match node.kind {
                    NodeKind::Dir => "folder",
                    NodeKind::File => "file",
                    NodeKind::SmallFiles { .. } => "files",
                };
                let path = match node.kind {
                    NodeKind::SmallFiles { .. } => self.tree.path(node.parent),
                    _ => self.tree.path(n),
                };
                let has_children = node.kind == NodeKind::Dir && self.visible_children(n).next().is_some();
                (node.name.to_string(), kind, Some(path.display().to_string()), Vec::new(), has_children)
            }
            // This walks the parent's children to count what is left, and size() below walks
            // them again. That is a second sort of a few thousand entries, once per level of
            // a page, which is not worth threading the answer through to avoid.
            Id::Tail(inner, offset) => {
                let left = self.children(*inner).len().saturating_sub(offset);
                (format!("{left} smaller items"), "more", None, Vec::new(), left > 0)
            }
        };
        let key = id.key();
        ViewNode { id: key, name, size: self.size(id.clone()), kind, category: self.category(id), path, locations, has_children, children: None }
    }

    /// The node plus `depth` levels of children, largest first. Long tails are bundled into one "more" entry.
    pub fn view(&self, id: Id, depth: u32) -> ViewNode {
        self.expand(id, depth, LIMIT_TOP, 0)
    }

    /// The node as the treemap draws it: nested levels inside every block big enough to show them.
    pub fn map(&self, id: Id) -> ViewNode {
        let min_size = self.size(id.clone()) / MAP_MIN_SHARE;
        self.expand(id, MAP_DEPTH, LIMIT_TOP, min_size)
    }

    fn expand(&self, id: Id, depth: u32, limit: usize, min_size: u64) -> ViewNode {
        let mut v = self.describe(id.clone());
        if depth == 0 || !v.has_children || v.size < min_size {
            return v;
        }
        let kids = self.children(id.clone());
        let mut out: Vec<ViewNode> = kids.iter().take(limit).map(|k| self.expand(k.clone(), depth - 1, LIMIT_PREVIEW, min_size)).collect();
        // An app's locations often share a folder name ("Docker"), so label them by path instead.
        if matches!(id, Id::Group(_)) && v.locations.len() > 1 {
            for child in &mut out {
                if let Some(path) = &child.path {
                    child.name = short_location(path);
                }
            }
        }
        if kids.len() > limit {
            out.push(ViewNode {
                id: Id::Tail(Box::new(id.clone()), limit).key(),
                name: format!("{} smaller items", kids.len() - limit),
                size: kids[limit..].iter().map(|k| self.size(k.clone())).sum(),
                kind: "more",
                category: v.category,
                path: None,
                locations: Vec::new(),
                // Opening the tile lists what is in here, rather than leaving it a dead end.
                has_children: true,
                children: None,
            });
        }
        v.children = Some(out);
        v
    }
}

/// `C:\Users\me\AppData\Local\Docker` -> `AppData\Local\Docker`, `C:\Program Files\Docker` -> `Program Files\Docker`.
fn short_location(path: &str) -> String {
    let rest = path.get(3..).unwrap_or(path);
    let mut parts = rest.splitn(3, '\\');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(users), Some(_profile), Some(inner)) if users.eq_ignore_ascii_case("users") => inner.to_string(),
        _ => rest.to_string(),
    }
}

fn claim(tree: &Tree, owner: &mut [u32], groups: &mut [Group], node: u32, g: u32) {
    if node == Tree::ROOT || tree.node(node).kind != NodeKind::Dir || owner[node as usize] != UNOWNED {
        return;
    }
    // Already covered by a folder this group owns higher up.
    let mut cur = tree.node(node).parent;
    while cur != crate::scan::NO_PARENT {
        if owner[cur as usize] == g {
            return;
        }
        cur = tree.node(cur).parent;
    }
    owner[node as usize] = g;
    groups[g as usize].locations.push(node);
}

/// `ProgramData` plus every profile's `AppData\Local`, `Roaming` and `LocalLow`.
fn data_folders(tree: &Tree) -> Vec<u32> {
    let root = &tree.root_path;
    let mut out: Vec<u32> = tree.find(&root.join("ProgramData")).into_iter().collect();
    if let Some(users) = tree.find(&root.join("Users")) {
        for &profile in &tree.node(users).children {
            if tree.node(profile).kind != NodeKind::Dir {
                continue;
            }
            let base = tree.path(profile).join("AppData");
            out.extend(["Local", "Roaming", "LocalLow"].iter().filter_map(|sub| tree.find(&base.join(sub))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{scan, Progress, SMALL_FILE_LIMIT};
    use std::fs;
    use std::path::Path;

    const MB: u64 = SMALL_FILE_LIMIT;

    fn write(root: &Path, rel: &str, mb: u64) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; (mb * MB) as usize]).unwrap();
    }

    fn app(root: &Path, name: &str, publisher: &str, rel: &str, steam: bool) -> InstalledApp {
        InstalledApp { name: name.into(), publisher: publisher.into(), location: root.join(rel), steam }
    }

    fn find_group<'a>(v: &'a View, name: &str) -> (u32, &'a Group) {
        let i = v.groups.iter().position(|g| g.name == name).unwrap_or_else(|| panic!("no group {name}: {:?}", v.groups));
        (i as u32, &v.groups[i])
    }

    fn assert_children_sum(v: &View, id: Id) {
        let node = v.view(id, 1);
        let total: u64 = node.children.unwrap().iter().map(|c| c.size).sum();
        assert_eq!(total, node.size, "children of {} should add up", node.name);
    }

    fn sample() -> (tempfile::TempDir, View) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, r"Program Files\Docker\Docker\app.bin", 4);
        write(r, r"Users\me\AppData\Local\Docker\wsl\disk.vhdx", 50);
        write(r, r"Program Files\Adobe\Photoshop\ps.bin", 7);
        write(r, r"Program Files\Adobe\Premiere\pr.bin", 9);
        write(r, r"Users\me\AppData\Roaming\Adobe\cache.bin", 30);
        write(r, r"Users\me\AppData\Local\Google\Chrome\User Data\profile.bin", 3);
        write(r, r"Program Files\Google\Chrome\Application\chrome.bin", 2);
        write(r, r"Users\me\AppData\Local\Unrelated\stuff.bin", 5);
        write(r, r"Users\me\Documents\report.bin", 6);
        write(r, r"Windows\System32\kernel.bin", 20);
        write(r, r"Games\Steam\steam.bin", 2);
        write(r, r"Games\Steam\steamapps\common\Elden Ring\game.bin", 40);
        let apps = vec![
            app(r, "Docker Desktop", "Docker Inc.", r"Program Files\Docker\Docker", false),
            app(r, "Adobe Photoshop 2025", "Adobe Inc.", r"Program Files\Adobe\Photoshop", false),
            app(r, "Adobe Premiere Pro 2025", "Adobe Inc.", r"Program Files\Adobe\Premiere", false),
            app(r, "Google Chrome", "Google LLC", r"Program Files\Google\Chrome\Application", false),
            app(r, "Steam", "Valve Corporation", r"Games\Steam", false),
            app(r, "ELDEN RING", "", r"Games\Steam\steamapps\common\Elden Ring", true),
        ];
        let tree = scan(r, &Progress::default());
        let view = View::build(tree, &apps);
        (dir, view)
    }

    #[test]
    fn app_data_outside_install_folder_is_grouped_with_the_app() {
        let (_d, v) = sample();
        let (_, docker) = find_group(&v, "Docker Desktop");
        assert_eq!(docker.locations.len(), 2);
        assert_eq!(docker.size, 54 * MB);
        assert_eq!(docker.category, Category::Dev);

        let (_, chrome) = find_group(&v, "Google Chrome");
        assert_eq!(chrome.size, 5 * MB, "Chrome gets AppData\\Local\\Google\\Chrome via the publisher folder");
    }

    #[test]
    fn apps_under_a_publisher_folder_form_a_suite() {
        let (_d, v) = sample();
        let (g, adobe) = find_group(&v, "Adobe");
        assert_eq!(adobe.size, 46 * MB);
        assert_eq!(adobe.category, Category::Creative);
        assert_children_sum(&v, Id::Group(g));
    }

    #[test]
    fn nested_apps_are_not_counted_twice() {
        let (_d, v) = sample();
        let (_, steam) = find_group(&v, "Steam");
        let (_, elden) = find_group(&v, "ELDEN RING");
        assert_eq!(steam.size, 2 * MB);
        assert_eq!(elden.size, 40 * MB);
        assert_eq!(elden.category, Category::Games);
    }

    #[test]
    fn every_level_adds_up_to_the_drive_total() {
        let (_d, v) = sample();
        let root = v.view(Id::Root, 2);
        assert_eq!(root.size, v.tree.node(Tree::ROOT).size);
        assert_children_sum(&v, Id::Root);
        for g in 0..v.groups.len() as u32 {
            if v.groups[g as usize].size > 0 {
                assert_children_sum(&v, Id::Group(g));
            }
        }
        let users = v.tree.find(&v.tree.root_path.join("Users")).unwrap();
        assert_children_sum(&v, Id::Node(users));
        // Unclaimed AppData stays in the user's folder.
        assert_eq!(v.size(Id::Node(users)), (5 + 6) * MB);
    }

    #[test]
    fn unowned_folders_get_categories_from_their_top_level_folder() {
        let (_d, v) = sample();
        let windows = v.tree.find(&v.tree.root_path.join(r"Windows\System32")).unwrap();
        assert_eq!(v.category(Id::Node(windows)), Some(Category::System));
        let docs = v.tree.find(&v.tree.root_path.join(r"Users\me\Documents")).unwrap();
        assert_eq!(v.category(Id::Node(docs)), Some(Category::User));
    }

    #[test]
    fn app_locations_are_labelled_by_path() {
        let (_d, v) = sample();
        let (g, _) = find_group(&v, "Docker Desktop");
        let names: Vec<String> = v.view(Id::Group(g), 2).children.unwrap().into_iter().map(|c| c.name).collect();
        assert!(names.iter().any(|n| n.ends_with(r"AppData\Local\Docker")), "{names:?}");
        assert_eq!(short_location(r"C:\Users\me\AppData\Local\Docker"), r"AppData\Local\Docker");
        assert_eq!(short_location(r"C:\Program Files\Docker"), r"Program Files\Docker");
    }

    #[test]
    fn map_nests_only_inside_blocks_big_enough_to_draw() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, r"big\a\b\c\d\e\deep.bin", 30);
        fs::create_dir_all(r.join(r"tiny\inner")).unwrap();
        fs::write(r.join(r"tiny\inner\note.txt"), vec![0u8; 40_000]).unwrap();
        let v = View::build(scan(r, &Progress::default()), &[]);

        let root = v.map(Id::Root);
        let kids = root.children.as_ref().unwrap();
        assert_eq!(kids.iter().map(|c| c.size).sum::<u64>(), root.size);

        let mut node = kids.iter().find(|c| c.name == "big").unwrap();
        for name in ["a", "b", "c", "d", "e"] {
            node = &node.children.as_ref().unwrap_or_else(|| panic!("{} should be nested", node.name))[0];
            assert_eq!(node.name, name);
        }
        assert!(node.has_children && node.children.is_none(), "nesting stops {MAP_DEPTH} levels down");

        let tiny = kids.iter().find(|c| c.name == "tiny").unwrap();
        assert!(tiny.has_children && tiny.children.is_none(), "too small to draw its contents");
    }

    #[test]
    fn ids_round_trip() {
        for id in [Id::Root, Id::Group(3), Id::Node(12345)] {
            assert_eq!(Id::parse(&id.key()), Some(id.clone()));
        }
        assert_eq!(Id::parse("x1"), None);
        assert_eq!(Id::parse("n"), None);
        assert_eq!(Id::parse("~"), None);
        // A tail nests, and the offset survives the round trip.
        let tail = Id::Tail(Box::new(Id::Tail(Box::new(Id::Root), 60)), 24);
        assert_eq!(Id::parse(&tail.key()), Some(tail));
        assert_eq!(Id::parse("root~60"), Some(Id::Tail(Box::new(Id::Root), 60)));
    }

    /// The "smaller items" tile used to be a dead end: its id could not even be parsed back.
    /// Opening it has to list the rest of its parent and still add up to the same total.
    #[test]
    fn the_smaller_items_tile_opens_and_adds_up() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let count = LIMIT_TOP + 7;
        for i in 0..count {
            write(r, &format!("d{i:02}/file.bin"), 1);
        }
        let tree = scan(r, &Progress::default());
        let v = View::build(tree, &[]);

        let root = v.view(Id::Root, 2);
        let more = root.children.unwrap().into_iter().find(|c| c.kind == "more").expect("a smaller items tile");
        assert_eq!(more.name, "7 smaller items");
        assert!(more.has_children, "the tile has to be openable to not be a dead end");

        // The id the UI sends back has to resolve.
        let tail = Id::parse(&more.id).expect("the tile id parses");
        assert!(v.contains(tail.clone()));
        assert_eq!(v.size(tail.clone()), more.size);

        let opened = v.view(tail.clone(), 2);
        assert_eq!(opened.name, "7 smaller items");
        let kids = opened.children.expect("listing what is inside");
        assert_eq!(kids.len(), 7);
        let listed: u64 = kids.iter().map(|k| k.size).sum();
        assert_eq!(listed, more.size, "the tail adds up to what the tile claimed");
        assert!(kids.iter().all(|k| k.kind == "folder"), "they are real folders: {kids:?}");
    }

    /// A folder with far more children than the limit caps over more than once.
    #[test]
    fn nested_smaller_items_tiles_keep_going() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        let deep = LIMIT_TOP * 2 + 5;
        for i in 0..deep {
            write(r, &format!("Users/me/AppData/d{i:03}/file.bin"), 1);
        }
        let tree = scan(r, &Progress::default());
        let v = View::build(tree, &[]);
        let users = v.tree.find(&r.join("Users/me/AppData")).unwrap();

        let mut id = Id::Node(users);
        let mut hops = 0;
        loop {
            let node = v.view(id.clone(), 2);
            let more = node.children.into_iter().flatten().find(|c| c.kind == "more");
            let Some(more) = more else { break };
            let next = Id::parse(&more.id).expect("each hop parses");
            assert_eq!(v.size(next.clone()), more.size);
            id = next;
            hops += 1;
            assert!(hops <= 3, "should bottom out rather than loop forever");
        }
        assert_eq!(hops, 2, "65 items under one limit of {LIMIT_TOP} needs two hops");
    }
}
