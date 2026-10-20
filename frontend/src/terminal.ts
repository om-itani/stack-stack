/**
 * Terminal / log viewer modal.
 *
 * One overlay is built lazily and reused; opening a second session disposes the
 * first. `exec` mode is bidirectional (keystrokes go back over JSON frames and
 * resize events are forwarded to the container's PTY); `logs` mode is a
 * read-only stream of text frames.
 */

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import { wsUrl } from "./api";

type Mode = "exec" | "logs";

const THEME = {
  background: "#0b0f14",
  foreground: "#c9d1d9",
  cursor: "#34d399",
  selectionBackground: "#1e2733",
};

interface Shell {
  overlay: HTMLElement;
  title: HTMLElement;
  subtitle: HTMLElement;
  mount: HTMLElement;
}

interface Session {
  dispose(): void;
}

let shell: Shell | null = null;
let current: Session | null = null;

function buildShell(): Shell {
  const overlay = document.createElement("div");
  overlay.className = "fixed inset-0 z-50 items-center justify-center bg-black/70 p-4";
  overlay.style.display = "none";

  const panel = document.createElement("div");
  panel.className = "flex h-[70vh] w-full max-w-4xl flex-col border border-[#1e2733] bg-[#0d131b]";

  const bar = document.createElement("div");
  bar.className = "flex items-center gap-3 border-b border-[#1e2733] px-3 py-1.5";

  const title = document.createElement("span");
  title.className = "font-semibold text-emerald-400";
  const subtitle = document.createElement("span");
  subtitle.className = "truncate text-slate-500";
  const spacer = document.createElement("span");
  spacer.className = "ml-auto";
  const close = document.createElement("button");
  close.type = "button";
  close.textContent = "close";
  close.className =
    "border border-[#1e2733] px-2 py-0.5 text-slate-400 hover:border-rose-500 hover:text-rose-400";
  bar.append(title, subtitle, spacer, close);

  const mount = document.createElement("div");
  mount.className = "min-h-0 flex-1 overflow-hidden bg-[#0b0f14] p-1";

  panel.append(bar, mount);
  overlay.append(panel);
  document.body.append(overlay);

  close.addEventListener("click", closeModal);
  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) {
      closeModal();
    }
  });

  return { overlay, title, subtitle, mount };
}

function closeModal(): void {
  current?.dispose();
  current = null;
  if (shell) {
    shell.mount.replaceChildren();
    shell.overlay.style.display = "none";
  }
}

function open(mode: Mode, containerId: string, containerName: string): void {
  const view = (shell ??= buildShell());
  current?.dispose();
  current = null;

  view.title.textContent = mode === "exec" ? "terminal" : "logs";
  view.subtitle.textContent = containerName;
  view.mount.replaceChildren();
  view.overlay.style.display = "flex";

  const term = new Terminal({
    convertEol: true,
    cursorBlink: mode === "exec",
    fontSize: 12,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    theme: THEME,
    scrollback: 5000,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(view.mount);

  const path =
    mode === "exec"
      ? `/api/containers/${encodeURIComponent(containerId)}/exec/ws`
      : `/api/containers/${encodeURIComponent(containerId)}/logs/ws?tail=500`;
  const socket = new WebSocket(wsUrl(path));

  const send = (payload: unknown): void => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify(payload));
    }
  };

  const resize = (): void => {
    try {
      fit.fit();
    } catch {
      // The panel can be display:none mid-close; fitting then is meaningless.
      return;
    }
    if (mode === "exec") {
      send({ type: "resize", cols: term.cols, rows: term.rows });
    }
  };

  // Layout must settle before the first fit, or xterm measures a zero box.
  const raf = window.requestAnimationFrame(resize);

  socket.onopen = () => {
    resize();
  };

  socket.onmessage = (event: MessageEvent<string>) => {
    if (typeof event.data !== "string") {
      return;
    }
    if (mode === "logs") {
      term.write(event.data);
      return;
    }
    let parsed: unknown = null;
    try {
      parsed = JSON.parse(event.data);
    } catch {
      return;
    }
    if (typeof parsed !== "object" || parsed === null) {
      return;
    }
    const frame = parsed as { type?: unknown; data?: unknown };
    if (frame.type === "data" && typeof frame.data === "string") {
      term.write(frame.data);
    } else if (frame.type === "exit") {
      term.write("\r\n\x1b[2m[session ended]\x1b[0m\r\n");
    }
  };

  socket.onclose = () => {
    term.write("\r\n\x1b[2m[disconnected]\x1b[0m\r\n");
  };

  socket.onerror = () => {
    term.write("\r\n\x1b[31m[connection error]\x1b[0m\r\n");
  };

  const dataDisposable =
    mode === "exec" ? term.onData((data) => send({ type: "data", data })) : null;

  window.addEventListener("resize", resize);

  current = {
    dispose() {
      window.cancelAnimationFrame(raf);
      window.removeEventListener("resize", resize);
      dataDisposable?.dispose();
      socket.onclose = null;
      socket.onerror = null;
      socket.onmessage = null;
      socket.close();
      term.dispose();
    },
  };
}

export function openTerminal(containerId: string, containerName: string): void {
  open("exec", containerId, containerName);
}

export function openLogs(containerId: string, containerName: string): void {
  open("logs", containerId, containerName);
}
