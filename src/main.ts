import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { canOpen, createTreemap } from "./treemap";
import {
  CATEGORIES,
  catColor,
  driveLetter,
  driveName,
  fmtBytes,
  fmtCount,
  h,
  kindLabel,
  pct,
  type Category,
  type Drive,
  type ScanProgress,
  type ScanSummary,
  type ViewNode,
} from "./ui";

interface DriveScan {
  summary: ScanSummary;
  /** `get_map` results: a node with the nested levels the treemap draws. */
  maps: Map<string, ViewNode>;
  /** `get_node` results: a node with its children, which is what a list row expands to. */
  nodes: Map<string, ViewNode>;
}

/** An item and the folders it sits inside, from the drive root down to its parent. */
interface Located {
  node: ViewNode;
  chain: ViewNode[];
}

type SortKey = "name" | "size";

const state = {
  drives: [] as Drive[],
  scans: new Map<string, DriveScan>(),
  drive: null as Drive | null,
  tab: "map" as "map" | "list",
  /** Map location: breadcrumb from the drive root to the folder being viewed. Empty until the drive is scanned. */
  trail: [] as ViewNode[],
  selected: null as Located | null,
  /** Ids of the expanded list rows. */
  open: new Set<string>(),
  sort: { key: "size" as SortKey, dir: -1 },
  scanning: null as { drive: string; progress: ScanProgress | null } | null,
  elevated: false,
  message: "",
};

const app = document.getElementById("app")!;
const scanOf = (d: Drive | null) => (d ? state.scans.get(d.mount) : undefined);
const focus = () => state.trail[state.trail.length - 1];
/** True when the selected drive has scan results on screen. */
const showing = () => state.trail.length > 0;

/* ---------- Data ---------- */

async function load(kind: "maps" | "nodes", id: string): Promise<ViewNode> {
  const drive = state.drive!.mount;
  const cache = state.scans.get(drive)![kind];
  const cached = cache.get(id);
  if (cached) return cached;
  const node = await invoke<ViewNode>(kind === "maps" ? "get_map" : "get_node", { drive, id });
  cache.set(id, node);
  return node;
}

async function refreshDrives() {
  state.drives = await invoke<Drive[]>("list_drives");
  if (state.drive) state.drive = state.drives.find((d) => d.mount === state.drive!.mount) ?? state.drive;
}

let messageTimer = 0;
/** Shows a problem in the status bar for a few seconds. */
function notify(message: string) {
  state.message = message;
  clearTimeout(messageTimer);
  messageTimer = window.setTimeout(() => ((state.message = ""), render()), 8000);
  render();
}

/** Runs a UI action and reports anything it throws. */
async function run(action: () => Promise<void> | void) {
  try {
    await action();
  } catch (e) {
    notify(String(e));
  }
}

async function startScan(drive: Drive) {
  state.message = "";
  state.scanning = { drive: drive.mount, progress: null };
  render();
  try {
    await invoke("start_scan", { drive: drive.mount });
  } catch (e) {
    state.scanning = null;
    notify(String(e));
  }
}

/** Relaunches through the UAC prompt; the new window picks up scanning `drive`. */
async function restartAsAdmin(drive: Drive | null) {
  try {
    await invoke("restart_as_admin", { scan: drive?.mount ?? null });
  } catch (e) {
    notify(e === "cancelled" ? "Administrator access wasn't granted. Storage View keeps running as before." : String(e));
  }
}

async function reveal(path: string) {
  try {
    await revealItemInDir(path);
  } catch (e) {
    notify(`Couldn't open File Explorer: ${e}`);
  }
}

/* ---------- Navigation ---------- */

async function showDrive(drive: Drive) {
  state.drive = drive;
  state.trail = [];
  state.selected = null;
  state.open = new Set(["root"]);
  hideTip();
  if (scanOf(drive)) {
    const [map] = await Promise.all([load("maps", "root"), load("nodes", "root")]);
    if (state.drive.mount === drive.mount) state.trail = [map];
  }
  render();
}

