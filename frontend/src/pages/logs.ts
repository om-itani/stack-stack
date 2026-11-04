/**
 * Logs page (journald).
 *
 * Unit picker, severity filter, tail depth, client-side search, and a follow
 * mode that streams new lines over the WebSocket. Strictly isolates and follows
 * logs for the chosen unit only.
 */

import { logsApi } from "../api";
import type { LogEntry, LogsState } from "../api";
import { showToast } from "../toast";
import { button, debounce, el, emptyNote, field, select, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
  onHide?: () => void;
}

const DEFAULT_PRIORITIES: [string, string][] = [
  ["", "any severity"],
  ["3", "error"],
  ["4", "warning"],
  ["5", "notice"],
  ["6", "info"],
  ["7", "debug"],
];

const LEVEL_CLASS: Record<number, string> = {
  0: "text-rose-500 font-bold",
  1: "text-rose-500 font-semibold",
  2: "text-rose-400 font-semibold",
  3: "text-rose-400 font-medium",
  4: "text-amber-400",
  5: "text-neutral-300",
  6: "text-neutral-400",
  7: "text-neutral-500",
};

const LEVEL_LABEL: Record<number, string> = {
  0: "EMERG",
  1: "ALERT",
  2: "CRIT",
  3: "ERR",
  4: "WARN",
  5: "NOTE",
  6: "INFO",
  7: "DEBUG",
};

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function formatTime(tsMs: number): string {
  return new Date(tsMs).toLocaleTimeString();
}

function matchesUnit(entryUnit: string, selectedUnit: string): boolean {
  if (!selectedUnit) return true;
  const s = selectedUnit.toLowerCase().replace(/\.service$/, "");
  const e = entryUnit.toLowerCase().replace(/\.service$/, "");
  return e === s || e.startsWith(s);
}

