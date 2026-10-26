/**
 * Shell, login gate and router.
 *
 * Boot order: the session is checked first; the operator sees the setup screen
 * (first run), the login screen, or the app shell. The header status and the
 * telemetry socket are process-wide; pages are built lazily on first visit and
 * kept in the DOM, so switching tabs never re-fetches or resets sparkline
 * history.
 */

import "./style.css";

import { authApi } from "./api";
import { createActivity } from "./activity";
import type { Activity } from "./activity";
import { createAlertsPage } from "./pages/alerts";
import { createAppsPage } from "./pages/apps";
import { createDeveloperPage } from "./pages/developer";
import { createBackupsPage } from "./pages/backups";
import { createContainersPage } from "./pages/containers";
import { createFilesPage } from "./pages/files";
import { createLogsPage } from "./pages/logs";
import { createNetworkPage } from "./pages/network";
import { createSharesPage } from "./pages/shares";
import { createSettingsPage } from "./pages/settings";
import { createStoragePage } from "./pages/storage";
import { createVpnPage } from "./pages/vpn";
import { createSystemPage } from "./pages/system";
import { createTasksPage } from "./pages/tasks";
import { clearToasts, showToast } from "./toast";
import { createTerminalPage } from "./pages/terminal";
import { createVirtualizationPage } from "./pages/virtualization";
import { createBell } from "./notifications";
import type { Bell } from "./notifications";
import { Telemetry, fetchVersion } from "./telemetry";
import type { LinkState, Snapshot } from "./telemetry";
import { button, el, field, must, textInput, uptime } from "./ui";

interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

interface PageDef {
  id: string;
  label: string;
  group: string;
  /** Short monospace mark shown when the sidebar is collapsed to icons. */
  glyph: string;
  create: () => PageHandle;
}

const app = must<HTMLElement>("#app");

let telemetry: Telemetry | null = null;
let bell: Bell | null = null;
let activity: Activity | null = null;
let shellStarted = false;

// ---------------------------------------------------------------------------
// Login / setup gate
// ---------------------------------------------------------------------------

async function boot(): Promise<void> {
  let status: Awaited<ReturnType<typeof authApi.session>>;
  try {
    status = await authApi.session();
  } catch (err) {
    renderGateMessage(
      "Cannot reach the backend",
      err instanceof Error ? err.message : "unknown error",
    );
    return;
  }

  if (status.error) {
    renderGateMessage("Authentication is unavailable", status.error);
    return;
  }
  if (status.setup_required) {
    renderSetup();
    return;
  }
  if (!status.authenticated) {
    renderLogin();
    return;
  }
  startShell();
}

/** A centred card used by the setup/login/error screens. */
function gateCard(title: string, subtitle: string): { card: HTMLElement; body: HTMLElement } {
  const card = el("div", {
    class:
      "mx-auto mt-[12vh] w-full max-w-md border border-white/5 bg-[#0e0e0e] p-6 shadow-lg",
  });
  card.append(
    el("div", { class: "mb-1 text-lg font-bold tracking-[0.2em] text-emerald-400", text: "STACK-STACK" }),
    el("div", { class: "mb-4 text-sm text-neutral-300", text: title }),
    el("div", { class: "mb-4 text-xs text-neutral-500", text: subtitle }),
  );
  const body = el("div", { class: "flex flex-col gap-3" });
  card.append(body);
  return { card, body };
}

function renderSetup(): void {
  const { card, body } = gateCard(
    "First-run setup",
    "Choose the admin password. It protects this whole control plane.",
  );
  let password = "";
  let confirm = "";
  const error = el("div", { class: "min-h-4 text-xs text-rose-400" });

  body.append(
    field(
      "Password",
      textInput("", (value) => (password = value), { type: "password", class: "w-full" }),
    ),
    field(
      "Confirm password",
      textInput("", (value) => (confirm = value), { type: "password", class: "w-full" }),
    ),
  );

  const submit = button(
    "Create password",
    "w-full border-emerald-700 text-emerald-300 hover:border-emerald-400",
    async () => {
      error.textContent = "";
      if (password.length < 12) {
        error.textContent = "Password must be at least 12 characters.";
        return;
      }
      if (password !== confirm) {
        error.textContent = "Passwords do not match.";
        return;
      }
      submit.disabled = true;
      try {
        await authApi.setup(password);
        startShell();
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
        submit.disabled = false;
      }
    },
  );
  body.append(error, submit);
  app.replaceChildren(card);
}

