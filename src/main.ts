import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { canOpen, drawTreemap } from "./treemap";
import {
  CATEGORIES,
  catColor,
  driveLetter,
  driveName,
  fmtBytes,
  fmtCount,
  h,
  pct,
  type Category,
  type Drive,
  type ScanProgress,
  type ScanSummary,
  type ViewNode,
} from "./ui";

interface DriveScan {
  summary: ScanSummary;
  cache: Map<string, ViewNode>;
}

const state = {
  screen: "picker" as "picker" | "map",
  drives: [] as Drive[],
  scans: new Map<string, DriveScan>(),
  drive: null as Drive | null,
  /** Breadcrumb from the drive root to the node being viewed. */
  trail: [] as ViewNode[],
  selected: null as ViewNode | null,
  scanning: null as { drive: string; progress: ScanProgress | null } | null,
  elevated: false,
  error: "",
};

const app = document.getElementById("app")!;
const focus = () => state.trail[state.trail.length - 1];

/* ---------- Data ---------- */

async function loadNode(drive: string, id: string): Promise<ViewNode> {
  const scan = state.scans.get(drive)!;
  const cached = scan.cache.get(id);
  if (cached) return cached;
  const node = await invoke<ViewNode>("get_node", { drive, id });
  scan.cache.set(id, node);
  return node;
}

async function refreshDrives() {
  state.drives = await invoke<Drive[]>("list_drives");
  if (state.drive) state.drive = state.drives.find((d) => d.mount === state.drive!.mount) ?? state.drive;
}

async function startScan(drive: Drive) {
  state.error = "";
  state.screen = "picker";
  state.scanning = { drive: drive.mount, progress: null };
  render();
  try {
    await invoke("start_scan", { drive: drive.mount });
  } catch (e) {
    state.scanning = null;
    state.error = String(e);
    render();
  }
}

/** Relaunches through the UAC prompt; the new window picks up scanning `drive`. */
async function restartAsAdmin(drive: Drive | null) {
  try {
    await invoke("restart_as_admin", { scan: drive?.mount ?? null });
  } catch (e) {
    toast(e === "cancelled" ? "Administrator access wasn't granted. Storage View keeps running as before." : String(e));
  }
}

function adminButton(drive: Drive | null) {
  return h("button", { class: "btn admin", title: "Opens a Windows prompt asking for administrator access", onclick: () => restartAsAdmin(drive) }, h("span", { class: "shield", "aria-hidden": "true" }), "Restart as administrator");
}

async function showDrive(drive: Drive) {
  state.drive = drive;
  state.trail = [await loadNode(drive.mount, "root")];
  state.selected = null;
  state.screen = "map";
  render();
}

async function open(node: ViewNode, parent: ViewNode | null) {
  const drive = state.drive!.mount;
  hideTip();
  // Keep the name the user clicked on; app locations are labelled by path in their parent.
  const load = async (n: ViewNode) => ({ ...(await loadNode(drive, n.id)), name: n.name });
  if (parent) state.trail.push(await load(parent));
  state.trail.push(await load(node));
  state.selected = null;
  render();
}

function select(node: ViewNode) {
  state.selected = state.selected?.id === node.id ? null : node;
  render();
}

function goUp() {
  if (state.selected) state.selected = null;
  else if (state.trail.length > 1) state.selected = state.trail.pop()!;
  render();
}

/* ---------- Shared pieces ---------- */

function categoryTotals(root: ViewNode): Map<Category, number> {
  const totals = new Map<Category, number>();
  for (const c of root.children ?? []) {
    const cat = c.category ?? "other";
    totals.set(cat, (totals.get(cat) ?? 0) + c.size);
  }
  return totals;
}

function usageBar(drive: Drive, root: ViewNode | null) {
  const bar = h("div", { class: "ubar", role: "img", "aria-label": `${fmtBytes(drive.total - drive.free)} used of ${fmtBytes(drive.total)}` });
  const seg = (bytes: number, color: string, title: string, cls = "") => {
    if (bytes <= 0) return;
    bar.append(h("span", { class: cls, title, style: `width:${(bytes / drive.total) * 100}%;background:${color}` }));
  };
  if (!root) {
    seg(drive.total - drive.free, "var(--ink)", `Used: ${fmtBytes(drive.total - drive.free)}`);
    return bar;
  }
  for (const [cat, bytes] of categoryTotals(root)) seg(bytes, catColor(cat), `${CATEGORIES[cat]}: ${fmtBytes(bytes)}`);
  const unaccounted = drive.total - drive.free - root.size;
  seg(unaccounted, "", `Not readable or reserved by Windows: ${fmtBytes(unaccounted)}`, "unread");
  return bar;
}

