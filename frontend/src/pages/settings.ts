/**
 * Settings page.
 *
 * The operator-facing toggles that are not part of a subsystem page: the admin
 * password, Dynamic DNS, and notification delivery. Each section is
 * self-contained; a failure in one never blocks the others.
 */

import { authApi, ddnsApi, notifyApi } from "../api";
import type { DdnsState } from "../api";
import { button, checkbox, el, emptyNote, field, panel, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function when(unix: number | null | undefined): string {
  return unix ? new Date(unix * 1000).toLocaleString() : "never";
}

export function createSettingsPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 overflow-auto p-3" });
  root.append(passwordCard(), ddnsCard(), notificationsCard());
  return { root };
}

// ---------------------------------------------------------------------------
// Password
// ---------------------------------------------------------------------------

function passwordCard(): HTMLElement {
  const { root: card, body: cardBody } = panel("admin password");
  cardBody.className = "flex flex-col gap-2 px-3 py-2";

  const status = el("div", { class: "text-[11px] text-slate-500" });
  let current = "";
  let next = "";
  let confirm = "";

  const currentInput = textInput("", (value) => (current = value), { type: "password" });
  const nextInput = textInput("", (value) => (next = value), { type: "password" });
  const confirmInput = textInput("", (value) => (confirm = value), { type: "password" });

  const submit = button(
    "change password",
    "text-emerald-400 hover:border-emerald-500",
    async () => {
      status.textContent = "";
      if (next.length < 12) {
        status.textContent = "the new password must be at least 12 characters";
        status.className = "text-[11px] text-rose-400";
        return;
      }
      if (next !== confirm) {
        status.textContent = "the new passwords do not match";
        status.className = "text-[11px] text-rose-400";
        return;
      }
      submit.disabled = true;
      try {
        await authApi.changePassword(current, next);
        status.textContent = "password changed; other sessions were signed out";
        status.className = "text-[11px] text-emerald-400";
        currentInput.value = "";
        nextInput.value = "";
        confirmInput.value = "";
      } catch (err) {
        status.textContent = errorText(err);
        status.className = "text-[11px] text-rose-400";
      } finally {
        submit.disabled = false;
      }
    },
  );

  cardBody.append(
    el("div", {
      class: "text-[11px] text-slate-500",
      text: "Changing the password signs out every other session, including other tabs.",
    }),
    field("current password", currentInput),
    field("new password", nextInput),
    field("confirm new password", confirmInput),
    status,
    submit,
  );
  return card;
}

// ---------------------------------------------------------------------------
// Dynamic DNS
// ---------------------------------------------------------------------------

