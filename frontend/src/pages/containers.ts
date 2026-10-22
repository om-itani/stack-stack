/**
 * Containers page: one card per Docker container with lifecycle actions, log
 * and terminal viewers, and "Open Web UI" links for published ports.
 *
 * Also hosts operator-authored compose "stacks": the Create Stack button writes
 * YAML under the stacks directory and brings it up under an explicit project
 * name. Each stack's containers are shown from the container list, which carries
 * its compose project.
 *
 * The list refreshes only while the page is visible, and never while an action
 * is in flight, so it does not fight the user or hammer a slow daemon.
 */

import { api, stacksApi } from "../api";
import type { Container, StackInfo } from "../api";
import { openLogs, openTerminal } from "../terminal";
import { HARDENING_FRAGMENT, composeForImage, serviceName, takeStackDraft } from "../stackdraft";
import { button, el, emptyNote, field, textInput } from "../ui";

const REFRESH_MS = 5000;

/** A minimal, hardened example so an empty editor is still a valid document. */
const STARTER_COMPOSE = `services:
  web:
    image: nginx:1.27
    restart: unless-stopped
    ports:
      - "8080:80"
${HARDENING_FRAGMENT}`;

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
  const createStack = button("create stack", "text-emerald-400 hover:border-emerald-500");
  const status = document.createElement("span");
  status.className = "tabular truncate text-[11px] text-slate-500";
  bar.append(heading, count, createStack, reload, status);

  const stacksHost = document.createElement("div");
  stacksHost.className = "flex flex-col gap-2";

  const grid = document.createElement("div");
  grid.className = "grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3";

  root.append(bar, stacksHost, grid);

  let busy = false;
  let stacks: StackInfo[] = [];

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
      const [containers, stackList] = await Promise.all([
        api.containers(),
        // A stack listing failure must not hide the container list.
        stacksApi.list().catch(() => [] as StackInfo[]),
      ]);
      stacks = stackList;
      count.textContent = `${containers.length} total`;
      renderStacks(containers);
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

  // Binds the modal to this page's state so the module-level editor stays a
  // plain function.
  const openEditor = (prefill: StackEditorPrefill = {}): void =>
    openStackEditor(
      {
        busy: () => busy,
        setBusy: (value) => {
          busy = value;
        },
        setStatus,
        errorText,
        reload: load,
      },
      prefill,
    );

  /// One row per operator-authored stack, with the state of each of its
  /// containers (matched by the compose project the daemon reports).
  function renderStacks(containers: Container[]): void {
    stacksHost.replaceChildren();
    if (stacks.length === 0) {
      return;
    }
    const byStack = new Map<string, Container[]>();
    for (const container of containers) {
      if (!container.stack) continue;
      const list = byStack.get(container.stack) ?? [];
      list.push(container);
      byStack.set(container.stack, list);
    }

    for (const stack of stacks) {
      const members = byStack.get(stack.name) ?? [];
      const panel = document.createElement("section");
      panel.className = "panel flex flex-col";

      const head = document.createElement("div");
      head.className =
        "flex flex-wrap items-baseline gap-3 border-b border-[#1e2733] px-3 py-1.5";
      const title = document.createElement("span");
      title.className = "font-semibold text-slate-200";
      title.textContent = stack.name;
      // A stack on disk with no containers is shown, not hidden: the operator
      // needs to see that it exists and is down, and be able to bring it up.
      const running = members.length > 0;
      const kind = document.createElement("span");
      kind.className = `tabular text-[10px] ${running ? "text-slate-600" : "text-amber-500"}`;
      kind.textContent = running
        ? `stack · ${members.length} container${members.length === 1 ? "" : "s"}`
        : "stack · not running";

      const actions = document.createElement("span");
      actions.className = "ml-auto flex gap-2";
      const edit = button("edit", "text-[10px]");
      edit.addEventListener("click", () =>
        openEditor({ name: stack.name, compose: stack.compose, lockName: true }),
      );
      const redeploy = button(running ? "redeploy" : "deploy", "text-[10px] text-emerald-400");
      redeploy.disabled = busy;
      redeploy.addEventListener("click", () =>
        void run(
          () => stacksApi.create(stack.name, stack.compose),
          `deploying ${stack.name}…`,
        ),
      );
      const removeBtn = button("remove", "text-[10px] text-rose-400 hover:border-rose-500");
      removeBtn.disabled = busy;
      removeBtn.addEventListener("click", () => {
        if (window.confirm(`Remove stack ${stack.name}? Its containers are stopped and removed.`)) {
          void run(() => stacksApi.remove(stack.name), `removing ${stack.name}…`);
        }
      });
      // Force removal skips `down`, so it is only offered behind its own, more
      // explicit confirmation.
      const forceBtn = button("force remove", "text-[10px] text-rose-500");
      forceBtn.disabled = busy;
      forceBtn.addEventListener("click", () => {
        if (
          window.confirm(
            `Force-remove ${stack.name}? Running containers are left untouched and the compose file is deleted.`,
          )
        ) {
          void run(() => stacksApi.remove(stack.name, true), `force-removing ${stack.name}…`);
        }
      });
      actions.append(edit, redeploy, removeBtn, forceBtn);
      head.append(title, kind, actions);

      const body = document.createElement("div");
      body.className = "tabular flex flex-wrap gap-x-3 gap-y-0.5 px-3 py-1.5 text-[10px]";
      if (members.length === 0) {
        body.append(
          el("span", {
            class: "text-amber-500",
            text: "no containers running — deploy to bring the stack up",
          }),
        );
      } else {
        for (const member of members) {
          const dot = el("span", {
            class: `uppercase ${STATE_STYLES[member.state] ?? "text-slate-500"}`,
            text: `${member.name}: ${member.state}`,
          });
          body.append(dot);
        }
      }

      panel.append(head, body);
      stacksHost.append(panel);
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
    if (container.stack) {
      const stackTag = document.createElement("span");
      stackTag.className = "text-sky-600";
      stackTag.textContent = `stack:${container.stack}`;
      meta.append(stackTag);
    }

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
  createStack.addEventListener("click", () => openEditor());

  // The page lives as long as the tab, so the interval needs no teardown. The
  // shell hides inactive pages with inline `display`, so that is the visibility
  // signal here too.
  window.setInterval(() => {
    if (root.style.display !== "none" && !busy) {
      void load();
    }
  }, REFRESH_MS);

  // The shell calls `onShow` as soon as the page is first displayed, so the
  // initial load happens there rather than eagerly here. A draft staged by the
  // App Store's "Deploy" button opens the editor pre-filled instead.
  return {
    root,
    onShow: () => {
      const draft = takeStackDraft();
      if (draft) {
        openEditor({
          name: serviceName(draft.image),
          compose: composeForImage(draft.image),
        });
      }
      void load();
    },
  };
}

/** Pre-filled values for the Create Stack editor. */
interface StackEditorPrefill {
  name?: string;
  compose?: string;
  /** Editing an existing stack: the project name is fixed, so lock it. */
  lockName?: boolean;
}

/** Page-scoped hooks the Create Stack modal needs, injected so the modal itself
 * can stay a plain function rather than closing over page state. */
interface StackEditorDeps {
  busy: () => boolean;
  setBusy: (value: boolean) => void;
  setStatus: (text: string, tone: "muted" | "error" | "ok") => void;
  errorText: (err: unknown) => string;
  reload: () => Promise<void>;
}

/**
 * The Create Stack modal. Writes the YAML and runs `docker compose -p <name> up
 * -d` through the API; the page reloads afterwards so the stack panel and the
 * container list both reflect the result.
 */
function openStackEditor(deps: StackEditorDeps, prefill: StackEditorPrefill = {}): void {
  let name = prefill.name ?? "";
  let compose = prefill.compose ?? STARTER_COMPOSE;

  const overlay = el("div", {
    class: "fixed inset-0 z-50 flex items-start justify-center overflow-auto bg-black/70 p-4",
  });
  const modal = el("div", {
    class: "panel mt-8 flex w-full max-w-2xl flex-col gap-3 p-4",
  });

  const heading = el("div", {
    class: "text-slate-300",
    text: prefill.name ? `edit stack · ${prefill.name}` : "create stack",
  });

  const nameInput = textInput(
    name,
    (value) => {
      name = value;
    },
    { placeholder: "my-stack" },
  );
  if (prefill.lockName) {
    nameInput.disabled = true;
  }

  const area = document.createElement("textarea");
  area.className =
    "h-72 w-full resize-y border border-[#1e2733] bg-[#0b0f14] p-2 font-mono text-[11px] text-slate-300 outline-none focus:border-slate-500";
  area.spellcheck = false;
  area.value = compose;
  area.addEventListener("input", () => {
    compose = area.value;
  });

  const feedback = el("div", { class: "tabular text-[11px] text-slate-500" });
  feedback.textContent = "runs docker compose -p <name> up -d";

  const deployBtn = button("deploy", "text-emerald-400 hover:border-emerald-500");
  const cancelBtn = button("cancel");
  const actions = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    deployBtn,
    cancelBtn,
    feedback,
  ]);

  function close(): void {
    overlay.remove();
  }

  cancelBtn.addEventListener("click", close);
  overlay.addEventListener("click", (event) => {
    if (event.target === overlay) {
      close();
    }
  });
  document.addEventListener("keydown", function onKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      close();
      document.removeEventListener("keydown", onKey);
    }
  });

  deployBtn.addEventListener("click", async () => {
    if (deps.busy()) {
      return;
    }
    deps.setBusy(true);
    deployBtn.disabled = true;
    feedback.className = "tabular text-[11px] text-slate-500";
    feedback.textContent = "deploying…";
    try {
      const result = await stacksApi.create(name, compose);
      const tail = result.output.split("\n").filter(Boolean).pop();
      deps.setStatus(`stack ${name} deployed${tail ? ` · ${tail}` : ""}`, "ok");
      close();
    } catch (err) {
      // The compose failure text (pull error, port conflict, YAML problem) is
      // multi-line; show all of it in a scrollable block rather than one line.
      feedback.className =
        "tabular w-full max-h-40 overflow-auto whitespace-pre-wrap text-[11px] text-rose-400";
      feedback.textContent = deps.errorText(err);
      return;
    } finally {
      deps.setBusy(false);
      deployBtn.disabled = false;
    }
    await deps.reload();
  });

  const hint = el("div", {
    class: "text-[10px] text-slate-600",
    text: "generated files include hardened defaults (no-new-privileges, 10m x 3 log rotation); pasted YAML is used verbatim",
  });
  modal.append(heading, field("name", nameInput), field("compose (yaml)", area), hint, actions);
  overlay.append(modal);
  document.body.append(overlay);
  nameInput.focus();
}

function linkPort(url: string): string {
  const parts = url.split(":");
  return parts.length > 0 ? (parts[parts.length - 1] ?? "") : "";
}
