// Frontend tests. Run with `npm run test:ui`; they live outside `src` so the app build and
// tsconfig do not have to know about Node's test runner.
//
// This file is deliberately not in tsconfig's `include`: it imports `node:test`, and adding
// @types/node to type-check it would mean type-checking nothing else in the project too.
import assert from "node:assert/strict";
import { test } from "node:test";

import { canOpen } from "../src/treemap.ts";
import {
  appClaims,
  CATEGORIES,
  claimNote,
  catColor,
  fmtBytes,
  fmtCount,
  foundLine,
  linkedNote,
  linkNotes,
  pct,
  scanNotes,
  usedGap,
  type LinkReport,
  type ScanErrors,
  type ScanProblem,
  type ViewNode,
} from "../src/ui.ts";

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

const GB = 1024 ** 3;
/** A 1000 GB drive with `used` GB in use. */
const drive = (used: number) => ({ total: 1000 * GB, free: (1000 - used) * GB });

test("usedGap says nothing when the scan is within half a percent of used space, either way", () => {
  assert.equal(usedGap(drive(500), 495 * GB, errors({ denied: 3 })), null);
  assert.equal(usedGap(drive(500), 505 * GB, errors({ denied: 3 })), null);
  assert.notEqual(usedGap(drive(500), 494 * GB, errors({})), null);
  assert.notEqual(usedGap(drive(500), 506 * GB, errors({})), null);
});

