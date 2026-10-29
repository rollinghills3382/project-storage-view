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
  /** Drive only: every scanned byte by category, largest first, counted before the
   * display limits bundle anything into a "smaller items" tile. */
  categories?: { category: Category; size: number }[];
  /** Bytes of hard links in here that are counted under another name, outside `size`.
   * Left out when there are none. */
  linked?: number;
}

/** A search match and what it sits inside, from the drive down to its parent, as the list
 * and the map show it. */
export interface Found {
  node: ViewNode;
  chain: ViewNode[];
}

export interface SearchResults {
  query: string;
  /** The largest matches. */
  found: Found[];
  /** Every match, including those past `found`. */
  total: number;
}

/** The line above search results. */
export function foundLine(r: Pick<SearchResults, "query" | "found" | "total">): string {
  if (r.total === 0) return `Nothing matches “${r.query}”`;
  const matches = r.total === 1 ? "1 match" : `${fmtCount(r.total)} matches`;
  return r.total > r.found.length ? `${matches}, showing the largest ${fmtCount(r.found.length)}` : `${matches}, largest first`;
}

/** Why some files never show up in search results. */
export const SEARCH_LIMIT = "Files under 1 MB are counted together per folder, so they can't be found by name.";

/** Why a path could not be read. Only `denied` is something administrator rights can fix. */
export type ScanProblem = "denied" | "vanished" | "open_dir" | "list_dir" | "file_type" | "metadata" | "cloud_size";

export interface ScanErrors {
  counts: Record<ScanProblem, number>;
  /** A few affected paths per kind; `counts` has the full numbers. */
  samples: { kind: ScanProblem; path: string; message: string }[];
}

/** Files with several hard links, each counted once. */
export interface LinkReport {
  /** Whether hard links were looked for at all. */
  checked: boolean;
  skipped_because?: string;
  /** Files found under more than one name. */
  files: number;
  /** Names whose bytes are counted under another name of the same file. */
  links: number;
  /** What those names would have added. */
  bytes: number;
  /** Files that couldn't be checked, so may be counted under several names. */
  unchecked: number;
}

export interface ScanSummary {
  /** The run that produced these results. Scan events all carry one. */
  scan: number;
  drive: string;
  files: number;
  dirs: number;
  /** Missing from preview data saved by older builds. */
  errors?: ScanErrors;
  /** Missing from preview data saved by older builds. */
  links?: LinkReport;
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

const LINK_RULE = "Each is counted once, under the name whose folder comes first alphabetically (a folder's own files before its subfolders), so a file shared by System32 and WinSxS counts in System32.";

/** Status-bar notes for files found under more than one name. */
export function linkNotes(links: LinkReport | undefined): ScanNote[] {
  if (!links?.checked) return [];
  const notes: ScanNote[] = [];
  if (links.bytes > 0) {
    const files = links.files === 1 ? "1 file has" : `${fmtCount(links.files)} files have`;
    const names = links.links === 1 ? "Its other name" : `Their other ${fmtCount(links.links)} names`;
    notes.push({
      text: `Hard links counted once: ${fmtBytes(links.bytes)}`,
      warn: false,
      detail: `${files} more than one name on this drive (hard links, which Windows uses throughout WinSxS). ${LINK_RULE} ${names} would have added ${fmtBytes(links.bytes)}.`,
    });
  }
  if (links.unchecked > 0) {
    notes.push({
      text: `${links.unchecked === 1 ? "1 file" : `${fmtCount(links.unchecked)} files`} not checked for hard links`,
      warn: true,
      detail: "These may be counted once for every name they have, so the totals can be a little high.",
    });
  }
  return notes;
}

/** The selection bar's mention of hard links in the selected item that count somewhere else. */
export function linkedNote(n: Pick<ViewNode, "linked">): ScanNote | null {
  if (!n.linked) return null;
  return {
    text: `${fmtBytes(n.linked)} in hard links counted elsewhere`,
    warn: false,
    detail: "Files in here that also have a name in a folder that comes earlier alphabetically. Their space is counted there, not in this total.",
  };
}

export interface UsedGap {
  label: string;
  bytes: number;
  /** The full explanation, for the entry's tooltip. */
  detail: string;
  /** False when the figure is not an area of the drive, so it gets no swatch. */
  swatch: boolean;
}

/** Differences smaller than this share of the drive get no legend entry. */
const GAP_SHARE = 0.005;

/**
 * The legend entry for the difference between the space Windows reports as used and the bytes
 * the scan counted. Only reads that failed are called "not readable"; the rest of the gap is
 * space the file system keeps for itself. Hard links are counted once per link, so the scan can
 * also add up to more than is used, which hides any unreadable space and is said as such.
 */
export function usedGap(drive: Pick<Drive, "total" | "free">, scanned: number, errors: ScanErrors | undefined, links?: LinkReport): UsedGap | null {
  const gap = drive.total - drive.free - scanned;
  if (Math.abs(gap) <= drive.total * GAP_SHARE) return null;
  // Once every file was checked for hard links, they no longer explain any of the difference.
  const linksCounted = !!links?.checked && links.unchecked === 0;
  // The same kinds the status bar reports as unread, so the two numbers agree.
  const unread = errors && (["denied", ...READ_FAILURES] as ScanProblem[]).reduce((sum, k) => sum + (errors.counts[k] ?? 0), 0);
  const items = (n: number) => (n === 1 ? "1 item" : `${fmtCount(n)} items`);
  if (gap < 0) {
    const over = -gap;
    return {
      label: "Overcounted",
      bytes: over,
      detail:
        `The scan adds up to ${fmtBytes(over)} more than Windows reports as used. ` +
        (linksCounted
          ? "Hard links are already counted once, so this is most likely files Windows stores compressed (for example with Compact OS) or as sparse files: they take less space than their length, and Storage View counts their length."
          : "Hard links are counted once per link, so a file stored once on disk can be counted more than once.") +
        (unread ? ` ${items(unread)} couldn't be read, and their size can't be shown while the scan counts more than is used.` : ""),
      swatch: false,
    };
  }
  if (unread) {
    return {
      label: "Not readable",
      bytes: gap,
      detail:
        `Used space the scan found in no file. ${items(unread)} couldn't be read. This also includes space the file system reserves for itself, which can't be measured separately` +
        (linksCounted ? "." : ", and hard links counted more than once can make it look smaller than it is."),
      swatch: true,
    };
  }
  return {
    label: "Not in files",
    bytes: gap,
    detail:
      unread === 0
        ? "Used space the scan found in no file. Every folder was read, so this is space the file system keeps for itself, such as its own records and the unused ends of clusters."
        : "Used space the scan found in no file: folders that couldn't be read, plus space the file system keeps for itself.",
    swatch: true,
  };
}

export interface ScanProgress {
  scan: number;
  drive: string;
  files: number;
  dirs: number;
  bytes: number;
  current: string;
}

export interface ScanCancelled {
  scan: number;
  drive: string;
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
