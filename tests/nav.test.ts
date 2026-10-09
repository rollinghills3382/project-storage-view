// Navigation and scan-event ordering (#3, #4). Responses are resolved by hand, in the
// order each test chooses, so every interleaving here is deterministic.
import assert from "node:assert/strict";
import { test } from "node:test";

import { navigator, type Place } from "../src/nav.ts";
import { isNewer, ScanRuns } from "../src/scanrun.ts";
import type { Drive, ScanSummary, ViewNode } from "../src/ui.ts";

const drive = (mount: string): Drive => ({ mount, label: "", kind: "SSD", total: 100, free: 10, removable: false, file_system: "NTFS" });
const C = drive("C:\\");
const D = drive("D:\\");

const node = (id: string, name = id): ViewNode => ({
  id,
  name,
  size: 10,
  kind: id === "root" ? "drive" : "folder",
  category: null,
  path: null,
  locations: [],
  has_children: true,
  children: [],
});

/** A loader whose responses are held until the test resolves or rejects them. */
function controlled() {
  const pending = new Map<string, { resolve: () => void; reject: (e: unknown) => void }>();
  const load = (d: string, kind: "maps" | "nodes", id: string) =>
    new Promise<ViewNode>((resolve, reject) => {
      pending.set(`${d}|${kind}|${id}`, { resolve: () => resolve(node(id, id === "root" ? d : id)), reject });
    });
  const settle = async (key: string, how: "resolve" | "reject" = "resolve") => {
    const p = pending.get(key);
    assert.ok(p, `nothing is waiting on ${key}; waiting: ${[...pending.keys()].join(", ")}`);
    pending.delete(key);
    if (how === "resolve") p.resolve();
    else p.reject(`couldn't load ${key}`);
    // Let the awaiting move run to completion.
    for (let i = 0; i < 5; i++) await Promise.resolve();
  };
  return { load, settle };
}

function setup(scanned = ["C:\\", "D:\\"]) {
  const place: Place = { drive: null, trail: [], selected: null, open: new Set() };
  const io = controlled();
  const nav = navigator(place, io.load, (m) => scanned.includes(m));
  return { place, nav, ...io };
}

const names = (trail: ViewNode[]) => trail.map((n) => n.name);

/** Opens `d` and lets it finish, as the starting point of a test. */
async function opened(t: ReturnType<typeof setup>, d: Drive) {
  const done = t.nav.showDrive(d);
  await t.settle(`${d.mount}|maps|root`);
  await t.settle(`${d.mount}|nodes|root`);
  assert.equal(await done, true);
}

test("drive loads answered out of order leave the last drive clicked on screen", async () => {
  const t = setup();
  const toC = t.nav.showDrive(C);
  const toD = t.nav.showDrive(D);
  // D answers first, then C.
  await t.settle("D:\\|maps|root");
  await t.settle("D:\\|nodes|root");
  await t.settle("C:\\|maps|root");
  await t.settle("C:\\|nodes|root");
  assert.equal(await toD, true);
  assert.equal(await toC, false, "the earlier click is dropped");
  assert.equal(t.place.drive, D);
  assert.deepEqual(names(t.place.trail), ["D:\\"], "the root shown belongs to the selected drive");
});

test("nothing changes on screen until the drive has loaded", async () => {
  const t = setup();
  await opened(t, C);
  const toD = t.nav.showDrive(D);
  assert.equal(t.place.drive, C, "drive, root and details change together, not the name first");
  assert.equal(t.nav.destination(), D);
  await t.settle("D:\\|maps|root");
  assert.equal(t.place.drive, C);
  await t.settle("D:\\|nodes|root");
  assert.equal(await toD, true);
  assert.equal(t.place.drive, D);
  assert.equal(t.nav.destination(), D);
});

test("sibling folders opened quickly never become parent and child", async () => {
  const t = setup();
  await opened(t, C);
  const root = t.place.trail;
  const toA = t.nav.goTo([...root, node("A")], null);
  const toB = t.nav.goTo([...root, node("B")], null);
  // B answers first, then A.
  await t.settle("C:\\|maps|B");
  await t.settle("C:\\|maps|A");
  assert.equal(await toB, true);
  assert.equal(await toA, false);
  assert.deepEqual(names(t.place.trail), ["C:\\", "B"]);
});

test("a folder still loading is dropped by a drive switch", async () => {
  const t = setup();
  await opened(t, C);
  const toA = t.nav.goTo([...t.place.trail, node("A")], null);
  const toD = t.nav.showDrive(D);
  await t.settle("D:\\|maps|root");
  await t.settle("D:\\|nodes|root");
  await t.settle("C:\\|maps|A");
  assert.equal(await toA, false);
  assert.equal(await toD, true);
  assert.equal(t.place.drive, D);
  assert.deepEqual(names(t.place.trail), ["D:\\"]);
});

test("a folder still loading is dropped by Up or a breadcrumb", async () => {
  const t = setup();
  await opened(t, C);
  const toX = t.nav.goTo([...t.place.trail, node("X")], null);
  await t.settle("C:\\|maps|X");
  await toX;
  const atX = t.place.trail;

  // Open Y inside X, then go Up (which is a move to the parent trail) before Y answers.
  const toY = t.nav.goTo([...atX, node("Y")], null);
  const up = t.nav.goTo(atX.slice(0, -1), { node: atX[1], chain: atX.slice(0, -1) });
  await t.settle("C:\\|maps|root");
  await t.settle("C:\\|maps|Y");
  assert.equal(await up, true);
  assert.equal(await toY, false);
  assert.deepEqual(names(t.place.trail), ["C:\\"]);
});

