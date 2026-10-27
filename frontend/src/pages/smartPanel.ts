/**
 * SMART self-test panel, mounted as a card on the System page.
 *
 * Shows live progress and the last logged result per device, the run history,
 * and the weekly-short / monthly-long schedule. Starting a test asks the disk
 * to run it in the background; progress comes from `smartctl -a`, and a running
 * test can be aborted.
 */

import { smartApi } from "../api";
import type { SmartKind, SmartState, TestProgress } from "../api";
import { button, el, emptyNote, panel, select, textInput } from "../ui";

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** Devices that smartctl can see but that are not scheduled yet. */
const ADD_PLACEHOLDER = "";

function errorText(err: unknown): string {
  const problems = (err as { problems?: Array<{ field: string; message: string }> }).problems;
  if (Array.isArray(problems) && problems.length > 0) {
    return problems.map((problem) => problem.message).join("; ");
  }
  return err instanceof Error ? err.message : String(err);
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

/** One line describing a device's self-test state. */
function statusText(progress: TestProgress): string {
  if (!progress.running) {
    return "idle";
  }
  const kind = progress.running_kind ?? "self-test";
  const done = progress.percent_complete;
  return done === null
    ? `self-test in progress: ${kind}`
    : `self-test in progress: ${kind}, ${done}% complete`;
}

export function buildSmartPanel(): HTMLElement {
  const { root: card, body: cardBody } = panel("SMART self-tests");
  cardBody.className = "flex flex-col gap-2 px-3 py-2 text-[11px]";

  let state: SmartState | null = null;
  let busy = false;
  let status = "";
  let selected = ADD_PLACEHOLDER;

  const statusLine = el("span", { class: "text-[10px] text-neutral-500" });
  const devicePicker = select(ADD_PLACEHOLDER, [[ADD_PLACEHOLDER, "add device…"]], (value) => {
    selected = value;
    updateAddButton();
  });
  const add = button("add", "", () => void addDevice());

  function updateAddButton(): void {
    const options = state?.detected ?? [];
    devicePicker.replaceChildren(
      el("option", { value: ADD_PLACEHOLDER, text: "add device…" }),
      ...options.map((device) => el("option", { value: device, text: device })),
    );
    devicePicker.value = selected;
    const canAdd = Boolean(state && selected !== ADD_PLACEHOLDER);
    devicePicker.disabled = !state || options.length === 0;
    add.disabled = !canAdd;
    devicePicker.title =
      options.length === 0 ? "no unscheduled disks detected" : "choose a disk to schedule";
  }

  const refresh = button("refresh", "", () => void load());
  const head = el("div", { class: "flex items-center gap-2" }, [
    el("span", { class: "ml-auto" }),
    devicePicker,
    add,
    refresh,
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
    if (!state || busy) return;
    busy = true;
    status = "saving…";
    render();
    try {
      state = await smartApi.saveConfig(state.config);
      status = "saved";
    } catch (err) {
      // A validation failure names the offending field, so show that.
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function addDevice(): Promise<void> {
    if (!state || selected === ADD_PLACEHOLDER) return;
    const device = selected;
    // The picker only offers unscheduled disks, so this cannot duplicate an
    // entry (which is what made the old hardcoded `/dev/sda` fail validation).
    state.config.devices.push({
      device,
      device_type: null,
      short_weekday: 0,
      short_at: "02:30",
      long_day: 1,
      long_at: "03:30",
    });
    selected = ADD_PLACEHOLDER;
    await save();
  }

  async function start(device: string, kind: SmartKind): Promise<void> {
    busy = true;
    status = `starting ${kind} test on ${device}…`;
    render();
    try {
      const result = await smartApi.start(device, kind);
      status = result.detail;
      state = await smartApi.state();
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function abort(device: string): Promise<void> {
    busy = true;
    status = `aborting the test on ${device}…`;
    render();
    try {
      const result = await smartApi.abort(device);
      status = result.detail || "aborted";
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
    updateAddButton();
    cardBody.querySelectorAll("[data-body]").forEach((node) => node.remove());

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
      const row = el("div", { class: "flex flex-col gap-0.5" }, [
        el("div", { class: "flex items-baseline gap-2" }, [
          el("span", { class: "text-neutral-300", text: device.device }),
          el("span", {
            class: device.running ? "text-amber-400" : "text-neutral-500",
            text: statusText(device),
          }),
          el("span", { class: "ml-auto" }),
          ...(device.running
            ? [button("abort", "text-[10px] text-rose-400", () => void abort(device.device))]
            : []),
          button("short", "ml-1 text-[10px]", () => void start(device.device, "short")),
          button("long", "ml-1 text-[10px]", () => void start(device.device, "long")),
        ]),
        el("div", {
          class: "text-neutral-500",
          text: `last: ${device.last_result ?? "unknown"}${device.last_percent ? ` (${device.last_percent} remaining)` : ""}`,
        }),
      ]);
      body.append(row);
    }

    if (state.config.devices.length > 0) {
      const table = el("table", { class: "w-full text-[11px]" });
      table.append(
        el("thead", {}, [
          el("tr", {}, [
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-neutral-600", text: "device" }),
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-neutral-600", text: "type" }),
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-neutral-600", text: "short" }),
            el("th", { class: "px-1 py-0.5 text-left text-[10px] uppercase text-neutral-600", text: "long" }),
            el("th", { class: "px-1 py-0.5" }),
          ]),
        ]),
      );
      const tbody = el("tbody");
      state.config.devices.forEach((plan, index) => {
        const weekdaySelect = el("select", {
          class: "border border-white/5 bg-black/40 px-1 py-0.5 text-neutral-200",
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
          el("tr", { class: "border-b border-white/5" }, [
            el("td", { class: "px-1 py-0.5", text: plan.device }),
            el("td", { class: "px-1 py-0.5 text-neutral-500", text: plan.device_type ?? "auto" }),
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
      body.append(
        el("div", {
          class: "text-[10px] uppercase tracking-wide text-neutral-600",
          text: "schedule (weekly short / monthly long, day of month)",
        }),
        table,
      );
    }

    if (state.history.length > 0) {
      const history = el("div", { class: "flex flex-col gap-0.5 text-neutral-500" });
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
