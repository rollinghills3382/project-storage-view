export type Category = "games" | "creative" | "dev" | "productivity" | "media" | "user" | "system" | "other";

export const CATEGORIES: Record<Category, string> = {
  games: "Games",
  creative: "Creative",
  dev: "Development",
  productivity: "Apps & tools",
  media: "Media",
  user: "Your files",
  system: "System",
  other: "Other",
};

export interface Drive {
  mount: string;
  label: string;
  kind: string;
  total: number;
  free: number;
  removable: boolean;
  file_system: string;
}

export interface ViewNode {
  id: string;
  name: string;
  size: number;
  kind: "drive" | "app" | "folder" | "file" | "files" | "more";
  category: Category | null;
  path: string | null;
  locations: { path: string; size: number }[];
  has_children: boolean;
  children: ViewNode[] | null;
}

/** Why a path could not be read. Only `denied` is something administrator rights can fix. */
export type ScanProblem = "denied" | "vanished" | "open_dir" | "list_dir" | "file_type" | "metadata" | "cloud_size";

export interface ScanErrors {
  counts: Record<ScanProblem, number>;
  /** A few affected paths per kind; `counts` has the full numbers. */
  samples: { kind: ScanProblem; path: string; message: string }[];
}

export interface ScanSummary {
  drive: string;
  files: number;
  dirs: number;
  /** Missing from preview data saved by older builds. */
  errors?: ScanErrors;
  elapsed_ms: number;
}

export interface ScanNote {
  text: string;
  warn: boolean;
  /** Affected paths and their errors, one per line, for the note's tooltip. */
  detail: string;
}

const READ_FAILURES: ScanProblem[] = ["open_dir", "list_dir", "file_type", "metadata"];

/**
 * Status-bar notes for what a scan could not read. Only permission failures are described as
 * protected or as needing administrator rights; any other failure means the scan is incomplete.
 */
export function scanNotes(errors: ScanErrors | undefined, elevated: boolean): ScanNote[] {
  if (!errors) return [];
  const count = (kinds: ScanProblem[]) => kinds.reduce((sum, k) => sum + (errors.counts[k] ?? 0), 0);
  const detail = (kinds: ScanProblem[]) => {
    const shown = errors.samples.filter((s) => kinds.includes(s.kind));
    const rest = count(kinds) - shown.length;
    return [...shown.map((s) => `${s.path}: ${s.message}`), ...(rest > 0 ? [`and ${fmtCount(rest)} more`] : [])].join("\n");
  };
  const items = (n: number) => (n === 1 ? "1 item" : `${fmtCount(n)} items`);
  const notes: ScanNote[] = [];
  const add = (kinds: ScanProblem[], warn: boolean, text: (n: number) => string) => {
    const n = count(kinds);
    if (n) notes.push({ text: text(n), warn, detail: detail(kinds) });
  };
  add(READ_FAILURES, true, (n) => `Scan incomplete: ${items(n)} couldn't be read`);
  add(["cloud_size"], true, (n) => `${n === 1 ? "1 cloud file" : `${fmtCount(n)} cloud files`} couldn't be measured and count as 0 bytes`);
  if (elevated) add(["denied"], false, (n) => `${items(n)} protected by Windows even from administrators`);
  else add(["denied"], true, (n) => `${items(n)} couldn't be read without administrator rights`);
  add(["vanished"], false, (n) => `${items(n)} changed or were removed during the scan`);
  return notes;
}

export interface ScanProgress {
  drive: string;
  files: number;
  dirs: number;
  bytes: number;
  current: string;
}

export const catColor = (c: Category | null) => `var(--c-${c ?? "other"})`;

/** Binary units labelled KB/MB/GB, matching File Explorer. */
export function fmtBytes(bytes: number): string {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  if (i === 0) return `${bytes} bytes`;
  return `${v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)} ${units[i]}`;
}

export const fmtCount = (n: number) => n.toLocaleString();

export function pct(part: number, whole: number): string {
  // Nothing to compare against, or nothing at all: "less than 0.1%" would be a lie here.
  if (whole <= 0 || part <= 0) return "0%";
  const p = (part / whole) * 100;
  return (p >= 10 ? p.toFixed(0) : p >= 0.1 ? p.toFixed(1) : "<0.1") + "%";
}

export const driveLetter = (mount: string) => mount.replace(/[\\/]+$/, "");
export const driveName = (d: Drive) => `${d.label || (d.removable ? "USB Drive" : "Local Disk")} (${driveLetter(d.mount)})`;

type Child = Node | string | null | undefined | false;
type Attrs = Record<string, string | number | boolean | null | undefined | EventListener>;

/** Small DOM builder. Styles go through the CSSOM so they are allowed by the app's CSP. */
export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Attrs = {}, ...kids: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (typeof v === "function") el.addEventListener(k.replace(/^on/, ""), v);
    else if (k === "style") el.style.cssText = String(v);
    else el.setAttribute(k, v === true ? "" : String(v));
  }
  for (const kid of kids) if (kid != null && kid !== false) el.append(kid);
  return el;
}

export function kindLabel(n: ViewNode): string {
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
