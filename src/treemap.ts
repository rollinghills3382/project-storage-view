import { hierarchy, treemap, treemapSquarify, type HierarchyRectangularNode } from "d3-hierarchy";
import { catColor, fmtBytes, h, type ViewNode } from "./ui";

/** Height of the name strip on a block that shows its contents. */
const HEAD = 16;

export interface TreemapEvents {
  /** `chain` lists the blocks the tile sits inside, outermost first. */
  select(node: ViewNode, chain: ViewNode[]): void;
  open(node: ViewNode, chain: ViewNode[]): void;
  hover(node: ViewNode | null, e?: MouseEvent): void;
}

type Rect = HierarchyRectangularNode<ViewNode>;

export const canOpen = (n: ViewNode) => n.has_children && n.kind !== "more";

export function createTreemap(on: TreemapEvents) {
  const el = h("div", { class: "map", role: "img", "aria-label": "Disk usage treemap. Tile area is proportional to size on disk." });
  const tiles = new WeakMap<Element, Rect>();
  const hit = (e: Event) => {
    const tile = (e.target as Element).closest(".tile");
    return tile ? tiles.get(tile) : undefined;
  };
  const chain = (d: Rect) => d.ancestors().slice(1, -1).reverse().map((a) => a.data);

  el.addEventListener("click", (e) => {
    const d = hit(e);
    if (!d) return;
    if (e.detail >= 2) on.open(d.data, chain(d));
    else on.select(d.data, chain(d));
  });
  el.addEventListener("mousemove", (e) => on.hover(hit(e)?.data ?? null, e));
  el.addEventListener("mouseleave", () => on.hover(null));

  /**
   * Draws `focus` with every nested level that fits. `marked` holds the ids of the selected
   * item and the folders around it; the deepest one that is drawn gets the outline.
   */
  function draw(focus: ViewNode, marked: Set<string>) {
    const width = el.clientWidth;
    const height = el.clientHeight;
    el.replaceChildren();
    if (!focus.children?.length || width < 20 || height < 20) return;

    const root = hierarchy<ViewNode>(focus, (n) => n.children ?? undefined)
      .sum((n) => (n.children?.length ? 0 : n.size))
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));

    // A block shows its contents when it has room for a name strip and readable tiles.
    const groups = new Map<Rect, boolean>();
    const isGroup = (d: Rect): boolean => {
      let group = groups.get(d);
      if (group === undefined) {
        group = !!d.children && d.depth > 0 && (d.depth === 1 || isGroup(d.parent!)) && d.x1 - d.x0 > 64 && d.y1 - d.y0 > HEAD + 20;
        groups.set(d, group);
      }
      return group;
    };
    const edge = (d: Rect) => (isGroup(d) ? 1 : 0);
    const laidOut = treemap<ViewNode>()
      .size([width, height])
      .tile(treemapSquarify.ratio(1.3))
      .round(true)
      .paddingInner((d) => (d.depth === 0 || isGroup(d) ? 1 : 0))
      .paddingTop((d) => (isGroup(d) ? HEAD : 0))
      .paddingRight(edge)
      .paddingBottom(edge)
      .paddingLeft(edge)(root);

    const place = (d: Rect) => `left:${d.x0}px;top:${d.y0}px;width:${d.x1 - d.x0}px;height:${d.y1 - d.y0}px`;
    const out = document.createDocumentFragment();
    const drawnMarks: Rect[] = [];
    const visit = (d: Rect) => {
      const w = d.x1 - d.x0;
      const hgt = d.y1 - d.y0;
      if (w < 1 || hgt < 1) return;
      const group = isGroup(d);
      const tile = h("div", { class: group ? "tile group" : "tile", style: `${place(d)};--c:${catColor(d.data.category)}` });
      tiles.set(tile, d);
      if (group) {
        tile.append(h("div", { class: "hd" }, h("span", {}, d.data.name), w > 120 && h("span", { class: "sz" }, fmtBytes(d.data.size))));
      } else if (w > 40 && hgt > 13) {
        tile.append(h("span", {}, d.data.name));
        if (hgt > 28) tile.append(h("span", { class: "sz" }, fmtBytes(d.data.size)));
      }
      out.append(tile);
      if (marked.has(d.data.id)) drawnMarks.push(d);
      if (group) d.children!.forEach(visit);
    };
    laidOut.children?.forEach(visit);
    const mark = drawnMarks[drawnMarks.length - 1];
    if (mark) out.append(h("div", { class: "mark", style: place(mark) }));
    el.append(out);
  }

  return { el, draw };
}
