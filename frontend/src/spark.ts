/**
 * Sparklines on a 100x28 viewBox, scaled to fit.
 *
 * The line is drawn as a Catmull-Rom spline (emitted as cubic beziers) rather
 * than a polyline, so a noisy series reads as a smooth curve instead of a
 * staircase. The fill under it is a vertical gradient that fades to transparent.
 */
const VIEW_W = 100;
const VIEW_H = 28;

export interface SparkOptions {
  /** Fixed upper bound (percentages); otherwise the window max is used. */
  max?: number;
  color?: string;
}

interface Point {
  x: number;
  y: number;
}

export class Sparkline {
  private readonly values: number[] = [];
  private readonly svg: SVGSVGElement;
  private readonly line: SVGPathElement;
  private readonly area: SVGPathElement;

  constructor(
    private readonly capacity = 60,
    private readonly options: SparkOptions = {},
  ) {
    const ns = "http://www.w3.org/2000/svg";
    const color = options.color ?? "#34d399";
    // Each instance needs its own gradient id or they would share the first one.
    const gradientId = `spark-${Math.random().toString(36).slice(2, 9)}`;

    this.svg = document.createElementNS(ns, "svg");
    this.svg.setAttribute("viewBox", `0 0 ${VIEW_W} ${VIEW_H}`);
    // "none" lets the graph stretch to whatever the panel width is.
    this.svg.setAttribute("preserveAspectRatio", "none");
    this.svg.setAttribute("class", "h-7 w-full");

    const defs = document.createElementNS(ns, "defs");
    const gradient = document.createElementNS(ns, "linearGradient");
    gradient.setAttribute("id", gradientId);
    gradient.setAttribute("x1", "0");
    gradient.setAttribute("y1", "0");
    gradient.setAttribute("x2", "0");
    gradient.setAttribute("y2", "1");
    const top = document.createElementNS(ns, "stop");
    top.setAttribute("offset", "0%");
    top.setAttribute("stop-color", color);
    top.setAttribute("stop-opacity", "0.38");
    const bottom = document.createElementNS(ns, "stop");
    bottom.setAttribute("offset", "100%");
    bottom.setAttribute("stop-color", color);
    bottom.setAttribute("stop-opacity", "0");
    gradient.append(top, bottom);
    defs.append(gradient);

    this.area = document.createElementNS(ns, "path");
    this.area.setAttribute("fill", `url(#${gradientId})`);
    this.area.setAttribute("stroke", "none");

    this.line = document.createElementNS(ns, "path");
    this.line.setAttribute("fill", "none");
    this.line.setAttribute("stroke", color);
    this.line.setAttribute("stroke-width", "1.5");
    this.line.setAttribute("stroke-linejoin", "round");
    this.line.setAttribute("stroke-linecap", "round");
    // Without this the 100x28 viewBox would scale the stroke width too.
    this.line.setAttribute("vector-effect", "non-scaling-stroke");

    this.svg.append(defs, this.area, this.line);
  }

  get element(): SVGSVGElement {
    return this.svg;
  }

  push(value: number): void {
    this.values.push(Number.isFinite(value) ? value : 0);
    if (this.values.length > this.capacity) {
      this.values.shift();
    }
    this.draw();
  }

  /** Replace the whole series at once, for a range fetched from the server. */
  set(values: number[]): void {
    this.values.length = 0;
    for (const value of values.slice(-this.capacity)) {
      this.values.push(Number.isFinite(value) ? value : 0);
    }
    this.draw();
  }

  /** Drop the series, e.g. when returning to the live view. */
  clear(): void {
    this.values.length = 0;
    this.line.removeAttribute("d");
    this.area.removeAttribute("d");
  }

  private draw(): void {
    const count = this.values.length;
    if (count === 0) {
      return;
    }

    const max = this.options.max ?? Math.max(1, ...this.values);
    const stepX = count > 1 ? VIEW_W / (count - 1) : 0;
    const points: Point[] = this.values.map((value, index) => ({
      x: count > 1 ? index * stepX : VIEW_W / 2,
      y: VIEW_H - Math.min(1, Math.max(0, value / max)) * VIEW_H,
    }));

    const curve = smoothPath(points);
    this.line.setAttribute("d", curve);

    const first = points[0];
    const last = points[points.length - 1];
    this.area.setAttribute(
      "d",
      `${curve} L ${last.x.toFixed(2)},${VIEW_H} L ${first.x.toFixed(2)},${VIEW_H} Z`,
    );
  }
}

/** Catmull-Rom through `points`, as cubic beziers with tension 1/6. */
function smoothPath(points: Point[]): string {
  if (points.length === 0) {
    return "";
  }
  if (points.length === 1) {
    return `M ${points[0].x.toFixed(2)},${points[0].y.toFixed(2)}`;
  }

  const clampY = (y: number): number => Math.min(VIEW_H, Math.max(0, y));
  const parts: string[] = [`M ${points[0].x.toFixed(2)},${points[0].y.toFixed(2)}`];

  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[i - 1] ?? points[i];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2] ?? p2;

    // Control points are clamped to the viewBox so a sharp step cannot make the
    // curve bulge outside the panel.
    const c1x = p1.x + (p2.x - p0.x) / 6;
    const c1y = clampY(p1.y + (p2.y - p0.y) / 6);
    const c2x = p2.x - (p3.x - p1.x) / 6;
    const c2y = clampY(p2.y - (p3.y - p1.y) / 6);

    parts.push(
      `C ${c1x.toFixed(2)},${c1y.toFixed(2)} ${c2x.toFixed(2)},${c2y.toFixed(2)} ` +
        `${p2.x.toFixed(2)},${p2.y.toFixed(2)}`,
    );
  }
  return parts.join(" ");
}
