/**
 * Terminal page.
 *
 * A host shell is a modal, not a docked pane: it must survive navigation and
 * read like an SSH window. The page itself is just the launcher, reusing the
 * same xterm component and frame protocol as container exec.
 */

import { button, el, panel } from "../ui";

// xterm is loaded only when the shell is actually opened, keeping it out of the
// initial bundle.
const loadTerminal = () => import("../terminal");

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

export function createTerminalPage(hostname: () => string): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 p-3" });
  const { root: card, body: cardBody } = panel("host terminal");
  cardBody.className = "flex flex-col gap-2 px-3 py-2 text-[11px]";

  cardBody.append(
    el("div", {
      class: "text-slate-400",
      text: "A root login shell on this host, behind the same login gate as everything else. Output is capped like the container console.",
    }),
  );

  const launch = button("open shell", "text-emerald-400 hover:border-emerald-500", () => {
    const host = hostname() || "this host";
    void loadTerminal().then((terminal) =>
      terminal.openConsole("/api/shell/ws", "host shell", host),
    );
  });
  cardBody.append(el("div", { class: "flex gap-2" }, [launch]));
  root.append(card);

  root.append(
    (() => {
      const { root: notes, body: notesBody } = panel("notes");
      notesBody.className = "flex flex-col gap-1 px-3 py-2 text-[11px] text-slate-500";
      for (const line of [
        "The shell is /bin/bash -l as root; job control works (Ctrl+C, Ctrl+Z).",
        "Closing the modal hangs up the session; the child is reaped either way.",
        "Output is rate-capped at 64 KiB/s per client, like the container console.",
      ]) {
        notesBody.append(el("div", { text: `• ${line}` }));
      }
      notes.append(notesBody);
      return notes;
    })()
  );

  return { root };
}
