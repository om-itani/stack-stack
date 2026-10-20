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

// ---------------------------------------------------------------------------
// Declarative element building for the form-heavy pages
// ---------------------------------------------------------------------------

type AttrValue = string | number | boolean | EventListener | null | undefined;

/**
 * `attrs` keys: `class`, `text`, `value`, `checked`, `disabled`, any other
 * attribute verbatim, and `on<event>` for listeners.
 */
export function el(
  tag: string,
  attrs: Record<string, AttrValue> = {},
  children: (Node | string)[] = [],
): HTMLElement {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === null || value === undefined) {
      continue;
    }
    if (key === "class") {
      node.className = String(value);
    } else if (key === "text") {
      node.textContent = String(value);
    } else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2), value as EventListener);
    } else if (key === "checked") {
      (node as HTMLInputElement).checked = Boolean(value);
    } else if (key === "disabled") {
      (node as HTMLButtonElement).disabled = Boolean(value);
    } else if (key === "value") {
      (node as HTMLInputElement).value = String(value);
    } else {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of children) {
    node.append(child);
  }
  return node;
}

const INPUT_CLASS =
  "border border-[#1e2733] bg-[#0b0f14] px-1.5 py-0.5 text-slate-200 outline-none focus:border-slate-500";

export function textInput(
  value: string,
  onInput: (value: string) => void,
  options: { class?: string; placeholder?: string; type?: string } = {},
): HTMLInputElement {
  const input = document.createElement("input");
  input.type = options.type ?? "text";
  input.value = value;
  input.className = `${INPUT_CLASS} ${options.class ?? ""}`.trim();
  if (options.placeholder) {
    input.placeholder = options.placeholder;
  }
  input.addEventListener("input", () => onInput(input.value));
  return input;
}

export function numberInput(
  value: number,
  onInput: (value: number) => void,
  options: { class?: string; min?: number; max?: number } = {},
): HTMLInputElement {
  const input = textInput(String(value), (raw) => {
    // An empty or partial field is normal while typing; treat it as 0 rather
    // than propagating NaN into the config.
    const parsed = Number.parseInt(raw, 10);
    onInput(Number.isFinite(parsed) ? parsed : 0);
  }, { ...options, type: "number" });
  if (options.min !== undefined) {
    input.min = String(options.min);
  }
  if (options.max !== undefined) {
    input.max = String(options.max);
  }
  return input;
}

export function checkbox(
  checked: boolean,
  onChange: (value: boolean) => void,
  label = "",
): HTMLLabelElement {
  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = checked;
  box.addEventListener("change", () => onChange(box.checked));
  const wrapper = document.createElement("label");
  wrapper.className = "inline-flex items-center gap-1 text-[11px] text-slate-400";
  wrapper.append(box);
  if (label) {
    wrapper.append(label);
  }
  return wrapper;
}

export function select(
  value: string,
  options: [value: string, label: string][],
  onChange: (value: string) => void,
): HTMLSelectElement {
  const node = document.createElement("select");
  node.className = INPUT_CLASS;
  for (const [optionValue, label] of options) {
    const option = document.createElement("option");
    option.value = optionValue;
    option.textContent = label;
    node.append(option);
  }
  node.value = value;
  node.addEventListener("change", () => onChange(node.value));
  return node;
}

/** A labelled row in a form grid. */
export function field(label: string, control: Node): HTMLElement {
  const wrapper = document.createElement("label");
  wrapper.className = "flex flex-col gap-0.5 text-[10px] uppercase tracking-wide text-slate-600";
  const caption = document.createElement("span");
  caption.textContent = label;
  wrapper.append(caption, control);
  return wrapper;
}

/** Comma-separated list editing, used for ports, upstreams and DNS servers. */
export function listInput(
  values: string[],
  onChange: (values: string[]) => void,
  options: { placeholder?: string; numeric?: boolean; class?: string } = {},
): HTMLInputElement {
  return textInput(
    values.join(", "),
    (raw) => {
      const parts = raw
        .split(",")
        .map((part) => part.trim())
        .filter((part) => part.length > 0);
      onChange(
        options.numeric
          ? parts.map((part) => String(Number.parseInt(part, 10) || 0))
          : parts,
      );
    },
    { ...options, placeholder: options.placeholder ?? "comma separated" },
  );
}

export function button(
  label: string,
  className = "",
  onClick?: () => void,
): HTMLButtonElement {
  const node = document.createElement("button");
  node.type = "button";
  node.textContent = label;
  node.className = `border border-[#1e2733] px-2 py-0.5 text-slate-300 transition-colors hover:border-slate-500 disabled:cursor-not-allowed disabled:opacity-40 ${className}`.trim();
  if (onClick) {
    node.addEventListener("click", onClick);
  }
  return node;
}

/** A table of editable rows with a per-row remove button. */
export interface Column<T> {
  header: string;
  cell: (row: T, index: number) => Node;
}

export function tableEditor<T>(
  rows: T[],
  columns: Column<T>[],
  factory: () => T,
  rerender: () => void,
  addLabel: string,
): HTMLElement {
  const wrapper = document.createElement("div");
  wrapper.className = "flex flex-col gap-1 overflow-x-auto";

  const table = document.createElement("table");
  table.className = "w-full text-[11px]";

  const head = document.createElement("thead");
  const headRow = document.createElement("tr");
  for (const column of columns) {
    headRow.append(
      el("th", {
        class:
          "border-b border-[#1e2733] px-2 py-1 text-left text-[10px] uppercase tracking-wide text-slate-600",
        text: column.header,
      }),
    );
  }
  headRow.append(el("th", { class: "border-b border-[#1e2733]" }));
  head.append(headRow);

  const body = document.createElement("tbody");
  rows.forEach((row, index) => {
    const tr = document.createElement("tr");
    for (const column of columns) {
      tr.append(
        el("td", { class: "border-b border-[#161d27] px-2 py-1 align-middle" }, [
          column.cell(row, index),
        ]),
      );
    }
    tr.append(
      el("td", { class: "border-b border-[#161d27] px-2 py-1 text-right" }, [
        button("remove", "text-rose-400 hover:border-rose-500", () => {
          rows.splice(index, 1);
          rerender();
        }),
      ]),
    );
    body.append(tr);
  });

  table.append(head, body);
  wrapper.append(table, button(addLabel, "", () => {
    rows.push(factory());
    rerender();
  }));
  return wrapper;
}
