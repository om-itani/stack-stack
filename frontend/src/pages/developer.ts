/**
 * Developer / diagnostics page.
 *
 * Lives under System. It exists to answer "what is actually broken" without an
 * SSH session: ping every read route from the server, read back the config
 * documents the backend saved, tail this service's own journal, and fire the
 * force-error buttons that exercise the toast pipeline.
 */

import { debugApi, mcpApi } from "../api";
import type { ConfigRef, PingReport } from "../api";
import { showToast } from "../toast";
import { badge, button, el, panel, select, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function statusBadge(status: number, ok: boolean): HTMLElement {
  if (ok) return badge(`${status} ok`, "ok");
  if (status === 0) return badge("unreachable", "err");
  return badge(`${status} error`, "err");
}

export function createDeveloperPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-4" });

  const status = el("div", { class: "tabular text-[11px] text-neutral-500" });
  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-2 px-3 py-2" });

  const pingHost = el("div", { class: "flex flex-col gap-2" });
  const configHost = el("div", { class: "flex flex-col gap-2" });
  const logHost = el("div", { class: "flex flex-col gap-2" });
  const mcpHost = el("div", { class: "flex flex-col gap-2" });

  async function loadMcp(): Promise<void> {
    const card = panel("AI Integration & Model Context Protocol (MCP)");
    card.body.className = "flex flex-col gap-3 p-3";

    const host = window.location.host;
    const endpointUrl = `${window.location.protocol}//${host}/api/mcp`;

    const statusRow = el("div", { class: "flex flex-wrap items-center justify-between gap-2 border-b border-white/5 pb-2" }, [
      el("div", { class: "flex items-center gap-2" }, [
        el("span", { class: "text-xs font-semibold text-neutral-200", text: "MCP Server Endpoint" }),
        badge("Active (2024-11-05)", "ok"),
      ]),
      el("code", { class: "font-mono text-xs text-emerald-400 bg-black/40 px-2 py-0.5 rounded border border-white/5", text: endpointUrl }),
    ]);

    const infoText = el("p", {
      class: "text-xs text-neutral-400 leading-relaxed",
      text: "Connect AI agents (Claude Desktop, Cursor, Antigravity, or custom agents) via Model Context Protocol to inspect, test, and control your homeserver without requiring terminal access.",
    });

    const claudeConfig = JSON.stringify({
      mcpServers: {
        "stack-stack": {
          url: endpointUrl,
        },
      },
    }, null, 2);

    const configBox = el("details", { class: "flex flex-col gap-1 text-xs text-neutral-300 bg-black/30 p-2 rounded border border-white/5" }, [
      el("summary", { class: "cursor-pointer font-semibold text-neutral-400 hover:text-white", text: "AI Client Configuration (Claude Desktop / Cursor)" }),
      el("pre", { class: "font-mono text-[11px] text-emerald-300 p-2 bg-[#090d16] rounded overflow-x-auto mt-2", text: claudeConfig }),
    ]);

    // Live AI Self-Test Runner
    const testOutput = el("pre", {
      class: "font-mono text-[11px] text-neutral-300 p-3 bg-[#090d16] rounded border border-white/5 overflow-x-auto min-h-16 whitespace-pre-wrap",
      text: "Click 'Run AI Diagnostic Self-Test' to evaluate homeserver health, Samba (port 445), Docker, and system resources via MCP.",
    });

    const runTestBtn = button(
      "⚡ Run AI Diagnostic Self-Test",
      "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold text-xs px-3 py-1.5 hover:bg-emerald-500/30",
      async () => {
        runTestBtn.disabled = true;
        runTestBtn.textContent = "Running Self-Test…";
        testOutput.textContent = "Executing self-test via MCP…";
        try {
          const report = await mcpApi.callTool("run_diagnostic_self_test");
          testOutput.textContent = report;
          showToast({ tone: "ok", title: "Diagnostic Complete", message: "Homeserver self-test completed." });
        } catch (err) {
          testOutput.textContent = `Error: ${errorText(err)}`;
          showToast({ tone: "error", title: "Self-Test Error", message: errorText(err) });
        } finally {
          runTestBtn.disabled = false;
          runTestBtn.textContent = "⚡ Run AI Diagnostic Self-Test";
        }
      },
    );

    // Network Port Probe tool
    let probeHost = "127.0.0.1";
    let probePort = 445;
    const probeHostInput = textInput(probeHost, (v) => { probeHost = v; }, { placeholder: "192.168.0.3 or 127.0.0.1" });
    const probePortInput = textInput(String(probePort), (v) => { probePort = parseInt(v) || 445; }, { placeholder: "445" });
    const probeResult = el("span", { class: "text-xs font-mono text-neutral-400" });

    const probeBtn = button(
      "Probe Port",
      "border-indigo-600 bg-indigo-500/20 text-indigo-300 text-xs px-3 py-1 hover:bg-indigo-500/30",
      async () => {
        probeBtn.disabled = true;
        probeResult.textContent = "Probing…";
        try {
          const res = await mcpApi.callTool("test_network_port", { host: probeHost.trim(), port: probePort, timeout_seconds: 3 });
          const parsed = JSON.parse(res);
          probeResult.textContent = parsed.status || res;
          probeResult.className = parsed.reachable ? "text-xs font-mono text-emerald-400" : "text-xs font-mono text-rose-400";
        } catch (err) {
          probeResult.textContent = errorText(err);
          probeResult.className = "text-xs font-mono text-rose-400";
        } finally {
          probeBtn.disabled = false;
        }
      },
    );

    const probeRow = el("div", { class: "flex flex-wrap items-center gap-2 p-2.5 bg-white/[0.02] border border-white/5 rounded-lg" }, [
      el("span", { class: "text-xs font-semibold text-neutral-300", text: "Port Reachability Tester:" }),
      fieldLike("Host", probeHostInput),
      fieldLike("Port", probePortInput),
      probeBtn,
      probeResult,
    ]);

    card.body.append(statusRow, infoText, configBox, probeRow, el("div", { class: "flex items-center justify-between" }, [runTestBtn]), testOutput);
    mcpHost.replaceChildren(card.root);
  }

  let configs: ConfigRef[] = [];
  let selectedConfig = "network";

  function setStatus(text: string, tone: "muted" | "ok" | "error" = "muted"): void {
    status.textContent = text;
    status.className =
      tone === "error"
        ? "tabular text-[11px] text-rose-400"
        : tone === "ok"
          ? "tabular text-[11px] text-emerald-400"
          : "tabular text-[11px] text-neutral-500";
  }

  // -------------------------------------------------------------------------
  // Ping all endpoints
  // -------------------------------------------------------------------------

  async function pingAll(): Promise<void> {
    setStatus("pinging every read route…");
    pingHost.replaceChildren(el("div", { class: "text-[11px] text-neutral-500", text: "pinging…" }));
    try {
      const report = await debugApi.ping();
      renderPing(report);
      setStatus(`${report.ok_count} ok, ${report.fail_count} failed`, report.fail_count ? "error" : "ok");
    } catch (err) {
      pingHost.replaceChildren(
        el("div", { class: "text-[11px] text-rose-400", text: errorText(err) }),
      );
      setStatus(errorText(err), "error");
    }
  }

  function renderPing(report: PingReport): void {
    const card = panel("endpoint ping");
    card.body.className = "flex flex-col gap-1 px-3 py-2";
    for (const result of report.results) {
      const row = el("div", { class: "flex items-center gap-3 text-[11px]" });
      row.append(
        el("span", { class: "tabular text-neutral-500", text: result.method }),
        el("span", { class: "min-w-0 flex-1 truncate text-neutral-200", text: result.path }),
        statusBadge(result.status, result.ok),
        el("span", { class: "tabular w-16 text-right text-neutral-500", text: `${result.ms} ms` }),
      );
      card.body.append(row);
      if (result.error) {
        card.body.append(
          el("div", { class: "pl-10 text-[10px] text-rose-400", text: result.error }),
        );
      }
    }
    pingHost.replaceChildren(card.root);
  }

  // -------------------------------------------------------------------------
  // Raw config viewer
  // -------------------------------------------------------------------------

  async function loadConfigs(): Promise<void> {
    try {
      configs = (await debugApi.configs()).configs;
      if (configs.length > 0 && !configs.some((entry) => entry.name === selectedConfig)) {
        selectedConfig = configs[0].name;
      }
      renderConfig();
    } catch (err) {
      configHost.replaceChildren(
        el("div", { class: "text-[11px] text-rose-400", text: errorText(err) }),
      );
    }
  }

  function renderConfig(): void {
    const card = panel("raw config");
    card.body.className = "flex flex-col gap-2 px-3 py-2";

    // Never say "missing": a document that has not been written yet is still
    // shown, because the endpoint returns the effective (defaults-merged) JSON.
    const options: [string, string][] = configs.map((entry) => [
      entry.name,
      entry.exists ? entry.name : `${entry.name} (defaults)`,
    ]);
    const bar = el("div", { class: "flex flex-wrap items-end gap-3" });
    bar.append(
      fieldLike(
        "document",
        select(selectedConfig, options.length ? options : [[selectedConfig, selectedConfig]], (value) => {
          selectedConfig = value;
          void showConfig();
        }),
      ),
    );
    card.body.append(bar);

    const pre = el("pre", {
      class:
        "max-h-80 overflow-auto rounded-md border border-white/10 bg-[#0d0d0d] p-2 text-[11px] text-neutral-300",
      text: "select a document",
    });
    card.body.append(pre);
    configHost.replaceChildren(card.root);

    async function showConfig(): Promise<void> {
      pre.textContent = "loading…";
      try {
        const body = await debugApi.config(selectedConfig);
        // Lead with where the values came from, so "defaults (no file)" is
        // obvious when the document was never written.
        const header = [
          `// ${body.path}`,
          `// source: ${body.source}`,
          ...(body.error ? [`// error: ${body.error}`] : []),
          "",
          "",
        ].join("\n");
        pre.textContent = header + JSON.stringify(body.json, null, 2);
      } catch (err) {
        pre.textContent = errorText(err);
      }
    }
    void showConfig();
  }

  // -------------------------------------------------------------------------
  // System log
  // -------------------------------------------------------------------------

  async function loadSystemLog(): Promise<void> {
    const card = panel("system log");
    card.body.className = "flex flex-col gap-2 px-3 py-2";
    const refresh = button("refresh", "", () => void loadSystemLog());
    card.body.append(
      el("div", { class: "flex flex-wrap items-center gap-2" }, [
        el("span", {
          class: "text-[11px] text-neutral-500",
          text: "last 50 lines of the stack-stack service",
        }),
        el("span", { class: "ml-auto" }),
        refresh,
      ]),
    );
    const logBox = el("pre", {
      class:
        "max-h-80 overflow-auto rounded-md border border-white/10 bg-[#0d0d0d] p-2 text-[11px] text-neutral-300",
      text: "loading…",
    });
    card.body.append(logBox);
    logHost.replaceChildren(card.root);

    try {
      const log = await debugApi.systemLog();
      if (!log.available) {
        logBox.textContent = "journalctl is not available on this host";
        return;
      }
      // Shown verbatim: journald's own line format is more reliable than any
      // column splitting we could do here.
      logBox.textContent = log.raw.trim() || (log.error ?? "no recent entries");
    } catch (err) {
      logBox.textContent = errorText(err);
    }
  }

  // -------------------------------------------------------------------------
  // Chrome
  // -------------------------------------------------------------------------

  toolbar.append(
    el("span", { class: "panel-title text-[10px]", text: "developer" }),
    button("ping all endpoints", "text-emerald-400 hover:border-emerald-500", () => void pingAll()),
    button("refresh configs", "", () => void loadConfigs()),
    el("span", { class: "ml-auto" }),
    button("force frontend error", "text-rose-400 hover:border-rose-500", () => {
      // Thrown from a timer so it surfaces as a window `error` event, which is
      // exactly what the global boundary must catch.
      window.setTimeout(() => {
        throw new Error("Forced frontend error (Developer page)");
      });
    }),
    button("force api error", "text-rose-400 hover:border-rose-500", async () => {
      try {
        await debugApi.forceError();
        // Reaching here means the 400 was swallowed somewhere; say so loudly.
        showToast({
          tone: "error",
          title: "Force error did not fail",
          message: "the endpoint was expected to return a 400",
        });
      } catch {
        // The API layer already logged and toasted the 400; confirm to the
        // operator that the handler round-tripped as designed.
        showToast({
          tone: "ok",
          title: "Error handler works!",
          message: "The forced 400 was caught, logged and shown as a toast.",
        });
      }
    }),
    status,
  );

  root.append(toolbar, mcpHost, pingHost, configHost, logHost);

  return {
    root,
    onShow: () => {
      void loadMcp();
      void pingAll();
      void loadConfigs();
      void loadSystemLog();
    },
  };
}

/** A labelled field without pulling in the wider form helpers. */
function fieldLike(label: string, control: HTMLElement): HTMLElement {
  return el("label", { class: "flex flex-col gap-0.5 text-[10px] text-neutral-500" }, [
    el("span", { text: label }),
    control,
  ]);
}
