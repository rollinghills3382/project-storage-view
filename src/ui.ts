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

export interface ScanSummary {
  drive: string;
  files: number;
  dirs: number;
  denied: number;
  elapsed_ms: number;
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
  if (whole <= 0) return "0%";
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