/** Points the map at the last folder in `trail`, which is the only one that needs its nested levels. */
async function goTo(trail: ViewNode[], selected: Located | null) {
  const last = trail[trail.length - 1];
  // Keep the name the user clicked on; app locations are labelled by path in their parent.
  const loaded = { ...(await load("maps", last.id)), name: last.name };
  state.trail = [...trail.slice(0, -1), loaded];
  state.selected = selected;
  hideTip();
  render();
}

function openTile(node: ViewNode, chain: ViewNode[]) {
  const inside = [...state.trail, ...chain];
  if (canOpen(node)) return goTo([...inside, node], null);
  // A file can't be opened, so zoom into the folder that holds it.
  if (chain.length) return goTo(inside, { node, chain: inside });
}

function select(node: ViewNode, chain: ViewNode[]) {
  state.selected = { node, chain };
  render();
}

function goUp() {
  if (state.tab === "map") {
    if (state.trail.length < 2) return;
    const trail = state.trail.slice(0, -1);
    return goTo(trail, { node: focus(), chain: trail });
  }
  const chain = state.selected?.chain;
  if (!chain?.length) return;
  revealRow = true;
  select(chain[chain.length - 1], chain.slice(0, -1));
}

const canGoUp = () => showing() && (state.tab === "map" ? state.trail.length > 1 : !!state.selected?.chain.length);

/** Switches tabs and carries the selection across. */
async function setTab(tab: "map" | "list") {
  const sel = state.selected;
  state.tab = tab;
  if (sel && showing()) {
    if (tab === "list") {
      for (const n of sel.chain) {
        await load("nodes", n.id);
        state.open.add(n.id);
      }
      revealRow = true;
    } else if (!sel.chain.length) {
      state.selected = null;
    } else if (!sel.chain.some((n) => n.id === focus().id)) {
      // The selection is outside the folder the map shows, so show the folder that holds it.
      return goTo(sel.chain, sel);
    }
  }
  render();
  // So the arrow keys work in the list straight away.
  if (tab === "list" && showing()) list.focus();
}

/* ---------- Toolbar and navigation row ---------- */

function button(label: string, onclick: () => void, disabled = false, title?: string) {
  return h("button", { class: "btn", onclick, disabled, title }, label);
}

function renderToolbar() {
  const cur = state.drive;
  const busy = !!state.scanning;
  return h(
    "div",
    { class: "toolbar" },
    ...state.drives.map((d) =>
      h(
        "button",
        { class: "drive", "aria-pressed": String(d.mount === cur?.mount), title: `${d.kind} · ${d.file_system}`, onclick: () => run(() => showDrive(d)) },
        h("span", { class: "dl" }, h("b", {}, driveLetter(d.mount)), ` ${d.label || (d.removable ? "USB Drive" : "Local Disk")} · ${d.kind}`),
        h("span", { class: "meter" }, h("i", { style: `width:${((d.total - d.free) / d.total) * 100}%` })),
        h("span", { class: "dm" }, `${fmtBytes(d.free)} free of ${fmtBytes(d.total)}${scanOf(d) ? "" : " · not scanned"}`),
      ),
    ),
    h("span", { class: "tsep" }),
    cur && button(`${scanOf(cur) ? "Rescan" : "Scan"} ${driveLetter(cur.mount)}`, () => startScan(cur), busy),
    button("Stop", () => invoke("cancel_scan"), !busy),
    h("span", { class: "grow" }),
    !state.elevated &&
      h(
        "button",
        { class: "btn", title: "Reads folders that need administrator rights. Windows will ask for permission.", onclick: () => restartAsAdmin(scanOf(cur) ? cur : null) },
        h("span", { class: "shield", "aria-hidden": "true" }),
        "Restart as administrator",
      ),
  );
}

