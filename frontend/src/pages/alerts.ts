/**
 * Alerts page.
 *
 * Two halves of one document: the notification providers (Discord, Slack,
 * generic webhook, email) a rule can target, and the rules themselves —
 * "if [metric] is [condition] [threshold] for [duration], send to [provider]".
 * The whole document is edited with modern modals and saved in one `PUT`, mirroring
 * the other document-backed pages.
 */

import { alertsApi, healthApi } from "../api";
import type {
  AlertComparison,
  AlertRule,
  AlertsState,
  HealthCheck,
  HealthState,
  ProbeKind,
  ProviderConfig,
  ProviderKind,
} from "../api";
import { showToast } from "../toast";
import {
  badge,
  button,
  checkbox,
  el,
  emptyNote,
  field,
  numberInput,
  openModal,
  panel,
  select,
  textInput,
} from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

const PROVIDER_KINDS: [ProviderKind, string][] = [
  ["discord", "Discord Webhook"],
  ["slack", "Slack Webhook"],
  ["webhook", "Generic HTTP Webhook"],
  ["email", "Email (sendmail / SMTP)"],
];

const CONDITIONS: [AlertComparison, string][] = [
  [">", "is above (>)"],
  ["<", "is below (<)"],
  ["==", "equals (==)"],
  ["!=", "is not (!=)"],
];

/** Continuous metrics are sampled floats, so only `container_status` (1/0) gets
 * equality; the backend rejects `==`/`!=` for anything else. */
const CONTINUOUS_CONDITIONS: [AlertComparison, string][] = CONDITIONS.filter(
  ([condition]) => condition !== "==" && condition !== "!=",
);

/** Base metric names; `disk_usage` and `container_status` take a target. */
const METRICS: [string, string][] = [
  ["cpu_usage", "CPU Usage (%)"],
  ["memory_usage", "Memory Usage (%)"],
  ["load1", "Load Average (1m)"],
  ["disk_usage", "Disk Usage (%)"],
  ["container_status", "Container Running Status (1/0)"],
];

function metricBase(metric: string): string {
  const [base] = metric.split(":");
  return base;
}

function metricTarget(metric: string): string {
  const index = metric.indexOf(":");
  return index < 0 ? "" : metric.slice(index + 1);
}

