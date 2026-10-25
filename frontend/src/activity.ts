/**
 * Global activity drawer.
 *
 * The companion to the notification bell, but for work rather than events: it
 * polls the job registry and shows a live list of running operations with
 * progress bars, plus the last 24 hours of finished ones. A job that transitions
 * to success or failure while the drawer is closed raises a toast, so a long
 * pull or download that finishes in the background still tells the operator.
 *
 * Polling is deliberate: the registry read is one short lock server-side, and a
 * socket would buy little over a 3-second refresh.
 */

import { jobsApi } from "./api";
import type { Job } from "./api";
import { el } from "./ui";

export interface Activity {
  root: HTMLElement;
  start(): void;
  stop(): void;
}

const POLL_MS = 3000;
const TOAST_MS = 6000;

function relativeTime(tsMs: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - tsMs) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function isFinished(job: Job): boolean {
  return job.status === "success" || job.status === "failed";
}

export function createActivity(): Activity {
  const root = el("div", { class: "relative" });

  const badge = el("span", {
    class:
      "absolute -right-1 -top-1 hidden min-w-4 rounded-full bg-sky-500 px-1 text-center text-[9px] font-bold leading-4 text-white",
  });
  badge.hidden = true;

  const trigger = el(
    "button",
    {
      type: "button",
      class:
        "relative border border-white/5 px-2 py-0.5 text-neutral-400 hover:border-neutral-500 hover:text-neutral-200",
      title: "Background activity",
    },
    [],
  );
  trigger.textContent = "activity";
  trigger.append(badge);

  const panel = el("div", {
    class:
      "absolute right-0 z-40 mt-1 hidden max-h-96 w-96 overflow-y-auto border border-white/5 bg-[#0e0e0e] shadow-xl",
  });

  const toastHost = el("div", {
    class: "fixed bottom-4 right-4 z-50 flex flex-col gap-2",
  });

  root.append(trigger, panel);

  let jobs: Job[] = [];
  let prevStatus = new Map<string, string>();
  let open = false;
  let timer: number | null = null;
  let polling = false;

  function progressBar(progress: number): HTMLElement {
    const outer = el("div", {
      class: "mt-1 h-1.5 w-full overflow-hidden rounded-full bg-white/10",
    });
    const inner = el("div", {
      class: "h-full rounded-full bg-emerald-400 transition-[width] duration-300",
    });
    inner.style.width = `${Math.min(100, Math.max(0, progress))}%`;
    outer.append(inner);
    return outer;
  }

  function toast(job: Job): void {
    const ok = job.status === "success";
    const node = el("div", {
      class: `w-72 border px-3 py-2 text-[11px] shadow-xl ${
        ok
          ? "border-emerald-500/40 bg-[#0d1512] text-emerald-300"
          : "border-rose-500/40 bg-[#170d0f] text-rose-300"
      }`,
    });
    node.append(
      el("div", {
        class: "font-semibold",
        text: `${job.name} ${ok ? "finished" : "failed"}`,
      }),
      el("div", {
        class: "mt-0.5 text-neutral-400",
        text: job.error ?? job.message ?? (ok ? "done" : "failed"),
      }),
    );
    toastHost.append(node);
    window.setTimeout(() => node.remove(), TOAST_MS);
  }

  function apply(next: Job[]): void {
    for (const job of next) {
      const previous = prevStatus.get(job.id);
      if (previous && previous !== "success" && previous !== "failed" && isFinished(job)) {
        toast(job);
      }
      prevStatus.set(job.id, job.status);
    }
    // Forget jobs that were cleared from history.
    const live = new Set(next.map((job) => job.id));
    for (const id of [...prevStatus.keys()]) {
      if (!live.has(id)) prevStatus.delete(id);
    }

    jobs = next;
    updateBadge();
    if (open) renderPanel();
  }

  function renderPanel(): void {
    panel.replaceChildren();
    const head = el("div", {
      class: "flex items-center gap-2 border-b border-white/5 px-3 py-1.5",
    });
    head.append(
      el("span", {
        class: "text-[10px] uppercase tracking-wide text-neutral-500",
        text: "Activity",
      }),
      el("span", { class: "ml-auto" }),
      el("span", {
        class: "text-[10px] text-neutral-600",
        text: `${jobs.filter((job) => !isFinished(job)).length} running`,
      }),
    );
    panel.append(head);

    if (jobs.length === 0) {
      panel.append(
        el("div", { class: "px-3 py-4 text-xs text-neutral-600", text: "No recent activity." }),
      );
      return;
    }

    for (const job of jobs) {
      panel.append(jobRow(job));
    }
  }

  function jobRow(job: Job): HTMLElement {
    const row = el("div", { class: "border-b border-white/5 px-3 py-2" });
    const top = el("div", { class: "flex items-baseline gap-2" });
    const tone =
      job.status === "failed"
        ? "text-rose-400"
        : job.status === "success"
          ? "text-emerald-400"
          : "text-sky-300";
    top.append(
      el("span", { class: `text-xs font-semibold ${tone}`, text: job.name }),
      el("span", {
        class: "ml-auto text-[9px] text-neutral-600",
        text: relativeTime(job.finished_ms ?? job.started_ms),
      }),
    );
    row.append(top);

    if (!isFinished(job)) {
      row.append(progressBar(job.progress));
    }
    if (job.error) {
      row.append(el("div", { class: "mt-1 text-[11px] text-rose-400", text: job.error }));
    } else if (job.message) {
      row.append(el("div", { class: "mt-1 text-[11px] text-neutral-400", text: job.message }));
    }
    if (job.logs.length > 0) {
      row.append(
        el("div", {
          class: "mt-1 truncate text-[10px] text-neutral-600",
          text: job.logs[job.logs.length - 1],
        }),
      );
    }

    if (isFinished(job)) {
      const clear = el("button", {
        type: "button",
        class: "mt-1 text-[10px] text-neutral-600 hover:text-rose-400",
        text: "clear",
      });
      clear.addEventListener("click", () => {
        void jobsApi.remove(job.id).then(() => {
          jobs = jobs.filter((existing) => existing.id !== job.id);
          updateBadge();
          renderPanel();
        });
      });
      row.append(clear);
    }
    return row;
  }

  function updateBadge(): void {
    const running = jobs.filter((job) => !isFinished(job)).length;
    badge.textContent = String(running);
    badge.hidden = running === 0;
  }

  function setOpen(next: boolean): void {
    open = next;
    panel.classList.toggle("hidden", !open);
    if (open) renderPanel();
  }

  trigger.addEventListener("click", (event) => {
    event.stopPropagation();
    setOpen(!open);
  });

  const onDocumentClick = (): void => {
    if (open) setOpen(false);
  };
  document.addEventListener("click", onDocumentClick);
  panel.addEventListener("click", (event) => event.stopPropagation());

  async function poll(): Promise<void> {
    // Skip a tick while one is still in flight, so a slow backend cannot pile
    // up overlapping requests with out-of-order responses.
    if (polling) {
      return;
    }
    polling = true;
    try {
      const state = await jobsApi.list();
      apply(state.jobs);
    } catch {
      // A transient error (or an expired session) must not spam the console.
    } finally {
      polling = false;
    }
  }

  return {
    root,
    start() {
      document.body.append(toastHost);
      void poll();
      timer = window.setInterval(() => void poll(), POLL_MS);
    },
    stop() {
      document.removeEventListener("click", onDocumentClick);
      if (timer !== null) {
        window.clearInterval(timer);
        timer = null;
      }
      toastHost.remove();
    },
  };
}