let toastTimer = 0;
function toast(message: string) {
  let t = document.getElementById("toast");
  if (!t) document.body.append((t = h("div", { id: "toast", class: "toast", role: "status" })));
  t.textContent = message;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (t!.hidden = true), 3000);
}

async function reveal(path: string) {
  try {
    await revealItemInDir(path);
  } catch (e) {
    toast(`Couldn't open File Explorer: ${e}`);
  }
}

/* ---------- Drive picker ---------- */

function driveIcon(d: Drive) {
  const icon = h("span", { class: "drive-icon", "aria-hidden": "true" });
  icon.innerHTML = d.removable
    ? `<svg viewBox="0 0 40 40" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="9" y="4" width="22" height="32" rx="4"/><circle cx="20" cy="29" r="2"/><path d="M14 10h12"/></svg>`
    : `<svg viewBox="0 0 40 40" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="4" y="11" width="32" height="18" rx="3"/><path d="M4 22h32"/><circle cx="30" cy="25.5" r="1.3" fill="currentColor"/><path d="M9 25.5h8"/></svg>`;
  return icon;
}

function renderPicker() {
  const busy = state.scanning;
  return h(
    "div",
    { class: "picker" },
    h("h1", {}, "Choose a drive to scan"),
    h("p", { class: "sub" }, "Storage View only reads file sizes. It never changes or deletes anything."),
    !state.elevated &&
      h(
        "div",
        { class: "admin-note" },
        h("p", {}, "Some system folders can only be read with administrator rights. Without them, their space shows as “Not readable”."),
        adminButton(null),
      ),
    state.error && h("p", { class: "error", role: "alert" }, state.error),
    h(
      "div",
      { class: "dcards" },
      ...state.drives.map((d) => {
        const scan = state.scans.get(d.mount);
        const mine = busy?.drive === d.mount;
        const root = scan?.cache.get("root") ?? null;
        return h(
          "article",
          { class: "dcard" },
          h("div", { class: "dcard-head" }, driveIcon(d), h("div", {}, h("h2", {}, driveName(d)), h("p", {}, `${d.kind} · ${d.file_system} · ${fmtBytes(d.total)}`))),
          usageBar(d, root),
          h("p", { class: "nums" }, h("span", {}, h("b", {}, fmtBytes(d.total - d.free)), " used"), h("span", {}, `${fmtBytes(d.free)} free`)),
          mine
            ? h(
                "div",
                { class: "scan" },
                h("div", { class: "scan-track" }, h("i", {})),
                h("p", { class: "scan-count", id: "scan-count" }, "Starting…"),
                h("p", { class: "scan-text", id: "scan-path" }, d.mount),
                h("button", { class: "btn", onclick: () => invoke("cancel_scan") }, "Cancel"),
              )
            : h(
                "div",
                { class: "actions" },
                scan && h("button", { class: "btn primary", disabled: !!busy, onclick: () => showDrive(d) }, "Open"),
                h("button", { class: "btn" + (scan ? "" : " primary"), disabled: !!busy, onclick: () => startScan(d) }, `${scan ? "Rescan" : "Scan"} ${driveLetter(d.mount)}`),
              ),
        );
      }),
    ),
  );
}

function updateProgress() {
  const p = state.scanning?.progress;
  const count = document.getElementById("scan-count");
  const path = document.getElementById("scan-path");
  if (!p || !count || !path) return;
  count.textContent = `${fmtCount(p.files)} files · ${fmtBytes(p.bytes)}`;
  path.textContent = p.current || p.drive;
}

/* ---------- Treemap screen ---------- */

function renderSidebar() {
  return h(
    "aside",
    { class: "drives", "aria-label": "Drives" },
    h("p", { class: "side-h" }, "Drives"),
    ...state.drives.map((d) => {
      const scan = state.scans.get(d.mount);
      return h(
        "button",
        {
          class: "drive-btn",
          "aria-current": String(d.mount === state.drive?.mount),
          onclick: () => (scan ? showDrive(d) : startScan(d)),
        },
        h("span", { class: "dn" }, d.label || "Local Disk", h("span", {}, driveLetter(d.mount))),
        usageBar(d, scan?.cache.get("root") ?? null),
        h("span", { class: "dm" }, scan ? `${fmtBytes(d.total - d.free)} of ${fmtBytes(d.total)}` : "Not scanned yet"),
      );
    }),
    h("button", { class: "btn", onclick: () => ((state.screen = "picker"), render()) }, "All drives"),
  );
}

