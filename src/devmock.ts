// Browser preview for UI work: `npm run dev` outside Tauri replays a scan saved by
// `cargo run --release --example report -- C:\ --json ../devdata/scan.json`.
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import type { Drive, ScanSummary, ViewNode } from "./ui";

interface Dump {
  drive: Drive;
  summary: ScanSummary;
  nodes: Record<string, ViewNode>;
}

export async function install() {
  const res = await fetch("/devdata/scan.json");
  if (!res.ok) throw new Error("No devdata/scan.json. Generate one with the report example (see src/devmock.ts).");
  const dump: Dump = await res.json();

  mockIPC(
    (cmd, args) => {
      const a = args as Record<string, string>;
      switch (cmd) {
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
        case "get_node": {
          const node = dump.nodes[a.id];
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
