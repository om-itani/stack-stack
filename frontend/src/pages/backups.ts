/**
 * Backups page.
 *
 * One card per job (status, last run, next run, Run now / Verify), an editor for
 * the job's sources, schedule, retention and backend, and a snapshot browser.
 * Repository passwords are written once through the secrets endpoint and are
 * never displayed back.
 */

import { backupApi, filesApi } from "../api";
import type { BackupJob, BackupRun, BackupState, LsEntry, Snapshot } from "../api";
import { bytes } from "../ui";
import { button, checkbox, el, emptyNote, field, panel, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function formatTime(val: number | string | null | undefined): string {
  if (!val) return "—";
  const date = typeof val === "number" ? new Date(val) : new Date(val);
  return Number.isNaN(date.getTime()) ? String(val) : date.toLocaleString();
}

function freshJob(): BackupJob {
  return {
    id: "",
    name: "",
    enabled: true,
    sources: {
      docker_volumes: false,
      compose_apps: true,
      virt_guests: false,
      documents: true,
      certs_ca: true,
      file_roots: false,
      custom: [],
    },
    excludes: [],
    schedule: { cadence: "daily", at: "03:00", weekday: 1 },
    retention: { keep_last: 7, keep_daily: 14, keep_weekly: 8, keep_monthly: 6, keep_yearly: 1 },
    backend: { kind: "local", path: "/srv/backups" },
    secret_ref: "",
  };
}

export function createBackupsPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 overflow-auto p-3" });

  let state: BackupState | null = null;
  let editing: BackupJob | null = null;
  let isNew = false;
  let snapshots: Snapshot[] | null = null;
  let browsePath: string | null = null;
  let browseEntries: LsEntry[] | null = null;
  let browseSnapshot: string | null = null;
  let busy = false;
  let status = "";
  let loaded = false;

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-500" });
  const refreshButton = button("refresh", "", () => void load());
  const addButton = button("add job", "text-emerald-400 hover:border-emerald-500", () => {
    editing = freshJob();
    isNew = true;
    snapshots = null;
    render();
  });

  const header = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-neutral-300", text: "backups" }),
    refreshButton,
    addButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  const body = el("div", { class: "flex flex-col gap-2" });
  root.append(header, body);

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      state = await backupApi.state();
      status = "";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      loaded = true;
      render();
    }
  }

  async function withBusy<T>(work: () => Promise<T>): Promise<T | null> {
    busy = true;
    status = "working…";
    renderChrome();
    try {
      return await work();
    } catch (err) {
      status = errorText(err);
      return null;
    } finally {
      busy = false;
      renderChrome();
    }
  }

  function renderChrome(): void {
    statusLine.textContent = status;
    refreshButton.disabled = busy;
    addButton.disabled = busy;
  }

  function render(): void {
    renderChrome();
    body.replaceChildren();
    if (!loaded) {
      body.append(emptyNote("loading…"));
      return;
    }
    if (!state) {
      body.append(emptyNote(status || "the backup API is unavailable"));
      return;
    }

    if (editing) {
      body.append(editor());
      return;
    }

    for (const job of state.config.jobs) {
      body.append(jobCard(job));
    }
    if (state.config.jobs.length === 0) {
      body.append(emptyNote("no backup jobs yet"));
    }
    body.append(historyPanel());
  }

  function hasSecret(job: BackupJob): boolean {
    const key = job.secret_ref.trim() || job.id;
    return state!.secrets_configured.includes(key);
  }

  function lastRun(job: BackupJob): BackupRun | undefined {
    return state!.history.find((run) => run.job_id === job.id);
  }

  function jobCard(job: BackupJob): HTMLElement {
    const { root: card, body: cardBody } = panel(`${job.name} (${job.id})`);
    cardBody.className = "flex flex-col gap-1 px-3 py-2 text-[11px]";

    const run = lastRun(job);
    const next = state!.next_runs[job.id];
    const lines = [
      job.enabled ? "enabled" : "paused",
      `${job.schedule.cadence} at ${job.schedule.at}`,
      `backend: ${job.backend.kind} → ${describeBackend(job)}`,
      hasSecret(job) ? "credentials stored" : "no credentials yet",
      `last run: ${run ? formatTime(run.ts_ms) : "never"}${run ? ` — ${run.ok ? "ok" : "failed"} (${run.detail})` : ""}`,
      `next run: ${next ? formatTime(next * 1000) : "—"}`,
    ];
    for (const line of lines) {
      cardBody.append(el("div", { class: "text-neutral-400", text: line }));
    }

    const actions = el("div", { class: "flex flex-wrap gap-2" });
    actions.append(
      button(
        "run now",
        "text-emerald-400 hover:border-emerald-500",
        () => void withBusy(() => backupApi.run(job.id)).then(() => load()),
      ),
      button("verify", "", () =>
        void withBusy(() => backupApi.verify(job.id)).then((result) => {
          status = result?.detail ?? "verified";
          render();
        }),
      ),
      button("snapshots", "", () => void openSnapshots(job)),
      button("credentials", "", () => void setCredentials(job)),
      button("edit", "mr-2", () => {
        editing = structuredClone(job);
        isNew = false;
        render();
      }),
      button("delete", "text-rose-400 hover:border-rose-500", async () => {
        if (!window.confirm(`Delete backup job ${job.name}?`)) return;
        const remaining = state!.config.jobs.filter((entry) => entry.id !== job.id);
        await withBusy(() => backupApi.saveConfig({ jobs: remaining }));
        await load();
      }),
    );
    cardBody.append(actions);
    return card;
  }

  function describeBackend(job: BackupJob): string {
    switch (job.backend.kind) {
      case "local":
        return job.backend.path;
      case "s3":
        return `${job.backend.endpoint}/${job.backend.bucket}`;
      case "sftp":
        return job.backend.target;
    }
  }

  function openFolderPicker(title: string, onSelect: (dirPath: string) => void): void {
    let currentDir = "";
    const overlay = el("div", {
      class: "fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4",
    });
    const panel = el("div", {
      class: "flex w-full max-w-lg flex-col gap-2 border border-white/5 bg-[#0e0e0e] p-3 text-[11px]",
    });
    const close = (): void => overlay.remove();

    const dirTitle = el("span", { class: "font-mono text-neutral-300", text: "loading…" });
    const header = el("div", { class: "flex items-center gap-2" }, [
      el("span", { class: "font-medium text-neutral-200", text: title }),
      el("span", { class: "ml-auto" }),
      button("close", "text-[10px]", close),
    ]);

    const dirList = el("div", {
      class: "flex max-h-60 min-h-32 flex-col gap-0.5 overflow-y-auto border border-white/5 p-1",
    });

    const selectBtn = button("Select this directory", "text-emerald-400 hover:border-emerald-500", () => {
      if (currentDir) {
        onSelect(currentDir);
        close();
      }
    });

    panel.append(
      header,
      el("div", { class: "border-b border-white/5 pb-1 text-neutral-400" }, [dirTitle]),
      dirList,
      el("div", { class: "flex justify-end gap-2 pt-1" }, [selectBtn]),
    );
    overlay.append(panel);
    document.body.append(overlay);
    overlay.addEventListener("mousedown", (event) => {
      if (event.target === overlay) close();
    });

    async function navigateTo(target: string): Promise<void> {
      dirList.replaceChildren(el("span", { class: "p-2 text-neutral-500", text: "loading…" }));
      try {
        const fileState = await filesApi.state(target);
        currentDir = fileState.listing.path;
        dirTitle.textContent = currentDir;
        dirList.replaceChildren();

        if (fileState.listing.parent) {
          const parentBtn = button("📁 .. (parent)", "w-full justify-start text-left text-neutral-400 hover:text-white", () => {
            void navigateTo(fileState.listing.parent!);
          });
          dirList.append(parentBtn);
        }

        const dirs = fileState.listing.entries.filter((e) => e.kind === "dir");
        if (dirs.length === 0) {
          dirList.append(el("span", { class: "p-2 italic text-neutral-500", text: "no subdirectories" }));
        } else {
          for (const d of dirs) {
            const btn = button(`📁 ${d.name}/`, "w-full justify-start text-left text-emerald-400 hover:underline", () => {
              void navigateTo(d.path);
            });
            dirList.append(btn);
          }
        }
      } catch (err) {
        dirList.replaceChildren(el("span", { class: "p-2 text-rose-400", text: errorText(err) }));
      }
    }

    void navigateTo("");
  }

  function editor(): HTMLElement {
    const job = editing!;
    const { root: card, body: cardBody } = panel(isNew ? "new job" : `edit ${job.id}`);
    cardBody.className = "flex flex-col gap-3 px-3 py-2";

    const identity = el("div", { class: "grid grid-cols-2 gap-2 md:grid-cols-3" });
    identity.append(
      field("id", textInput(job.id, (value) => (job.id = value), { placeholder: "auto" })),
      field("name", textInput(job.name, (value) => (job.name = value))),
      field("enabled", checkbox(job.enabled, (value) => (job.enabled = value), "job enabled")),
    );

    const sources = el("div", { class: "flex flex-wrap gap-3 text-[11px]" });
    const toggle = (label: string, key: keyof typeof job.sources) =>
      sources.append(
        checkbox(
          Boolean(job.sources[key]),
          (value) => {
            (job.sources[key] as boolean) = value;
          },
          label,
        ),
      );
    toggle("docker volumes", "docker_volumes");
    toggle("compose apps", "compose_apps");
    toggle("guests", "virt_guests");
    toggle("documents", "documents");
    toggle("certificates + CA", "certs_ca");
    toggle("file roots", "file_roots");

    const customInput = textInput(job.sources.custom.join(", "), (value) => {
      job.sources.custom = value
        .split(",")
        .map((part) => part.trim())
        .filter(Boolean);
    }) as HTMLInputElement;
    const browseCustom = button("browse…", "text-[10px]", () => {
      openFolderPicker("Choose source directory", (selectedPath) => {
        if (!job.sources.custom.includes(selectedPath)) {
          job.sources.custom.push(selectedPath);
          customInput.value = job.sources.custom.join(", ");
        }
      });
    });

    sources.append(
      field("custom paths", el("div", { class: "flex items-center gap-1" }, [customInput, browseCustom])),
      field(
        "excludes",
        textInput(job.excludes.join(", "), (value) => {
          job.excludes = value
            .split(",")
            .map((part) => part.trim())
            .filter(Boolean);
        }),
      ),
    );

    const schedule = el("div", { class: "flex flex-wrap items-end gap-3" });
    const cadenceSelect = el("select", {
      class: "border border-white/5 bg-black/40 px-1.5 py-0.5 text-neutral-200",
    }) as HTMLSelectElement;
    for (const [value, label] of [
      ["hourly", "hourly"],
      ["daily", "daily"],
      ["weekly", "weekly"],
    ] as const) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = label;
      cadenceSelect.append(option);
    }
    cadenceSelect.value = job.schedule.cadence;
    cadenceSelect.addEventListener("change", () => {
      job.schedule.cadence = cadenceSelect.value as typeof job.schedule.cadence;
    });
    schedule.append(
      field("cadence", cadenceSelect),
      field("time (HH:MM)", textInput(job.schedule.at, (value) => (job.schedule.at = value))),
      field(
        "weekday (weekly)",
        textInput(String(job.schedule.weekday), (value) => (job.schedule.weekday = Number.parseInt(value, 10) || 0)),
      ),
    );

    const retention = el("div", { class: "flex flex-wrap items-end gap-3" });
    const keep = (label: string, key: keyof typeof job.retention) =>
      retention.append(
        field(
          label,
          textInput(String(job.retention[key]), (value) => {
            job.retention[key] = Number.parseInt(value, 10) || 0;
          }),
        ),
      );
    keep("keep last", "keep_last");
    keep("keep daily", "keep_daily");
    keep("keep weekly", "keep_weekly");
    keep("keep monthly", "keep_monthly");
    keep("keep yearly", "keep_yearly");

    const backend = el("div", { class: "flex flex-wrap items-end gap-3" });
    const kindSelect = el("select", {
      class: "border border-white/5 bg-black/40 px-1.5 py-0.5 text-neutral-200",
    }) as HTMLSelectElement;
    for (const value of ["local", "s3", "sftp"]) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      kindSelect.append(option);
    }
    kindSelect.value = job.backend.kind;
    const previousBackend = job.backend;
    kindSelect.addEventListener("change", () => {
      const kind = kindSelect.value as typeof previousBackend.kind;
      job.backend =
        kind === "local"
          ? {
              kind,
              path: previousBackend.kind === "local" ? previousBackend.path : "/srv/backups",
            }
          : kind === "s3"
            ? { kind, endpoint: "https://s3.example.test", bucket: "" }
            : { kind, target: "user@host:/path" };
      render();
    });
    backend.append(field("backend", kindSelect));
    const currentBackend = job.backend;
    if (currentBackend.kind === "local") {
      const pathInput = textInput(currentBackend.path, (value) => (job.backend = { kind: "local", path: value })) as HTMLInputElement;
      const browseBackend = button("browse…", "text-[10px]", () => {
        openFolderPicker("Choose backup destination directory", (selectedPath) => {
          job.backend = { kind: "local", path: selectedPath };
          pathInput.value = selectedPath;
        });
      });
      backend.append(field("path", el("div", { class: "flex items-center gap-1" }, [pathInput, browseBackend])));
    } else if (currentBackend.kind === "s3") {
      backend.append(
        field(
          "endpoint",
          textInput(currentBackend.endpoint, (value) => {
            if (job.backend.kind === "s3") job.backend.endpoint = value;
          }),
        ),
        field(
          "bucket",
          textInput(currentBackend.bucket, (value) => {
            if (job.backend.kind === "s3") job.backend.bucket = value;
          }),
        ),
      );
    } else {
      backend.append(
        field(
          "target",
          textInput(currentBackend.target, (value) => {
            if (job.backend.kind === "sftp") job.backend.target = value;
          }),
        ),
      );
    }

    const credentials = el("div", { class: "flex flex-wrap items-end gap-3" });
    const passwordInput = el("input", {
      type: "password",
      class: "border border-white/5 bg-black/40 px-1.5 py-0.5 text-neutral-200 text-[11px]",
      placeholder: hasSecret(job) ? "leave blank to keep current" : "min 8 characters",
    }) as HTMLInputElement;
    credentials.append(field("repository password", passwordInput));

    let accessKeyInput: HTMLInputElement | null = null;
    let secretKeyInput: HTMLInputElement | null = null;
    if (job.backend.kind === "s3") {
      accessKeyInput = textInput("", () => {}, { placeholder: "AWS access key id" }) as HTMLInputElement;
      secretKeyInput = el("input", {
        type: "password",
        class: "border border-white/5 bg-black/40 px-1.5 py-0.5 text-neutral-200 text-[11px]",
        placeholder: "AWS secret access key",
      }) as HTMLInputElement;
      credentials.append(field("s3 access key", accessKeyInput), field("s3 secret key", secretKeyInput));
    }

    const actions = el("div", { class: "flex gap-2" }, [
      button("save job", "text-emerald-400 hover:border-emerald-500", () => void saveJob()),
      button("cancel", "", () => {
        editing = null;
        isNew = false;
        render();
      }),
    ]);

    cardBody.append(
      identity,
      panelSection("sources", sources),
      panelSection("schedule", schedule),
      panelSection("retention", retention),
      panelSection("backend", backend),
      panelSection("credentials", credentials),
      actions,
    );
    return card;

    async function saveJob(): Promise<void> {
      if (!job.id.trim()) {
        const slug = job.name
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-+|-+$/g, "");
        job.id = slug || `job-${Date.now()}`;
      }
      if (!job.secret_ref.trim()) {
        job.secret_ref = job.id;
      }
      const pw = passwordInput.value.trim();
      const jobs = isNew
        ? [...state!.config.jobs, job]
        : state!.config.jobs.map((entry) => (entry.id === job.id ? job : entry));
      const result = await withBusy(() => backupApi.saveConfig({ jobs }));
      if (result) {
        state = result;
        if (pw) {
          try {
            await backupApi.saveSecrets(
              job.id,
              pw,
              accessKeyInput?.value.trim() || null,
              secretKeyInput?.value.trim() || null,
              true,
            );
          } catch (err) {
            status = `job saved but credentials error: ${errorText(err)}`;
          }
        }
        editing = null;
        isNew = false;
        status = "saved";
        await load();
      }
    }
  }

  function panelSection(title: string, content: HTMLElement): HTMLElement {
    const box = el("div", { class: "flex flex-col gap-1" });
    box.append(el("div", { class: "text-[10px] uppercase tracking-wide text-neutral-600", text: title }), content);
    return box;
  }

  async function setCredentials(job: BackupJob): Promise<void> {
    const password = window.prompt(
      `Repository password for ${job.name} (min 8 chars, shown once):`,
    );
    if (!password) return;
    const accessKey = job.backend.kind === "s3" ? window.prompt("S3 access key id (blank to skip)") : null;
    const secretKey = job.backend.kind === "s3" ? window.prompt("S3 secret access key (blank to skip)") : null;
    const replace = hasSecret(job);
    const result = await withBusy(() =>
      backupApi.saveSecrets(job.id, password, accessKey || null, secretKey || null, replace),
    );
    status = result?.detail ?? "credentials stored";
    await load();
  }

  async function openSnapshots(job: BackupJob): Promise<void> {
    const list = await withBusy(() => backupApi.snapshots(job.id));
    if (!list) return;
    snapshots = list;
    browseSnapshot = null;
    browseEntries = null;
    browsePath = null;
    status = `${list.length} snapshot(s)`;
    render();
    body.replaceChildren(snapshotsPanel(job));
  }

  function snapshotsPanel(job: BackupJob): HTMLElement {
    const { root: card, body: cardBody } = panel(`snapshots of ${job.name}`);
    cardBody.className = "flex flex-col gap-2 px-3 py-2";

    const close = button("close", "", () => {
      snapshots = null;
      render();
    });
    cardBody.append(close);

    if (!snapshots || snapshots.length === 0) {
      cardBody.append(emptyNote("no snapshots in this repository yet"));
      return card;
    }

    const table = el("table", { class: "w-full text-[11px]" });
    table.append(
      el("thead", {}, [
        el("tr", {}, [
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "id" }),
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "time" }),
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "paths" }),
          el("th", { class: "px-2 py-1" }),
        ]),
      ]),
    );
    const tbody = el("tbody");
    for (const snapshot of snapshots) {
      const browse = button("browse", "mr-2 text-[10px]", () =>
        void browseTree(job, snapshot.short_id || snapshot.id, "/"),
      );
      const restore = button("restore", "text-[10px]", () => void restoreSnapshot(job, snapshot));
      tbody.append(
        el("tr", { class: "border-b border-white/5" }, [
          el("td", { class: "px-2 py-1 text-neutral-300", text: snapshot.short_id || snapshot.id }),
          el("td", { class: "px-2 py-1 text-neutral-500", text: formatTime(snapshot.time) }),
          el("td", { class: "px-2 py-1 text-neutral-500", text: snapshot.paths.join(", ") }),
          el("td", { class: "px-2 py-1 text-right", text: "" }, [browse, restore]),
        ]),
      );
    }
    table.append(tbody);
    cardBody.append(table);

    if (browseEntries) {
      cardBody.append(browsePanel(job));
    }
    return card;
  }

  function browsePanel(job: BackupJob): HTMLElement {
    const box = el("div", { class: "flex flex-col gap-1 border-t border-white/5 pt-2" });
    box.append(
      el("div", {
        class: "text-[11px] text-neutral-400",
        text: `inside ${browsePath ?? "/"} (snapshot ${browseSnapshot ?? ""})`,
      }),
    );
    if (browsePath && browsePath !== "/") {
      const parent = browsePath.replace(/\/[^/]*$/, "") || "/";
      box.append(button("up", "text-[10px]", () => void browseTree(job, browseSnapshot!, parent)));
    }
    if (!browseEntries || browseEntries.length === 0) {
      box.append(emptyNote("empty"));
      return box;
    }
    for (const entry of browseEntries) {
      const row = el("div", { class: "flex items-center gap-2 text-[11px]" });
      row.append(
        el("span", {
          class: entry.kind === "dir" ? "text-emerald-400" : "text-neutral-400",
          text: entry.kind === "dir" ? `${entry.name}/` : entry.name,
        }),
        el("span", { class: "ml-auto text-neutral-600", text: entry.kind === "dir" ? "" : bytes(entry.size) }),
      );
      if (entry.kind === "dir") {
        row.append(button("open", "text-[10px]", () => void browseTree(job, browseSnapshot!, entry.path)));
      } else {
        row.append(
          el("a", {
            href: backupApi.downloadUrl(job.id, browseSnapshot!, entry.path),
            class: "text-emerald-400 underline",
            text: "download",
          }),
        );
      }
      box.append(row);
    }
    return box;
  }

  async function browseTree(job: BackupJob, snapshot: string, path: string): Promise<void> {
    const entries = await withBusy(() => backupApi.browse(job.id, snapshot, path));
    if (!entries) return;
    browseEntries = entries;
    browsePath = path;
    browseSnapshot = snapshot;
    render();
    body.replaceChildren(snapshotsPanel(job));
  }

  async function restoreSnapshot(job: BackupJob, snapshot: Snapshot): Promise<void> {
    openFolderPicker("Choose restore destination directory", async (target) => {
      const result = await withBusy(() =>
        backupApi.restore(job.id, snapshot.short_id || snapshot.id, target, null),
      );
      status = result?.detail ?? "restore finished";
      render();
    });
  }

  function historyPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("history");
    cardBody.className = "flex flex-col gap-1 px-3 py-2 text-[11px]";
    const history = state!.history;
    if (history.length === 0) {
      cardBody.append(emptyNote("no runs recorded yet"));
      return card;
    }
    const table = el("table", { class: "w-full text-[11px]" });
    table.append(
      el("thead", {}, [
        el("tr", {}, [
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "when" }),
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "job" }),
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "result" }),
          el("th", { class: "px-2 py-1 text-right text-[10px] uppercase text-neutral-600", text: "data" }),
          el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "detail" }),
        ]),
      ]),
    );
    const tbody = el("tbody");
    for (const run of history) {
      tbody.append(
        el("tr", { class: "border-b border-white/5" }, [
          el("td", { class: "px-2 py-1 text-neutral-500", text: formatTime(run.ts_ms) }),
          el("td", { class: "px-2 py-1 text-neutral-300", text: run.name }),
          el("td", { class: `px-2 py-1 ${run.ok ? "text-emerald-400" : "text-rose-400"}`, text: run.ok ? "ok" : "failed" }),
          el("td", { class: "tabular px-2 py-1 text-right text-neutral-500", text: run.bytes ? bytes(run.bytes) : "—" }),
          el("td", { class: "px-2 py-1 text-neutral-500", text: run.detail }),
        ]),
      );
    }
    table.append(tbody);
    cardBody.append(table);
    return card;
  }

  void load();
  return { root };
}
