# Storage View

A Windows desktop app that shows what is taking up space on your drives.

Pick a drive and Storage View scans it, then draws a treemap where each block's **area is
proportional to its size on disk**. Files are grouped by the application that owns them, so
Docker Desktop's WSL disk in `AppData` shows up under Docker Desktop, not buried in your user
folder. Double-click any block to zoom in. A second tab lists the same data as a sortable tree.
Storage View is read-only: it never changes or deletes anything.

## Running it

Requirements: Windows 10/11, [Rust](https://rustup.rs), Node.js 20+, and the Visual Studio
C++ build tools.

```bash
npm install
npm run tauri dev      # run the app
npm run tauri build    # installer in src-tauri/target/release/bundle/
npm test               # Rust unit tests
```

## How it works

| Part | File | What it does |
| --- | --- | --- |
| Scanner | `src-tauri/src/scan.rs` | Parallel directory walk (rayon) into a compact tree. Files under 1 MB are bundled per folder. Skips junctions and symlinks; counts cloud-only OneDrive files as 0 bytes. |
| App detection | `src-tauri/src/apps.rs` | Reads installed apps and their install folders from the Windows uninstall registry; assigns categories. |
| Grouping | `src-tauri/src/grouping.rs` | Each app claims its install folder plus matching folders in `AppData` and `ProgramData`. Apps side by side under a publisher folder (`Program Files\Adobe\…`) become one block. Claimed folders are removed from where they sit, so nothing is counted twice. |
| Commands | `src-tauri/src/lib.rs` | `list_drives`, `start_scan` (with progress events), `cancel_scan`, `get_map` (a folder with the nested levels the map draws), `get_node` (a folder and its children, for the list), `app_status`, `restart_as_admin`. |
| Elevation | `src-tauri/src/elevation.rs` | Detects administrator rights and relaunches through the UAC prompt with `--scan C:` so the new window resumes the same drive. |
| UI | `src/main.ts`, `src/treemap.ts` | Drive buttons, Map tab (d3-hierarchy squarified layout), List tab (tree table), selection and status bars. |

### Checking grouping without the UI

```bash
cd src-tauri
cargo run --release --example report -- C:\
```

prints the top-level blocks for a drive and every location each app was found in. Adding
`--json ../devdata/scan.json` saves the scan so the UI can be previewed in a plain browser
with `npm run dev` (see `src/devmock.ts`). `devdata/` is gitignored.

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

## Known limitations

- Sizes are file lengths, not allocated clusters, so compressed and sparse files count at
  full size.
- Hard links (common in `C:\Windows\WinSxS`) are counted once per link, so `Windows` can read
  higher than its real footprint.
- Without administrator rights some folders can't be read. The status bar shows how many and
  the toolbar offers **Restart as administrator**. A few folders (such as `System Volume
  Information`) stay unreadable even then.
- A folder shows its 60 largest items; the rest are bundled into one "smaller items" entry.
- Under `npm run tauri dev`, restarting as administrator leaves the original window open,
  because closing it would stop the dev server the new window loads from.
- Store (UWP/MSIX) apps aren't in the uninstall registry, so they show as folders instead of apps.