function renderToolbar() {
  const crumbs = h("nav", { class: "crumbs", "aria-label": "Location" });
  state.trail.forEach((n, i) => {
    if (i) crumbs.append(h("span", { class: "sep", "aria-hidden": "true" }, "›"));
    const current = i === state.trail.length - 1;
    crumbs.append(
      h(
        "button",
        {
          "aria-current": current ? "page" : null,
          onclick: () => {
            if (current) return;
            state.trail = state.trail.slice(0, i + 1);
            state.selected = null;
            render();
          },
        },
        i === 0 ? driveLetter(n.name) : n.name,
      ),
    );
  });
  return h(
    "div",
    { class: "toolbar" },
    h("button", { class: "btn", title: "Up one level (Esc)", disabled: state.trail.length < 2 && !state.selected, onclick: goUp }, "↑ Up"),
    crumbs,
    h("span", { class: "total" }, fmtBytes(focus().size)),
    h("button", { class: "btn", onclick: () => startScan(state.drive!) }, "Rescan"),
  );
}

function renderUsage() {
  const d = state.drive!;
  const root = state.trail[0];
  const legend = h("div", { class: "legend" });
  for (const [cat, bytes] of [...categoryTotals(root)].sort((a, b) => b[1] - a[1])) {
    legend.append(h("span", {}, h("i", { style: `background:${catColor(cat)}` }), CATEGORIES[cat], " ", h("b", {}, fmtBytes(bytes))));
  }
  const unaccounted = d.total - d.free - root.size;
  if (unaccounted > d.total * 0.005) {
    legend.append(h("span", { class: "unread", title: "Folders Windows wouldn't let Storage View read, plus space the file system reserves." }, h("i", {}), "Not readable ", h("b", {}, fmtBytes(unaccounted))));
  }
  legend.append(h("span", { class: "free" }, h("i", {}), "Free ", h("b", {}, fmtBytes(d.free))));
  return h("div", { class: "usage" }, usageBar(d, root), legend);
}

const tip = h("div", { class: "tip", hidden: true });
function hideTip() {
  tip.hidden = true;
}
function showTip(node: ViewNode | null, e?: MouseEvent) {
  if (!node || !e) return hideTip();
  const viz = tip.parentElement!;
  tip.replaceChildren(
    h("b", {}, node.name),
    h("span", {}, `${fmtBytes(node.size)} · ${pct(node.size, focus().size)} of ${state.trail.length > 1 ? focus().name : driveLetter(focus().name)}`),
    ...(node.path ? [h("em", {}, node.path)] : []),
    ...(canOpen(node) ? [h("em", {}, "Click to open")] : []),
  );
  tip.hidden = false;
  const r = viz.getBoundingClientRect();
  let x = e.clientX - r.left + 14;
  let y = e.clientY - r.top + 14;
  if (x + tip.offsetWidth > r.width) x = e.clientX - r.left - tip.offsetWidth - 14;
  if (y + tip.offsetHeight > r.height) y = e.clientY - r.top - tip.offsetHeight - 14;
  tip.style.transform = `translate(${Math.max(0, x)}px, ${Math.max(0, y)}px)`;
}

const vizSvg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
vizSvg.setAttribute("role", "img");
vizSvg.setAttribute("aria-label", "Disk usage treemap");
const viz = h("div", { class: "viz" }, vizSvg, tip);
let frame = 0;
new ResizeObserver(() => {
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(drawViz);
}).observe(viz);

function drawViz() {
  if (state.screen !== "map" || !viz.isConnected) return;
  const { clientWidth: w, clientHeight: hgt } = viz;
  if (w < 20 || hgt < 20) return;
  drawTreemap(vizSvg, focus(), state.selected?.id ?? null, w, hgt, { open, select, hover: showTip });
}

function kindLabel(n: ViewNode) {
  switch (n.kind) {
    case "drive":
      return "Drive";
    case "app":
      return n.locations.length > 1 ? `App · ${n.locations.length} locations` : "App";
    case "folder":
      return "Folder";
    case "file":
      return "File";
    case "files":
      return "Small files";
    case "more":
      return "Smaller items";
  }
}

