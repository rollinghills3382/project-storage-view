// Browser preview for UI work: `npm run dev` outside Tauri replays a scan saved by
// `cargo run --release --example report -- C:\ --json ../devdata/scan.json`.
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { Drive, ScanSummary, ViewNode } from "./ui";

interface Dump {
  drive: Drive;
  summary: ScanSummary;
  /** `get_map` and `get_node` results by id, for the drive root and its largest folders. */
  maps: Record<string, ViewNode>;
  nodes: Record<string, ViewNode>;
}

export async function install() {
  const res = await fetch("/devdata/scan.json");
  if (!res.ok) throw new Error("No devdata/scan.json. Generate one with the report example (see src/devmock.ts).");
  const dump: Dump = await res.json();

  // Folders that were only saved nested inside another map can still be opened, with what was saved of them.
  const nested = new Map<string, ViewNode>();
  const index = (n: ViewNode) => {
    if (!n.children) return;
    if (!nested.has(n.id)) nested.set(n.id, n);
    n.children.forEach(index);
  };
  Object.values(dump.maps).forEach(index);

  mockIPC(
    (cmd, args) => {
      const a = args as Record<string, string>;
      switch (cmd) {
        case "app_status":
          return { elevated: false, startup_scan: null };
        case "restart_as_admin":
          throw "Restarting as administrator only works in the desktop app.";
        case "list_drives":
          return [dump.drive];
        case "start_scan":
          setTimeout(async () => {
            for (let i = 1; i <= 5; i++) {
              await emit("scan-progress", { drive: a.drive, files: (dump.summary.files * i) / 5, dirs: 0, bytes: 0, current: `${a.drive}Users` });
              await new Promise((r) => setTimeout(r, 200));
            }
            await emit("scan-done", dump.summary);
          });
          return null;
        case "get_map":
        case "get_node": {
          const node = (cmd === "get_map" ? dump.maps : dump.nodes)[a.id] ?? nested.get(a.id);
          if (!node) throw `Not in the preview data: ${a.id}`;
          return node;
        }
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}
