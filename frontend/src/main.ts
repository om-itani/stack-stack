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
import { createAppsPage } from "./pages/apps";
import { createBackupsPage } from "./pages/backups";
import { createContainersPage } from "./pages/containers";
import { createFilesPage } from "./pages/files";
import { createLogsPage } from "./pages/logs";
import { createNetworkPage } from "./pages/network";
import { createSharesPage } from "./pages/shares";
import { createSettingsPage } from "./pages/settings";
import { createVpnPage } from "./pages/vpn";
import { createSystemPage } from "./pages/system";
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
  create: () => PageHandle;
}

const app = must<HTMLElement>("#app");

let telemetry: Telemetry | null = null;
let bell: Bell | null = null;
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
      "mx-auto mt-[12vh] w-full max-w-md border border-[#1e2733] bg-[#0d131b] p-6 shadow-lg",
  });
  card.append(
    el("div", { class: "mb-1 text-lg font-bold tracking-[0.2em] text-emerald-400", text: "STACK-STACK" }),
    el("div", { class: "mb-4 text-sm text-slate-300", text: title }),
    el("div", { class: "mb-4 text-xs text-slate-500", text: subtitle }),
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

function startShell(): void {
  if (shellStarted) {
    return;
  }
  shellStarted = true;

  app.innerHTML = `
<div class="flex h-screen flex-col">
  <header class="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-white/10 bg-slate-900/50 px-4 py-2 backdrop-blur">
    <span class="font-bold tracking-[0.25em] text-emerald-400">STACK-STACK</span>
    <span data-ref="host" class="text-slate-300">—</span>
    <span data-ref="uptime" class="tabular text-slate-500">up —</span>
    <span data-ref="load" class="tabular text-slate-500">load —</span>
    <span data-ref="actions" class="ml-auto flex items-center gap-2">
      <span data-ref="frame" class="tabular text-slate-600"></span>
      <span data-ref="version" class="text-slate-600"></span>
      <span data-ref="dot" class="inline-block size-2 rounded-full bg-slate-600"></span>
      <span data-ref="link" class="text-slate-400">connecting</span>
      <button data-ref="logout" type="button" class="rounded-lg border border-white/10 bg-white/5 px-2 py-0.5 text-slate-400 transition-all duration-200 hover:border-rose-500/50 hover:text-rose-400">sign out</button>
    </span>
  </header>
  <div class="flex min-h-0 flex-1">
    <nav data-ref="nav" class="flex w-40 shrink-0 flex-col overflow-y-auto border-r border-white/10 bg-slate-900/30 p-2 pb-4"></nav>
    <div data-ref="view" class="min-h-0 flex-1 overflow-auto"></div>
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
    { id: "system", label: "System", group: "Monitor", create: () => createSystemPage(tl) },
    { id: "logs", label: "Logs", group: "Monitor", create: () => createLogsPage() },
    { id: "containers", label: "Containers", group: "Data", create: () => createContainersPage() },
    { id: "apps", label: "App Store", group: "Data", create: () => createAppsPage() },
    { id: "files", label: "Files", group: "Data", create: () => createFilesPage() },
    { id: "shares", label: "Shares", group: "Data", create: () => createSharesPage() },
    { id: "backups", label: "Backups", group: "Data", create: () => createBackupsPage() },
    { id: "network", label: "Network", group: "Services", create: () => createNetworkPage() },
    { id: "virtualization", label: "Virtualization", group: "Services", create: () => createVirtualizationPage() },
    { id: "vpn", label: "VPN", group: "Services", create: () => createVpnPage() },
    { id: "terminal", label: "Terminal", group: "Admin", create: () => createTerminalPage(() => hostEl.textContent ?? "") },
    { id: "settings", label: "Settings", group: "Admin", create: () => createSettingsPage() },
  ];

  const built = new Map<string, PageHandle>();
  const navButtons = new Map<string, HTMLButtonElement>();
  let activeId = "";

  // Grouped navigation: twelve flat entries stopped fitting.
  const groups = [...new Set(pages.map((page) => page.group))];
  for (const group of groups) {
    navEl.append(
      el("div", {
        class: "px-2 pt-2 pb-0.5 text-[9px] uppercase tracking-wider text-slate-600",
        text: group,
      }),
    );
    for (const def of pages.filter((page) => page.group === group)) {
      const navButton = document.createElement("button");
      navButton.type = "button";
      navButton.textContent = def.label;
      navButton.className =
        "rounded-lg px-2 py-1 text-left text-slate-400 transition-all duration-200 hover:bg-white/5 hover:text-slate-200";
      navButton.addEventListener("click", () => show(def.id));
      navEl.append(navButton);
      navButtons.set(def.id, navButton);
    }
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
        navButton.className = isActive
          ? "rounded-lg bg-white/10 px-2 py-1 text-left font-medium text-emerald-400 shadow-[inset_2px_0_0_0_rgb(16_185_129)]"
          : "rounded-lg px-2 py-1 text-left text-slate-400 transition-all duration-200 hover:bg-white/5 hover:text-slate-200";
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
  shellStarted = false;
  void boot();
});

void boot();
