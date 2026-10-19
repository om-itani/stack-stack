/** Bytes per second -> sparklines on a 100x28 viewBox, scaled to fit. */
const VIEW_W = 100;
const VIEW_H = 28;

export interface SparkOptions {
  /** Fixed upper bound (percentages); otherwise the window max is used. */
  max?: number;
  color?: string;
}

export class Sparkline {
  private readonly values: number[] = [];
  private readonly svg: SVGSVGElement;
  private readonly line: SVGPolylineElement;
  private readonly area: SVGPolygonElement;

  constructor(
    private readonly capacity = 60,
    private readonly options: SparkOptions = {},
  ) {
    const ns = "http://www.w3.org/2000/svg";
    const color = options.color ?? "#34d399";

    this.svg = document.createElementNS(ns, "svg");
    this.svg.setAttribute("viewBox", `0 0 ${VIEW_W} ${VIEW_H}`);
    // "none" lets the graph stretch to whatever the panel width is.
    this.svg.setAttribute("preserveAspectRatio", "none");
    this.svg.setAttribute("class", "h-7 w-full");

    this.area = document.createElementNS(ns, "polygon");
    this.area.setAttribute("fill", color);
    this.area.setAttribute("fill-opacity", "0.12");

    this.line = document.createElementNS(ns, "polyline");
    this.line.setAttribute("fill", "none");
    this.line.setAttribute("stroke", color);
    this.line.setAttribute("stroke-width", "1.5");
    // Without this the 100x28 viewBox would scale the stroke width too.
    this.line.setAttribute("vector-effect", "non-scaling-stroke");

    this.svg.append(this.area, this.line);
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

  private draw(): void {
    const count = this.values.length;
    if (count === 0) {
      return;
    }

    const max = this.options.max ?? Math.max(1, ...this.values);
    const stepX = count > 1 ? VIEW_W / (count - 1) : VIEW_W;

    const points: string[] = [];
    for (let i = 0; i < count; i++) {
      const x = i * stepX;
      const y = VIEW_H - Math.min(1, Math.max(0, this.values[i] / max)) * VIEW_H;
      points.push(`${x.toFixed(2)},${y.toFixed(2)}`);
    }

    const lastX = (count > 1 ? (count - 1) * stepX : VIEW_W).toFixed(2);
    this.line.setAttribute("points", points.join(" "));
    this.area.setAttribute("points", `0,${VIEW_H} ${points.join(" ")} ${lastX},${VIEW_H}`);
  }
}