test("a failed load keeps the last view and reports the error", async () => {
  const t = setup();
  await opened(t, C);
  const before = { ...t.place };
  const toD = t.nav.showDrive(D);
  await t.settle("D:\\|maps|root", "reject");
  await t.settle("D:\\|nodes|root");
  await assert.rejects(toD, /couldn't load D/);
  assert.equal(t.place.drive, before.drive);
  assert.equal(t.place.trail, before.trail);
  assert.equal(t.nav.destination(), C, "nothing is left half opened");

  const toA = t.nav.goTo([...t.place.trail, node("A")], null);
  await t.settle("C:\\|maps|A", "reject");
  await assert.rejects(toA);
  assert.deepEqual(names(t.place.trail), ["C:\\"]);
});

test("a failure from a move the user already left is not reported", async () => {
  const t = setup();
  await opened(t, C);
  const toA = t.nav.goTo([...t.place.trail, node("A")], null);
  const toB = t.nav.goTo([...t.place.trail, node("B")], null);
  await t.settle("C:\\|maps|A", "reject");
  await t.settle("C:\\|maps|B");
  assert.equal(await toA, false);
  assert.equal(await toB, true);
});

test("list rows expand independently, but not after a move", async () => {
  const t = setup();
  await opened(t, C);
  const read = (id: string) =>
    t.nav.read(async () => {
      await t.load("C:\\", "nodes", id);
      return () => t.place.open.add(id);
    });
  // Two rows opened in a row both expand, in whatever order they answer.
  const a = read("A");
  const b = read("B");
  await t.settle("C:\\|nodes|B");
  await t.settle("C:\\|nodes|A");
  assert.deepEqual([await a, await b], [true, true]);
  assert.ok(t.place.open.has("A") && t.place.open.has("B"));

  // A row that answers after a drive switch does not expand on the new drive.
  const c = read("C1");
  const toD = t.nav.showDrive(D);
  await t.settle("D:\\|maps|root");
  await t.settle("D:\\|nodes|root");
  await t.settle("C:\\|nodes|C1");
  assert.equal(await toD, true);
  assert.equal(await c, false);
  assert.ok(!t.place.open.has("C1"));
});

test("an unscanned drive opens straight away with nothing to show", async () => {
  const t = setup(["C:\\"]);
  await opened(t, C);
  assert.equal(await t.nav.showDrive(D), true);
  assert.equal(t.place.drive, D);
  assert.deepEqual(t.place.trail, []);
});

/* ---------- Scan events (#4) ---------- */

const progress = (scan: number, drive = "C:\\") => ({ scan, drive, files: 1, dirs: 1, bytes: 1, current: "" });
const summary = (scan: number, drive = "C:\\"): ScanSummary => ({ scan, drive, files: 1, dirs: 1, elapsed_ms: 1 });

test("a cancelled scan's late events don't touch the rescan of the same drive", () => {
  const runs = new ScanRuns();
  runs.started(runs.begin("C:\\"), 1);
  assert.equal(runs.end(1), true, "scan 1 is cancelled");

  const rescan = runs.begin("C:\\");
  runs.started(rescan, 2);
  assert.equal(runs.progress(progress(1)), false, "late progress from scan 1 is ignored");
  assert.equal(runs.end(1), false, "a repeated end for scan 1 does not clear scan 2");
  assert.equal(runs.running, rescan);
  assert.equal(runs.progress(progress(2)), true);
  assert.equal(runs.end(2), true);
  assert.equal(runs.running, null);
});

test("a superseded scan's end does not clear the scan that replaced it", () => {
  const runs = new ScanRuns();
  runs.started(runs.begin("C:\\"), 1);
  const newer = runs.begin("C:\\");
  runs.started(newer, 2);
  assert.equal(runs.end(1), false);
  assert.equal(runs.running?.id, 2);
});

test("events from before start_scan returns are not taken as the new scan's", () => {
  const runs = new ScanRuns();
  const run = runs.begin("C:\\");
  assert.equal(runs.progress(progress(7)), false);
  assert.equal(runs.end(7), false, "the id is unknown yet, so nothing is cleared");
  assert.equal(runs.running, run);
});

test("a scan that ends before start_scan returns its id still ends", () => {
  const runs = new ScanRuns();
  const run = runs.begin("C:\\");
  runs.end(3);
  runs.started(run, 3);
  assert.equal(runs.running, null);
});

test("a failed start clears only its own run", () => {
  const runs = new ScanRuns();
  const first = runs.begin("C:\\");
  const second = runs.begin("D:\\");
  runs.failed(first);
  assert.equal(runs.running, second);
  runs.failed(second);
  assert.equal(runs.running, null);
});

test("older results never replace newer ones for the same drive", () => {
  assert.equal(isNewer(summary(1), undefined), true);
  assert.equal(isNewer(summary(3), summary(2)), true);
  assert.equal(isNewer(summary(2), summary(3)), false);
});
