/**
 * App store: searchable catalog of container templates with one-click install,
 * plus a Docker Hub search whose results deploy into the Containers stack editor.
 *
 * The catalog is fetched once per visit; filtering is purely client-side over a
 * handful of apps, so it stays instant without extra requests. Hub searches go
 * through the backend (which caches them) because the browser cannot reach
 * Docker Hub directly without hitting CORS and the unauthenticated rate limit.
 */

import { api, hubApi } from "../api";
import type { App, HubRepo } from "../api";
import { stageStackDraft } from "../stackdraft";
import { button, emptyNote } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

export function createAppsPage(): PageHandle {
  const root = document.createElement("div");
  root.className = "flex min-h-0 flex-col gap-3 p-3";

  const bar = document.createElement("div");
  bar.className = "flex flex-wrap items-center gap-3";

  const heading = document.createElement("span");
  heading.className = "text-slate-300";
  heading.textContent = "app store";

  const search = document.createElement("input");
  search.type = "search";
  search.placeholder = "search name, tag…";
  search.className =
    "w-48 border border-[#1e2733] bg-[#0b0f14] px-2 py-0.5 text-slate-200 outline-none focus:border-slate-500";

  const category = document.createElement("select");
  category.className =
    "border border-[#1e2733] bg-[#0b0f14] px-2 py-0.5 text-slate-200 outline-none focus:border-slate-500";

  const installedOnly = document.createElement("label");
  installedOnly.className = "flex items-center gap-1 text-[11px] text-slate-500";
  const installedBox = document.createElement("input");
  installedBox.type = "checkbox";
  installedOnly.append(installedBox, "installed only");

  const count = document.createElement("span");
  count.className = "tabular text-[11px] text-slate-600";

  const status = document.createElement("span");
  status.className = "tabular truncate text-[11px] text-slate-500";

  bar.append(heading, search, category, installedOnly, count, status);

  // Docker Hub search: separate from the local catalog because it hits the
  // network and returns a different shape.
  const hubPanel = document.createElement("section");
  hubPanel.className = "panel flex flex-col";
  const hubBar = document.createElement("div");
  hubBar.className = "flex flex-wrap items-center gap-2 border-b border-[#1e2733] px-3 py-1.5";
  const hubHeading = document.createElement("span");
  hubHeading.className = "panel-title text-[10px]";
  hubHeading.textContent = "docker hub";
  const hubInput = document.createElement("input");
  hubInput.type = "search";
  hubInput.placeholder = "search docker hub…";
  hubInput.className =
    "w-56 border border-[#1e2733] bg-[#0b0f14] px-2 py-0.5 text-slate-200 outline-none focus:border-slate-500";
  const hubBtn = button("search", "text-[10px]");
  const hubStatus = document.createElement("span");
  hubStatus.className = "tabular truncate text-[11px] text-slate-500";
  hubBar.append(hubHeading, hubInput, hubBtn, hubStatus);

  const hubResults = document.createElement("div");
  hubResults.className = "flex flex-col divide-y divide-[#161d27]";
  hubPanel.append(hubBar, hubResults);

  const grid = document.createElement("div");
  grid.className = "grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3";

  root.append(bar, hubPanel, grid);

  let apps: App[] = [];
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
      apps = await api.catalog();
      syncCategories();
      render();
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

  function setHubStatus(text: string, tone: "muted" | "error"): void {
    hubStatus.textContent = text;
    hubStatus.className =
      tone === "error"
        ? "tabular truncate text-[11px] text-rose-400"
        : "tabular truncate text-[11px] text-slate-500";
  }

  // Typing debounces; the button and Enter search immediately. An in-flight
  // request is aborted when a newer one starts, so a five-letter query does not
  // fire five upstream calls.
  let hubDebounce: number | null = null;
  let hubController: AbortController | null = null;

  function scheduleHubSearch(): void {
    if (hubDebounce !== null) {
      window.clearTimeout(hubDebounce);
    }
    hubDebounce = window.setTimeout(() => void searchHub(), 300);
  }

  async function searchHub(): Promise<void> {
    if (hubDebounce !== null) {
      window.clearTimeout(hubDebounce);
      hubDebounce = null;
    }
    hubController?.abort();
    const query = hubInput.value.trim();
    if (!query) {
      hubResults.replaceChildren();
      setHubStatus("", "muted");
      return;
    }

    const controller = new AbortController();
    hubController = controller;
    hubBtn.disabled = true;
    setHubStatus("searching…", "muted");
    try {
      const response = await hubApi.search(query, controller.signal);
      if (controller.signal.aborted) {
        return;
      }
      if (!response.available) {
        hubResults.replaceChildren(
          emptyNote("docker hub search is unavailable: curl was not found on the server"),
        );
        setHubStatus("unavailable", "muted");
        return;
      }
      renderHub(response.results);
      setHubStatus(
        `${response.results.length} result${response.results.length === 1 ? "" : "s"}`,
        "muted",
      );
    } catch (err) {
      // A superseded request is expected; only surface real failures.
      if (controller.signal.aborted || (err instanceof Error && err.name === "AbortError")) {
        return;
      }
      hubResults.replaceChildren(emptyNote(errorText(err)));
      setHubStatus(errorText(err), "error");
    } finally {
      if (hubController === controller) {
        hubController = null;
        hubBtn.disabled = false;
      }
    }
  }

  function renderHub(repos: HubRepo[]): void {
    hubResults.replaceChildren();
    if (repos.length === 0) {
      hubResults.append(emptyNote("no repositories matched"));
      return;
    }
    for (const repo of repos) {
      hubResults.append(hubRow(repo));
    }
  }

  function hubRow(repo: HubRepo): HTMLElement {
    const row = document.createElement("div");
    row.className = "flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-1.5 text-[11px]";

    const name = document.createElement("span");
    name.className = "font-semibold text-slate-200";
    name.textContent = repo.name;

    const official = document.createElement("span");
    if (repo.is_official) {
      official.className = "border border-emerald-800 px-1 text-[9px] text-emerald-400";
      official.textContent = "official";
    }

    const stats = document.createElement("span");
    stats.className = "tabular text-[10px] text-slate-600";
    stats.textContent = `★ ${compact(repo.star_count)} · ${compact(repo.pull_count)} pulls`;

    const description = document.createElement("span");
    description.className = "w-full truncate text-slate-500";
    description.textContent = repo.description;

    const deploy = button("deploy", "ml-auto text-[10px] text-emerald-400 hover:border-emerald-500");
    deploy.addEventListener("click", () => {
      // Stage the image and hop to the Containers page, whose stack editor
      // picks the draft up pre-filled.
      stageStackDraft({ image: repo.name });
      location.hash = "#/containers";
    });

    row.append(name, official, stats, deploy, description);
    return row;
  }

  hubBtn.addEventListener("click", () => void searchHub());
  hubInput.addEventListener("input", scheduleHubSearch);
  hubInput.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      void searchHub();
    }
  });

  function syncCategories(): void {
    const current = category.value;
    const categories = Array.from(new Set(apps.map((app) => app.category).filter(Boolean))).sort();
    category.replaceChildren();
    const all = document.createElement("option");
    all.value = "";
    all.textContent = "all categories";
    category.append(all);
    for (const name of categories) {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name;
      category.append(option);
    }
    category.value = categories.includes(current) ? current : "";
  }

  function visible(): App[] {
    const query = search.value.trim().toLowerCase();
    const wanted = category.value;
    return apps.filter((app) => {
      if (installedBox.checked && !app.installed) {
        return false;
      }
      if (wanted && app.category !== wanted) {
        return false;
      }
      if (!query) {
        return true;
      }
      const haystack = `${app.name} ${app.id} ${app.description} ${app.tags.join(" ")}`.toLowerCase();
      return haystack.includes(query);
    });
  }

  function render(): void {
    const list = visible();
    count.textContent = `${list.length}/${apps.length}`;
    grid.replaceChildren();
    if (list.length === 0) {
      grid.append(emptyNote("no apps match the current filter"));
      return;
    }
    for (const app of list) {
      grid.append(card(app));
    }
  }

  function card(app: App): HTMLElement {
    const el = document.createElement("section");
    el.className = "panel flex flex-col";

    const head = document.createElement("div");
    head.className = "flex items-baseline gap-2 border-b border-[#1e2733] px-3 py-1.5";
    const icon = document.createElement("span");
    icon.textContent = app.icon || "▢";
    const name = document.createElement("span");
    name.className = "font-semibold text-slate-200";
    name.textContent = app.name;
    const categoryTag = document.createElement("span");
    categoryTag.className = "tabular ml-auto text-[10px] text-slate-600";
    categoryTag.textContent = app.category;
    head.append(icon, name, categoryTag);

    const body = document.createElement("div");
    body.className = "flex-1 px-3 py-2 text-[11px] text-slate-500";
    const description = document.createElement("p");
    description.className = "text-slate-400";
    description.textContent = app.description;
    body.append(description);

    if (app.ports.length > 0) {
      const ports = document.createElement("div");
      ports.className = "tabular mt-1 flex flex-wrap gap-x-3 text-[10px] text-slate-600";
      for (const port of app.ports) {
        const span = document.createElement("span");
        span.textContent = `${port.host}:${port.container}/${port.protocol}`;
        ports.append(span);
      }
      body.append(ports);
    }

    if (app.tags.length > 0) {
      const tags = document.createElement("div");
      tags.className = "mt-1 flex flex-wrap gap-1 text-[10px] text-slate-600";
      for (const tag of app.tags) {
        const span = document.createElement("span");
        span.className = "border border-[#1e2733] px-1";
        span.textContent = tag;
        tags.append(span);
      }
      body.append(tags);
    }

    const details = document.createElement("details");
    details.className = "mt-2";
    const summary = document.createElement("summary");
    summary.className = "cursor-pointer text-[10px] text-slate-600";
    summary.textContent = "compose";
    const pre = document.createElement("pre");
    pre.className =
      "mt-1 max-h-40 overflow-auto border border-[#1e2733] bg-[#0b0f14] p-2 text-[10px] text-slate-500";
    pre.textContent = app.compose;
    details.append(summary, pre);
    body.append(details);

    const actions = document.createElement("div");
    actions.className = "flex flex-wrap items-center gap-2 border-t border-[#161d27] px-3 py-2";

    const installBtn = button(app.installed ? "reinstall" : "install", "text-emerald-400 hover:border-emerald-500");
    installBtn.disabled = busy;
    installBtn.addEventListener("click", () =>
      void run(() => api.install(app.id), `installing ${app.name}…`),
    );

    const removeBtn = button("remove", "text-rose-400 hover:border-rose-500");
    removeBtn.disabled = busy || !app.installed;
    removeBtn.addEventListener("click", () => {
      if (window.confirm(`Remove ${app.name}? Its containers are stopped and removed.`)) {
        void run(() => api.uninstall(app.id), `removing ${app.name}…`);
      }
    });

    const badge = document.createElement("span");
    badge.className = app.installed
      ? "tabular text-[10px] text-emerald-500"
      : "tabular text-[10px] text-slate-600";
    badge.textContent = app.installed ? "installed" : "not installed";

    actions.append(installBtn, removeBtn, badge);
    el.append(head, body, actions);
    return el;
  }

  search.addEventListener("input", render);
  category.addEventListener("change", render);
  installedBox.addEventListener("change", render);

  // The shell calls `onShow` as soon as the page is first displayed.
  return {
    root,
    onShow: () => void load(),
  };
}

/** Compact star/pull counts: 1234567 → `1.2M`. */
function compact(value: number): string {
  if (value >= 1_000_000_000) {
    return `${(value / 1_000_000_000).toFixed(1)}B`;
  }
  if (value >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(1)}M`;
  }
  if (value >= 1_000) {
    return `${(value / 1_000).toFixed(1)}k`;
  }
  return String(value);
}