test("usedGap calls the gap not readable only when reads failed, and counts them the way the status bar does", () => {
  const gap = usedGap(drive(500), 480 * GB, errors({ denied: 2, metadata: 1, vanished: 5, cloud_size: 4 }))!;
  assert.equal(gap.label, "Not readable");
  assert.equal(gap.bytes, 20 * GB);
  assert.ok(gap.swatch);
  assert.match(gap.detail, /3 items couldn't be read/);
  assert.match(gap.detail, /file system reserves/, "the reserved space it also holds is named, not hidden");
  assert.match(usedGap(drive(500), 480 * GB, errors({ open_dir: 1 }))!.detail, /1 item couldn't be read/);
});

test("usedGap does not blame unreadable folders for the gap after a scan that read everything", () => {
  const gap = usedGap(drive(500), 480 * GB, errors({ vanished: 2, cloud_size: 1 }))!;
  assert.equal(gap.label, "Not in files");
  assert.ok(gap.swatch);
  assert.match(gap.detail, /Every folder was read/);
  assert.doesNotMatch(gap.detail, /couldn't be read/);
});

test("usedGap makes no claim about reads for preview data saved without errors", () => {
  const gap = usedGap(drive(500), 480 * GB, undefined)!;
  assert.equal(gap.label, "Not in files");
  assert.doesNotMatch(gap.detail, /Every folder was read/);
});

test("usedGap reports a scan that adds up to more than is used instead of hiding it", () => {
  const gap = usedGap(drive(500), 530 * GB, errors({}))!;
  assert.equal(gap.label, "Overcounted");
  assert.equal(gap.bytes, 30 * GB);
  assert.equal(gap.swatch, false, "an overcount is not an area of the drive");
  assert.match(gap.detail, /30\.0 GB more than Windows reports as used/);
  assert.match(gap.detail, /Hard links are counted once per link/);
  assert.doesNotMatch(gap.detail, /couldn't be read/);
  assert.match(usedGap(drive(500), 530 * GB, errors({ denied: 271 }))!.detail, /271 items couldn't be read, and their size can't be shown/);
});

const links = (r: Partial<LinkReport>): LinkReport => ({ checked: true, files: 0, links: 0, bytes: 0, unchecked: 0, ...r });

test("usedGap stops blaming hard links once every file was checked for them", () => {
  const over = usedGap(drive(500), 530 * GB, errors({}), links({ files: 10, links: 12, bytes: 5 * GB }))!;
  assert.equal(over.label, "Overcounted");
  assert.doesNotMatch(over.detail, /counted once per link/);
  assert.match(over.detail, /already counted once/);
  assert.match(over.detail, /compressed/, "names what is left to explain it");
  const unread = usedGap(drive(500), 480 * GB, errors({ denied: 2 }), links({}))!;
  assert.doesNotMatch(unread.detail, /hard links/);
  // A scan that skipped some files, or never looked, keeps the old explanation.
  assert.match(usedGap(drive(500), 530 * GB, errors({}), links({ unchecked: 3 }))!.detail, /counted once per link/);
  assert.match(usedGap(drive(500), 530 * GB, errors({}), links({ checked: false }))!.detail, /counted once per link/);
  assert.match(usedGap(drive(500), 480 * GB, errors({ denied: 2 }), undefined)!.detail, /hard links counted more than once/);
});

test("linkNotes says how much counting hard links once saved, and the rule it used", () => {
  const [note] = linkNotes(links({ files: 61234, links: 64410, bytes: 5.4 * GB }));
  assert.equal(note.text, "Hard links counted once: 5.40 GB");
  assert.equal(note.warn, false);
  assert.match(note.detail, /61,234 files have more than one name/);
  assert.match(note.detail, /Their other 64,410 names would have added 5\.40 GB/);
  assert.match(note.detail, /counts in System32/);
  const [one] = linkNotes(links({ files: 1, links: 1, bytes: 2048 }));
  assert.match(one.detail, /^1 file has more than one name.*Its other name would have added 2\.00 KB\.$/);
});

test("linkNotes says nothing when there is nothing to say, and warns about files it could not check", () => {
  assert.deepEqual(linkNotes(undefined), []);
  assert.deepEqual(linkNotes(links({})), []);
  assert.deepEqual(linkNotes(links({ checked: false, skipped_because: "network drive" })), []);
  const notes = linkNotes(links({ unchecked: 1 }));
  assert.deepEqual(
    notes.map((n) => [n.text, n.warn]),
    [["1 file not checked for hard links", true]],
  );
});

test("linkedNote explains bytes counted in another folder, and only when there are some", () => {
  assert.equal(linkedNote({}), null);
  assert.equal(linkedNote({ linked: 0 }), null);
  const note = linkedNote({ linked: 5 * GB })!;
  assert.equal(note.text, "5.00 GB in hard links counted elsewhere");
  assert.match(note.detail, /counted there, not in this total/);
});

test("foundLine says how many matched and when only the largest are listed", () => {
  const found = (n: number) => Array.from({ length: n }, () => ({}) as never);
  assert.equal(foundLine({ query: "vhdx", found: [], total: 0 }), "Nothing matches \u201cvhdx\u201d");
  assert.equal(foundLine({ query: "vhdx", found: found(1), total: 1 }), "1 match, largest first");
  assert.equal(foundLine({ query: "log", found: found(12), total: 12 }), "12 matches, largest first");
  assert.equal(foundLine({ query: "log", found: found(200), total: 4100 }), `${(4100).toLocaleString()} matches, showing the largest 200`);
});

test("claimNote says which rule put a folder under its app", () => {
  assert.equal(claimNote({ rule: "install", app: "Docker Desktop", from: "install_location" }).text, "Why here: its install folder");
  assert.match(claimNote({ rule: "install", app: "Spotify", from: "display_icon" }).detail, /no install folder for Spotify/);
  const one = claimNote({ rule: "publisher", publisher: "Docker Inc.", apps: ["Docker Desktop"] });
  assert.equal(one.text, "Why here: publisher folder of Docker Desktop");
  assert.match(one.detail, /Everything in this folder is counted with Docker Desktop/);
  const suite = claimNote({ rule: "publisher", publisher: "Adobe Inc.", apps: ["Photoshop", "Premiere Pro", "Illustrator"] });
  assert.equal(suite.text, "Why here: publisher folder of 3 apps");
  assert.match(suite.detail, /Photoshop, Premiere Pro and Illustrator side by side/);
  const deeper = claimNote({
    rule: "named",
    name: "Chrome",
    matched: { kind: "install_folder", name: "Chrome" },
    container: "AppData\\Local",
    inside: "Google",
  });
  assert.equal(deeper.text, "Why here: named like its install folder");
  assert.equal(
    deeper.detail,
    "\u201cChrome\u201d in AppData\\Local\\Google matches its install folder, \u201cChrome\u201d, and no other installed app (ignoring case, spaces and punctuation). " +
      "\u201cGoogle\u201d itself matches no single app, so the folders inside it were checked.",
  );
});

test("appClaims explains one folder directly and several folder by folder", () => {
  const install = { rule: "install", app: "Docker Desktop", from: "install_location" } as const;
  const named = { rule: "named", name: "Docker", matched: { kind: "app_name", name: "Docker Desktop" }, container: "AppData\\Local", inside: null } as const;
  assert.equal(appClaims({ locations: [{ path: "C:\\Docker", size: 1, claim: install }] })?.text, "Why here: its install folder");
  const both = appClaims({
    locations: [
      { path: "C:\\Program Files\\Docker", size: 2, claim: install },
      { path: "C:\\Users\\me\\AppData\\Local\\Docker", size: 1, claim: named },
    ],
  });
  assert.equal(both?.text, "Why these 2 folders");
  assert.equal(both?.detail.split("\n\n").length, 2);
  assert.ok(both?.detail.startsWith("C:\\Program Files\\Docker\nWindows' list"));
  // Preview data saved before reasons existed has none to show.
  assert.equal(appClaims({ locations: [{ path: "C:\\Docker", size: 1 }] }), null);
});