function renderNav() {
  const tab = (id: "map" | "list", label: string) => h("button", { role: "tab", "aria-selected": String(state.tab === id), onclick: () => run(() => setTab(id)) }, label);
  const row = h(
    "div",
    { class: "nav" },
    h("div", { class: "tabs", role: "tablist" }, tab("map", "Map"), tab("list", "List")),
    button("Up", () => run(goUp), !canGoUp(), "Up one level (Backspace)"),
  );
  if (state.tab === "map" && showing()) {
    const crumbs = h("nav", { class: "crumbs", "aria-label": "Location" });
    state.trail.forEach((n, i) => {
      if (i) crumbs.append(h("span", { class: "sep", "aria-hidden": "true" }, "›"));
      const current = i === state.trail.length - 1;
      const trail = state.trail.slice(0, i + 1);
      crumbs.append(
        h(
          "button",
          { "aria-current": current ? "page" : null, onclick: () => !current && run(() => goTo(trail, { node: state.trail[i + 1], chain: trail })) },
          i === 0 ? driveLetter(n.name) : n.name,
        ),
      );
    });
    row.append(crumbs, h("span", { class: "total" }, fmtBytes(focus().size)));
  } else if (state.tab === "list") {
    row.append(button("Collapse all", () => ((state.open = new Set(["root"])), render()), !showing()));
  }
  const path = showing() ? state.selected?.node.path : null;
  row.append(h("span", { class: "grow" }), button("Show in File Explorer", () => path && reveal(path), !path));
  return row;
}

/* ---------- Map ---------- */

const tip = h("div", { class: "tip", hidden: true });
let tipNode: ViewNode | null = null;
function hideTip() {
  tip.hidden = true;
  tipNode = null;
}
function showTip(node: ViewNode | null, e?: MouseEvent) {
  if (!node || !e) return hideTip();
  if (tipNode !== node) {
    tipNode = node;
    tip.replaceChildren(
      h("b", {}, node.name),
      h("span", {}, `${fmtBytes(node.size)} · ${pct(node.size, focus().size)} of ${state.trail.length > 1 ? focus().name : driveLetter(focus().name)}`),
      h("span", { class: "dim" }, node.path),
      h("span", { class: "dim" }, canOpen(node) && "Double-click to zoom in"),
    );
  }
  tip.hidden = false;
  const r = mapView.getBoundingClientRect();
  let x = e.clientX - r.left + 14;
  let y = e.clientY - r.top + 16;
  if (x + tip.offsetWidth > r.width) x = e.clientX - r.left - tip.offsetWidth - 10;
  if (y + tip.offsetHeight > r.height) y = e.clientY - r.top - tip.offsetHeight - 10;
  tip.style.transform = `translate(${Math.max(0, x)}px, ${Math.max(0, y)}px)`;
}

const map = createTreemap({
  open: (node, chain) => run(() => openTile(node, chain)),
  select: (node, chain) => select(node, [...state.trail, ...chain]),
  hover: showTip,
});
const mapView = h("div", { class: "view" }, map.el, tip);
let frame = 0;
new ResizeObserver(() => {
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(drawMap);
}).observe(map.el);

function drawMap() {
  if (state.tab !== "map" || !showing() || !map.el.isConnected) return;
  const sel = state.selected;
  map.draw(focus(), new Set(sel ? [...sel.chain, sel.node].map((n) => n.id) : []));
}

/* ---------- List ---------- */

const COLUMNS: { id: string; label: string; sort?: SortKey; width?: string; num?: boolean }[] = [
  { id: "name", label: "Name", sort: "name" },
  { id: "size", label: "Size", sort: "size", width: "84px", num: true },
  { id: "parent", label: "% of parent", width: "150px" },
  { id: "drive", label: "% of drive", width: "76px", num: true },
  { id: "type", label: "Type", width: "132px" },
  { id: "category", label: "Category", width: "112px" },
  { id: "path", label: "Location", width: "27%" },
];

const list = h("div", { class: "view tw", tabindex: "0", "aria-label": "Folders and files by size" });
let rows: Located[] = [];
/** Set when the selected row should be scrolled into view after the next render. */
let revealRow = false;

function sorted(kids: ViewNode[]) {
  const { key, dir } = state.sort;
  const more = (n: ViewNode) => Number(n.kind === "more");
  return [...kids].sort((a, b) => more(a) - more(b) || dir * (key === "name" ? a.name.localeCompare(b.name) : a.size - b.size));
}

