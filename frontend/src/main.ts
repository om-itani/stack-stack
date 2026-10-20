/**
 * Shell + router.
 *
 * The header status and the telemetry socket are process-wide; pages are built
 * lazily on first visit and then kept in the DOM, so switching tabs never
 * re-fetches or resets sparkline history.
 */

import "./style.css";

import { createAppsPage } from "./pages/apps";
import { createContainersPage } from "./pages/containers";
import { createSystemPage } from "./pages/system";
import { Telemetry, fetchVersion } from "./telemetry";
import type { LinkState, Snapshot } from "./telemetry";
import { must, uptime } from "./ui";

interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

interface PageDef {
  id: string;
  label: string;
  create: () => PageHandle;
}

const telemetry = new Telemetry(setLinkState);

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

const app = must<HTMLElement>("#app");
app.innerHTML = `
<div class="flex h-screen flex-col">
  <header class="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-[#1e2733] bg-[#0d131b] px-4 py-2">
    <span class="font-bold tracking-[0.25em] text-emerald-400">STACK-STACK</span>
    <span data-ref="host" class="text-slate-300">—</span>
    <span data-ref="uptime" class="tabular text-slate-500">up —</span>
    <span data-ref="load" class="tabular text-slate-500">load —</span>
    <span class="ml-auto flex items-center gap-2">
      <span data-ref="frame" class="tabular text-slate-600"></span>
      <span data-ref="version" class="text-slate-600"></span>
      <span data-ref="dot" class="inline-block size-2 rounded-full bg-slate-600"></span>
      <span data-ref="link" class="text-slate-400">connecting</span>
    </span>
  </header>
  <div class="flex min-h-0 flex-1">
    <nav data-ref="nav" class="flex w-36 shrink-0 flex-col gap-0.5 border-r border-[#1e2733] bg-[#0d131b] p-2"></nav>
    <div data-ref="view" class="min-h-0 flex-1 overflow-auto"></div>
  </div>
</div>
`;

const ref = <T extends Element>(name: string): T => must<T>(`[data-ref="${name}"]`, app);
const hostEl = ref<HTMLElement>("host");
const uptimeEl = ref<HTMLElement>("uptime");
const loadEl = ref<HTMLElement>("load");
const frameEl = ref<HTMLElement>("frame");
const versionEl = ref<HTMLElement>("version");
const dotEl = ref<HTMLElement>("dot");
const linkEl = ref<HTMLElement>("link");
const navEl = ref<HTMLElement>("nav");
const viewEl = ref<HTMLElement>("view");

const pages: PageDef[] = [
  { id: "system", label: "System", create: () => createSystemPage(telemetry) },
  { id: "containers", label: "Containers", create: () => createContainersPage() },
  { id: "apps", label: "App Store", create: () => createAppsPage() },
];

const built = new Map<string, PageHandle>();
const navButtons = new Map<string, HTMLButtonElement>();
let activeId = "";

for (const def of pages) {
  const navButton = document.createElement("button");
  navButton.type = "button";
  navButton.textContent = def.label;
  navButton.className =
    "px-2 py-1 text-left text-slate-400 transition-colors hover:bg-[#111721] hover:text-slate-200";
  navButton.addEventListener("click", () => show(def.id));
  navEl.append(navButton);
  navButtons.set(def.id, navButton);
}

function show(id: string, updateHash = true): void {
  const resolved = pages.some((page) => page.id === id) ? id : pages[0].id;
  if (resolved === activeId) {
    return;
  }
  activeId = resolved;

  for (const def of pages) {
    let handle = built.get(def.id);
    if (!handle) {
      handle = def.create();
      viewEl.append(handle.root);
      built.set(def.id, handle);
    }
    const isActive = def.id === resolved;
    // Inline `display`, not the `hidden` attribute: an author `display` utility
    // (e.g. `grid`, `flex`) outranks the UA rule for `[hidden]`.
    if (isActive) {
      handle.root.style.removeProperty("display");
    } else {
      handle.root.style.display = "none";
    }

    const navButton = navButtons.get(def.id);
    if (navButton) {
      navButton.className = isActive
        ? "bg-[#111721] px-2 py-1 text-left text-emerald-400"
        : "px-2 py-1 text-left text-slate-400 transition-colors hover:bg-[#111721] hover:text-slate-200";
    }
  }

  built.get(resolved)?.onShow?.();

  const target = `#/${resolved}`;
  if (updateHash && location.hash !== target) {
    location.hash = target;
  }
}

function idFromHash(): string {
  return location.hash.replace(/^#\/?/, "") || pages[0].id;
}

window.addEventListener("hashchange", () => show(idFromHash(), false));

// ---------------------------------------------------------------------------
// Header telemetry
// ---------------------------------------------------------------------------

function renderHeader(snapshot: Snapshot): void {
  document.title = `${snapshot.hostname} · stack-stack`;
  hostEl.textContent = snapshot.hostname;
  uptimeEl.textContent = `up ${uptime(snapshot.uptime_s)}`;
  loadEl.textContent = `load ${snapshot.load.map((value) => value.toFixed(2)).join(" ")}`;
  frameEl.textContent = new Date(snapshot.ts_ms).toLocaleTimeString();
}

function setLinkState(state: LinkState): void {
  linkEl.textContent = state;
  dotEl.className =
    state === "live"
      ? "inline-block size-2 rounded-full bg-emerald-400"
      : state === "connecting"
        ? "inline-block size-2 rounded-full bg-amber-400"
        : "inline-block size-2 rounded-full bg-rose-500";
}

telemetry.onSnapshot(renderHeader);
void telemetry.currentSnapshot().then((snapshot) => {
  if (snapshot) {
    renderHeader(snapshot);
  }
});

void fetchVersion().then((version) => {
  versionEl.textContent = version;
});

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

show(idFromHash(), false);
telemetry.start();
