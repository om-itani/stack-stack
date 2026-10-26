/**
 * Global toast host.
 *
 * One fixed bottom-right stack shared by the API layer, the global error
 * boundary and the activity drawer, so a failure looks the same wherever it
 * originates. Identical toasts within a few seconds are collapsed, since a
 * flapping backend can otherwise make the same error repeat many times a second.
 */

export type ToastTone = "info" | "ok" | "error";

export interface ToastOptions {
  title: string;
  message?: string;
  tone?: ToastTone;
  /** Auto-dismiss after this many ms; 0 keeps it until clicked. */
  timeoutMs?: number;
}

const TONES: Record<ToastTone, string> = {
  info: "border-sky-500/40 bg-[#0d1216] text-sky-300",
  ok: "border-emerald-500/40 bg-[#0d1512] text-emerald-300",
  error: "border-rose-500/40 bg-[#170d0f] text-rose-300",
};

const DEDUPE_MS = 4000;

let host: HTMLElement | null = null;
const recent = new Map<string, number>();

function ensureHost(): HTMLElement {
  if (!host) {
    host = document.createElement("div");
    host.setAttribute("data-toast-host", "");
    host.className = "pointer-events-none fixed bottom-4 right-4 z-[60] flex flex-col gap-2";
    document.body.append(host);
  }
  return host;
}

/** Drop the host; used when the shell tears down on sign-out. */
export function clearToasts(): void {
  host?.remove();
  host = null;
  recent.clear();
}

function isDuplicate(key: string): boolean {
  const now = Date.now();
  for (const [seen, at] of recent) {
    if (now - at > DEDUPE_MS) {
      recent.delete(seen);
    }
  }
  const last = recent.get(key);
  recent.set(key, now);
  return last !== undefined && now - last < DEDUPE_MS;
}

export function showToast(options: ToastOptions): void {
  const tone = options.tone ?? "info";
  if (isDuplicate(`${tone}:${options.title}:${options.message ?? ""}`)) {
    return;
  }

  const node = document.createElement("div");
  node.className = `pointer-events-auto w-80 cursor-pointer rounded-[10px] border px-3 py-2 text-[11px] shadow-float ${TONES[tone]}`;
  const title = document.createElement("div");
  title.className = "font-semibold";
  title.textContent = options.title;
  node.append(title);

  if (options.message) {
    const body = document.createElement("div");
    body.className = "mt-0.5 break-words text-neutral-400";
    body.textContent = options.message;
    node.append(body);
  }

  node.addEventListener("click", () => node.remove());
  ensureHost().append(node);

  const timeout = options.timeoutMs ?? 6000;
  if (timeout > 0) {
    window.setTimeout(() => node.remove(), timeout);
  }
}
