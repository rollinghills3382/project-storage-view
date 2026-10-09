// Which scan's events the UI listens to. Kept out of main.ts so it can be tested.
import type { ScanProgress, ScanSummary } from "./ui";

export interface Running {
  drive: string;
  /** The id `start_scan` returned. Null until it has. */
  id: number | null;
  progress: ScanProgress | null;
}

/**
 * Tracks the scan the user started. Every scan event carries the id of the run that sent
 * it, and only the running scan's events update progress or end it, so a late event from
 * a cancelled or replaced run, even one of the same drive, cannot clear a newer scan.
 */
export class ScanRuns {
  running: Running | null = null;
  /** Runs that have ended. A short scan can end before `start_scan` has returned its id. */
  private ended = new Set<number>();

  begin(drive: string): Running {
    this.running = { drive, id: null, progress: null };
    return this.running;
  }

  /** `start_scan` for `run` returned `id`. */
  started(run: Running, id: number) {
    if (this.running !== run) return;
    if (this.ended.has(id)) this.running = null;
    else run.id = id;
  }

  /** `start_scan` for `run` failed. */
  failed(run: Running) {
    if (this.running === run) this.running = null;
  }

  /** Records progress if it is from the running scan. */
  progress(p: ScanProgress): boolean {
    if (this.running?.id !== p.scan) return false;
    this.running.progress = p;
    return true;
  }

  /** Run `id` finished or was cancelled. True if it was the running scan, which is then cleared. */
  end(id: number): boolean {
    this.ended.add(id);
    if (this.running?.id !== id) return false;
    this.running = null;
    return true;
  }
}

/** Whether `next` should replace the results held for its drive. The backend only reports
 * results it stored, and it stores runs in the order they started, so the higher id wins. */
export const isNewer = (next: ScanSummary, held: ScanSummary | undefined) => !held || next.scan > held.scan;
