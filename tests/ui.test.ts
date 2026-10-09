// Frontend tests. Run with `npm run test:ui`; they live outside `src` so the app build and
// tsconfig do not have to know about Node's test runner.
//
// This file is deliberately not in tsconfig's `include`: it imports `node:test`, and adding
// @types/node to type-check it would mean type-checking nothing else in the project too.
import assert from "node:assert/strict";
import { test } from "node:test";

import { canOpen } from "../src/treemap.ts";
import { CATEGORIES, catColor, fmtBytes, fmtCount, pct, scanNotes, type ScanErrors, type ScanProblem, type ViewNode } from "../src/ui.ts";

test("fmtBytes uses binary units labelled the way File Explorer does", () => {
  assert.equal(fmtBytes(0), "0 bytes");
  assert.equal(fmtBytes(512), "512 bytes");
  assert.equal(fmtBytes(1024), "1.00 KB");
  assert.equal(fmtBytes(1536), "1.50 KB");
  assert.equal(fmtBytes(1024 ** 2), "1.00 MB");
  assert.equal(fmtBytes(1024 ** 3), "1.00 GB");
  assert.equal(fmtBytes(1024 ** 4), "1.00 TB");
  // Past the largest unit it stays in TB rather than inventing one.
  assert.equal(fmtBytes(1024 ** 5), "1024 TB");
});

test("fmtBytes drops precision as the number grows", () => {
  assert.equal(fmtBytes(1 * 1024), "1.00 KB");
  assert.equal(fmtBytes(10 * 1024), "10.0 KB");
  assert.equal(fmtBytes(100 * 1024), "100 KB");
  assert.equal(fmtBytes(999 * 1024), "999 KB");
});

test("pct keeps small shares visible instead of rounding them to zero", () => {
  assert.equal(pct(0, 0), "0%");
  assert.equal(pct(5, 0), "0%", "nothing to compare against");
  assert.equal(pct(1, 2), "50%");
  assert.equal(pct(1, 10), "10%");
  assert.equal(pct(1, 20), "5.0%");
  assert.equal(pct(1, 1000), "0.1%");
  assert.equal(pct(1, 2000), "<0.1%");
  assert.equal(pct(0, 100), "0%");
});

test("every category has a label and a colour, and a missing one falls back", () => {
  assert.equal(catColor(null), "var(--c-other)");
  assert.equal(catColor("games"), "var(--c-games)");
  for (const [key, label] of Object.entries(CATEGORIES)) {
    assert.ok(label, `${key} needs a label`);
    assert.equal(catColor(key as keyof typeof CATEGORIES), `var(--c-${key})`);
  }
});

const node = (over: Partial<ViewNode>): ViewNode => ({
  id: "n1",
  name: "thing",
  size: 10,
  kind: "folder",
  category: null,
  path: null,
  locations: [],
  has_children: false,
  children: null,
  ...over,
});

test("only a node with children can be opened", () => {
  assert.equal(canOpen(node({})), false);
  assert.equal(canOpen(node({ kind: "file", has_children: false })), false);
  assert.equal(canOpen(node({ has_children: true })), true);
  // A "smaller items" tile is openable: it lists the rest of its folder. Treating it as
  // a dead end is what hid the tail of every capped folder.
  assert.equal(canOpen(node({ kind: "more", has_children: true })), true);
});

test("fmtCount groups thousands", () => {
  assert.equal(fmtCount(0), "0");
  assert.equal(fmtCount(999), "999");
  assert.equal(fmtCount(1000), "1,000");
});
const errors = (counts: Partial<Record<ScanProblem, number>>, samples: ScanErrors["samples"] = []): ScanErrors => ({
  counts: { denied: 0, vanished: 0, open_dir: 0, list_dir: 0, file_type: 0, metadata: 0, cloud_size: 0, ...counts },
  samples,
});

test("scanNotes says nothing for a clean scan or preview data without errors", () => {
  assert.deepEqual(scanNotes(undefined, false), []);
  assert.deepEqual(scanNotes(errors({}), true), []);
});

test("scanNotes only blames permissions for permission failures", () => {
  const io = errors({ open_dir: 2, list_dir: 1, file_type: 1, metadata: 1 }, [{ kind: "open_dir", path: "D:\\Bad", message: "The request could not be performed because of an I/O device error." }]);
  for (const elevated of [false, true]) {
    const notes = scanNotes(io, elevated);
    assert.equal(notes.length, 1);
    assert.equal(notes[0].text, "Scan incomplete: 5 items couldn't be read");
    assert.ok(notes[0].warn);
    assert.doesNotMatch(notes[0].text, /administrator|protected/);
    assert.equal(notes[0].detail, "D:\\Bad: The request could not be performed because of an I/O device error.\nand 4 more");
  }
});

test("scanNotes offers administrator rights only for denials, and calls them protected only once elevated", () => {
  const denied = errors({ denied: 3 }, [{ kind: "denied", path: "C:\\System Volume Information", message: "Access is denied." }]);
  assert.deepEqual(scanNotes(denied, false), [{ text: "3 items couldn't be read without administrator rights", warn: true, detail: "C:\\System Volume Information: Access is denied.\nand 2 more" }]);
  assert.equal(scanNotes(denied, true)[0].text, "3 items protected by Windows even from administrators");
  assert.equal(scanNotes(denied, true)[0].warn, false);
});

test("scanNotes keeps each kind of problem separate", () => {
  const mixed = errors({ denied: 1, vanished: 2, metadata: 1, cloud_size: 1 }, [
    { kind: "vanished", path: "C:\\tmp\\a", message: "not found" },
    { kind: "cloud_size", path: "C:\\OneDrive\\v.mp4", message: "The cloud file provider is not running." },
  ]);
  const notes = scanNotes(mixed, false);
  assert.deepEqual(
    notes.map((n) => n.text),
    [
      "Scan incomplete: 1 item couldn't be read",
      "1 cloud file couldn't be measured and count as 0 bytes",
      "1 item couldn't be read without administrator rights",
      "2 items changed or were removed during the scan",
    ],
  );
  assert.equal(notes[1].detail, "C:\\OneDrive\\v.mp4: The cloud file provider is not running.");
  assert.equal(notes[3].warn, false, "a file deleted mid-scan is not a failure to read data that exists");
});
