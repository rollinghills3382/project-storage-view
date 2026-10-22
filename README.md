# Storage View

A Windows desktop app that shows what is taking up space on your drives.

Pick a drive and Storage View scans it, then draws a treemap where each block's **area is
proportional to its size on disk**. Files are grouped by the application that owns them, so
Docker Desktop's WSL disk in `AppData` shows up under Docker Desktop, not buried in your user
folder. Double-click any block to zoom in. A second tab lists the same data as a sortable tree, and
the search box finds apps, folders and files by name across the whole drive.
Storage View is read-only: it never changes or deletes anything.

## Running it

Requirements: Windows 10/11, [Rust](https://rustup.rs), Node.js 22.18+, and the Visual Studio
C++ build tools. The Node floor is set by the frontend test runner, which runs TypeScript
directly; Node 20 reached end of life in April 2026.

```bash
npm install
npm run tauri dev      # run the app
npm run tauri build    # installer in src-tauri/target/release/bundle/
npm test               # all tests: Rust unit tests, then the frontend
```

## How it works

| Part | File | What it does |
| --- | --- | --- |
| Scanner | `src-tauri/src/scan.rs` | Parallel directory walk (rayon) into a compact tree. Files under 1 MB are bundled per folder. Skips junctions and symlinks; counts only the part of a cloud (OneDrive) file stored on this PC. On NTFS each folder listing also returns file IDs, so a file with several hard links is counted once (`scan/links.rs`, `scan/winlist.rs`). |
| App detection | `src-tauri/src/apps.rs` | Reads installed apps and their install folders from the Windows uninstall registry; assigns categories. |
| Grouping | `src-tauri/src/grouping.rs` | Each app claims its install folder plus matching folders in `AppData` and `ProgramData`. Apps side by side under a publisher folder (`Program Files\Adobe\…`) become one block. Claimed folders are removed from where they sit, so nothing is counted twice. |
| Commands | `src-tauri/src/lib.rs` | `list_drives`, `start_scan` (with progress events), `cancel_scan`, `get_map` (a folder with the nested levels the map draws), `get_node` (a folder and its children, for the list), `search` (`grouping/search.rs`), `app_status`, `restart_as_admin`. |
| Elevation | `src-tauri/src/elevation.rs` | Detects administrator rights and relaunches through the UAC prompt with `--scan C:` so the new window resumes the same drive. |
| UI | `src/main.ts`, `src/treemap.ts` | Drive buttons, Map tab (d3-hierarchy squarified layout), List tab (tree table), search results, selection and status bars. |

### Checking grouping without the UI

```bash
cd src-tauri
cargo run --release --example report -- C:\
```

prints the top-level blocks for a drive and every location each app was found in. Adding
`--json ../devdata/scan.json` saves the scan so the UI can be previewed in a plain browser
with `npm run dev` (see `src/devmock.ts`). `devdata/` is gitignored.

`cargo run --release --example scan_bench -- C:\ --mode std|ids|links` times one scan and prints
its memory peak, file counts and hard-link figures as JSON. The *Scan benchmark* workflow runs it
on a Windows runner whenever the scanner changes and compares WinSxS with DISM's own figures.

## Design decisions

- **Platform:** Windows only.
- **Visualization:** squarified treemap, and it is the main view. Blocks nest up to six levels
  deep, but only where a block is at least 1/300 of the folder being viewed; smaller blocks stay
  solid. Click selects, double-click zooms in.
- **Map and List are separate tabs.** The list is a sortable tree of the same data. The selection
  carries across, so switching tabs shows the selected item in the other view.
- **Look:** plain desktop-utility chrome. System font at 12px, square corners, 1px rules, no
  cards, shadows or popups. Color is reserved for the map and its legend.
- **Grouping:** by owning application, including data outside its install folder. An app opens
  into every location it occupies.
- **View-only:** the only action is *Show in File Explorer*.
- **Search** (Ctrl+F) matches names, ignoring case; every word typed has to appear. Results
  replace the map or list, largest first (the 200 largest when there are more). Each result
  carries the way to it as the view shows it, through the app that claimed it and any
  "smaller items" tile, so double-clicking it (or Enter) opens it in the Map or List tab, and
  clicking the search box brings the results back.
- **Hard links count once,** under the name whose folder comes first alphabetically (a folder's
  own files before its subfolders). Files Windows shares between `System32` and `WinSxS` count
  in `System32`, so `WinSxS` shows what only it holds, as DISM reports it. The status bar says
  how much this saved, and the selection bar shows how much of a folder counts elsewhere.

## Known limitations

- Sizes are file lengths, not allocated clusters, so compressed and sparse files count at
  full size.
- Hard links are only recognized on NTFS. On other file systems (such as ReFS on a Dev Drive)
  a file with several names counts once per name. When the scan adds up to more than Windows
  reports as used, the legend says by how much instead of showing unreadable space.
- Without administrator rights some folders can't be read. The status bar shows how many and
  the toolbar offers **Restart as administrator**. A few folders (such as `System Volume
  Information`) stay unreadable even then.
- A folder shows its 60 largest items; the rest are bundled into one "smaller items" entry.
- Search can't find files under 1 MB by name, because the scan counts them together per folder
  and keeps no names for them.
- Under `npm run tauri dev`, restarting as administrator leaves the original window open,
  because closing it would stop the dev server the new window loads from.
- Store (UWP/MSIX) apps aren't in the uninstall registry, so they show as folders instead of apps.
