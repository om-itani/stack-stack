/**
 * Backups page.
 *
 * One card per job (status, last run, next run, Run now / Verify), a comprehensive
 * modal editor for the job's sources, schedule, retention, and backend, and an
 * interactive snapshot explorer modal with directory tree navigation and restore.
 * Repository passwords are written securely through the secrets endpoint and are
 * never displayed back.
 */

import { backupApi, filesApi } from "../api";
import type { BackupJob, BackupRun, BackupState, Snapshot } from "../api";
import { showToast } from "../toast";
import { bytes } from "../ui";
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
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-3" });

  let state: BackupState | null = null;
  let busy = false;
  let status = "";
  let loaded = false;

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-500" });
  const refreshButton = button("refresh", "", () => void load());
  const addButton = button(
    "+ Create Backup Job",
    "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-3 py-1 text-xs shadow-sm",
    () => openJobModal(freshJob(), true),
  );

  const header = el("div", { class: "flex flex-wrap items-center gap-3 border-b border-white/5 pb-2" }, [
    el("div", { class: "flex items-center gap-2" }, [
      el("span", { class: "font-semibold text-sm text-neutral-200 tracking-wide", text: "Automated Backups" }),
      el("span", { class: "text-xs text-neutral-500", text: "· Restic Snapshot Engine" }),
    ]),
    addButton,
    refreshButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  const body = el("div", { class: "flex flex-col gap-3" });
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
      body.append(emptyNote("loading backup engine state…"));
      return;
    }
    if (!state) {
      body.append(emptyNote(status || "The backup API is currently unavailable."));
      return;
    }

    if (state.config.jobs.length === 0) {
      body.append(
        emptyNote("No backup jobs configured yet. Click '+ Create Backup Job' above to safeguard your homelab data."),
      );
    } else {
      for (const job of state.config.jobs) {
        body.append(jobCard(job));
      }
    }
    body.append(historyPanel());
  }

  function hasSecret(job: BackupJob): boolean {
    const key = job.secret_ref.trim() || job.id;
    return state?.secrets_configured.includes(key) ?? false;
  }

  function lastRun(job: BackupJob): BackupRun | undefined {
    return state?.history.find((run) => run.job_id === job.id);
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

  function jobCard(job: BackupJob): HTMLElement {
    const { root: card, body: cardBody } = panel(`${job.name}`);
    cardBody.className = "flex flex-col gap-3 p-3.5 text-xs bg-[#11141c]/60 rounded border border-white/5";

    const run = lastRun(job);
    const next = state?.next_runs[job.id];
    const credentialsOk = hasSecret(job);

    // Top Header info
    const topRow = el("div", { class: "flex flex-wrap items-center justify-between gap-2 border-b border-white/5 pb-2" });
    topRow.append(
      el("div", { class: "flex items-center gap-2" }, [
        el("span", { class: "font-mono font-medium text-neutral-200 text-sm", text: job.name }),
        el("span", { class: "font-mono text-[10px] text-neutral-500 bg-white/5 px-1.5 py-0.5 rounded", text: job.id }),
        job.enabled
          ? badge("Active", "ok")
          : badge("Paused", "muted"),
        credentialsOk
          ? badge("Encrypted Key OK", "ok")
          : badge("Key Missing", "warn"),
      ]),
      el("div", { class: "flex items-center gap-1.5 font-mono text-[11px] text-neutral-400" }, [
        el("span", { text: "Backend:" }),
        el("span", { class: "text-neutral-200 font-semibold", text: `${job.backend.kind.toUpperCase()} → ${describeBackend(job)}` }),
      ]),
    );

    // Grid status overview
    const statsGrid = el("div", { class: "grid grid-cols-2 md:grid-cols-4 gap-2 text-[11px] bg-black/20 p-2.5 rounded border border-white/5" });
    statsGrid.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "text-[10px] uppercase text-neutral-500 tracking-wider", text: "Schedule" }),
        el("span", { class: "text-neutral-200 font-medium", text: `${job.schedule.cadence} @ ${job.schedule.at}` }),
      ]),
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "text-[10px] uppercase text-neutral-500 tracking-wider", text: "Next Run" }),
        el("span", { class: "text-neutral-300", text: next ? formatTime(next * 1000) : "—" }),
      ]),
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "text-[10px] uppercase text-neutral-500 tracking-wider", text: "Last Run" }),
        el("span", { class: run?.ok ? "text-emerald-400" : run ? "text-rose-400" : "text-neutral-400" }, [
          el("span", { text: run ? (run.ok ? "✔ Succeeded" : "✖ Failed") : "Never" }),
          run ? el("span", { class: "text-[10px] text-neutral-500 ml-1", text: `(${formatTime(run.ts_ms)})` }) : el("span"),
        ]),
      ]),
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "text-[10px] uppercase text-neutral-500 tracking-wider", text: "Data Transferred" }),
        el("span", { class: "text-neutral-300 font-mono", text: run?.bytes ? bytes(run.bytes) : "—" }),
      ]),
    );

    // Action buttons
    const actions = el("div", { class: "flex flex-wrap items-center gap-2 pt-1 border-t border-white/5" });
    actions.append(
      button(
        "▶ Run Now",
        "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-3 py-1 text-xs shadow-sm",
        async () => {
          showToast({ tone: "info", title: "Backup Initiated", message: `Initiating backup for '${job.name}'…` });
          await withBusy(() => backupApi.run(job.id));
          showToast({ tone: "ok", title: "Backup Finished", message: `Backup run for '${job.name}' finished` });
          await load();
        },
      ),
      button(
        "Verify Repository",
        "text-xs px-2.5 py-1 text-neutral-300 hover:text-white hover:bg-white/5 rounded border border-white/10",
        async () => {
          showToast({ tone: "info", title: "Verifying Repository", message: `Verifying repository for '${job.name}'…` });
          const result = await withBusy(() => backupApi.verify(job.id));
          showToast({
            tone: result?.ok ? "ok" : "error",
            title: "Verification Result",
            message: result?.detail ?? "Repository integrity verified successfully",
          });
          await load();
        },
      ),
      button(
        "Snapshots Explorer",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1 text-xs shadow-sm",
        () => openSnapshotsModal(job),
      ),
      button(
        "Credentials",
        "border-amber-600 bg-amber-500/20 text-amber-300 font-semibold hover:bg-amber-500/30 px-2.5 py-1 text-xs shadow-sm",
        () => openCredentialsModal(job),
      ),
      button(
        "Edit Job",
        "text-xs px-2.5 py-1 text-neutral-300 hover:text-white hover:bg-white/5 rounded border border-white/10",
        () => openJobModal(structuredClone(job), false),
      ),
      el("span", { class: "ml-auto" }),
      button(
        "Delete Job",
        "text-xs px-2.5 py-1 text-rose-400 hover:text-rose-300 hover:bg-rose-500/10 rounded border border-rose-500/20",
        async () => {
          if (!window.confirm(`Delete backup job '${job.name}' (${job.id})? Existing repository snapshots will not be destroyed.`)) return;
          const remaining = state!.config.jobs.filter((entry) => entry.id !== job.id);
          await withBusy(() => backupApi.saveConfig({ jobs: remaining }));
          showToast({ tone: "info", title: "Job Deleted", message: `Backup job '${job.name}' removed` });
          await load();
        },
      ),
    );

    cardBody.append(topRow, statsGrid, actions);
    return card;
  }

  function openFolderPicker(title: string, onSelect: (path: string) => void): void {
    const modal = openModal(title, "max-w-md");
    modal.body.className = "flex flex-col gap-2 p-1";
    let currentDir = "";
    const dirTitle = el("div", {
      class: "font-mono text-xs text-emerald-400 break-all p-1.5 bg-black/40 rounded border border-white/5",
      text: "/srv",
    });
    const dirList = el("div", {
      class: "flex max-h-60 min-h-36 flex-col gap-0.5 overflow-y-auto border border-white/5 p-1 rounded bg-[#0e1118]",
    });

    const selectBtn = button(
      "Select This Directory",
      "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold px-3 py-1 text-xs",
      () => {
        if (currentDir) {
          onSelect(currentDir);
          modal.close();
        }
      },
    );

    modal.body.append(
      dirTitle,
      dirList,
      el("div", { class: "flex justify-end gap-2 pt-2 border-t border-white/10" }, [selectBtn]),
    );

    async function navigateTo(target: string): Promise<void> {
      dirList.replaceChildren(el("span", { class: "p-2 text-neutral-500 text-xs", text: "loading…" }));
      try {
        const fileState = await filesApi.state(target);
        currentDir = fileState.listing.path;
        dirTitle.textContent = currentDir;
        dirList.replaceChildren();

        if (fileState.listing.parent) {
          const parentBtn = button(
            "📁 .. (parent directory)",
            "w-full justify-start text-left text-neutral-400 hover:text-white text-xs py-1 px-1.5",
            () => {
              void navigateTo(fileState.listing.parent!);
            },
          );
          dirList.append(parentBtn);
        }

        const dirs = fileState.listing.entries.filter((e) => e.kind === "dir");
        if (dirs.length === 0) {
          dirList.append(el("span", { class: "p-2 italic text-neutral-500 text-xs", text: "no subdirectories" }));
        } else {
          for (const d of dirs) {
            const btn = button(
              `📁 ${d.name}/`,
              "w-full justify-start text-left text-emerald-400 hover:underline text-xs py-1 px-1.5",
              () => {
                void navigateTo(d.path);
              },
            );
            dirList.append(btn);
          }
        }
      } catch (err) {
        dirList.replaceChildren(el("span", { class: "p-2 text-rose-400 text-xs", text: errorText(err) }));
      }
    }

    void navigateTo("/srv");
  }

  function openCredentialsModal(job: BackupJob): void {
    const modal = openModal(`Encryption & Credentials · ${job.name}`, "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    let password = "";
    let accessKey = "";
    let secretKey = "";
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const passInput = textInput(
      "",
      (v) => {
        password = v;
      },
      {
        type: "password",
        placeholder: hasSecret(job) ? "Leave blank to keep existing password" : "Minimum 8 characters",
      },
    );

    modal.body.append(
      el("div", {
        class: "text-xs text-neutral-400 leading-relaxed",
        text: "Configure the master repository encryption passphrase. Restic uses client-side AES-256 encryption before any byte leaves your machine.",
      }),
      field(
        "Repository Password",
        passInput,
        hasSecret(job)
          ? "Credentials already stored on system. Enter a new password only if changing it."
          : "Required for repository initialization and encryption.",
      ),
    );

    let accessInput: HTMLInputElement | null = null;
    let secretInput: HTMLInputElement | null = null;
    if (job.backend.kind === "s3") {
      accessInput = textInput(
        "",
        (v) => {
          accessKey = v;
        },
        { placeholder: "e.g. AKIAIOSFODNN7EXAMPLE" },
      ) as HTMLInputElement;
      secretInput = textInput(
        "",
        (v) => {
          secretKey = v;
        },
        { type: "password", placeholder: "e.g. wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" },
      ) as HTMLInputElement;
      modal.body.append(
        field("S3 Access Key ID", accessInput),
        field("S3 Secret Access Key", secretInput),
      );
    }

    modal.body.append(errorEl);

    const saveBtn = button(
      "Store Credentials",
      "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm",
      async () => {
        errorEl.textContent = "";
        if (!password.trim() && !hasSecret(job)) {
          errorEl.textContent = "A repository password is required (min 8 characters).";
          return;
        }
        if (password.trim() && password.length < 8) {
          errorEl.textContent = "Password must be at least 8 characters long.";
          return;
        }
        try {
          saveBtn.disabled = true;
          saveBtn.textContent = "Saving…";
          const replace = hasSecret(job);
          const res = await backupApi.saveSecrets(
            job.id,
            password,
            accessKey.trim() || null,
            secretKey.trim() || null,
            replace,
          );
          showToast({ tone: "ok", title: "Credentials Stored", message: res.detail || "Credentials stored securely" });
          modal.close();
          await load();
        } catch (err) {
          errorEl.textContent = errorText(err);
          saveBtn.disabled = false;
          saveBtn.textContent = "Store Credentials";
        }
      },
    );

    modal.body.append(
      el("div", { class: "flex justify-end gap-2 pt-2 border-t border-white/10" }, [
        button("Cancel", "text-neutral-400 hover:text-white text-xs px-3 py-1", () => modal.close()),
        saveBtn,
      ]),
    );
  }

  function openSnapshotsModal(job: BackupJob): void {
    const modal = openModal(`Snapshots Explorer · ${job.name}`, "max-w-4xl");
    modal.body.className = "flex flex-col gap-3 min-h-[420px]";

    const listContainer = el("div", { class: "flex flex-col gap-2" }, [
      el("span", { class: "text-xs text-neutral-400", text: "Loading snapshots from repository…" }),
    ]);
    modal.body.append(listContainer);

    void loadSnapshots();

    async function loadSnapshots(): Promise<void> {
      try {
        const list = await backupApi.snapshots(job.id);
        if (!list || list.length === 0) {
          listContainer.replaceChildren(
            emptyNote("No snapshots recorded in this repository yet. Run a backup job first."),
          );
          return;
        }
        renderSnapshotsList(list);
      } catch (err) {
        listContainer.replaceChildren(el("div", { class: "text-xs text-rose-400 p-2", text: errorText(err) }));
      }
    }

    function renderSnapshotsList(snaps: Snapshot[]): void {
      listContainer.replaceChildren();

      const topSummary = el("div", {
        class: "flex items-center justify-between text-xs text-neutral-400 pb-2 border-b border-white/5",
      });
      topSummary.append(
        el("span", { text: `Found ${snaps.length} immutable snapshot${snaps.length === 1 ? "" : "s"}` }),
        el("span", {
          class: "font-mono text-[11px] text-neutral-500",
          text: `Destination: ${job.backend.kind.toUpperCase()} → ${describeBackend(job)}`,
        }),
      );
      listContainer.append(topSummary);

      const table = el("table", { class: "w-full text-xs text-left" });
      table.append(
        el("thead", {}, [
          el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
            el("th", { class: "py-2 px-3", text: "Snapshot ID" }),
            el("th", { class: "py-2 px-3", text: "Created At" }),
            el("th", { class: "py-2 px-3", text: "Paths Included" }),
            el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
          ]),
        ]),
      );

      const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
      for (const snap of snaps) {
        const snapId = snap.short_id || snap.id;
        const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
        tr.append(
          el("td", { class: "py-2 px-3 text-emerald-400 font-semibold", text: snapId }),
          el("td", { class: "py-2 px-3 text-neutral-300 font-sans", text: formatTime(snap.time) }),
          el("td", { class: "py-2 px-3 text-neutral-400 truncate max-w-xs", text: snap.paths.join(", ") }),
          el("td", { class: "py-2 px-3 text-right font-sans flex items-center justify-end gap-1.5" }, [
            button(
              "Browse Files",
              "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold px-2 py-0.5 text-xs hover:bg-sky-500/30",
              () => {
                void openBrowseView(snapId, "/");
              },
            ),
            button(
              "Restore To…",
              "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold px-2 py-0.5 text-xs hover:bg-emerald-500/30",
              () => {
                openFolderPicker("Select destination directory for restore", async (dest) => {
                  try {
                    showToast({ tone: "info", title: "Restoring Snapshot", message: `Restoring snapshot ${snapId} to ${dest}…` });
                    const res = await backupApi.restore(job.id, snapId, dest, null);
                    showToast({ tone: "ok", title: "Restore Complete", message: res.detail || "Snapshot restored successfully!" });
                  } catch (e) {
                    showToast({ tone: "error", title: "Restore Failed", message: errorText(e) });
                  }
                });
              },
            ),
          ]),
        );
        tbody.append(tr);
      }
      table.append(tbody);
      listContainer.append(table);

      const treeContainer = el("div", { class: "flex flex-col gap-2 border-t border-white/10 pt-3 hidden" });
      listContainer.append(treeContainer);

      async function openBrowseView(snapId: string, dirPath: string): Promise<void> {
        treeContainer.classList.remove("hidden");
        treeContainer.replaceChildren(
          el("span", { class: "text-xs text-neutral-500", text: `Reading directory ${dirPath}…` }),
        );

        try {
          const entries = await backupApi.browse(job.id, snapId, dirPath);
          treeContainer.replaceChildren();

          const bar = el("div", {
            class: "flex items-center justify-between bg-black/40 p-2 rounded border border-white/5",
          });
          bar.append(
            el("div", { class: "flex items-center gap-2 text-xs" }, [
              el("span", { class: "text-neutral-400", text: "Browsing:" }),
              el("span", { class: "font-mono text-emerald-400", text: `${dirPath} (snapshot ${snapId})` }),
            ]),
            button("✕ Close Tree", "text-neutral-400 hover:text-white text-xs px-2 py-0.5", () => {
              treeContainer.classList.add("hidden");
              treeContainer.replaceChildren();
            }),
          );
          treeContainer.append(bar);

          if (dirPath !== "/") {
            const parent = dirPath.replace(/\/[^/]*$/, "") || "/";
            const upBtn = button(
              "📁 .. (parent directory)",
              "text-left text-neutral-400 hover:text-white text-xs py-1 px-2",
              () => {
                void openBrowseView(snapId, parent);
              },
            );
            treeContainer.append(upBtn);
          }

          const entriesList = el("div", {
            class:
              "flex flex-col max-h-64 overflow-y-auto divide-y divide-white/5 border border-white/5 rounded bg-[#0a0d14] p-1 font-mono text-xs",
          });
          if (!entries || entries.length === 0) {
            entriesList.append(el("span", { class: "p-2 italic text-neutral-500", text: "Directory is empty" }));
          } else {
            for (const item of entries) {
              const row = el("div", { class: "flex items-center justify-between py-1 px-2 hover:bg-white/[0.03]" });
              if (item.kind === "dir") {
                const openDirBtn = button(
                  `📁 ${item.name}/`,
                  "text-sky-300 hover:underline text-xs p-0 text-left",
                  () => {
                    void openBrowseView(snapId, item.path);
                  },
                );
                row.append(openDirBtn, el("span", { class: "text-neutral-500 text-[10px]", text: "folder" }));
              } else {
                const downloadLink = el("a", {
                  href: backupApi.downloadUrl(job.id, snapId, item.path),
                  class: "text-emerald-400 hover:underline flex items-center gap-1",
                  target: "_blank",
                  text: `📄 ${item.name}`,
                });
                row.append(
                  downloadLink,
                  el("div", { class: "flex items-center gap-2" }, [
                    el("span", { class: "text-neutral-500 text-[11px]", text: bytes(item.size) }),
                    el("a", {
                      href: backupApi.downloadUrl(job.id, snapId, item.path),
                      class: "text-sky-400 hover:text-sky-300 text-[11px] underline ml-2",
                      text: "download",
                    }),
                  ]),
                );
              }
              entriesList.append(row);
            }
          }
          treeContainer.append(entriesList);
        } catch (e) {
          treeContainer.replaceChildren(
            el("div", { class: "text-xs text-rose-400 p-2", text: `Browse failed: ${errorText(e)}` }),
          );
        }
      }
    }
  }

  function openJobModal(job: BackupJob, isNewJob: boolean): void {
    const modal = openModal(isNewJob ? "Create Backup Job" : `Edit Backup Job · ${job.name}`, "max-w-2xl");
    modal.body.className = "flex flex-col gap-3";

    let activeTab = "general";
    const tabHeaders = el("div", { class: "flex gap-2 border-b border-white/10 pb-2 text-xs" });
    const contentArea = el("div", { class: "flex flex-col gap-3 min-h-[300px]" });
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    let repositoryPassword = "";
    let s3AccessKey = "";
    let s3SecretKey = "";

    function switchTab(newTab: string): void {
      activeTab = newTab;
      renderTabs();
    }

    function renderTabs(): void {
      tabHeaders.replaceChildren();
      const tabDefs = [
        { id: "general", label: "1. General & Schedule" },
        { id: "sources", label: "2. Data Sources" },
        { id: "backend", label: "3. Storage Backend" },
        { id: "security", label: "4. Security & Retention" },
      ];

      for (const def of tabDefs) {
        const btn = button(
          def.label,
          `px-3 py-1.5 text-xs rounded transition-colors ${
            activeTab === def.id
              ? "bg-emerald-500/20 text-emerald-300 font-semibold border border-emerald-500/40"
              : "text-neutral-400 hover:text-white border border-transparent"
          }`,
          () => switchTab(def.id),
        );
        tabHeaders.append(btn);
      }

      contentArea.replaceChildren();
      switch (activeTab) {
        case "general":
          renderGeneralTab();
          break;
        case "sources":
          renderSourcesTab();
          break;
        case "backend":
          renderBackendTab();
          break;
        case "security":
          renderSecurityTab();
          break;
      }
    }

    function renderGeneralTab(): void {
      const nameInput = textInput(job.name, (v) => {
        job.name = v;
      }, { placeholder: "e.g. Daily Homelab Backup, Cloud Storage Mirror" });

      const idInput = textInput(job.id, (v) => {
        job.id = v;
      }, { placeholder: "e.g. daily-homelab (auto-generated if empty)" });

      const cadenceSelect = select(
        job.schedule.cadence,
        [
          ["hourly", "Hourly"],
          ["daily", "Daily"],
          ["weekly", "Weekly"],
        ],
        (v) => {
          job.schedule.cadence = v as typeof job.schedule.cadence;
        },
      );

      const timeInput = textInput(job.schedule.at, (v) => {
        job.schedule.at = v;
      }, { placeholder: "03:00" });

      const weekdaySelect = select(
        String(job.schedule.weekday),
        [
          ["0", "Sunday"],
          ["1", "Monday"],
          ["2", "Tuesday"],
          ["3", "Wednesday"],
          ["4", "Thursday"],
          ["5", "Friday"],
          ["6", "Saturday"],
        ],
        (v) => {
          job.schedule.weekday = Number.parseInt(v, 10) || 0;
        },
      );

      contentArea.append(
        el("div", { class: "grid grid-cols-2 gap-3" }, [
          field("Job Name", nameInput, "Human-friendly label for this backup task."),
          field("Job Identifier", idInput, "Unique alphanumeric identifier."),
        ]),
        el("div", { class: "p-2.5 bg-black/30 rounded border border-white/5" }, [
          checkbox(job.enabled, (v) => {
            job.enabled = v;
          }, "Enable automated scheduled execution"),
        ]),
        el("div", { class: "grid grid-cols-3 gap-3" }, [
          field("Cadence", cadenceSelect),
          field("Execution Time (HH:MM)", timeInput, "24-hour time format (UTC or local host time)"),
          field("Day of Week", weekdaySelect, "Used when cadence is set to weekly"),
        ]),
      );
    }

    function renderSourcesTab(): void {
      const hostSourcesBox = el("div", { class: "grid grid-cols-2 gap-2 p-3 bg-black/30 rounded border border-white/5 text-xs" });
      const toggle = (label: string, key: keyof typeof job.sources, desc: string) => {
        const wrap = el("div", { class: "flex flex-col gap-0.5" }, [
          checkbox(Boolean(job.sources[key]), (v) => {
            (job.sources[key] as boolean) = v;
          }, label),
          el("span", { class: "text-[10px] text-neutral-500 pl-6", text: desc }),
        ]);
        hostSourcesBox.append(wrap);
      };

      toggle("Docker Volumes", "docker_volumes", "All named persistent container volumes");
      toggle("Compose Apps", "compose_apps", "Compose files and stack configs in /var/lib/stack-stack/stacks");
      toggle("Virtual Guests", "virt_guests", "QEMU/KVM virtual machine disk images and LXC containers");
      toggle("Documents", "documents", "Standard document directory (/srv/documents)");
      toggle("Certificates & CA", "certs_ca", "Internal Certificate Authority and Let's Encrypt certificates");
      toggle("File Roots", "file_roots", "Filesystem root mounts configured in file explorer");

      // Custom paths
      const customPathsContainer = el("div", { class: "flex flex-col gap-2" });
      const customPathsList = el("div", { class: "flex flex-wrap gap-1.5 min-h-8 p-2 rounded bg-black/20 border border-white/5 text-xs font-mono" });

      function renderCustomPaths(): void {
        customPathsList.replaceChildren();
        if (job.sources.custom.length === 0) {
          customPathsList.append(el("span", { class: "text-neutral-500 italic text-[11px]", text: "No custom directories added yet." }));
          return;
        }
        for (let i = 0; i < job.sources.custom.length; i++) {
          const path = job.sources.custom[i];
          const badgeEl = el("div", { class: "flex items-center gap-1.5 px-2 py-1 rounded bg-sky-950/40 border border-sky-800/50 text-sky-300 text-xs" }, [
            el("span", { text: `📁 ${path}` }),
            button("✕", "text-sky-400 hover:text-white text-[10px] p-0 font-bold ml-1", () => {
              job.sources.custom.splice(i, 1);
              renderCustomPaths();
            }),
          ]);
          customPathsList.append(badgeEl);
        }
      }
      renderCustomPaths();

      const addFolderBtn = button(
        "+ Add Directory",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold px-3 py-1 text-xs hover:bg-sky-500/30",
        () => {
          openFolderPicker("Choose custom folder to back up", (dirPath) => {
            if (!job.sources.custom.includes(dirPath)) {
              job.sources.custom.push(dirPath);
              renderCustomPaths();
            }
          });
        },
      );

      customPathsContainer.append(
        el("div", { class: "flex items-center justify-between" }, [
          el("span", { class: "font-semibold text-xs text-neutral-300", text: "Custom Paths to Include" }),
          addFolderBtn,
        ]),
        customPathsList,
      );

      const excludesInput = textInput(
        job.excludes.join(", "),
        (v) => {
          job.excludes = v.split(",").map((s) => s.trim()).filter(Boolean);
        },
        { placeholder: "*.tmp, cache, node_modules, .git" },
      );

      contentArea.append(
        el("span", { class: "font-semibold text-xs text-neutral-300", text: "Automated Homelab Data Sources" }),
        hostSourcesBox,
        customPathsContainer,
        field("Exclude Patterns (comma-separated)", excludesInput, "File patterns or folder names to ignore during snapshot generation."),
      );
    }

    function renderBackendTab(): void {
      const backendKindSelect = select(
        job.backend.kind,
        [
          ["local", "Local Directory / Mounted NFS / External Drive"],
          ["s3", "S3 Compatible Object Storage (AWS, MinIO, Cloudflare R2, Wasabi)"],
          ["sftp", "SFTP / SSH Target Server"],
        ],
        (v) => {
          const kind = v as typeof job.backend.kind;
          if (kind === "local") {
            job.backend = { kind: "local", path: "/srv/backups" };
          } else if (kind === "s3") {
            job.backend = { kind: "s3", endpoint: "https://s3.amazonaws.com", bucket: "my-homelab-backups" };
          } else {
            job.backend = { kind: "sftp", target: "backup@remote.nas:/srv/backups" };
          }
          renderBackendTab();
        },
      );

      const backendDetails = el("div", { class: "flex flex-col gap-3 p-3 bg-black/30 rounded border border-white/5" });

      if (job.backend.kind === "local") {
        const localPath = job.backend.path;
        const pathInput = textInput(localPath, (v) => {
          if (job.backend.kind === "local") job.backend.path = v;
        }, { placeholder: "/srv/backups or /mnt/storage/backups" }) as HTMLInputElement;

        const browseBtn = button(
          "Browse…",
          "border-neutral-600 bg-white/5 text-neutral-300 hover:text-white text-xs px-3 py-1",
          () => {
            openFolderPicker("Choose destination folder for repository", (dir) => {
              if (job.backend.kind === "local") {
                job.backend.path = dir;
                pathInput.value = dir;
              }
            });
          },
        );

        backendDetails.append(
          field("Local Directory Path", el("div", { class: "flex items-center gap-2" }, [pathInput, browseBtn])),
          el("span", { class: "text-[11px] text-neutral-400", text: "Target directory on the server or an externally mounted ZFS/NFS pool." }),
        );
      } else if (job.backend.kind === "s3") {
        const currentEndpoint = job.backend.endpoint;
        const currentBucket = job.backend.bucket;

        const endpointInput = textInput(currentEndpoint, (v) => {
          if (job.backend.kind === "s3") job.backend.endpoint = v;
        }, { placeholder: "https://s3.us-east-1.amazonaws.com or https://minio.local:9000" });

        const bucketInput = textInput(currentBucket, (v) => {
          if (job.backend.kind === "s3") job.backend.bucket = v;
        }, { placeholder: "bucket-name" });

        backendDetails.append(
          field("S3 Endpoint URL", endpointInput),
          field("S3 Bucket Name", bucketInput),
        );
      } else {
        const currentTarget = job.backend.target;
        const targetInput = textInput(currentTarget, (v) => {
          if (job.backend.kind === "sftp") job.backend.target = v;
        }, { placeholder: "user@hostname:/remote/backup/path" });

        backendDetails.append(
          field("SFTP SSH Target", targetInput, "Format: user@host:/path. Ensure SSH key authentication is configured."),
        );
      }

      contentArea.append(
        field("Destination Storage Backend", backendKindSelect),
        backendDetails,
      );
    }

    function renderSecurityTab(): void {
      const passInput = textInput(
        repositoryPassword,
        (v) => {
          repositoryPassword = v;
        },
        {
          type: "password",
          placeholder: hasSecret(job) ? "Leave blank to keep current stored password" : "Minimum 8 characters",
        },
      );

      const securityBox = el("div", { class: "flex flex-col gap-2 p-3 bg-black/30 rounded border border-white/5 text-xs" }, [
        el("div", { class: "flex items-center gap-2 text-emerald-400 font-semibold" }, [
          el("span", { text: "🔒 AES-256 Client-Side Encryption" }),
        ]),
        el("span", {
          class: "text-neutral-400 text-[11px] leading-relaxed",
          text: "Restic repositories are end-to-end encrypted. Even if backed up to an unverified cloud provider, no one can read your files without this password.",
        }),
        field("Repository Encryption Passphrase", passInput),
      ]);

      if (job.backend.kind === "s3") {
        const s3Access = textInput(s3AccessKey, (v) => {
          s3AccessKey = v;
        }, { placeholder: "AKIAIOSFODNN7EXAMPLE" });
        const s3Secret = textInput(s3SecretKey, (v) => {
          s3SecretKey = v;
        }, { type: "password", placeholder: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" });

        securityBox.append(
          field("S3 Access Key ID", s3Access),
          field("S3 Secret Access Key", s3Secret),
        );
      }

      // Retention rules
      const retentionGrid = el("div", { class: "grid grid-cols-2 md:grid-cols-5 gap-2 text-xs" });
      const addRet = (label: string, key: keyof typeof job.retention) => {
        const inp = numberInput(job.retention[key], (v) => {
          job.retention[key] = v;
        }, { min: 0 });
        retentionGrid.append(field(label, inp));
      };

      addRet("Keep Last", "keep_last");
      addRet("Daily", "keep_daily");
      addRet("Weekly", "keep_weekly");
      addRet("Monthly", "keep_monthly");
      addRet("Yearly", "keep_yearly");

      contentArea.append(
        securityBox,
        el("div", { class: "flex flex-col gap-1 mt-1" }, [
          el("span", { class: "font-semibold text-xs text-neutral-300", text: "Snapshot Retention Policy" }),
          el("span", { class: "text-[11px] text-neutral-500", text: "Automatically prunes older snapshots during maintenance runs." }),
          retentionGrid,
        ]),
      );
    }

    renderTabs();

    modal.body.append(
      tabHeaders,
      contentArea,
      errorEl,
    );

    const saveBtn = button(
      isNewJob ? "Create Job" : "Save Changes",
      "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm",
      async () => {
        errorEl.textContent = "";
        if (!job.name.trim()) {
          errorEl.textContent = "Job Name is required.";
          switchTab("general");
          return;
        }
        if (!job.id.trim()) {
          job.id = job.name
            .toLowerCase()
            .replace(/[^a-z0-9]+/g, "-")
            .replace(/^-+|-+$/g, "");
          if (!job.id) job.id = `job-${Date.now()}`;
        }
        if (!job.secret_ref.trim()) {
          job.secret_ref = job.id;
        }

        saveBtn.disabled = true;
        saveBtn.textContent = "Saving…";

        try {
          const jobs = isNewJob
            ? [...state!.config.jobs, job]
            : state!.config.jobs.map((entry) => (entry.id === job.id ? job : entry));

          const res = await backupApi.saveConfig({ jobs });
          state = res;

          const pw = repositoryPassword.trim();
          if (pw) {
            try {
              await backupApi.saveSecrets(
                job.id,
                pw,
                s3AccessKey.trim() || null,
                s3SecretKey.trim() || null,
                true,
              );
            } catch (err) {
              showToast({ tone: "error", title: "Secret Storage Warning", message: `Warning: Job saved but secret could not be stored: ${errorText(err)}` });
            }
          }

          showToast({ tone: "ok", title: isNewJob ? "Job Created" : "Job Updated", message: isNewJob ? `Created backup job '${job.name}'` : `Updated backup job '${job.name}'` });
          modal.close();
          await load();
        } catch (err) {
          errorEl.textContent = errorText(err);
          saveBtn.disabled = false;
          saveBtn.textContent = isNewJob ? "Create Job" : "Save Changes";
        }
      },
    );

    modal.body.append(
      el("div", { class: "flex items-center justify-between pt-2 border-t border-white/10" }, [
        button("Cancel", "text-neutral-400 hover:text-white text-xs px-3 py-1", () => modal.close()),
        saveBtn,
      ]),
    );
  }

  function historyPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("Recent Backup History");
    cardBody.className = "flex flex-col gap-2 p-3 text-xs bg-[#11141c]/60 rounded border border-white/5";
    const history = state?.history ?? [];
    if (history.length === 0) {
      cardBody.append(emptyNote("No backup runs recorded yet."));
      return card;
    }
    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "When" }),
          el("th", { class: "py-2 px-3", text: "Job Name" }),
          el("th", { class: "py-2 px-3", text: "Result" }),
          el("th", { class: "py-2 px-3 text-right", text: "Bytes Processed" }),
          el("th", { class: "py-2 px-3", text: "Status Detail" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    for (const run of history) {
      tbody.append(
        el("tr", { class: "hover:bg-white/[0.02]" }, [
          el("td", { class: "py-2 px-3 text-neutral-400 font-sans", text: formatTime(run.ts_ms) }),
          el("td", { class: "py-2 px-3 text-neutral-200 font-medium", text: run.name }),
          el("td", { class: `py-2 px-3 font-semibold ${run.ok ? "text-emerald-400" : "text-rose-400"}`, text: run.ok ? "✔ OK" : "✖ Failed" }),
          el("td", { class: "py-2 px-3 text-right text-neutral-400 tabular", text: run.bytes ? bytes(run.bytes) : "—" }),
          el("td", { class: "py-2 px-3 text-neutral-400 font-sans truncate max-w-sm", text: run.detail }),
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