export function createLogsPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 p-3 font-mono text-xs" });

  let state: LogsState | null = null;
  let entries: LogEntry[] = [];
  let unit = "";
  let priority = "";
  let lines = 200;
  let filter = "";
  let follow = false;
  let autoScroll = true;
  let busy = false;
  let status = "";
  let loaded = false;
  let socket: WebSocket | null = null;

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-400" });
  const countBadge = el("span", { class: "tabular text-[11px] text-neutral-500" });

  const unitSelect = el("select", {
    class: "border border-white/10 bg-black/50 px-2 py-1 text-neutral-200 rounded max-w-64 font-sans text-xs focus:border-cyan-500",
  }) as HTMLSelectElement;
  unitSelect.append(el("option", { value: "", text: "All Units (Global)" }));
  unitSelect.addEventListener("change", () => {
    switchUnit(unitSelect.value);
  });

  const prioritySelect = select(
    priority,
    DEFAULT_PRIORITIES,
    (value) => {
      priority = value;
      if (follow) restartFollow();
      else void tail();
    },
  );
  prioritySelect.className = "border border-white/10 bg-black/50 px-2 py-1 text-neutral-200 rounded font-sans text-xs";

  const linesSelect = select(
    String(lines),
    [["100", "100 lines"], ["200", "200 lines"], ["500", "500 lines"], ["1000", "1000 lines"], ["2000", "2000 lines"]],
    (value) => {
      lines = Number.parseInt(value, 10) || 200;
      void tail();
    },
  );
  linesSelect.className = "border border-white/10 bg-black/50 px-2 py-1 text-neutral-200 rounded font-sans text-xs";

  const debouncedRenderTable = debounce(renderTable, 100);
  const search = textInput("", (value) => {
    filter = value.toLowerCase();
    debouncedRenderTable();
  }, { class: "w-44 text-xs font-sans", placeholder: "Filter messages…" });

  const followButton = button("Follow Live", "px-3 py-1 font-sans text-xs font-semibold", () => {
    follow = !follow;
    if (follow) restartFollow();
    else stopFollow();
  });

  const refreshButton = button("Refresh", "px-3 py-1 font-sans text-xs", () => void tail());

  const autoScrollBtn = button(
    "Auto-scroll: ON",
    "px-2 py-1 font-sans text-[11px] text-neutral-400 hover:text-white border border-white/10",
    () => {
      autoScroll = !autoScroll;
      autoScrollBtn.textContent = autoScroll ? "Auto-scroll: ON" : "Auto-scroll: OFF";
      autoScrollBtn.className = autoScroll
        ? "px-2 py-1 font-sans text-[11px] text-cyan-400 border border-cyan-500/30 bg-cyan-500/10"
        : "px-2 py-1 font-sans text-[11px] text-neutral-400 hover:text-white border border-white/10";
    },
  );
  autoScrollBtn.className = "px-2 py-1 font-sans text-[11px] text-cyan-400 border border-cyan-500/30 bg-cyan-500/10";

  const clearBtn = button("Clear View", "px-2 py-1 font-sans text-[11px] text-neutral-400 hover:text-white border border-white/10", () => {
    entries = [];
    renderTable();
    showToast({ title: "View cleared", tone: "info" });
  });

  const copyBtn = button("Copy", "px-2 py-1 font-sans text-[11px] text-neutral-400 hover:text-white border border-white/10", () => {
    const text = entries
      .filter((e) => !unit || matchesUnit(e.unit, unit))
      .map((e) => `[${formatTime(e.ts_ms)}] [${e.unit}] [${LEVEL_LABEL[e.priority] ?? e.priority}] ${e.message}`)
      .join("\n");
    navigator.clipboard.writeText(text).then(() => {
      showToast({ title: `Copied ${entries.length} log lines to clipboard`, tone: "ok" });
    }).catch(() => {
      showToast({ title: "Clipboard copy failed", tone: "error" });
    });
  });

  const header = el("div", { class: "panel flex flex-wrap items-center gap-2 px-3 py-2" }, [
    el("span", { class: "text-neutral-200 font-semibold text-xs tracking-wider uppercase font-sans mr-1", text: "System Logs" }),
    field("unit", unitSelect),
    field("severity", prioritySelect),
    field("lines", linesSelect),
    field("filter", search),
    refreshButton,
    followButton,
    autoScrollBtn,
    clearBtn,
    copyBtn,
    el("div", { class: "ml-auto flex items-center gap-3" }, [statusLine, countBadge]),
  ]);

  const quickPills = el("div", { class: "flex flex-wrap items-center gap-1.5 px-1 py-1" });
  const pillLabel = el("span", { class: "text-[10px] font-semibold uppercase tracking-wider text-neutral-400 mr-1 font-sans", text: "Quick Presets:" });
  quickPills.append(pillLabel);

  const presets = [
    { label: "All Logs", unit: "" },
    { label: "stack-stack", unit: "stack-stack.service" },
    { label: "Docker", unit: "docker.service" },
    { label: "Kernel", unit: "kernel" },
  ];

  const pillButtons: HTMLButtonElement[] = [];
  const updatePillStyles = () => {
    presets.forEach((p, idx) => {
      const btn = pillButtons[idx];
      if (btn) {
        if (unit === p.unit) {
          btn.className = "rounded-full border border-cyan-500/50 bg-cyan-500/20 px-2.5 py-0.5 text-[11px] font-medium text-cyan-300 transition-colors font-sans";
        } else {
          btn.className = "rounded-full border border-white/10 bg-white/5 px-2.5 py-0.5 text-[11px] text-neutral-400 hover:text-neutral-200 hover:bg-white/10 transition-colors font-sans";
        }
      }
    });
  };

  presets.forEach((p) => {
    const btn = button(p.label, "", () => {
      switchUnit(p.unit);
    });
    pillButtons.push(btn);
    quickPills.append(btn);
  });
  updatePillStyles();

  function switchUnit(newUnit: string): void {
    if (unit === newUnit && loaded) return;
    unit = newUnit;

    // Ensure option exists in dropdown so it does not clear
    if (unit && !Array.from(unitSelect.options).some((o) => o.value === unit)) {
      unitSelect.append(el("option", { value: unit, text: unit }));
    }
    unitSelect.value = unit;
    updatePillStyles();

    // Immediately isolate: clear old entries so user only sees the selected unit
    entries = [];
    tableBody.replaceChildren(
      el("tr", { "data-empty": "true" }, [
        el("td", { colspan: "4", class: "px-3 py-6 text-center text-neutral-400" }, [
          el("span", { class: "animate-pulse", text: `Loading logs for ${unit || "all units"}…` }),
        ]),
      ]),
    );

    if (follow) restartFollow();
    else void tail();
  }

  const tableHost = el("div", { class: "min-h-0 flex-1 overflow-auto rounded border border-white/5 bg-[#0a0d14] p-1 font-mono" });
  const tableBody = el("tbody");
  const table = el("table", { class: "w-full text-left text-[11px] border-collapse" });
  table.append(
    el("thead", { class: "sticky top-0 bg-[#0d121c] border-b border-white/10 text-neutral-400 text-[10px] uppercase tracking-wider z-10" }, [
      el("tr", {}, [
        el("th", { class: "px-2 py-1.5 w-24 text-neutral-500", text: "Time" }),
        el("th", { class: "px-2 py-1.5 w-16 text-neutral-500", text: "Level" }),
        el("th", { class: "px-2 py-1.5 w-48 text-neutral-500", text: "Unit / Source" }),
        el("th", { class: "px-2 py-1.5 text-neutral-500", text: "Message" }),
      ]),
    ]),
    tableBody,
  );
  tableHost.append(table);

  root.append(header, quickPills, tableHost);

  function renderChrome(): void {
    statusLine.textContent = status + (follow ? " · 🔴 live follow" : "");
    followButton.textContent = follow ? "Stop Follow" : "Follow Live";
    followButton.className = follow
      ? "px-3 py-1 font-sans text-xs font-semibold rounded border border-rose-500/40 bg-rose-500/20 text-rose-300 hover:bg-rose-500/30"
      : "px-3 py-1 font-sans text-xs font-semibold rounded border border-cyan-500/40 bg-cyan-500/20 text-cyan-300 hover:bg-cyan-500/30";
    refreshButton.disabled = busy;
  }

  async function tail(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    renderChrome();
    try {
      state = await logsApi.state(unit, lines, priority);
      // Strictly isolate: assign entries returned for this unit
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
      if (autoScroll) {
        tableHost.scrollTop = 0;
      }
    }
  }

  function syncSelectors(): void {
    if (state && state.units.length > 0) {
      const wanted = unit;
      const currentOptions = Array.from(unitSelect.options).map((o) => o.value);
      for (const info of state.units) {
        if (!currentOptions.includes(info.unit)) {
          const label = info.description ? `${info.unit} (${info.description})` : info.unit;
          unitSelect.append(el("option", { value: info.unit, text: label }));
        }
      }
      unitSelect.value = wanted;
    }
    if (state && state.priorities.length > 0) {
      const current = priority;
      prioritySelect.replaceChildren(el("option", { value: "", text: "any severity" }));
      for (const [val, lbl] of state.priorities) {
        prioritySelect.append(el("option", { value: val, text: lbl }));
      }
      prioritySelect.value = current;
    }
  }

  function renderLogRow(entry: LogEntry): HTMLElement {
    const tr = el("tr", { class: "hover:bg-white/[0.03] border-b border-white/[0.02] transition-colors leading-relaxed" });

    const timeTd = el("td", { class: "px-2 py-0.5 whitespace-nowrap text-neutral-500 tabular" });
    timeTd.textContent = formatTime(entry.ts_ms);

    const levelTd = el("td", { class: "px-2 py-0.5 whitespace-nowrap" });
    const levelTag = el("span", {
      class: `text-[10px] uppercase font-bold tracking-wider ${LEVEL_CLASS[entry.priority] ?? "text-neutral-400"}`,
      text: LEVEL_LABEL[entry.priority] ?? String(entry.priority),
    });
    levelTd.append(levelTag);

    const unitTd = el("td", { class: "px-2 py-0.5 whitespace-nowrap text-cyan-400/80 truncate max-w-48 font-medium" });
    unitTd.textContent = entry.unit;
    unitTd.title = entry.unit;

    const msgTd = el("td", {
      class: `px-2 py-0.5 break-all ${LEVEL_CLASS[entry.priority] ?? "text-neutral-300"}`,
      text: entry.message,
    });

    tr.append(timeTd, levelTd, unitTd, msgTd);
    return tr;
  }

  function renderTable(): void {
    tableBody.replaceChildren();
    if (!loaded && entries.length === 0) {
      tableBody.append(
        el("tr", { "data-empty": "true" }, [
          el("td", { colspan: "4", class: "px-3 py-6 text-center text-neutral-400" }, [
            el("span", { class: "animate-pulse", text: "Loading system journal…" }),
          ]),
        ]),
      );
      countBadge.textContent = "";
      return;
    }

    // Filter strictly by unit and by search filter
    const visible = entries.filter((entry) => {
      if (unit && !matchesUnit(entry.unit, unit)) return false;
      if (!filter) return true;
      return (
        entry.message.toLowerCase().includes(filter) ||
        entry.unit.toLowerCase().includes(filter)
      );
    });

    countBadge.textContent = `${visible.length} lines`;

    if (visible.length === 0) {
      tableBody.append(
        el("tr", { "data-empty": "true" }, [
          el("td", { colspan: "4", class: "px-3 py-6 text-center text-neutral-500" }, [
            emptyNote(
              filter
                ? `No log lines matching "${filter}"`
                : unit
                  ? `No recent log entries found for ${unit}`
                  : "No log entries found in current boot",
            ),
          ]),
        ]),
      );
      return;
    }

    for (const entry of visible) {
      tableBody.append(renderLogRow(entry));
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
        // ignore
      }
    };

    socket.onclose = () => {
      follow = false;
      status = "live follow ended";
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

  function pauseFollow(): void {
    if (follow) {
      follow = false;
      socket?.close();
      socket = null;
      status = "live follow paused";
      renderChrome();
    }
  }

  const onVisibilityChange = (): void => {
    if (document.hidden && follow) {
      pauseFollow();
    }
  };
  document.addEventListener("visibilitychange", onVisibilityChange);

  function appendFollowLine(line: string): void {
    try {
      const value = JSON.parse(line) as Record<string, unknown>;
      const entry: LogEntry = {
        ts_ms: Number(value.__REALTIME_TIMESTAMP ?? 0) / 1000,
        priority: Number(value.PRIORITY ?? 6),
        unit: String(value._SYSTEMD_UNIT ?? value.SYSLOG_IDENTIFIER ?? value._COMM ?? "kernel"),
        message: String(value.MESSAGE ?? ""),
      };

      // Strict unit check for incoming live lines
      if (unit && !matchesUnit(entry.unit, unit)) {
        return;
      }

      entries.unshift(entry);
      if (entries.length > 3000) {
        entries.pop();
      }

      if (!loaded) return;

      const matchesFilter =
        !filter ||
        entry.message.toLowerCase().includes(filter) ||
        entry.unit.toLowerCase().includes(filter);

      if (matchesFilter) {
        const first = tableBody.firstElementChild;
        if (first && first.getAttribute("data-empty") === "true") {
          tableBody.replaceChildren();
        }
        tableBody.prepend(renderLogRow(entry));
        countBadge.textContent = `${entries.length} lines`;

        if (autoScroll) {
          tableHost.scrollTop = 0;
        }
      }
    } catch {
      // ignore parse faults
    }
  }

  return {
    root,
    onShow: () => {
      if (!loaded) void tail();
      else if (follow) restartFollow();
    },
    onHide: () => {
      if (follow) pauseFollow();
    },
  };
}
