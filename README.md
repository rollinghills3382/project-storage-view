# Storage View

A desktop application for visualizing what is taking up space on your computer.

Pick a drive, and Storage View scans it and draws every application and folder as a
shape whose **visual size is proportional to its size on disk** — so the biggest space
consumers are obvious at a glance. Click into any block to drill down further.

## Goals

- List the drives on the machine with used / free space.
- Scan a selected drive and group usage by installed application (plus folders that
  don't belong to an app, such as user files and system data).
- Visualize the results so area on screen maps directly to bytes on disk.
- Drill down: click an application or folder to see what is inside it.
- Stay fast on large drives (incremental results while scanning).

## Design decisions

- **Platform:** Windows only.
- **Visualization:** squarified treemap. Area is proportional to bytes on disk; top-level
  blocks show one level of children inside them, and clicking a block zooms in.
- **Grouping:** files are grouped by the application that owns them, including data outside
  its install folder (for example Docker's WSL disk in `AppData`, or Adobe's media cache).
  The detail panel lists every location an app occupies.

## Status

Design phase. See [`mockups/`](mockups/) for the current UI mockup.

## Repository layout

```
mockups/   Static HTML mockups of the proposed UI
```
