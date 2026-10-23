/**
 * Header notification bell.
 *
 * Fetches the feed once on start, then keeps it live over the WebSocket. The
 * unread badge counts frames that arrive while the panel is closed; opening the
 * panel resets it. Rendered as an absolutely-positioned dropdown so it never
 * disturbs the header layout.
 */

import { notifyApi, wsUrl } from "./api";
import type { Notification, NotifyLevel } from "./api";
import { button, el } from "./ui";

export interface Bell {
  root: HTMLElement;
  start(): void;
  stop(): void;
}

const LEVEL_CLASS: Record<NotifyLevel, string> = {
  info: "text-sky-400",
  warning: "text-amber-400",
  error: "text-rose-400",
};

function relativeTime(tsMs: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - tsMs) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function createBell(): Bell {
  const root = el("div", { class: "relative" });

  const badge = el("span", {
    class:
      "absolute -right-1 -top-1 hidden min-w-4 rounded-full bg-rose-500 px-1 text-center text-[9px] font-bold leading-4 text-white",
  });
  badge.hidden = true;

  const trigger = el(
    "button",
    {
      type: "button",
      class:
        "relative border border-white/5 px-2 py-0.5 text-neutral-400 hover:border-neutral-500 hover:text-neutral-200",
      title: "Notifications",
    },
    [badge],
  );
  trigger.textContent = "bell";
  trigger.append(badge);

  const panel = el("div", {
    class:
      "absolute right-0 z-40 mt-1 hidden max-h-96 w-80 overflow-y-auto border border-white/5 bg-[#0e0e0e] shadow-xl",
  });

  root.append(trigger, panel);

  let items: Notification[] = [];
  let unread = 0;
  let open = false;
  let socket: WebSocket | null = null;

  function renderPanel(): void {
    panel.replaceChildren();
    const head = el("div", {
      class: "flex items-center gap-2 border-b border-white/5 px-3 py-1.5",
    });
    head.append(
      el("span", {
        class: "text-[10px] uppercase tracking-wide text-neutral-500",
        text: "Notifications",
      }),
      el("span", { class: "ml-auto" }),
      button("clear", "text-[10px]", () => {
        void notifyApi.clear().then(() => {
          items = [];
          unread = 0;
          updateBadge();
          renderPanel();
        });
      }),
    );
    panel.append(head);

    if (items.length === 0) {
      panel.append(
        el("div", { class: "px-3 py-4 text-xs text-neutral-600", text: "Nothing to report." }),
      );
      return;
    }

    for (const item of items) {
      const row = el("div", { class: "border-b border-white/5 px-3 py-2" });
      const top = el("div", { class: "flex items-baseline gap-2" });
      top.append(
        el("span", { class: `text-xs font-semibold ${LEVEL_CLASS[item.level]}`, text: item.title }),
        el("span", { class: "ml-auto text-[9px] text-neutral-600", text: relativeTime(item.ts_ms) }),
      );
      row.append(
        top,
        el("div", { class: "mt-0.5 text-[11px] text-neutral-400", text: item.message }),
      );
      panel.append(row);
    }
  }

  function updateBadge(): void {
    badge.textContent = String(unread);
    badge.hidden = unread === 0;
  }

  function setOpen(next: boolean): void {
    open = next;
    panel.classList.toggle("hidden", !open);
    if (open) {
      unread = 0;
      updateBadge();
      renderPanel();
    }
  }

  trigger.addEventListener("click", (event) => {
    event.stopPropagation();
    setOpen(!open);
  });

  // A click anywhere else closes the panel.
  const onDocumentClick = (): void => {
    if (open) {
      setOpen(false);
    }
  };
  document.addEventListener("click", onDocumentClick);
  panel.addEventListener("click", (event) => event.stopPropagation());

  return {
    root,
    start() {
      void notifyApi.feed(200).then((state) => {
        items = state.items;
        renderPanel();
      });
      try {
        socket = new WebSocket(wsUrl("/api/notifications/ws"));
      } catch {
        socket = null;
        return;
      }
      socket.onmessage = (event: MessageEvent<string>) => {
        if (typeof event.data !== "string") return;
        let parsed: unknown;
        try {
          parsed = JSON.parse(event.data);
        } catch {
          return;
        }
        const item = parsed as Notification;
        if (typeof item !== "object" || item === null || typeof item.title !== "string") {
          return;
        }
        items = [item, ...items].slice(0, 200);
        if (open) {
          renderPanel();
        } else {
          unread += 1;
          updateBadge();
        }
      };
    },
    stop() {
      document.removeEventListener("click", onDocumentClick);
      socket?.close();
      socket = null;
    },
  };
}