function needsTarget(base: string): boolean {
  return base === "disk_usage" || base === "container_status";
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function createAlertsPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-4" });

  let draft: AlertsState = { providers: [], rules: [], doc_error: null };
  let healthDraft: HealthState = { checks: [], doc_error: null };
  const status = el("div", { class: "tabular text-[11px] text-neutral-500" });

  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-3 px-3 py-2" });
  toolbar.append(
    el("div", { class: "flex items-center gap-2" }, [
      el("span", { class: "panel-title text-xs font-semibold text-neutral-200", text: "Alerting & Probes" }),
      el("span", { class: "text-[11px] text-neutral-500", text: "· Homelab telemetry thresholds & notification relays" }),
    ]),
    el("div", { class: "ml-auto" }),
    status,
  );

  const providersHost = el("div", { class: "flex flex-col gap-2" });
  const rulesHost = el("div", { class: "flex flex-col gap-2" });
  const healthHost = el("div", { class: "flex flex-col gap-2" });

  function setStatus(text: string, tone: "muted" | "ok" | "error" = "muted"): void {
    status.textContent = text;
    status.className =
      tone === "error"
        ? "tabular text-[11px] text-rose-400"
        : tone === "ok"
          ? "tabular text-[11px] text-emerald-400"
          : "tabular text-[11px] text-neutral-500";
  }

  async function load(): Promise<void> {
    setStatus("loading…");
    try {
      const [alerts, health] = await Promise.all([alertsApi.state(), healthApi.state()]);
      draft = alerts;
      healthDraft = health;
      render();
      setStatus("");
    } catch (err) {
      setStatus(errorText(err), "error");
    }
  }

  async function save(): Promise<void> {
    setStatus("saving…");
    try {
      const [alerts, health] = await Promise.all([
        alertsApi.save({ providers: draft.providers, rules: draft.rules }),
        healthApi.save({ checks: healthDraft.checks }),
      ]);
      draft = alerts;
      healthDraft = health;
      render();
      setStatus("saved", "ok");
      showToast({ tone: "ok", title: "Alerts Saved", message: "Alert configuration and health probes updated successfully" });
    } catch (err) {
      setStatus(errorText(err), "error");
      showToast({ tone: "error", title: "Save Failed", message: errorText(err) });
    }
  }

  function render(): void {
    if (draft.doc_error) {
      providersHost.replaceChildren(
        el("div", { class: "text-[11px] text-rose-400", text: draft.doc_error }),
      );
    } else {
      renderProviders();
    }
    renderRules();
    renderHealth();
  }

  // -------------------------------------------------------------------------
  // Providers Modal & Table
  // -------------------------------------------------------------------------

  function openProviderModal(existing?: ProviderConfig, idx?: number): void {
    const isNew = existing === undefined;
    const modal = openModal(isNew ? "Add Notification Provider" : `Edit Provider · ${existing.name || existing.id}`, "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    const row: ProviderConfig = existing
      ? structuredClone(existing)
      : {
          id: `provider-${draft.providers.length + 1}`,
          name: "",
          kind: "discord",
          url: "",
          to: null,
          enabled: true,
        };

    const isEmail = () => row.kind === "email";
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const idInput = textInput(row.id, (v) => { row.id = v.trim(); }, { placeholder: "e.g. discord-homelab" });
    const nameInput = textInput(row.name, (v) => { row.name = v; }, { placeholder: "e.g. Homelab Discord Channel" });

    const kindSelect = select(row.kind, PROVIDER_KINDS, (v) => {
      row.kind = v as ProviderKind;
      updateTargetField();
    });

    const targetContainer = el("div", { class: "flex flex-col gap-1" });
    function updateTargetField(): void {
      targetContainer.replaceChildren();
      if (isEmail()) {
        const emailInput = textInput(row.to ?? "", (v) => {
          row.to = v.trim() ? v.trim() : null;
        }, { placeholder: "admin@example.com" });
        targetContainer.append(field("Recipient Email Address", emailInput, "Destination mailbox for alert notifications."));
      } else {
        const urlInput = textInput(row.url ?? "", (v) => {
          row.url = v.trim() ? v.trim() : null;
        }, { placeholder: "https://discord.com/api/webhooks/..." });
        targetContainer.append(field("Webhook URL", urlInput, "Full HTTPS webhook endpoint for dispatching alerts."));
      }
    }
    updateTargetField();

    const enabledCheck = checkbox(row.enabled, (v) => { row.enabled = v; }, "Enable this provider");

    modal.body.append(
      field("Provider Identifier", idInput, "Unique alphanumeric identifier referenced by alert rules."),
      field("Display Name", nameInput, "Human-friendly label."),
      field("Channel Kind", kindSelect),
      targetContainer,
      el("div", { class: "p-2 bg-black/20 rounded border border-white/5" }, [enabledCheck]),
      errorEl,
    );

    const testBtn = button("Test Webhook", "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm", async () => {
      errorEl.textContent = "";
      if (!row.id.trim()) {
        errorEl.textContent = "Identifier is required.";
        return;
      }
      testBtn.disabled = true;
      testBtn.textContent = "Testing…";
      try {
        if (isNew) {
          draft.providers.push(row);
        } else if (idx !== undefined) {
          draft.providers[idx] = row;
        }
        draft = await alertsApi.save({ providers: draft.providers, rules: draft.rules });
        const res = await alertsApi.test(row.id);
        showToast({ tone: "ok", title: "Test Sent", message: res.detail || "Test notification dispatched" });
      } catch (e) {
        errorEl.textContent = errorText(e);
        showToast({ tone: "error", title: "Test Failed", message: errorText(e) });
      } finally {
        testBtn.disabled = false;
        testBtn.textContent = "Test Webhook";
        renderProviders();
      }
    });

    const saveBtn = button(isNew ? "Add Provider" : "Save Changes", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", () => {
      errorEl.textContent = "";
      if (!row.id.trim()) {
        errorEl.textContent = "Identifier is required.";
        return;
      }
      if (isEmail() && !row.to) {
        errorEl.textContent = "Recipient email is required.";
        return;
      }
      if (!isEmail() && !row.url) {
        errorEl.textContent = "Webhook URL is required.";
        return;
      }

      if (isNew) {
        draft.providers.push(row);
      } else if (idx !== undefined) {
        draft.providers[idx] = row;
      }
      modal.close();
      renderProviders();
    });

    modal.body.append(
      el("div", { class: "flex items-center justify-between pt-2 border-t border-white/10" }, [
        testBtn,
        el("div", { class: "flex items-center gap-2" }, [
          button("Cancel", "text-neutral-400 hover:text-white text-xs px-3 py-1", () => modal.close()),
          saveBtn,
        ]),
      ]),
    );
  }

  function renderProviders(): void {
    providersHost.replaceChildren();
    const card = panel("notification providers");
    card.body.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "Notification Channels" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Dispatch targets for telemetry threshold alerts and failed probes." }),
      ]),
      button(
        "+ Add Provider",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openProviderModal(),
      ),
    );
    card.body.append(topBar);

    if (draft.providers.length === 0) {
      card.body.append(emptyNote("No notification providers configured yet. Click '+ Add Provider' above to link Discord, Slack, Webhook, or Email."));
      providersHost.append(card.root);
      return;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "Provider Name" }),
          el("th", { class: "py-2 px-3", text: "Identifier" }),
          el("th", { class: "py-2 px-3", text: "Channel Type" }),
          el("th", { class: "py-2 px-3", text: "Target URL / Recipient" }),
          el("th", { class: "py-2 px-3", text: "Status" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    draft.providers.forEach((prov, idx) => {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
      const targetStr = prov.kind === "email" ? (prov.to || "—") : (prov.url || "—");

      tr.append(
        el("td", { class: "py-2 px-3 text-neutral-200 font-sans font-medium", text: prov.name || prov.id }),
        el("td", { class: "py-2 px-3 text-neutral-400", text: prov.id }),
        el("td", { class: "py-2 px-3 font-sans" }, [
          prov.kind === "discord"
            ? badge("Discord", "ok")
            : prov.kind === "slack"
              ? badge("Slack", "ok")
              : prov.kind === "email"
                ? badge("Email", "warn")
                : badge("Webhook", "muted"),
        ]),
        el("td", { class: "py-2 px-3 text-neutral-400 truncate max-w-xs", text: targetStr }),
        el("td", { class: "py-2 px-3 font-sans" }, [
          prov.enabled ? badge("Enabled", "ok") : badge("Disabled", "muted"),
        ]),
        el("td", { class: "py-2 px-3 text-right font-sans flex items-center justify-end gap-1" }, [
          button("Test", "text-neutral-300 hover:text-white px-2 py-0.5 text-xs", async () => {
            try {
              draft = await alertsApi.save({ providers: draft.providers, rules: draft.rules });
              const res = await alertsApi.test(prov.id);
              showToast({ tone: "ok", title: "Test Sent", message: res.detail || `Test alert dispatched to ${prov.name || prov.id}` });
            } catch (e) {
              showToast({ tone: "error", title: "Test Failed", message: errorText(e) });
            }
          }),
          button("Edit", "text-neutral-300 hover:text-white px-2 py-0.5 text-xs", () => openProviderModal(prov, idx)),
          button("Delete", "text-rose-400 hover:text-rose-300 px-2 py-0.5 text-xs", () => {
            if (window.confirm(`Delete notification provider '${prov.name || prov.id}'?`)) {
              draft.providers.splice(idx, 1);
              render();
            }
          }),
        ]),
      );
      tbody.append(tr);
    });
    table.append(tbody);
    card.body.append(table);
    providersHost.append(card.root);
  }

  // -------------------------------------------------------------------------
  // Rules Modal & Table
  // -------------------------------------------------------------------------

  function openRuleModal(existing?: AlertRule, idx?: number): void {
    const isNew = existing === undefined;
    const modal = openModal(isNew ? "Add Alert Rule" : `Edit Alert Rule · ${existing.name || existing.id}`, "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    const row: AlertRule = existing
      ? structuredClone(existing)
      : {
          id: `rule-${draft.rules.length + 1}`,
          name: "",
          metric: "cpu_usage",
          condition: ">",
          threshold: 90,
          duration_s: 300,
          action: draft.providers[0]?.id ?? "",
          enabled: true,
          cooldown_s: 3600,
          last_fired: null,
        };

    let base = metricBase(row.metric);
    let target = metricTarget(row.metric);
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const idInput = textInput(row.id, (v) => { row.id = v.trim(); }, { placeholder: "e.g. high-cpu" });
    const nameInput = textInput(row.name, (v) => { row.name = v; }, { placeholder: "e.g. Sustained High CPU Usage" });

    const targetInput = textInput(target, (v) => {
      target = v.trim();
      row.metric = target ? `${base}:${target}` : base;
    }, { placeholder: base === "disk_usage" ? "/mnt/data" : "caddy" });

    const metricContainer = el("div", { class: "flex flex-col gap-2" });
    const metricSelect = select(base, METRICS, (v) => {
      base = v;
      if (base !== "container_status" && (row.condition === "==" || row.condition === "!=")) {
        row.condition = ">";
      }
      row.metric = needsTarget(base) && target ? `${base}:${target}` : base;
      updateMetricView();
    });

    function updateMetricView(): void {
      metricContainer.replaceChildren();
      metricContainer.append(field("System Telemetry Metric", metricSelect));
      if (needsTarget(base)) {
        metricContainer.append(
          field(base === "disk_usage" ? "Disk Mount Target" : "Container Name", targetInput, "Specific partition mount path or container name"),
        );
      }
    }
    updateMetricView();

    const condOptions = base === "container_status" ? CONDITIONS : CONTINUOUS_CONDITIONS;
    const condSelect = select(row.condition, condOptions, (v) => { row.condition = v as AlertComparison; });
    const thresholdInput = numberInput(row.threshold, (v) => { row.threshold = v; });
    const durationInput = numberInput(row.duration_s, (v) => { row.duration_s = v; }, { min: 0 });
    const cooldownInput = numberInput(row.cooldown_s, (v) => { row.cooldown_s = v; }, { min: 0 });

    const provOptions: [string, string][] = draft.providers.map((p) => [p.id, p.name || p.id]);
    const actionSelect = select(row.action, provOptions, (v) => { row.action = v; });
    const enabledCheck = checkbox(row.enabled, (v) => { row.enabled = v; }, "Enable alert rule immediately");

    modal.body.append(
      field("Rule Identifier", idInput),
      field("Rule Name", nameInput),
      metricContainer,
      el("div", { class: "grid grid-cols-2 gap-2" }, [
        field("Condition", condSelect),
        field("Threshold Value", thresholdInput),
      ]),
      el("div", { class: "grid grid-cols-2 gap-2" }, [
        field("Sustained Duration (s)", durationInput, "Seconds condition must hold before firing."),
        field("Cooldown Period (s)", cooldownInput, "Seconds to wait between repeat notifications."),
      ]),
      field("Dispatch to Provider", actionSelect),
      el("div", { class: "p-2 bg-black/20 rounded border border-white/5" }, [enabledCheck]),
      errorEl,
    );

    const saveBtn = button(isNew ? "Add Rule" : "Save Changes", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", () => {
      errorEl.textContent = "";
      if (!row.id.trim()) {
        errorEl.textContent = "Identifier is required.";
        return;
      }
      if (isNew) {
        draft.rules.push(row);
      } else if (idx !== undefined) {
        draft.rules[idx] = row;
      }
      modal.close();
      renderRules();
    });

    modal.body.append(
      el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
        button("Cancel", "text-neutral-400 hover:text-white text-xs px-3 py-1", () => modal.close()),
        saveBtn,
      ]),
    );
  }

  function renderRules(): void {
    rulesHost.replaceChildren();
    const card = panel("alert rules");
    card.body.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "Telemetry Alert Rules" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Trigger conditions evaluated continuously against homelab system metrics." }),
      ]),
      button(
        "+ Add Alert Rule",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openRuleModal(),
      ),
    );
    card.body.append(topBar);

    if (draft.rules.length === 0) {
      card.body.append(emptyNote("No alert rules configured. Click '+ Add Alert Rule' above to watch CPU, Memory, or Disks."));
      rulesHost.append(card.root);
      return;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "Rule Name" }),
          el("th", { class: "py-2 px-3", text: "Condition" }),
          el("th", { class: "py-2 px-3", text: "Provider Relay" }),
          el("th", { class: "py-2 px-3", text: "Cooldown" }),
          el("th", { class: "py-2 px-3", text: "Status" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    draft.rules.forEach((rule, idx) => {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
      const prov = draft.providers.find((p) => p.id === rule.action);

      tr.append(
        el("td", { class: "py-2 px-3 text-neutral-200 font-sans font-medium", text: rule.name || rule.id }),
        el("td", { class: "py-2 px-3 text-emerald-400 font-medium", text: `${rule.metric} ${rule.condition} ${rule.threshold} (${rule.duration_s}s)` }),
        el("td", { class: "py-2 px-3 text-neutral-300 font-sans", text: prov ? (prov.name || prov.id) : rule.action }),
        el("td", { class: "py-2 px-3 text-neutral-400 font-sans", text: `${rule.cooldown_s}s` }),
        el("td", { class: "py-2 px-3 font-sans" }, [
          rule.enabled ? badge("Active", "ok") : badge("Disabled", "muted"),
        ]),
        el("td", { class: "py-2 px-3 text-right font-sans flex items-center justify-end gap-1" }, [
          button("Edit", "text-neutral-300 hover:text-white px-2 py-0.5 text-xs", () => openRuleModal(rule, idx)),
          button("Delete", "text-rose-400 hover:text-rose-300 px-2 py-0.5 text-xs", () => {
            if (window.confirm(`Delete alert rule '${rule.name || rule.id}'?`)) {
              draft.rules.splice(idx, 1);
              render();
            }
          }),
        ]),
      );
      tbody.append(tr);
    });
    table.append(tbody);
    card.body.append(table);
    rulesHost.append(card.root);
  }

  // -------------------------------------------------------------------------
  // Health Checks Modal & Table
  // -------------------------------------------------------------------------

  function openHealthCheckModal(existing?: HealthCheck, idx?: number): void {
    const isNew = existing === undefined;
    const modal = openModal(isNew ? "Add Container Health Check" : `Edit Health Check · ${existing.stack}`, "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    const row: HealthCheck = existing
      ? structuredClone(existing)
      : {
          stack: "",
          enabled: true,
          kind: "http",
          target: "http://10.0.0.5:8080/health",
          timeout_s: 5,
          retries: 3,
          cooldown_s: 300,
          last_restart: null,
        };

    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });
    const stackInput = textInput(row.stack, (v) => { row.stack = v.trim(); }, { placeholder: "e.g. web, caddy, nextcloud" });
    const probeSelect = select(row.kind, [["http", "HTTP GET"], ["tcp", "TCP Connect"]], (v) => { row.kind = v as ProbeKind; });
    const targetInput = textInput(row.target, (v) => { row.target = v.trim(); }, { placeholder: "http://10.0.0.5:8080/health" });
    const timeoutInput = numberInput(row.timeout_s, (v) => { row.timeout_s = v; }, { min: 1, max: 60 });
    const retriesInput = numberInput(row.retries, (v) => { row.retries = v; }, { min: 1, max: 10 });
    const cooldownInput = numberInput(row.cooldown_s, (v) => { row.cooldown_s = v; }, { min: 30 });
    const enabledCheck = checkbox(row.enabled, (v) => { row.enabled = v; }, "Enable automated probe and restart");

    modal.body.append(
      field("Stack / Container Name", stackInput, "Name of the Docker Compose stack or container."),
      field("Probe Protocol", probeSelect),
      field("Probe Target Endpoint", targetInput, "HTTP endpoint or IP:port to test."),
      el("div", { class: "grid grid-cols-3 gap-2" }, [
        field("Timeout (s)", timeoutInput),
        field("Max Retries", retriesInput),
        field("Cooldown (s)", cooldownInput),
      ]),
      el("div", { class: "p-2 bg-black/20 rounded border border-white/5" }, [enabledCheck]),
      errorEl,
    );

    const saveBtn = button(isNew ? "Add Health Check" : "Save Changes", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", () => {
      errorEl.textContent = "";
      if (!row.stack.trim()) {
        errorEl.textContent = "Stack Name is required.";
        return;
      }
      if (!row.target.trim()) {
        errorEl.textContent = "Target endpoint is required.";
        return;
      }
      if (isNew) {
        healthDraft.checks.push(row);
      } else if (idx !== undefined) {
        healthDraft.checks[idx] = row;
      }
      modal.close();
      renderHealth();
    });

    modal.body.append(
      el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
        button("Cancel", "text-neutral-400 hover:text-white text-xs px-3 py-1", () => modal.close()),
        saveBtn,
      ]),
    );
  }

  function renderHealth(): void {
    healthHost.replaceChildren();
    const card = panel("container health checks");
    card.body.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "Automated Service Self-Healing" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Probe stacks every 30s. Automatically restarts failed containers and fires alerts." }),
      ]),
      button(
        "+ Add Health Check",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openHealthCheckModal(),
      ),
    );
    card.body.append(topBar);

    if (healthDraft.doc_error) {
      card.body.append(
        el("div", { class: "text-[11px] text-rose-400", text: healthDraft.doc_error }),
      );
    }
    if (healthDraft.checks.length === 0) {
      card.body.append(emptyNote("No health checks configured. Click '+ Add Health Check' above to enable auto-restart."));
      healthHost.append(card.root);
      return;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "Stack Name" }),
          el("th", { class: "py-2 px-3", text: "Probe" }),
          el("th", { class: "py-2 px-3", text: "Target Endpoint" }),
          el("th", { class: "py-2 px-3", text: "Thresholds" }),
          el("th", { class: "py-2 px-3", text: "Status" }),
          el("th", { class: "py-2 px-3", text: "Last Restart" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    healthDraft.checks.forEach((chk, idx) => {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
      tr.append(
        el("td", { class: "py-2 px-3 text-sky-300 font-semibold", text: chk.stack }),
        el("td", { class: "py-2 px-3 uppercase text-neutral-300 font-sans" }, [
          badge(chk.kind.toUpperCase(), "muted"),
        ]),
        el("td", { class: "py-2 px-3 text-neutral-300 truncate max-w-xs", text: chk.target }),
        el("td", { class: "py-2 px-3 text-neutral-400 font-sans", text: `${chk.timeout_s}s timeout, ${chk.retries} retries` }),
        el("td", { class: "py-2 px-3 font-sans" }, [
          chk.enabled ? badge("Active", "ok") : badge("Disabled", "muted"),
        ]),
        el("td", { class: "py-2 px-3 text-neutral-500 font-sans", text: chk.last_restart ? new Date(chk.last_restart * 1000).toLocaleString() : "Never" }),
        el("td", { class: "py-2 px-3 text-right font-sans flex items-center justify-end gap-1" }, [
          button("Edit", "text-neutral-300 hover:text-white px-2 py-0.5 text-xs", () => openHealthCheckModal(chk, idx)),
          button("Delete", "text-rose-400 hover:text-rose-300 px-2 py-0.5 text-xs", () => {
            if (window.confirm(`Delete health check for '${chk.stack}'?`)) {
              healthDraft.checks.splice(idx, 1);
              render();
            }
          }),
        ]),
      );
      tbody.append(tr);
    });
    table.append(tbody);
    card.body.append(table);
    healthHost.append(card.root);
  }

  const saveBtn = button("save", "text-emerald-400 hover:border-emerald-500", () => void save());
  const reloadBtn = button("reload", "", () => void load());
  toolbar.insertBefore(reloadBtn, status);
  toolbar.insertBefore(saveBtn, status);

  root.append(toolbar, providersHost, rulesHost, healthHost);

  return { root, onShow: () => void load() };
}
