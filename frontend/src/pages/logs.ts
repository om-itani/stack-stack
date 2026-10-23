/**
 * Logs page (journald).
 *
 * Unit picker, severity filter, tail depth, client-side search, and a follow
 * mode that streams new lines over the WebSocket. Follow and tail share the
 * same server-side caps, so a chatty unit cannot freeze the page.
 */

import { logsApi } from "../api";
import type { LogEntry, LogsState } from "../api";
import { button, el, emptyNote, field, select, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

const LEVEL_CLASS: Record<number, string> = {
  0: "text-rose-500",
  1: "text-rose-500",
  2: "text-rose-400",
  3: "text-rose-400",
  4: "text-amber-400",
  5: "text-neutral-400",
  6: "text-neutral-400",
  7: "text-neutral-600",
};

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function formatTime(tsMs: number): string {
  return new Date(tsMs).toLocaleTimeString();
}

export function createLogsPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 p-3" });

  let state: LogsState | null = null;
  let entries: LogEntry[] = [];
  let unit = "";
  let priority = "";
  let lines = 200;
  let filter = "";
  let follow = false;
  let busy = false;
  let status = "";
  let loaded = false;
  let socket: WebSocket | null = null;

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-500" });
  const unitSelect = el("select", {
    class: "border border-white/5 bg-black/40 px-1.5 py-0.5 text-neutral-200 max-w-56",
  }) as HTMLSelectElement;
  unitSelect.append(el("option", { value: "", text: "all units" }));
  unitSelect.addEventListener("change", () => {
    unit = unitSelect.value;
    if (follow) restartFollow();
    else void tail();
  });

  const prioritySelect = select(
    priority,
    [["", "any severity"], ...([] as [string, string][])],
    (value) => {
      priority = value;
      if (follow) restartFollow();
      else void tail();
    },
  );
  prioritySelect.disabled = true; // filled once the state arrives

  const linesSelect = select(
    String(lines),
    [["100", "100"], ["200", "200"], ["500", "500"], ["1000", "1000"], ["2000", "2000"]],
    (value) => {
      lines = Number.parseInt(value, 10) || 200;
      void tail();
    },
  );

  const search = textInput("", (value) => {
    filter = value.toLowerCase();
    renderTable();
  }, { class: "w-48", placeholder: "filter…" });

  const followButton = button("follow", "", () => {
    follow = !follow;
    if (follow) restartFollow();
    else stopFollow();
  });
  const refreshButton = button("refresh", "", () => void tail());

  const header = el("div", { class: "flex flex-wrap items-end gap-2" }, [
    el("span", { class: "text-neutral-300", text: "logs" }),
    field("unit", unitSelect),
    field("severity", prioritySelect),
    field("lines", linesSelect),
    field("search", search),
    refreshButton,
    followButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  const tableHost = el("div", { class: "min-h-0 flex-1 overflow-auto" });
  const tableBody = el("tbody");
  const table = el("table", { class: "w-full text-[11px]" });
  table.append(
    el("thead", {}, [
      el("tr", {}, [
        el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "time" }),
        el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "unit" }),
        el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "msg" }),
      ]),
    ]),
    tableBody,
  );
  tableHost.append(table);

  root.append(header, tableHost);

  function renderChrome(): void {
    statusLine.textContent = status + (follow ? " · following" : "");
    followButton.textContent = follow ? "stop follow" : "follow";
    followButton.className = follow
      ? "text-emerald-400 hover:border-emerald-500"
      : "";
    refreshButton.disabled = busy;
  }

  async function tail(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    renderChrome();
    try {
      state = await logsApi.state(unit, lines, priority);
      entries = state.entries;
      if (state.units_error) status = state.units_error;
      else if (state.entries_error) status = state.entries_error;
      else status = "";
      syncSelectors();
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      loaded = true;
      renderChrome();
      renderTable();
    }
  }

  function syncSelectors(): void {
    if (!state || state.units.length === 0) return;
    const wanted = unit;
    unitSelect.replaceChildren(el("option", { value: "", text: "all units" }));
    for (const info of state.units) {
      const label = info.description
        ? `${info.unit} — ${info.description}`
        : info.unit;
      unitSelect.append(el("option", { value: info.unit, text: label }));
    }
    unitSelect.value = wanted;
    if (unitSelect.selectedIndex === -1) {
      unit = "";
      unitSelect.value = "";
    }
    if (prioritySelect.options.length <= 1 && state.priorities.length > 0) {
      prioritySelect.disabled = false;
      for (const [value, label] of state.priorities) {
        prioritySelect.append(el("option", { value, text: label }));
      }
      prioritySelect.value = priority;
    }
  }

  function renderTable(): void {
    tableBody.replaceChildren();
    if (!loaded) {
      tableBody.append(el("tr", {}, [el("td", { class: "px-2 py-2 text-neutral-600", text: "loading…" })]));
      return;
    }
    const visible = entries.filter((entry) =>
      filter === ""
        ? true
        : entry.message.toLowerCase().includes(filter) ||
          entry.unit.toLowerCase().includes(filter),
    );
    if (visible.length === 0) {
      tableBody.append(
        el("tr", {}, [el("td", { colspan: "3", class: "px-2 py-2", text: "" }), emptyNote(filter ? "no matching lines" : "no entries")]),
      );
      return;
    }
    for (const entry of visible) {
      tableBody.append(
        el("tr", { class: "border-b border-white/5" }, [
          el("td", { class: "tabular px-2 py-0.5 whitespace-nowrap text-neutral-600", text: formatTime(entry.ts_ms) }),
          el("td", { class: "px-2 py-0.5 whitespace-nowrap text-neutral-500", text: entry.unit }),
          el("td", {
            class: `px-2 py-0.5 break-all ${LEVEL_CLASS[entry.priority] ?? "text-neutral-400"}`,
            text: entry.message,
          }),
        ]),
      );
    }
  }

  function restartFollow(): void {
    stopFollow();
    follow = true;
    try {
      socket = new WebSocket(logsApi.followUrl(unit, priority));
    } catch (err) {
      status = errorText(err);
      follow = false;
      renderChrome();
      return;
    }
    socket.onmessage = (event) => {
      if (typeof event.data !== "string") return;
      try {
        const parsed = JSON.parse(event.data) as { type?: string; line?: string; data?: string };
        if (parsed.type === "line" && typeof parsed.line === "string") {
          appendFollowLine(parsed.line);
        } else if (typeof parsed.data === "string") {
          status = parsed.data.trim();
          renderChrome();
        }
      } catch {
        // ignore malformed frames
      }
    };
    socket.onclose = () => {
      follow = false;
      status = "follow ended";
      renderChrome();
    };
    renderChrome();
  }

  function stopFollow(): void {
    follow = false;
    socket?.close();
    socket = null;
    renderChrome();
  }

  function appendFollowLine(line: string): void {
    try {
      const value = JSON.parse(line) as Record<string, unknown>;
      const entry: LogEntry = {
        ts_ms: Number(value.__REALTIME_TIMESTAMP ?? 0) / 1000,
        priority: Number(value.PRIORITY ?? 6),
        unit: String(value._SYSTEMD_UNIT ?? value.SYSLOG_IDENTIFIER ?? value._COMM ?? "kernel"),
        message: String(value.MESSAGE ?? ""),
      };
      entries = [entry, ...entries].slice(0, 2000);
      renderTable();
    } catch {
      // journalctl only emits JSON here; ignore anything else.
    }
  }

  // Severity options arrive from the server state; seed the common set so the
  // control is usable before the first fetch completes.
  void tail();
  return { root };
}
