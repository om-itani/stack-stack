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
import { createServicesPage } from "./pages/services";
import { createSharesPage } from "./pages/shares";
import { createSettingsPage } from "./pages/settings";
import { createStoragePage } from "./pages/storage";
import { createUsersPage } from "./pages/users";
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
  onHide?: () => void;
}

interface PageDef {
  id: string;
  label: string;
  group: string;
  icon: string;
  color: string;
  badgeBg: string;
  badgeBorder: string;
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
  const { card, body } = gateCard("Sign in", "Enter your credentials to continue.");
  let username = "admin";
  let password = "";
  const error = el("div", { class: "min-h-4 text-xs text-rose-400" });

  const userInput = textInput("admin", (value) => (username = value.trim()), {
    class: "w-full",
    placeholder: "admin",
  });
  const input = textInput("", (value) => (password = value), { type: "password", class: "w-full" });
  body.append(field("Username", userInput), field("Password", input));

  const submit = button(
    "Sign in",
    "w-full border-emerald-700 text-emerald-300 hover:border-emerald-400",
    async () => {
      error.textContent = "";
      submit.disabled = true;
      try {
        await authApi.login(password, username);
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
// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

const ICONS: Record<string, string> = {
  system: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="20" height="14" x="2" y="3" rx="2"/><line x1="8" x2="16" y1="21" y2="21"/><line x1="12" x2="12" y1="17" y2="21"/></svg>`,
  containers: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"/><path d="m3.3 7 8.7 5 8.7-5"/><path d="M12 22V12"/></svg>`,
  apps: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/></svg>`,
  virtualization: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="16" height="16" x="4" y="4" rx="2"/><rect width="6" height="6" x="9" y="9" rx="1"/><path d="M15 2v2"/><path d="M15 20v2"/><path d="M2 15h2"/><path d="M2 9h2"/><path d="M20 15h2"/><path d="M20 9h2"/><path d="M9 2v2"/><path d="M9 20v2"/></svg>`,
  files: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 8 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/></svg>`,
  storage: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" x2="2" y1="12" y2="12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/><line x1="6" x2="6.01" y1="16" y2="16"/><line x1="10" x2="10.01" y1="16" y2="16"/></svg>`,
  shares: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><line x1="8.59" x2="15.42" y1="13.51" y2="17.49"/><line x1="15.41" x2="8.59" y1="6.51" y2="10.49"/></svg>`,
  backups: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/></svg>`,
  network: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/></svg>`,
  vpn: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10"/><path d="m9 12 2 2 4-4"/></svg>`,
  logs: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" x2="8" y1="13" y2="13"/><line x1="16" x2="8" y1="17" y2="17"/></svg>`,
  terminal: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/></svg>`,
  tasks: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 11 3 3L22 4"/><path d="M21 12v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11"/></svg>`,
  alerts: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/></svg>`,
  developer: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>`,
  services: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="8" height="8" x="2" y="2" rx="2"/><path d="M14 2c1.1 0 2 .9 2 2v4c0 1.1-.9 2-2 2"/><path d="M20 2c1.1 0 2 .9 2 2v4c0 1.1-.9 2-2 2"/><rect width="8" height="8" x="2" y="14" rx="2"/><path d="M14 14c1.1 0 2 .9 2 2v4c0 1.1-.9 2-2 2"/><path d="M20 14c1.1 0 2 .9 2 2v4c0 1.1-.9 2-2 2"/></svg>`,
  users: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/></svg>`,
  settings: `<svg class="size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/></svg>`,
};

/** Modern nav item styling with clean hover highlight and active indicator. */
const NAV_IDLE =
  "group relative flex items-center gap-3 rounded-lg px-2.5 py-2 text-left text-[12.5px] text-neutral-400 transition-colors duration-150 hover:bg-white/[0.06] hover:text-white";
const NAV_ACTIVE =
  "group relative flex items-center gap-3 rounded-lg bg-white/[0.08] px-2.5 py-2 text-left text-[12.5px] font-medium text-white ring-1 ring-white/10 before:absolute before:left-0 before:top-2 before:bottom-2 before:w-0.5 before:rounded-r-full before:bg-emerald-400";

function startShell(): void {
  if (shellStarted) {
    return;
  }
  shellStarted = true;

  app.innerHTML = `
<div class="flex h-screen bg-[#0b0d13]">
  <aside data-ref="sidebar" class="flex w-56 shrink-0 flex-col border-r border-white/10 bg-[#0f1219]/95 backdrop-blur-md transition-[width] duration-200 ease-out">
    <div class="flex items-center gap-2 border-b border-white/10 bg-white/[0.02] px-3.5 py-3">
      <div class="flex min-w-0 items-center gap-2.5">
        <span class="inline-block size-2 shrink-0 rounded-full bg-emerald-500"></span>
        <span data-ref="brand" class="min-w-0 truncate text-xs font-bold tracking-[0.18em] text-white">STACK-STACK</span>
      </div>
      <button data-ref="collapse" type="button" class="ml-auto shrink-0 rounded-lg border border-white/10 bg-white/5 px-2 py-0.5 text-[11px] text-neutral-400 transition-colors duration-150 hover:border-white/20 hover:bg-white/10 hover:text-white" title="Collapse sidebar">&laquo;</button>
    </div>
    <nav data-ref="nav" class="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-2 py-3"></nav>
  </aside>
  <div class="flex min-w-0 flex-1 flex-col">
    <header class="z-20 flex items-center gap-x-3 border-b border-white/10 bg-[#0f1219]/95 backdrop-blur-md px-4 py-2.5 shadow-sm">
      <span data-ref="host" class="font-semibold text-white tracking-wide">—</span>
      <span data-ref="uptime" class="tabular text-xs text-neutral-400">up —</span>
      <span data-ref="load" class="tabular text-xs text-neutral-400">load —</span>
      <span data-ref="actions" class="ml-auto flex shrink-0 items-center gap-3">
        <span data-ref="clock" class="tabular text-[11px] font-mono text-neutral-400"></span>
        <span data-ref="frame" class="tabular text-xs text-neutral-500"></span>
        <span data-ref="version" class="text-xs text-neutral-500"></span>
        <span data-ref="dot" class="inline-block size-2 rounded-full bg-neutral-600"></span>
        <span data-ref="link" class="text-xs font-medium text-neutral-400">connecting</span>
        <span data-ref="userbadge" class="hidden items-center gap-1.5 rounded-full border border-purple-500/30 bg-purple-500/10 px-2.5 py-0.5 text-xs text-purple-300 font-mono"></span>
        <button data-ref="logout" type="button" class="rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-xs font-medium text-neutral-300 transition-all duration-150 hover:border-rose-500/50 hover:bg-rose-500/10 hover:text-rose-400 shadow-xs">sign out</button>
      </span>
    </header>
    <main data-ref="view" class="min-h-0 flex-1 overflow-auto bg-[#0b0d13]"></main>
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
  const userbadgeEl = ref<HTMLElement>("userbadge");
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

  void authApi.session().then((sess) => {
    if (sess.username) {
      userbadgeEl.textContent = `${sess.username} (${sess.role || "admin"})`;
      userbadgeEl.classList.remove("hidden");
      userbadgeEl.classList.add("inline-flex");
    }
  });

  const pages: PageDef[] = [
    { id: "system", label: "System", group: "Overview", icon: ICONS.system, color: "text-cyan-400", badgeBg: "bg-cyan-500/10", badgeBorder: "border-cyan-500/25", create: () => createSystemPage(tl) },
    { id: "containers", label: "Containers", group: "Compute", icon: ICONS.containers, color: "text-blue-400", badgeBg: "bg-blue-500/10", badgeBorder: "border-blue-500/25", create: () => createContainersPage() },
    { id: "apps", label: "App Store", group: "Compute", icon: ICONS.apps, color: "text-amber-400", badgeBg: "bg-amber-500/10", badgeBorder: "border-amber-500/25", create: () => createAppsPage() },
    { id: "virtualization", label: "Virtualization", group: "Compute", icon: ICONS.virtualization, color: "text-purple-400", badgeBg: "bg-purple-500/10", badgeBorder: "border-purple-500/25", create: () => createVirtualizationPage() },
    { id: "files", label: "Files", group: "Storage", icon: ICONS.files, color: "text-yellow-400", badgeBg: "bg-yellow-500/10", badgeBorder: "border-yellow-500/25", create: () => createFilesPage() },
    { id: "storage", label: "Storage", group: "Storage", icon: ICONS.storage, color: "text-teal-400", badgeBg: "bg-teal-500/10", badgeBorder: "border-teal-500/25", create: () => createStoragePage() },
    { id: "shares", label: "Shares", group: "Storage", icon: ICONS.shares, color: "text-indigo-400", badgeBg: "bg-indigo-500/10", badgeBorder: "border-indigo-500/25", create: () => createSharesPage() },
    { id: "backups", label: "Backups", group: "Storage", icon: ICONS.backups, color: "text-emerald-400", badgeBg: "bg-emerald-500/10", badgeBorder: "border-emerald-500/25", create: () => createBackupsPage() },
    { id: "network", label: "Network", group: "Network", icon: ICONS.network, color: "text-sky-400", badgeBg: "bg-sky-500/10", badgeBorder: "border-sky-500/25", create: () => createNetworkPage() },
    { id: "vpn", label: "VPN", group: "Network", icon: ICONS.vpn, color: "text-rose-400", badgeBg: "bg-rose-500/10", badgeBorder: "border-rose-500/25", create: () => createVpnPage() },
    { id: "services", label: "Services", group: "System", icon: ICONS.services, color: "text-amber-400", badgeBg: "bg-amber-500/10", badgeBorder: "border-amber-500/25", create: () => createServicesPage() },
    { id: "logs", label: "Logs", group: "System", icon: ICONS.logs, color: "text-slate-400", badgeBg: "bg-slate-500/10", badgeBorder: "border-slate-500/25", create: () => createLogsPage() },
    { id: "terminal", label: "Terminal", group: "System", icon: ICONS.terminal, color: "text-emerald-400", badgeBg: "bg-emerald-500/10", badgeBorder: "border-emerald-500/25", create: () => createTerminalPage(() => hostEl.textContent ?? "") },
    { id: "tasks", label: "Tasks", group: "System", icon: ICONS.tasks, color: "text-violet-400", badgeBg: "bg-violet-500/10", badgeBorder: "border-violet-500/25", create: () => createTasksPage() },
    { id: "users", label: "Users & RBAC", group: "System", icon: ICONS.users, color: "text-purple-400", badgeBg: "bg-purple-500/10", badgeBorder: "border-purple-500/25", create: () => createUsersPage() },
    { id: "alerts", label: "Alerts", group: "System", icon: ICONS.alerts, color: "text-amber-400", badgeBg: "bg-amber-500/10", badgeBorder: "border-amber-500/25", create: () => createAlertsPage() },
    { id: "developer", label: "Developer", group: "System", icon: ICONS.developer, color: "text-pink-400", badgeBg: "bg-pink-500/10", badgeBorder: "border-pink-500/25", create: () => createDeveloperPage() },
    { id: "settings", label: "Settings", group: "System", icon: ICONS.settings, color: "text-zinc-400", badgeBg: "bg-zinc-500/10", badgeBorder: "border-zinc-500/25", create: () => createSettingsPage() },
  ];

  const built = new Map<string, PageHandle>();
  const navButtons = new Map<string, HTMLButtonElement>();
  let activeId = "";

  // Grouped sidebar: a hairline-separated section per group, each item an icon
  // plus label. Group names are hidden when the sidebar is collapsed to icons.
  for (const group of [...new Set(pages.map((page) => page.group))]) {
    const section = el("div", { class: "flex flex-col gap-1" });
    const header = el("span", {
      class: "px-2.5 pb-1 text-[10px] font-semibold uppercase tracking-[0.14em] text-neutral-500",
      text: group,
    });
    header.setAttribute("data-group-header", "");
    section.append(header);

    for (const def of pages.filter((page) => page.group === group)) {
      const navButton = document.createElement("button");
      navButton.type = "button";
      navButton.title = def.label;
      navButton.className = NAV_IDLE;
      const iconWrap = document.createElement("span");
      iconWrap.className = `flex size-6 shrink-0 items-center justify-center rounded-md border ${def.badgeBorder} ${def.badgeBg} ${def.color} transition-colors duration-150`;
      iconWrap.innerHTML = def.icon;

      const label = el("span", { class: "truncate font-medium", text: def.label });
      label.setAttribute("data-nav-label", "");
      navButton.append(iconWrap, label);
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
    for (const [id, btn] of navButtons.entries()) {
      const isActive = id === activeId;
      btn.className = `${isActive ? NAV_ACTIVE : NAV_IDLE}${collapsed ? " justify-center px-0" : ""}`;
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

    // 1. Hide previously visible pages and pause their timers/subscriptions
    for (const [pageId, handle] of built.entries()) {
      if (pageId !== resolved && handle.root.style.display !== "none") {
        handle.onHide?.();
        handle.root.style.display = "none";
      }
    }

    // 2. Lazily build the requested page on first visit, keeping unvisited DOM clean
    let handle = built.get(resolved);
    if (!handle) {
      const def = pages.find((page) => page.id === resolved);
      if (def) {
        handle = def.create();
        viewEl.append(handle.root);
        built.set(resolved, handle);
      }
    }

    // 3. Reveal active page and trigger entrance animation & onShow
    if (handle) {
      handle.root.style.removeProperty("display");
      handle.root.classList.remove("page-enter");
      void handle.root.offsetWidth;
      handle.root.classList.add("page-enter");
      handle.onShow?.();
    }

    // 4. Update sidebar navigation state
    for (const [navId, navButton] of navButtons.entries()) {
      const isActive = navId === resolved;
      navButton.className = `${isActive ? NAV_ACTIVE : NAV_IDLE}${collapsed ? " justify-center px-0" : ""}`;
    }

    const target = `#/${resolved}`;
    if (updateHash && location.hash !== target) {
      location.hash = target;
    }
  }

  function idFromHash(): string {
    return location.hash.replace(/^#\/?/, "") || pages[0].id;
  }

  window.addEventListener("hashchange", () => show(idFromHash(), false));

  // Suspend polling and streaming when the browser tab is hidden/minimized
  document.addEventListener("visibilitychange", () => {
    const handle = built.get(activeId);
    if (document.hidden) {
      handle?.onHide?.();
    } else {
      handle?.onShow?.();
    }
  });

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