function listRows(): Located[] {
  const nodes = scanOf(state.drive)!.nodes;
  const out: Located[] = [];
  const walk = (node: ViewNode, chain: ViewNode[]) => {
    out.push({ node, chain });
    const kids = state.open.has(node.id) ? nodes.get(node.id)?.children : null;
    if (kids) for (const c of sorted(kids)) walk(c, [...chain, node]);
  };
  walk(nodes.get("root")!, []);
  return out;
}

async function toggleRow(node: ViewNode) {
  if (state.open.has(node.id)) state.open.delete(node.id);
  else if (canOpen(node)) {
    await load("nodes", node.id);
    state.open.add(node.id);
  }
  render();
}

function cell(id: string, { node, chain }: Located) {
  const d = state.drive!;
  const parent = chain[chain.length - 1];
  switch (id) {
    case "name": {
      const open = state.open.has(node.id);
      return h(
        "td",
        { title: node.name },
        h(
          "div",
          { class: "name", style: `padding-left:${chain.length * 16}px` },
          h("button", { class: canOpen(node) ? "arrow" : "arrow none", tabindex: "-1", "aria-label": open ? "Collapse" : "Expand" }, open ? "▼" : "▶"),
          h("i", { class: "sw", style: `background:${node.kind === "drive" ? "var(--muted)" : catColor(node.category)}` }),
          h("span", {}, node.name),
        ),
      );
    }
    case "size":
      return h("td", { class: "num", title: `${fmtCount(node.size)} bytes` }, fmtBytes(node.size));
    case "parent":
      return h("td", {}, h("div", { class: "pc" }, h("span", { class: "meter" }, h("i", { style: `width:${parent ? (node.size / parent.size) * 100 : 100}%` })), parent ? pct(node.size, parent.size) : "100%"));
    case "drive":
      return h("td", { class: "num" }, pct(node.size, d.total));
    case "type":
      return h("td", { class: "dim" }, kindLabel(node));
    case "category":
      return h("td", { class: "dim" }, node.category ? CATEGORIES[node.category] : "");
    default:
      return h("td", { class: "dim", title: node.path ?? "" }, node.path ?? "");
  }
}

function drawList() {
  rows = listRows();
  const sortBy = (key: SortKey) => {
    state.sort = state.sort.key === key ? { key, dir: -state.sort.dir } : { key, dir: key === "name" ? 1 : -1 };
    render();
  };
  const header = COLUMNS.map((c) => {
    const { sort } = c;
    const mark = sort === state.sort.key ? (state.sort.dir < 0 ? " ▾" : " ▴") : "";
    return h("th", { class: (c.num ? "num " : "") + (sort ? "sortable" : ""), onclick: sort ? () => sortBy(sort) : null }, c.label + mark);
  });
  const body = rows.map((row) =>
    h(
      "tr",
      {
        class: state.selected?.node.id === row.node.id ? "sel" : "",
        onclick: (e) => {
          if ((e.target as Element).closest(".arrow") || (e as MouseEvent).detail >= 2) run(() => toggleRow(row.node));
          else select(row.node, row.chain);
        },
      },
      ...COLUMNS.map((c) => cell(c.id, row)),
    ),
  );
  list.replaceChildren(
    h("table", { class: "grid" }, h("colgroup", {}, ...COLUMNS.map((c) => h("col", { style: c.width ? `width:${c.width}` : "" }))), h("thead", {}, h("tr", {}, ...header)), h("tbody", {}, ...body)),
  );
  if (revealRow) list.querySelector("tr.sel")?.scrollIntoView({ block: "nearest" });
  revealRow = false;
}