function renderLogin(): void {
  const { card, body } = gateCard("Sign in", "Enter the admin password to continue.");
  let password = "";
  const error = el("div", { class: "min-h-4 text-xs text-rose-400" });

  const input = textInput("", (value) => (password = value), { type: "password", class: "w-full" });
  input.autofocus = true;
  body.append(field("Password", input));

  const submit = button(
    "Sign in",
    "w-full border-emerald-700 text-emerald-300 hover:border-emerald-400",
    async () => {
      error.textContent = "";
      submit.disabled = true;
      try {
        await authApi.login(password);
        startShell();
      } catch (err) {
        error.textContent = err instanceof Error ? err.message : String(err);
        submit.disabled = false;
      }
    },
  );

  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      submit.click();
    }
  });

  body.append(error, submit);
  app.replaceChildren(card);
  input.focus();
}

function renderGateMessage(title: string, detail: string): void {
  const { card, body } = gateCard(title, detail);
  const retry = button("Retry", "w-full", () => location.reload());
  body.append(retry);
  app.replaceChildren(card);
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/** Top-nav item styling. The active state is a soft press, not a glowing pill. */
const NAV_IDLE =
  "flex items-center gap-2 rounded-md px-2 py-1.5 text-left text-[12px] text-neutral-400 transition-colors duration-200 hover:bg-white/5 hover:text-neutral-100";
const NAV_ACTIVE =
  "flex items-center gap-2 rounded-md bg-white/10 px-2 py-1.5 text-left text-[12px] font-medium text-white";

function startShell(): void {
  if (shellStarted) {
    return;
  }
  shellStarted = true;

  app.innerHTML = `
<div class="flex h-screen">
  <aside data-ref="sidebar" class="flex w-56 shrink-0 flex-col border-r border-white/10 bg-[#0d0d0d] transition-[width] duration-200 ease-out">
    <div class="flex items-center gap-2 border-b border-white/10 px-3 py-2.5">
      <span data-ref="brand" class="min-w-0 truncate text-sm font-semibold tracking-[0.18em] text-white">STACK-STACK</span>
      <button data-ref="collapse" type="button" class="ml-auto rounded-md border border-white/10 px-1.5 py-0.5 text-[11px] text-neutral-400 transition-colors duration-200 hover:border-white/20 hover:text-neutral-100" title="Collapse sidebar">&laquo;</button>
    </div>
    <nav data-ref="nav" class="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-2 py-3"></nav>
  </aside>
  <div class="flex min-w-0 flex-1 flex-col">
    <header class="z-20 flex items-center gap-x-3 border-b border-white/10 bg-[#0d0d0d] px-4 py-2">
      <span data-ref="host" class="font-medium text-neutral-100">—</span>
      <span data-ref="uptime" class="tabular text-neutral-500">up —</span>
      <span data-ref="load" class="tabular text-neutral-500">load —</span>
      <span data-ref="actions" class="ml-auto flex shrink-0 items-center gap-3">
        <span data-ref="clock" class="tabular text-[11px] text-neutral-500"></span>
        <span data-ref="frame" class="tabular text-neutral-600"></span>
        <span data-ref="version" class="text-neutral-600"></span>
        <span data-ref="dot" class="inline-block size-2 rounded-full bg-neutral-600"></span>
        <span data-ref="link" class="text-neutral-400">connecting</span>
        <button data-ref="logout" type="button" class="rounded-lg border border-white/10 bg-white/5 px-2 py-0.5 text-neutral-400 transition-all duration-200 hover:border-rose-500/50 hover:text-rose-400">sign out</button>
      </span>
    </header>
    <main data-ref="view" class="min-h-0 flex-1 overflow-auto"></main>
  </div>
</div>
`;

  const tl = new Telemetry(setLinkState);
  telemetry = tl;

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
  const logoutEl = ref<HTMLButtonElement>("logout");
  const actionsEl = ref<HTMLElement>("actions");
  const sidebarEl = ref<HTMLElement>("sidebar");
  const brandEl = ref<HTMLElement>("brand");
  const collapseEl = ref<HTMLButtonElement>("collapse");

  // A wall clock in the global header, the way an OS menu bar has one.
  const clockEl = ref<HTMLElement>("clock");
  const tickClock = (): void => {
    clockEl.textContent = new Date().toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  };
  tickClock();
  window.setInterval(tickClock, 1000);

  activity = createActivity();
  actionsEl.insertBefore(activity.root, logoutEl);
  activity.start();

  bell = createBell();
  actionsEl.insertBefore(bell.root, logoutEl);
  bell.start();

  logoutEl.addEventListener("click", async () => {
    try {
      await authApi.logout();
    } finally {
      location.reload();
    }
  });

  const pages: PageDef[] = [
    { id: "system", label: "System", group: "Overview", glyph: "SY", create: () => createSystemPage(tl) },
    { id: "containers", label: "Containers", group: "Compute", glyph: "CT", create: () => createContainersPage() },
    { id: "apps", label: "App Store", group: "Compute", glyph: "AP", create: () => createAppsPage() },
    { id: "virtualization", label: "Virtualization", group: "Compute", glyph: "VM", create: () => createVirtualizationPage() },
    { id: "files", label: "Files", group: "Storage", glyph: "FL", create: () => createFilesPage() },
    { id: "storage", label: "Storage", group: "Storage", glyph: "ST", create: () => createStoragePage() },
    { id: "shares", label: "Shares", group: "Storage", glyph: "SH", create: () => createSharesPage() },
    { id: "backups", label: "Backups", group: "Storage", glyph: "BK", create: () => createBackupsPage() },
    { id: "network", label: "Network", group: "Network", glyph: "NW", create: () => createNetworkPage() },
    { id: "vpn", label: "VPN", group: "Network", glyph: "VP", create: () => createVpnPage() },
    { id: "logs", label: "Logs", group: "System", glyph: "LG", create: () => createLogsPage() },
    { id: "terminal", label: "Terminal", group: "System", glyph: "TM", create: () => createTerminalPage(() => hostEl.textContent ?? "") },
    { id: "tasks", label: "Tasks", group: "System", glyph: "TK", create: () => createTasksPage() },
    { id: "alerts", label: "Alerts", group: "System", glyph: "AL", create: () => createAlertsPage() },
    { id: "developer", label: "Developer", group: "System", glyph: "DV", create: () => createDeveloperPage() },
    { id: "settings", label: "Settings", group: "System", glyph: "SE", create: () => createSettingsPage() },
  ];

  const built = new Map<string, PageHandle>();
  const navButtons = new Map<string, HTMLButtonElement>();
  let activeId = "";

  // Grouped sidebar: a hairline-separated section per group, each item a glyph
  // plus label. Group names are hidden when the sidebar is collapsed to icons.
  for (const group of [...new Set(pages.map((page) => page.group))]) {
    const section = el("div", { class: "flex flex-col gap-0.5" });
    const header = el("span", {
      class: "px-2 pb-1 text-[10px] uppercase tracking-[0.12em] text-neutral-600",
      text: group,
    });
    header.setAttribute("data-group-header", "");
    section.append(header);

    for (const def of pages.filter((page) => page.group === group)) {
      const navButton = document.createElement("button");
      navButton.type = "button";
      navButton.title = def.label;
      navButton.className = NAV_IDLE;
      const glyph = el("span", {
        class: "w-6 shrink-0 text-center text-[10px] tracking-wider text-neutral-500",
        text: def.glyph,
      });
      const label = el("span", { class: "truncate", text: def.label });
      label.setAttribute("data-nav-label", "");
      navButton.append(glyph, label);
      navButton.addEventListener("click", () => show(def.id));
      section.append(navButton);
      navButtons.set(def.id, navButton);
    }
    navEl.append(section);
  }

  // Collapse to icons only; the choice survives a reload.
  let collapsed = localStorage.getItem("stack-stack:sidebar") === "collapsed";
  const applyCollapse = (): void => {
    sidebarEl.classList.toggle("w-56", !collapsed);
    sidebarEl.classList.toggle("w-14", collapsed);
    for (const node of navEl.querySelectorAll<HTMLElement>("[data-group-header]")) {
      node.style.display = collapsed ? "none" : "";
    }
    for (const node of navEl.querySelectorAll<HTMLElement>("[data-nav-label]")) {
      node.style.display = collapsed ? "none" : "";
    }
    brandEl.style.display = collapsed ? "none" : "";
    collapseEl.textContent = collapsed ? "»" : "«";
    collapseEl.title = collapsed ? "Expand sidebar" : "Collapse sidebar";
  };
  collapseEl.addEventListener("click", () => {
    collapsed = !collapsed;
    localStorage.setItem("stack-stack:sidebar", collapsed ? "collapsed" : "expanded");
    applyCollapse();
  });
  applyCollapse();

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
        // Replay the entrance animation on every visit. Re-adding the class
        // needs a reflow in between or the browser coalesces it away.
        handle.root.classList.remove("page-enter");
        void handle.root.offsetWidth;
        handle.root.classList.add("page-enter");
      } else {
        handle.root.style.display = "none";
      }

      const navButton = navButtons.get(def.id);
      if (navButton) {
        navButton.className = isActive ? NAV_ACTIVE : NAV_IDLE;
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

  // Header telemetry
  function renderHeader(snapshot: Snapshot): void {
    document.title = `${snapshot.hostname} · stack-stack`;
    hostEl.textContent = snapshot.hostname;
    uptimeEl.textContent = `up ${uptime(snapshot.uptime_s)}`;
    loadEl.textContent = `load ${snapshot.load.map((value) => value.toFixed(2)).join(" ")}`;
    frameEl.textContent = new Date(snapshot.ts_ms).toLocaleTimeString();
  }

  function setLinkState(state: LinkState): void {
    linkEl.textContent = state === "polling" ? "polling" : state;
    dotEl.className =
      state === "live"
        ? "inline-block size-2 rounded-full bg-emerald-400"
        : state === "connecting"
          ? "inline-block size-2 rounded-full bg-amber-400"
          : state === "polling"
            ? "inline-block size-2 rounded-full bg-amber-400"
            : "inline-block size-2 rounded-full bg-rose-500";
  }

  tl.onSnapshot(renderHeader);
  void tl.currentSnapshot().then((snapshot) => {
    if (snapshot) {
      renderHeader(snapshot);
    }
  });

  void fetchVersion().then((version) => {
    versionEl.textContent = version;
  });

  show(idFromHash(), false);
  tl.start();
}

// A session that dies mid-use drops the shell and returns to the gate.
window.addEventListener("stack-stack:unauthorized", () => {
  telemetry?.stop();
  bell?.stop();
  bell = null;
  activity?.stop();
  activity = null;
  clearToasts();
  shellStarted = false;
  void boot();
});

/**
 * Global error boundary.
 *
 * Anything that escapes a handler — a synchronous throw or an unhandled
 * rejection — is logged in full and surfaced as a red toast, so a crash is
 * never silent.
 */
function installGlobalErrorHandlers(): void {
  window.addEventListener("error", (event) => {
    // Resource-load errors (e.g. a missing image) arrive without an Error and
    // carry a useless "Script error." message; skip those.
    if (!event.error && (!event.message || event.message === "Script error.")) {
      return;
    }
    const detail =
      event.error instanceof Error ? (event.error.stack ?? event.error.message) : event.message;
    console.error("[Frontend Error]", detail);
    showToast({
      tone: "error",
      title: "A frontend error occurred",
      message: "Check the console for the full stack trace.",
    });
  });

  window.addEventListener("unhandledrejection", (event) => {
    const reason: unknown = event.reason;
    const detail = reason instanceof Error ? (reason.stack ?? reason.message) : String(reason);
    console.error("[Unhandled Rejection]", detail);
    showToast({
      tone: "error",
      title: "An unhandled error occurred",
      message: "Check the console for the full stack trace.",
    });
  });
}

installGlobalErrorHandlers();
void boot();
