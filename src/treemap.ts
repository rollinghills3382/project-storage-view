import { hierarchy, treemap, treemapSquarify, type HierarchyRectangularNode } from "d3-hierarchy";
import { catColor, fmtBytes, type ViewNode } from "./ui";

const NS = "http://www.w3.org/2000/svg";
const HEADER = 24;

export interface TreemapEvents {
  /** `parent` is set when a tile inside a top-level block was clicked. */
  open(node: ViewNode, parent: ViewNode | null): void;
  select(node: ViewNode): void;
  hover(node: ViewNode | null, e?: MouseEvent): void;
}

type Rect = HierarchyRectangularNode<ViewNode>;

function svg<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number>): SVGElementTagNameMap[K] {
  const el = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  return el;
}

function fit(text: string, width: number, charWidth: number): string {
  const n = Math.floor(width / charWidth);
  if (n < 3) return "";
  return text.length <= n ? text : text.slice(0, n - 1) + "…";
}

export const canOpen = (n: ViewNode) => n.has_children && n.kind !== "more";

/** Draws `focus` and one level of its children. Tile area is proportional to size on disk. */
export function drawTreemap(el: SVGSVGElement, focus: ViewNode, selectedId: string | null, width: number, height: number, on: TreemapEvents) {
  el.replaceChildren();
  el.setAttribute("viewBox", `0 0 ${width} ${height}`);
  if (!focus.children?.length) return;

  const root = hierarchy<ViewNode>(focus, (n) => n.children ?? undefined)
    .sum((n) => (n.children ? 0 : n.size))
    .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));
  const edge = (d: Rect) => (d.depth === 1 ? 3 : 0);
  const laidOut = treemap<ViewNode>()
    .size([width, height])
    .tile(treemapSquarify.ratio(1.3))
    .round(true)
    .paddingInner((d) => (d.depth === 0 ? 4 : 2))
    .paddingTop((d) => (d.depth === 1 ? HEADER : 0))
    .paddingRight(edge)
    .paddingBottom(edge)
    .paddingLeft(edge)(root);

  const tile = (d: Rect, parent: Rect | null, radius: number) => {
    const w = d.x1 - d.x0;
    const h = d.y1 - d.y0;
    const r = svg("rect", { x: d.x0, y: d.y0, width: w, height: h, rx: radius, class: "cell" + (d.data.id === selectedId ? " sel" : "") });
    r.style.fill = catColor(d.data.category);
    r.setAttribute("tabindex", "0");
    r.setAttribute("role", "button");
    r.setAttribute("aria-label", `${d.data.name}, ${fmtBytes(d.data.size)}`);
    const activate = () => (canOpen(d.data) ? on.open(d.data, parent?.data ?? null) : on.select(d.data));
    r.addEventListener("click", activate);
    r.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        activate();
      }
    });
    r.addEventListener("mouseenter", (e) => on.hover(d.data, e));
    r.addEventListener("mousemove", (e) => on.hover(d.data, e));
    r.addEventListener("mouseleave", () => on.hover(null));
    return r;
  };

  const label = (g: SVGGElement, x: number, y: number, name: string, size: string, maxWidth: number, small: boolean) => {
    const text = fit(name, maxWidth, small ? 6.4 : 7.2);
    if (!text) return null;
    const t = svg("text", { x, y, class: "lbl" + (small ? " sm" : "") });
    const n = svg("tspan", {});
    n.textContent = text;
    t.append(n);
    if (size) {
      const s = svg("tspan", { x, dy: small ? 13 : 15, class: "sz" });
      s.textContent = size;
      t.append(s);
    }
    g.append(t);
    return t;
  };

  for (const d of laidOut.children ?? []) {
    const w = d.x1 - d.x0;
    const h = d.y1 - d.y0;
    if (w < 1 || h < 1) continue;
    const g = svg("g", {});
    el.append(g);
    g.append(tile(d, null, 4));
    if (d.children && h > HEADER + 6) {
      g.append(svg("rect", { x: d.x0, y: d.y0, width: w, height: h, rx: 4, class: "shade" }));
      for (const c of d.children) {
        const cw = c.x1 - c.x0;
        const ch = c.y1 - c.y0;
        if (cw < 1 || ch < 1) continue;
        g.append(tile(c, d, 2));
        if (cw > 56 && ch > 36) label(g, c.x0 + 6, c.y0 + 15, c.data.name, fmtBytes(c.data.size), cw - 12, true);
        else if (cw > 56 && ch > 18) label(g, c.x0 + 6, c.y0 + 13, c.data.name, "", cw - 12, true);
      }
      const size = fmtBytes(d.data.size);
      const room = w > 150 ? w - size.length * 6.8 - 26 : w - 16;
      if (label(g, d.x0 + 8, d.y0 + 16, d.data.name, "", room, false) && w > 150) {
        const s = svg("text", { x: d.x1 - 8, y: d.y0 + 16, "text-anchor": "end", class: "lbl" });
        const t = svg("tspan", { class: "sz" });
        t.textContent = size;
        s.append(t);
        g.append(s);
      }
    } else if (w > 46 && h > 36) {
      label(g, d.x0 + 8, d.y0 + 18, d.data.name, fmtBytes(d.data.size), w - 16, false);
    }
  }
}