list.addEventListener("keydown", (e) => {
  const i = rows.findIndex((r) => r.node.id === state.selected?.node.id);
  const row = rows[i];
  const pick = (r: Located | undefined) => {
    if (!r) return;
    revealRow = true;
    select(r.node, r.chain);
  };
  switch (e.key) {
    case "ArrowDown":
      pick(rows[i + 1]);
      break;
    case "ArrowUp":
      pick(rows[i - 1]);
      break;
    case "ArrowRight":
      if (!row) return;
      if (state.open.has(row.node.id)) pick(rows[i + 1]);
      else run(() => toggleRow(row.node));
      break;
    case "ArrowLeft":
      if (!row) return;
      if (state.open.has(row.node.id)) run(() => toggleRow(row.node));
      else pick(rows.find((r) => r.node === row.chain[row.chain.length - 1]));
      break;
    case "Enter":
      if (row) run(() => toggleRow(row.node));
      break;
    default:
      return;
  }
  e.preventDefault();
});

/* ---------- Bottom rows ---------- */

function renderLegend() {
  const d = state.drive;
  const root = scanOf(d)?.maps.get("root");
  const row = h("div", { class: "legend" });
  if (!d || !root || !showing()) return row;
  const totals = new Map<Category, number>();
  for (const c of root.children ?? []) {
    const cat = c.category ?? "other";
    totals.set(cat, (totals.get(cat) ?? 0) + c.size);
  }
  const item = (label: string, bytes: number, color: string, title?: string) => h("span", { title }, h("i", { class: "sw", style: `background:${color}` }), h("b", {}, label), fmtBytes(bytes));
  for (const [cat, bytes] of [...totals].sort((a, b) => b[1] - a[1])) row.append(item(CATEGORIES[cat], bytes, catColor(cat)));
  const unaccounted = d.total - d.free - root.size;
  if (unaccounted > d.total * 0.005) {
    row.append(item("Not readable", unaccounted, "var(--muted)", "Folders Windows wouldn't let Storage View read, plus space the file system reserves."));
  }
  row.append(item("Free", d.free, "var(--track)"));
  return row;
}

function renderSelection() {
  const d = state.drive;
  const bar = h("div", { class: "selbar" });
  // With nothing selected, the map describes the folder it is showing.
  const n = showing() ? (state.selected?.node ?? (state.tab === "map" ? focus() : null)) : null;
  if (!d || !n) {
    bar.append(h("span", { class: "dim" }, !d ? "No drives found" : showing() ? "Nothing selected" : `${driveLetter(d.mount)} has not been scanned`));
    return bar;
  }
  const isDrive = n.kind === "drive";
  const parent = state.selected ? state.selected.chain[state.selected.chain.length - 1] : null;
  const more = n.locations.length > 1 ? `  (+${n.locations.length - 1} more)` : "";
  bar.append(
    h("span", { class: "name" }, !isDrive && h("i", { class: "sw", style: `background:${catColor(n.category)}` }), h("b", {}, isDrive ? driveName(d) : n.name)),
    h("span", {}, kindLabel(n) + (n.category ? ` · ${CATEGORIES[n.category]}` : "")),
    h(
      "span",
      {},
      `${fmtBytes(n.size)} (${fmtCount(n.size)} bytes) · ${pct(n.size, d.total)} of ${driveLetter(d.mount)}` + (parent && parent.kind !== "drive" ? ` · ${pct(n.size, parent.size)} of ${parent.name}` : ""),
    ),
    h("span", { class: "p", title: n.locations.length > 1 ? n.locations.map((l) => `${l.path}  ${fmtBytes(l.size)}`).join("\n") : n.path }, n.path && n.path + more),
  );
  return bar;
}

