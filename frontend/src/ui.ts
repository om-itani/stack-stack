/** Small DOM + formatting helpers shared by the pages. */

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

export function bytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  let scaled = value;
  let unit = 0;
  while (scaled >= 1024 && unit < BYTE_UNITS.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  const decimals = unit > 0 && scaled < 10 ? 1 : 0;
  return `${scaled.toFixed(decimals)} ${BYTE_UNITS[unit]}`;
}

export function rate(value: number): string {
  return `${bytes(value)}/s`;
}

export function percent(value: number): string {
  return `${value.toFixed(value < 10 ? 1 : 0)}%`;
}

export function uptime(seconds: number): string {
  if (!seconds) {
    return "—";
  }
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) {
    return `${days}d ${hours}h ${minutes}m`;
  }
  return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
}

/** Paint a usage bar and recolour it as it approaches saturation. */
export function fill(bar: HTMLElement, value: number): void {
  const clamped = Math.min(100, Math.max(0, value));
  bar.style.width = `${clamped}%`;
  bar.className =
    clamped >= 90
      ? "h-full bg-rose-500"
      : clamped >= 75
        ? "h-full bg-amber-400"
        : "h-full bg-emerald-500";
}

/** Query that throws on a miss; a missing element is a programming error. */
export function must<T extends Element>(selector: string, root: ParentNode = document): T {
  const found = root.querySelector<T>(selector);
  if (!found) {
    throw new Error(`missing element ${selector}`);
  }
  return found;
}

export function emptyNote(text: string): HTMLElement {
  const note = document.createElement("div");
  note.className = "px-3 py-3 text-slate-600";
  note.textContent = text;
  return note;
}

/** A titled panel; the body is returned for the caller to fill. */
export function panel(title: string, extraClass = ""): { root: HTMLElement; body: HTMLElement } {
  const root = document.createElement("section");
  root.className = `panel ${extraClass}`.trim();
  const head = document.createElement("div");
  head.className = "panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]";
  head.textContent = title;
  const body = document.createElement("div");
  root.append(head, body);
  return { root, body };
}

export function button(label: string, className = ""): HTMLButtonElement {
  const el = document.createElement("button");
  el.type = "button";
  el.textContent = label;
  el.className = `border border-[#1e2733] px-2 py-0.5 text-slate-300 transition-colors hover:border-slate-500 disabled:cursor-not-allowed disabled:opacity-40 ${className}`.trim();
  return el;
}
