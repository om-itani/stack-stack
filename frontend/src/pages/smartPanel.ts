/**
 * SMART self-test panel, mounted as a card on the System page.
 *
 * Shows live progress and the last logged result per device, the run history,
 * and the weekly-short / monthly-long schedule. Starting a test asks the disk
 * to run it in the background; progress comes from `smartctl -a`.
 */

import { smartApi } from "../api";
import type { DevicePlan, SmartKind, SmartState } from "../api";
import { button, el, emptyNote, panel, textInput } from "../ui";

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

function freshPlan(): DevicePlan {
  return {
    device: "/dev/sda",
    short_weekday: 0,
    short_at: "02:30",
    long_day: 1,
    long_at: "03:30",
  };
}

export function buildSmartPanel(): HTMLElement {
  const { root: card, body: cardBody } = panel("SMART self-tests");
  cardBody.className = "flex flex-col gap-2 px-3 py-2 text-[11px]";

  let state: SmartState | null = null;
  let busy = false;
  let status = "";

  const statusLine = el("span", { class: "text-[10px] text-slate-500" });
  const refresh = button("refresh", "", () => void load());
  const add = button("add device", "", () => {
    if (!state) return;
    state.config.devices.push(freshPlan());
    void save();
  });
  const head = el("div", { class: "flex items-center gap-2" }, [
    el("span", { class: "ml-auto" }),
    refresh,
    add,
    statusLine,
  ]);
  cardBody.append(head);

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      state = await smartApi.state();
      status = state.available ? "" : "smartctl is not installed";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function save(): Promise<void> {
    if (!state) return;
    busy = true;
    status = "saving…";
    render();
    try {
      state = await smartApi.saveConfig(state.config);
      status = "saved";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function start(device: string, kind: SmartKind): Promise<void> {
    busy = true;
    status = `starting ${kind} test on ${device}…`;
    render();
    try {
      const record = await smartApi.start(device, kind);
      status = record.accepted ? `${kind} test started on ${device}` : record.detail;
      state = await smartApi.state();
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  function render(): void {
    statusLine.textContent = status;
    cardBody
      .querySelectorAll("[data-body]")
      .forEach((node) => node.remove());

    const body = el("div", { class: "flex flex-col gap-2", "data-body": "" });
    cardBody.append(body);

    if (!state) {
      body.append(emptyNote(status || "the SMART API is unavailable"));
      return;
    }

    for (const problem of state.problems) {
      body.append(
        el("div", { class: "text-rose-400", text: `${problem.field}: ${problem.message}` }),
      );
    }

    if (state.progress.length === 0 && state.config.devices.length === 0) {
      body.append(emptyNote("no disks scheduled — add one to run weekly/monthly self-tests"));
    }

    for (const device of state.progress) {
      const running = device.running
        ? `running — ${device.percent_remaining ?? "?"}% remaining`
        : device.detail;
      body.append(
        el("div", { class: "flex flex-col gap-0.5" }, [
          el("div", { class: "flex items-baseline gap-2" }, [
            el("span", { class: "text-slate-300", text: device.device }),
            el("span", {
              class: device.running ? "text-amber-400" : "text-slate-500",
              text: running,
            }),
            el("span", { class: "ml-auto" }),
            button("short", "text-[10px]", () => void start(device.device, "short")),
            button("long", "ml-1 text-[10px]", () => void start(device.device, "long")),
          ]),
          el("div", {
            class: "text-slate-500",
            text: `last: ${device.last_result ?? "unknown"}${device.last_percent ? ` (${device.last_percent} life)` : ""}`,
          }),
        ]),
      );
    }

    if (state.config.devices.length > 0) {
      const table = el("table", { class: "w-full text-[11px]" });
      table.append(
        el("thead", {}, [
          el("tr", {}, [
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-slate-600", text: "device" }),
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-slate-600", text: "short" }),
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-slate-600", text: "long" }),
            el("th", { class: "px-1 py-0.5" }),
          ]),
        ]),
      );
      const tbody = el("tbody");
      state.config.devices.forEach((plan, index) => {
        const weekdaySelect = el("select", {
          class: "border border-[#1e2733] bg-[#0b0f14] px-1 py-0.5 text-slate-200",
        }) as HTMLSelectElement;
        weekdaySelect.append(
          el("option", { value: "", text: "off" }),
          ...WEEKDAYS.map((label, value) => el("option", { value: String(value), text: label })),
        );
        weekdaySelect.value = plan.short_weekday === null ? "" : String(plan.short_weekday);
        weekdaySelect.addEventListener("change", () => {
          plan.short_weekday = weekdaySelect.value === "" ? null : Number(weekdaySelect.value);
        });

        const dayInput = textInput(plan.long_day === null ? "" : String(plan.long_day), (value) => {
          plan.long_day = value.trim() === "" ? null : Number.parseInt(value, 10) || null;
        }, { class: "w-10" });

        tbody.append(
          el("tr", { class: "border-b border-[#161d27]" }, [
            el("td", { class: "px-1 py-0.5", text: plan.device }),
            el("td", { class: "px-1 py-0.5" }, [weekdaySelect]),
            el("td", { class: "px-1 py-0.5" }, [dayInput]),
            el("td", { class: "px-1 py-0.5 text-right" }, [
              button("remove", "text-[10px] text-rose-400", () => {
                state!.config.devices.splice(index, 1);
                void save();
              }),
            ]),
          ]),
        );
      });
      table.append(tbody);
      body.append(el("div", { class: "text-[10px] uppercase tracking-wide text-slate-600", text: "schedule (weekly short / monthly long, day of month)" }), table);
    }

    if (state.history.length > 0) {
      const history = el("div", { class: "flex flex-col gap-0.5 text-slate-500" });
      for (const record of state.history.slice(0, 5)) {
        history.append(
          el("div", {
            class: record.accepted ? "" : "text-rose-400",
            text: `${formatTime(record.started_ms)} · ${record.kind} · ${record.device} · ${record.detail}`,
          }),
        );
      }
      body.append(history);
    }
  }

  void load();
  return card;
}
