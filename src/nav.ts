// Moving between drives and folders. Kept out of main.ts, which needs a DOM, so the
// ordering rules can be tested with controlled responses.
import type { Drive, ViewNode } from "./ui";

/** An item and the folders it sits inside, from the drive root down to its parent. */
export interface Located {
  node: ViewNode;
  chain: ViewNode[];
}

/** The part of the app state that says what is on screen. */
export interface Place {
  drive: Drive | null;
  /** Map location: breadcrumb from the drive root to the folder being viewed. Empty until the drive is scanned. */
  trail: ViewNode[];
  selected: Located | null;
  /** Ids of the expanded list rows. */
  open: Set<string>;
}

export type Load = (drive: string, kind: "maps" | "nodes", id: string) => Promise<ViewNode>;

/** Changes `place` once everything a move needed has loaded. */
export type Commit = () => void;

/**
 * Every move loads what it needs first and then changes `place` in one step, and only if
 * no later move started while it was loading. Responses can come back in any order, so
 * without this a slow one lands on top of a newer view: one drive's map under another
 * drive's name, or two sibling folders shown as parent and child.
 *
 * A move that fails changes nothing and rethrows, so the last view stays up and the caller
 * can report it. A move that was overtaken is dropped, failure included: the user has
 * already gone somewhere else.
 */
export function navigator(place: Place, load: Load, scanned: (mount: string) => boolean) {
  let latest = 0;
  /** The drive the latest move is opening, while it loads. */
  let heading: Drive | null = null;

  /** Runs a move. Resolves to whether it was applied. */
  async function move(step: () => Promise<Commit>, to: Drive | null = null): Promise<boolean> {
    const me = ++latest;
    heading = to;
    try {
      const commit = await step();
      if (me !== latest) return false;
      commit();
      return true;
    } catch (e) {
      if (me !== latest) return false;
      throw e;
    } finally {
      if (me === latest) heading = null;
    }
  }

  /** Loads something that does not change the location, such as a list row's children.
   * Reads don't overtake each other, but a move started meanwhile discards them. */
  async function read(step: () => Promise<Commit>): Promise<boolean> {
    const at = latest;
    try {
      const commit = await step();
      if (at !== latest) return false;
      commit();
      return true;
    } catch (e) {
      if (at !== latest) return false;
      throw e;
    }
  }

  return {
    move,
    read,

    /** The drive on screen, or the one being opened if a drive switch is still loading. */
    destination: () => heading ?? place.drive,

    showDrive: (drive: Drive) =>
      move(async () => {
        const trail = scanned(drive.mount) ? [(await Promise.all([load(drive.mount, "maps", "root"), load(drive.mount, "nodes", "root")]))[0]] : [];
        return () => {
          place.drive = drive;
          place.trail = trail;
          place.selected = null;
          place.open = new Set(["root"]);
        };
      }, drive),

    /** Points the map at the last folder in `trail`, which is the only one that needs its
     * nested levels. `also` runs as part of the same commit. */
    goTo(trail: ViewNode[], selected: Located | null, also?: Commit) {
      const drive = place.drive!.mount;
      const last = trail[trail.length - 1];
      return move(async () => {
        // Keep the name the user clicked on; app locations are labelled by path in their parent.
        const loaded = { ...(await load(drive, "maps", last.id)), name: last.name };
        return () => {
          place.trail = [...trail.slice(0, -1), loaded];
          place.selected = selected;
          also?.();
        };
      });
    },
  };
}