function renderStatus() {
  const bar = h("footer", { class: "status" });
  if (state.message) bar.append(h("span", { class: "err", role: "alert" }, state.message));
  const busy = state.scanning;
  const s = scanOf(state.drive)?.summary;
  if (busy) {
    bar.append(
      h("span", { class: "prog" }, h("i", {})),
      h("span", {}, `Scanning ${driveLetter(busy.drive)}`),
      h("span", { id: "scan-count" }, "Starting…"),
      h("span", { class: "cur", id: "scan-path" }, busy.drive),
    );
  } else if (s) {
    bar.append(h("span", {}, `${fmtCount(s.files)} files in ${fmtCount(s.dirs)} folders · scanned in ${(s.elapsed_ms / 1000).toFixed(1)} s`));
    if (s.denied && !state.elevated) bar.append(h("span", { class: "warn" }, `${fmtCount(s.denied)} folders couldn't be read without administrator rights`));
    else if (s.denied) bar.append(h("span", {}, `${fmtCount(s.denied)} folders are protected by Windows even from administrators`));
  } else {
    bar.append(h("span", {}, state.drives.length === 1 ? "1 drive found" : `${state.drives.length} drives found`));
  }
  bar.append(h("span", { class: "grow" }));
  if (state.elevated) bar.append(h("span", { class: "badge" }, h("span", { class: "shield", "aria-hidden": "true" }), "Administrator"));
  bar.append(h("span", {}, "Read-only: nothing is changed or deleted"));
  return bar;
}

function updateProgress() {
  const p = state.scanning?.progress;
  const count = document.getElementById("scan-count");
  const path = document.getElementById("scan-path");
  if (!p || !count || !path) return;
  count.textContent = `${fmtCount(p.files)} files · ${fmtBytes(p.bytes)}`;
  path.textContent = p.current || p.drive;
}

/* ---------- Render ---------- */

function renderView() {
  if (showing()) return state.tab === "map" ? mapView : list;
  const d = state.drive;
  if (!d) return h("div", { class: "view" }, h("div", { class: "empty" }, "No drives found."));
  const mine = state.scanning?.drive === d.mount;
  return h(
    "div",
    { class: "view" },
    h(
      "div",
      { class: "empty" },
      h("span", {}, mine ? `Scanning ${driveLetter(d.mount)}…` : `${driveLetter(d.mount)} has not been scanned.`),
      !mine && button(`Scan ${driveLetter(d.mount)}`, () => startScan(d), !!state.scanning),
    ),
  );
}

function render() {
  const view = renderView();
  // Re-inserting the list drops its scroll position and keyboard focus, so put both back.
  const hadFocus = document.activeElement === list;
  const scroll = list.scrollTop;
  app.replaceChildren(renderToolbar(), renderNav(), view, ...(showing() ? [renderLegend(), renderSelection()] : []), renderStatus());
  if (view === mapView) drawMap();
  else if (view === list) {
    list.scrollTop = scroll;
    drawList();
    if (hadFocus) list.focus();
  }
  updateProgress();
}

/* ---------- Boot ---------- */

document.addEventListener("keydown", (e) => {
  if (!showing() || e.target instanceof HTMLInputElement) return;
  if (e.key === "Backspace") run(goUp);
  else if (e.key === "Escape" && state.selected) {
    state.selected = null;
    render();
  }
});

async function boot() {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) await (await import("./devmock")).install();

  await listen<ScanProgress>("scan-progress", (e) => {
    if (state.scanning?.drive !== e.payload.drive) return;
    state.scanning.progress = e.payload;
    updateProgress();
  });

  await listen<ScanSummary>("scan-done", (e) =>
    run(async () => {
      const s = e.payload;
      state.scans.set(s.drive, { summary: s, maps: new Map(), nodes: new Map() });
      if (state.scanning?.drive === s.drive) state.scanning = null;
      await refreshDrives();
      // Don't pull the user away from another drive they opened while this one was scanning.
      if (state.drive?.mount === s.drive) await showDrive(state.drive);
      else render();
    }),
  );

  await listen<string>("scan-cancelled", (e) => {
    if (state.scanning?.drive === e.payload) state.scanning = null;
    render();
  });

  const status = await invoke<{ elevated: boolean; startup_scan: string | null }>("app_status");
  state.elevated = status.elevated;
  try {
    await refreshDrives();
  } catch (e) {
    state.message = `Couldn't list drives: ${e}`;
  }
  // After restarting as administrator, carry on with the drive the user was looking at.
  const resume = state.drives.find((d) => d.mount.toUpperCase() === status.startup_scan?.toUpperCase());
  state.drive = resume ?? state.drives[0] ?? null;
  if (resume) await startScan(resume);
  else render();
}

boot();
