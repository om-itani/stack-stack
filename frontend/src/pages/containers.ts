/**
 * Containers page: one card per Docker container with lifecycle actions, log
 * and terminal viewers, and "Open Web UI" links for published ports.
 *
 * The list refreshes only while the page is visible, and never while an action
 * is in flight, so it does not fight the user or hammer a slow daemon.
 */

import { api } from "../api";
import type { Container } from "../api";
import { openLogs, openTerminal } from "../terminal";
import { button, emptyNote } from "../ui";

const REFRESH_MS = 5000;

const STATE_STYLES: Record<string, string> = {
  running: "text-emerald-400",
  paused: "text-amber-400",
  restarting: "text-amber-400",
  created: "text-sky-400",
  exited: "text-slate-500",
  dead: "text-rose-500",
  removing: "text-rose-500",
  stopping: "text-amber-400",
};

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

export function createContainersPage(): PageHandle {
  const root = document.createElement("div");
  root.className = "flex min-h-0 flex-col gap-3 p-3";

  const bar = document.createElement("div");
  bar.className = "flex flex-wrap items-center gap-3";
  const heading = document.createElement("span");
  heading.className = "text-slate-300";
  heading.textContent = "containers";
  const count = document.createElement("span");
  count.className = "tabular text-[11px] text-slate-600";
  const reload = button("refresh");
  const status = document.createElement("span");
  status.className = "tabular truncate text-[11px] text-slate-500";
  bar.append(heading, count, reload, status);

  const grid = document.createElement("div");
  grid.className = "grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3";

  root.append(bar, grid);

  let busy = false;

  function setStatus(text: string, tone: "muted" | "error" | "ok"): void {
    status.textContent = text;
    status.className =
      tone === "error"
        ? "tabular truncate text-[11px] text-rose-400"
        : tone === "ok"
          ? "tabular truncate text-[11px] text-emerald-400"
          : "tabular truncate text-[11px] text-slate-500";
  }

  function errorText(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }

  async function load(): Promise<void> {
    if (busy) {
      return;
    }
    setStatus("loading…", "muted");
    try {
      const containers = await api.containers();
      count.textContent = `${containers.length} total`;
      render(containers);
      setStatus("", "muted");
    } catch (err) {
      setStatus(errorText(err), "error");
    }
  }

  async function run(action: () => Promise<unknown>, note: string): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    setStatus(note, "muted");
    try {
      await action();
      setStatus(note.replace("…", " done"), "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
    }
    await load();
  }

  function render(containers: Container[]): void {
    grid.replaceChildren();
    if (containers.length === 0) {
      grid.append(
        emptyNote(
          "no containers. Docker may not be running, or no containers are deployed yet.",
        ),
      );
      return;
    }
    for (const container of containers) {
      grid.append(card(container));
    }
  }

  function card(container: Container): HTMLElement {
    const el = document.createElement("section");
    el.className = "panel flex flex-col";

    const head = document.createElement("div");
    head.className = "flex items-baseline justify-between gap-3 border-b border-[#1e2733] px-3 py-1.5";
    const name = document.createElement("span");
    name.className = "truncate font-semibold text-slate-200";
    name.textContent = container.name || container.id.slice(0, 12);
    const state = document.createElement("span");
    state.className = `tabular text-[10px] uppercase ${STATE_STYLES[container.state] ?? "text-slate-500"}`;
    state.textContent = container.state;
    head.append(name, state);

    const meta = document.createElement("div");
    meta.className = "tabular flex flex-wrap items-baseline gap-x-3 px-3 py-1 text-[10px] text-slate-600";
    const image = document.createElement("span");
    image.className = "truncate";
    image.textContent = container.image || "—";
    const source = document.createElement("span");
    source.className = container.source === "app-store" ? "text-emerald-600" : "text-slate-600";
    source.textContent = container.source === "app-store" ? `app:${container.app_id ?? "?"}` : "manual";
    meta.append(image, source);

    const body = document.createElement("div");
    body.className = "flex-1 px-3 py-2 text-[11px] text-slate-500";
    const statusLine = document.createElement("div");
    statusLine.className = "truncate";
    statusLine.textContent = container.status || container.id.slice(0, 12);
    body.append(statusLine);

    if (container.ports.length > 0) {
      const ports = document.createElement("div");
      ports.className = "tabular mt-1 flex flex-wrap gap-x-3 text-[10px] text-slate-600";
      for (const port of container.ports) {
        const span = document.createElement("span");
        span.textContent = port.public
          ? `${port.public}→${port.private}/${port.protocol}`
          : `${port.private}/${port.protocol}`;
        ports.append(span);
      }
      body.append(ports);
    }

    const links = document.createElement("div");
    links.className = "flex flex-wrap gap-2 px-3 pb-1";
    for (const url of container.web_urls.slice(0, 4)) {
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.target = "_blank";
      anchor.rel = "noreferrer";
      anchor.textContent = `open :${linkPort(url)}`;
      anchor.className =
        "border border-emerald-800 px-2 py-0.5 text-emerald-400 hover:border-emerald-500";
      links.append(anchor);
    }

    const actions = document.createElement("div");
    actions.className = "flex flex-wrap gap-2 border-t border-[#161d27] px-3 py-2";

    const running = container.state === "running";
    const startBtn = button("start");
    startBtn.disabled = running || busy;
    startBtn.addEventListener("click", () =>
      void run(() => api.start(container.id), `starting ${container.name}…`),
    );

    const stopBtn = button("stop");
    stopBtn.disabled = !running || busy;
    stopBtn.addEventListener("click", () =>
      void run(() => api.stop(container.id), `stopping ${container.name}…`),
    );

    const restartBtn = button("restart");
    restartBtn.disabled = busy;
    restartBtn.addEventListener("click", () =>
      void run(() => api.restart(container.id), `restarting ${container.name}…`),
    );

    const logsBtn = button("logs");
    logsBtn.addEventListener("click", () => openLogs(container.id, container.name));

    const termBtn = button("terminal");
    termBtn.disabled = !running || busy;
    termBtn.addEventListener("click", () => openTerminal(container.id, container.name));

    const deleteBtn = button("delete", "text-rose-400 hover:border-rose-500");
    deleteBtn.disabled = busy;
    deleteBtn.addEventListener("click", () => {
      if (window.confirm(`Delete container ${container.name}? This cannot be undone.`)) {
        void run(() => api.remove(container.id), `deleting ${container.name}…`);
      }
    });

    actions.append(startBtn, stopBtn, restartBtn, logsBtn, termBtn, deleteBtn);

    el.append(head, meta, body, links, actions);
    return el;
  }

  reload.addEventListener("click", () => void load());

  // The page lives as long as the tab, so the interval needs no teardown. The
  // shell hides inactive pages with inline `display`, so that is the visibility
  // signal here too.
  window.setInterval(() => {
    if (root.style.display !== "none" && !busy) {
      void load();
    }
  }, REFRESH_MS);

  // The shell calls `onShow` as soon as the page is first displayed, so the
  // initial load happens there rather than eagerly here.
  return {
    root,
    onShow: () => void load(),
  };
}

function linkPort(url: string): string {
  const parts = url.split(":");
  return parts.length > 0 ? (parts[parts.length - 1] ?? "") : "";
}