function renderDetail() {
  const n = state.selected ?? focus();
  const d = state.drive!;
  const parent = state.selected ? focus() : state.trail[state.trail.length - 2];
  const isDrive = n.kind === "drive";
  const kids = n.children?.slice(0, 8) ?? [];

  return h(
    "aside",
    { class: "detail", "aria-label": "Details" },
    h(
      "div",
      { class: "detail-head" },
      h("h2", {}, isDrive ? driveName(d) : n.name),
      h("div", { class: "kind" }, n.category && h("span", { class: "chip" }, h("i", { style: `background:${catColor(n.category)}` }), CATEGORIES[n.category]), h("span", {}, kindLabel(n))),
    ),
    h("div", { class: "big" }, fmtBytes(n.size)),
    h(
      "div",
      { class: "share" },
      ...(isDrive
        ? [h("div", { class: "meter" }, h("i", { style: `width:${((d.total - d.free) / d.total) * 100}%;background:var(--ink)` })), h("span", {}, `${pct(d.total - d.free, d.total)} of ${fmtBytes(d.total)} used · ${fmtBytes(d.free)} free`)]
        : [
            h("div", { class: "meter" }, h("i", { style: `width:${Math.max(0.6, (n.size / d.total) * 100)}%;background:${catColor(n.category)}` })),
            h("span", {}, `${pct(n.size, d.total)} of ${driveLetter(d.mount)}` + (parent && parent.kind !== "drive" ? ` · ${pct(n.size, parent.size)} of ${parent.name}` : "")),
          ]),
    ),
    n.kind === "files" && h("p", { class: "note" }, "Files under 1 MB in this folder are grouped into one block to keep the map readable."),
    n.locations.length > 1
      ? h(
          "div",
          { class: "inside" },
          h("h3", {}, `Found in ${n.locations.length} locations`),
          h("ol", { class: "locs" }, ...n.locations.map((l) => h("li", {}, h("button", { class: "path link", title: "Show in File Explorer", onclick: () => reveal(l.path) }, l.path), h("span", { class: "sz" }, fmtBytes(l.size))))),
        )
      : n.path && h("p", { class: "path" }, n.path),
    n.path && !isDrive && h("div", { class: "actions" }, h("button", { class: "btn", onclick: () => reveal(n.path!) }, "Show in File Explorer")),
    kids.length > 0 &&
      h(
        "div",
        { class: "inside" },
        h("h3", {}, "Largest inside"),
        h(
          "ol",
          {},
          ...kids.map((c) =>
            h(
              "li",
              {},
              h(
                "button",
                { class: "row", onclick: () => (canOpen(c) ? open(c, state.selected && n !== focus() ? n : null) : select(c)) },
                h("span", { class: "nm" }, c.name),
                h("span", { class: "sz" }, fmtBytes(c.size)),
                h("span", { class: "meter" }, h("i", { style: `width:${(c.size / kids[0].size) * 100}%;background:${catColor(c.category)}` })),
              ),
            ),
          ),
        ),
      ),
  );
}

function renderMap() {
  return h("div", { class: "map" }, renderSidebar(), h("div", { class: "stage" }, renderToolbar(), renderUsage(), viz), renderDetail());
}

function renderStatus() {
  const s = state.drive && state.scans.get(state.drive.mount)?.summary;
  const bar = h("footer", { class: "statusbar" });
  if (state.screen === "map" && s) {
    bar.append(h("span", {}, `${fmtCount(s.files)} files in ${fmtCount(s.dirs)} folders · scanned in ${(s.elapsed_ms / 1000).toFixed(1)} s`));
    if (s.denied && !state.elevated) {
      bar.append(h("span", { class: "warn" }, `${fmtCount(s.denied)} folders couldn't be read.`, adminButton(state.drive)));
    } else if (s.denied) {
      bar.append(h("span", {}, `${fmtCount(s.denied)} folders are protected by Windows even from administrators.`));
    }
  } else {
    bar.append(h("span", {}, state.scanning ? "Scanning…" : `${state.drives.length} drives found`));
  }
  if (state.elevated) bar.append(h("span", { class: "badge" }, h("span", { class: "shield", "aria-hidden": "true" }), "Administrator"));
  return bar;
}

function render() {
  const main = state.screen === "picker" ? renderPicker() : renderMap();
  app.replaceChildren(main, renderStatus());
  if (state.screen === "map") drawViz();
  else updateProgress();
}

/* ---------- Boot ---------- */

document.addEventListener("keydown", (e) => {
  if (state.screen === "map" && (e.key === "Escape" || (e.key === "Backspace" && !(e.target instanceof HTMLInputElement)))) goUp();
});

async function boot() {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) await (await import("./devmock")).install();

  await listen<ScanProgress>("scan-progress", (e) => {
    if (state.scanning?.drive !== e.payload.drive) return;
    state.scanning.progress = e.payload;
    updateProgress();
  });

  await listen<ScanSummary>("scan-done", async (e) => {
    const s = e.payload;
    state.scans.set(s.drive, { summary: s, cache: new Map() });
    if (state.scanning?.drive === s.drive) state.scanning = null;
    await refreshDrives();
    const drive = state.drives.find((d) => d.mount === s.drive);
    if (drive) await showDrive(drive);
  });

  await listen<string>("scan-cancelled", (e) => {
    if (state.scanning?.drive === e.payload) state.scanning = null;
    render();
  });

  const status = await invoke<{ elevated: boolean; startup_scan: string | null }>("app_status");
  state.elevated = status.elevated;
  try {
    await refreshDrives();
  } catch (e) {
    state.error = `Couldn't list drives: ${e}`;
  }
  // After restarting as administrator, carry on with the drive the user was looking at.
  const resume = state.drives.find((d) => d.mount.toUpperCase() === status.startup_scan?.toUpperCase());
  if (resume) await startScan(resume);
  else render();
}

boot();