function ddnsCard(): HTMLElement {
  const { root: card, body: cardBody } = panel("dynamic dns (cloudflare)");
  cardBody.className = "flex flex-col gap-2 px-3 py-2 text-[11px]";

  let state: DdnsState | null = null;
  const status = el("div", { class: "text-slate-500" });
  const info = el("div", { class: "flex flex-col gap-0.5 text-slate-500" });

  const load = async (): Promise<void> => {
    try {
      state = await ddnsApi.state();
      status.textContent = "";
    } catch (err) {
      status.textContent = errorText(err);
      status.className = "text-rose-400";
    }
    render();
  };

  const render = (): void => {
    cardBody
      .querySelectorAll("[data-body]")
      .forEach((node) => node.remove());
    const body = el("div", { class: "flex flex-col gap-2", "data-body": "" });
    cardBody.append(body);

    if (!state) {
      body.append(emptyNote(status.textContent || "the DDNS API is unavailable"));
      return;
    }

    for (const problem of state.problems) {
      body.append(
        el("div", { class: "text-rose-400", text: `${problem.field}: ${problem.message}` }),
      );
    }
    if (state.config_error) {
      body.append(el("div", { class: "text-rose-400", text: state.config_error }));
    }

    info.replaceChildren(
      el("div", { text: `token: ${state.has_token ? "stored" : "not set"}` }),
      el("div", { text: `WAN address: ${state.wan_ip ?? "unknown"}` }),
      el("div", {
        text: `last push: ${when(state.config.last_ok_at)}${state.config.last_seen_ip ? ` (${state.config.last_seen_ip})` : ""}`,
      }),
    );
    body.append(info);

    const draft = state.config;
    const zoneInput = textInput(draft.zone_id, (value) => (draft.zone_id = value));
    const recordsInput = textInput(draft.record_names.join(", "), (value) => {
      draft.record_names = value
        .split(",")
        .map((part) => part.trim())
        .filter(Boolean);
    });
    const intervalInput = textInput(String(draft.interval_min), (value) => {
      draft.interval_min = Number.parseInt(value, 10) || 15;
    });
    const enabledToggle = checkbox(draft.enabled, (value) => (draft.enabled = value), "enabled");
    const proxiedToggle = checkbox(draft.proxied, (value) => (draft.proxied = value), "proxied (orange cloud)");

    const tokenInput = textInput("", () => {}, { type: "password", placeholder: "paste to replace" });

    body.append(
      el("div", { class: "flex flex-wrap items-end gap-3" }, [
        field("enabled", enabledToggle),
        field("zone id", zoneInput),
        field("records (a, b)", recordsInput),
        field("interval (min)", intervalInput),
        field("proxied", proxiedToggle),
      ]),
      el("div", { class: "flex flex-wrap items-end gap-3" }, [
        field("api token", tokenInput),
        button("store token", "", async () => {
          if (!tokenInput.value.trim()) {
            status.textContent = "paste a token first";
            return;
          }
          try {
            await ddnsApi.saveToken(tokenInput.value.trim());
            tokenInput.value = "";
            status.textContent = "token stored";
            await load();
          } catch (err) {
            status.textContent = errorText(err);
          }
        }),
      ]),
      el("div", { class: "flex gap-2" }, [
        button("save", "text-emerald-400 hover:border-emerald-500", async () => {
          try {
            state = await ddnsApi.saveConfig(draft);
            status.textContent = "saved";
            render();
          } catch (err) {
            status.textContent = errorText(err);
          }
        }),
        button("update now", "", async () => {
          try {
            const report = await ddnsApi.update();
            state = report.state;
            status.textContent = report.outcomes
              .map((outcome) => `${outcome.ok ? "ok" : "fail"}: ${outcome.detail}`)
              .join(" · ");
            render();
          } catch (err) {
            status.textContent = errorText(err);
          }
        }),
      ]),
      status,
    );
  };

  void load();
  return card;
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

function notificationsCard(): HTMLElement {
  const { root: card, body: cardBody } = panel("notifications");
  cardBody.className = "flex flex-col gap-2 px-3 py-2 text-[11px]";

  const status = el("div", { class: "text-slate-500" });
  const load = async (): Promise<void> => {
    try {
      const feed = await notifyApi.feed();
      render(feed.config);
    } catch (err) {
      status.textContent = errorText(err);
    }
  };

  const render = (config: Awaited<ReturnType<typeof notifyApi.feed>>["config"]): void => {
    cardBody
      .querySelectorAll("[data-body]")
      .forEach((node) => node.remove());
    const body = el("div", { class: "flex flex-col gap-2", "data-body": "" });
    cardBody.append(body);

    const webhookInput = textInput(config.webhook_url ?? "", (value) => {
      config.webhook_url = value.trim() === "" ? null : value.trim();
    });

    body.append(
      el("div", { class: "flex flex-wrap gap-3" }, [
        checkbox(config.enabled, (value) => (config.enabled = value), "deliver to webhook"),
        checkbox(config.backup_failed, (value) => (config.backup_failed = value), "backup failed"),
        checkbox(config.cert_expiring, (value) => (config.cert_expiring = value), "cert expiring"),
        checkbox(config.firewall_rollback, (value) => (config.firewall_rollback = value), "firewall rollback"),
        checkbox(config.smart_prefail, (value) => (config.smart_prefail = value), "smart prefail"),
        checkbox(config.service_down, (value) => (config.service_down = value), "service down"),
      ]),
      el("div", { class: "flex flex-wrap items-end gap-3" }, [
        field("disk usage threshold (%)", textInput(String(config.disk_used_pct), (value) => {
          config.disk_used_pct = Number.parseInt(value, 10) || 90;
        })),
        field("cert warn (days)", textInput(String(config.cert_expiring_days), (value) => {
          config.cert_expiring_days = Number.parseInt(value, 10) || 30;
        })),
      ]),
      field("webhook url", webhookInput),
      el("div", { class: "flex gap-2" }, [
        button("save", "text-emerald-400 hover:border-emerald-500", async () => {
          try {
            const feed = await notifyApi.saveConfig(config);
            status.textContent = "saved";
            render(feed.config);
          } catch (err) {
            status.textContent = errorText(err);
          }
        }),
        button("test webhook", "", async () => {
          try {
            const result = await notifyApi.test();
            status.textContent = result.detail;
          } catch (err) {
            status.textContent = errorText(err);
          }
        }),
      ]),
      status,
    );
  };

  void load();
  return card;
}
