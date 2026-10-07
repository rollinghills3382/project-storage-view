// Frontend tests. Run with `npm run test:ui`; they live outside `src` so the app build and
// tsconfig do not have to know about Node's test runner.
//
// This file is deliberately not in tsconfig's `include`: it imports `node:test`, and adding
// @types/node to type-check it would mean type-checking nothing else in the project too.
import assert from "node:assert/strict";
import { test } from "node:test";

import { canOpen } from "../src/treemap.ts";
import { CATEGORIES, catColor, fmtBytes, fmtCount, pct, type ViewNode } from "../src/ui.ts";

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