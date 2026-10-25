/**
 * Alerts page.
 *
 * Two halves of one document: the notification providers (Discord, Slack,
 * generic webhook, email) a rule can target, and the rules themselves —
 * "if [metric] is [condition] [threshold] for [duration], send to [provider]".
 * The whole document is edited locally and saved in one `PUT`, mirroring the
 * other document-backed pages.
 */

import { alertsApi } from "../api";
import type {
  AlertComparison,
  AlertRule,
  AlertsState,
  ProviderConfig,
  ProviderKind,
} from "../api";
import { badge, button, checkbox, el, emptyNote, field, numberInput, panel, select, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

const PROVIDER_KINDS: [ProviderKind, string][] = [
  ["discord", "Discord webhook"],
  ["slack", "Slack webhook"],
  ["webhook", "Generic webhook"],
  ["email", "Email (sendmail)"],
];

const CONDITIONS: [AlertComparison, string][] = [
  [">", "is above"],
  ["<", "is below"],
  ["==", "equals"],
  ["!=", "is not"],
];

/** Continuous metrics are sampled floats, so only `container_status` (1/0) gets
 * equality; the backend rejects `==`/`!=` for anything else. */
const CONTINUOUS_CONDITIONS: [AlertComparison, string][] = CONDITIONS.filter(
  ([condition]) => condition !== "==" && condition !== "!=",
);

/** Base metric names; `disk_usage` and `container_status` take a target. */
const METRICS: [string, string][] = [
  ["cpu_usage", "CPU usage (%)"],
  ["memory_usage", "Memory usage (%)"],
  ["load1", "Load average (1m)"],
  ["disk_usage", "Disk usage (%)"],
  ["container_status", "Container running (1/0)"],
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
  const status = el("div", { class: "tabular text-[11px] text-neutral-500" });

  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-3 px-3 py-2" });
  toolbar.append(
    el("span", { class: "panel-title text-[10px]", text: "alerting" }),
    el("div", { class: "ml-auto" }),
    status,
  );

  const providersHost = el("div", { class: "flex flex-col gap-2" });
  const rulesHost = el("div", { class: "flex flex-col gap-2" });

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
      draft = await alertsApi.state();
      render();
      setStatus("");
    } catch (err) {
      setStatus(errorText(err), "error");
    }
  }

  async function save(): Promise<void> {
    setStatus("saving…");
    try {
      draft = await alertsApi.save({ providers: draft.providers, rules: draft.rules });
      render();
      setStatus("saved", "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
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
  }

  // -------------------------------------------------------------------------
  // Providers
  // -------------------------------------------------------------------------

  function renderProviders(): void {
    providersHost.replaceChildren();
    const card = panel("notification providers");
    card.body.className = "flex flex-col gap-2 px-3 py-2";

    if (draft.providers.length === 0) {
      card.body.append(emptyNote("no providers configured yet"));
    }

    for (const provider of draft.providers) {
      card.body.append(providerRow(provider));
    }

    card.body.append(
      button("add provider", "text-emerald-400 hover:border-emerald-500", () => {
        draft.providers.push({
          id: `provider-${draft.providers.length + 1}`,
          name: "",
          kind: "discord",
          url: "",
          to: null,
          enabled: true,
        });
        renderProviders();
      }),
    );
    providersHost.append(card.root);
  }

  function providerRow(provider: ProviderConfig): HTMLElement {
    const wrap = el("div", { class: "flex flex-col gap-1 border border-white/5 px-2 py-2" });
    const head = el("div", { class: "flex flex-wrap items-end gap-3" });
    head.append(
      field("id", textInput(provider.id, (value) => (provider.id = value.trim()))),
      field("name", textInput(provider.name, (value) => (provider.name = value))),
      field(
        "kind",
        select(provider.kind, PROVIDER_KINDS, (value) => {
          provider.kind = value as ProviderKind;
          renderProviders();
        }),
      ),
      field("enabled", checkbox(provider.enabled, (value) => (provider.enabled = value))),
    );
    wrap.append(head);

    const isEmail = provider.kind === "email";
    const targetLabel = isEmail ? "recipient" : "webhook url";
    const targetValue = (isEmail ? provider.to : provider.url) ?? "";
    const targetInput = textInput(targetValue, (value) => {
      const trimmed = value.trim();
      if (isEmail) {
        provider.to = trimmed === "" ? null : trimmed;
      } else {
        provider.url = trimmed === "" ? null : trimmed;
      }
    });
    wrap.append(field(targetLabel, targetInput));

    const actions = el("div", { class: "flex items-center gap-2" });
    actions.append(
      button("test", "", async () => {
        // The test endpoint reads the persisted document, so persist the draft
        // first; otherwise an unsaved provider 404s and an edited one tests the
        // old target.
        setStatus("saving…");
        try {
          draft = await alertsApi.save({ providers: draft.providers, rules: draft.rules });
          const result = await alertsApi.test(provider.id);
          setStatus(result.detail, "ok");
          render();
        } catch (err) {
          setStatus(errorText(err), "error");
        }
      }),
      button("remove", "text-rose-400 hover:border-rose-500", () => {
        draft.providers = draft.providers.filter((p) => p !== provider);
        render();
      }),
    );
    wrap.append(actions);
    return wrap;
  }

  // -------------------------------------------------------------------------
  // Rules
  // -------------------------------------------------------------------------

  function renderRules(): void {
    rulesHost.replaceChildren();
    const card = panel("alert rules");
    card.body.className = "flex flex-col gap-2 px-3 py-2";

    if (draft.rules.length === 0) {
      card.body.append(emptyNote("no rules; alerts are idle"));
    }

    for (const rule of draft.rules) {
      card.body.append(ruleRow(rule));
    }

    card.body.append(
      button("add rule", "text-emerald-400 hover:border-emerald-500", () => {
        draft.rules.push({
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
        });
        renderRules();
      }),
    );
    rulesHost.append(card.root);
  }

  function ruleRow(rule: AlertRule): HTMLElement {
    const wrap = el("div", { class: "flex flex-col gap-1 border border-white/5 px-2 py-2" });
    const head = el("div", { class: "flex flex-wrap items-end gap-3" });
    head.append(
      field("id", textInput(rule.id, (value) => (rule.id = value.trim()))),
      field("name", textInput(rule.name, (value) => (rule.name = value))),
      field("enabled", checkbox(rule.enabled, (value) => (rule.enabled = value))),
    );
    wrap.append(head);

    const base = metricBase(rule.metric);
    // Only container_status supports equality; coerce a stale condition back to
    // a comparison when the metric is continuous.
    if (base !== "container_status" && (rule.condition === "==" || rule.condition === "!=")) {
      rule.condition = ">";
    }
    const conditionOptions = base === "container_status" ? CONDITIONS : CONTINUOUS_CONDITIONS;
    const target = metricTarget(rule.metric);
    const targetInput = textInput(target, (value) => {
      rule.metric = value.trim() === "" ? base : `${base}:${value.trim()}`;
    }, { placeholder: base === "disk_usage" ? "/mnt/data" : "nginx" });

    const secondRow = el("div", { class: "flex flex-wrap items-end gap-3" });
    secondRow.append(
      field(
        "metric",
        select(base, METRICS, (value) => {
          rule.metric = needsTarget(value) && target ? `${value}:${target}` : value;
          renderRules();
        }),
      ),
    );
    if (needsTarget(base)) {
      secondRow.append(field("target", targetInput));
    }
    secondRow.append(
      field("condition", select(rule.condition, conditionOptions, (value) => {
        rule.condition = value as AlertComparison;
      })),
      field("threshold", numberInput(rule.threshold, (value) => (rule.threshold = value))),
      field("for (seconds)", numberInput(rule.duration_s, (value) => (rule.duration_s = value))),
      field("cooldown (s)", numberInput(rule.cooldown_s, (value) => (rule.cooldown_s = value))),
    );

    const actionOptions: [string, string][] = draft.providers.map((provider) => [
      provider.id,
      provider.name || provider.id,
    ]);
    secondRow.append(
      field("send to", select(rule.action, actionOptions, (value) => (rule.action = value))),
    );
    wrap.append(secondRow);

    const foot = el("div", { class: "flex items-center gap-2 text-[10px] text-neutral-600" });
    if (rule.last_fired) {
      foot.append(badge(`last fired ${new Date(rule.last_fired * 1000).toLocaleString()}`, "muted"));
    }
    foot.append(el("div", { class: "ml-auto" }));
    foot.append(
      button("remove", "text-rose-400 hover:border-rose-500", () => {
        draft.rules = draft.rules.filter((r) => r !== rule);
        render();
      }),
    );
    wrap.append(foot);
    return wrap;
  }

  const saveBtn = button("save", "text-emerald-400 hover:border-emerald-500", () => void save());
  const reloadBtn = button("reload", "", () => void load());
  toolbar.insertBefore(reloadBtn, status);
  toolbar.insertBefore(saveBtn, status);

  root.append(toolbar, providersHost, rulesHost);

  return { root, onShow: () => void load() };
}
