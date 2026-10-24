/**
 * Scheduled tasks.
 *
 * A task runs a shell command on a cron-like schedule. Commands run as the
 * service user, which on a normal deployment is root, so the warning is part of
 * the page rather than a footnote, and every run is written to the audit log.
 */

import { tasksApi } from "../api";
import type { ScheduledTask, TasksState } from "../api";
import { badge, button, el, emptyNote, field, select, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

/** The friendly options offered alongside a custom cron expression. */
const PRESETS: [string, string][] = [
  ["daily", "daily at 03:00"],
  ["weekly", "weekly, Sunday 03:00"],
  ["hourly", "hourly"],
  ["custom", "custom cron"],
];

export function createTasksPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-3 p-4" });

  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-3 px-3 py-2" });
  const heading = el("span", { class: "panel-title text-[10px]", text: "scheduled tasks" });
  const reload = button("refresh");
  const status = el("span", { class: "tabular truncate text-[11px] text-neutral-500" });
  toolbar.append(heading, reload, el("span", { class: "ml-auto" }), status);

  const warning = el("div", {
    class: "border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-[11px] text-amber-300",
    text: "Commands run through `sh -c` as the service user — root on a standard deployment. Treat this page like a root shell.",
  });

  const form = el("section", { class: "panel flex flex-col gap-3 p-3" });
  form.append(el("div", { class: "panel-title text-[10px]", text: "new task" }));

  let name = "";
  let schedule = "daily";
  let cron = "0 3 * * *";
  let command = "";

  const cronInput = textInput(
    "0 3 * * *",
    (value) => {
      cron = value;
    },
    { placeholder: "0 3 * * *" },
  );
  const cronField = field("cron expression", cronInput);
  cronField.hidden = true;

  const scheduleSelect = select(
    schedule,
    PRESETS,
    (value) => {
      schedule = value;
      cronField.hidden = value !== "custom";
    },
  );

  const commandArea = el("textarea", {
    class:
      "h-20 w-full resize-y border border-white/5 bg-black/40 p-2 font-mono text-[11px] text-neutral-200 outline-none focus:border-neutral-500",
    placeholder: "docker system prune -f",
  }) as HTMLTextAreaElement;
  commandArea.spellcheck = false;
  commandArea.addEventListener("input", () => {
    command = commandArea.value;
  });

  const feedback = el("div", { class: "tabular text-[11px] text-neutral-500" });
  const save = button("save task", "text-emerald-400 hover:border-emerald-500");
  save.addEventListener("click", async () => {
    if (!name.trim() || !command.trim()) {
      feedback.className = "tabular text-[11px] text-amber-400";
      feedback.textContent = "a task needs a name and a command";
      return;
    }
    save.disabled = true;
    try {
      const state = await tasksApi.save({
        name: name.trim(),
        schedule: schedule === "custom" ? cron.trim() : schedule,
        command: command.trim(),
      });
      feedback.className = "tabular text-[11px] text-emerald-400";
      feedback.textContent = "saved";
      name = "";
      command = "";
      nameInput.value = "";
      commandArea.value = "";
      render(state);
    } catch (err) {
      feedback.className = "tabular text-[11px] text-rose-400";
      feedback.textContent = errorText(err);
    } finally {
      save.disabled = false;
    }
  });

  const nameInput = textInput(
    "",
    (value) => {
      name = value;
    },
    { placeholder: "nightly cleanup" },
  );
  form.append(
    el("div", { class: "grid grid-cols-1 gap-2 xl:grid-cols-3" }, [
      field("name", nameInput),
      field("schedule", scheduleSelect),
      cronField,
    ]),
    field("command", commandArea),
    el("div", { class: "flex flex-wrap items-center gap-2" }, [save, feedback]),
  );

  const list = el("div", { class: "flex flex-col divide-y divide-white/5 border border-white/5" });
  root.append(toolbar, warning, form, list);

  function setStatus(text: string, tone: "muted" | "error" | "ok" = "muted"): void {
    status.textContent = text;
    status.className =
      tone === "error"
        ? "tabular truncate text-[11px] text-rose-400"
        : tone === "ok"
          ? "tabular truncate text-[11px] text-emerald-400"
          : "tabular truncate text-[11px] text-neutral-500";
  }

  async function load(): Promise<void> {
    setStatus("loading…");
    try {
      render(await tasksApi.list());
      setStatus("");
    } catch (err) {
      list.replaceChildren(emptyNote(errorText(err)));
      setStatus(errorText(err), "error");
    }
  }

  function render(state: TasksState): void {
    list.replaceChildren();
    if (state.doc_error) {
      list.append(
        el("div", { class: "px-3 py-2 text-[11px] text-rose-400", text: state.doc_error }),
      );
    }
    if (state.tasks.length === 0) {
      list.append(emptyNote("no scheduled tasks"));
      return;
    }
    for (const task of state.tasks) {
      list.append(row(task));
    }
  }

  function row(task: ScheduledTask): HTMLElement {
    const wrap = el("div", { class: "flex flex-col gap-1 px-3 py-2" });
    const head = el("div", { class: "flex flex-wrap items-baseline gap-x-3 gap-y-1 text-[11px]" });
    head.append(
      el("span", { class: "font-medium text-neutral-200", text: task.name }),
      el("span", { class: "tabular text-neutral-500", text: task.schedule }),
      el("span", { class: "tabular text-neutral-600", text: task.id }),
    );
    if (task.last_status) {
      head.append(
        badge(
          `${task.last_status}${task.last_run ? ` · ${new Date(task.last_run * 1000).toLocaleString()}` : ""}`,
          task.last_status === "ok" ? "ok" : "err",
        ),
      );
    } else {
      head.append(badge("never run", "muted"));
    }

    const actions = el("span", { class: "ml-auto flex gap-2" });
    const runBtn = button("run now", "text-[10px] text-emerald-400");
    runBtn.addEventListener("click", async () => {
      runBtn.disabled = true;
      setStatus(`running ${task.name}…`);
      try {
        const result = await tasksApi.run(task.id);
        setStatus(`${task.name}: ${result.status}`, result.ok ? "ok" : "error");
        render({ tasks: result.tasks, doc_error: null });
      } catch (err) {
        setStatus(errorText(err), "error");
      } finally {
        runBtn.disabled = false;
      }
    });
    const removeBtn = button("delete", "text-[10px] text-rose-400 hover:border-rose-500");
    removeBtn.addEventListener("click", async () => {
      if (!window.confirm(`Delete task ${task.name}?`)) {
        return;
      }
      try {
        render(await tasksApi.remove(task.id));
        setStatus(`${task.name} deleted`, "ok");
      } catch (err) {
        setStatus(errorText(err), "error");
      }
    });
    actions.append(runBtn, removeBtn);
    head.append(actions);

    wrap.append(
      head,
      el("pre", {
        class: "tabular whitespace-pre-wrap break-words text-[10px] text-neutral-500",
        text: task.command,
      }),
    );
    if (task.last_output) {
      wrap.append(
        el("pre", {
          class:
            "max-h-40 overflow-auto whitespace-pre-wrap break-words border border-white/5 bg-black/30 p-2 text-[10px] text-neutral-400",
          text: task.last_output,
        }),
      );
    }
    return wrap;
  }

  reload.addEventListener("click", () => void load());

  return { root, onShow: () => void load() };
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
