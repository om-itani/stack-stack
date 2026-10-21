/**
 * Shares page: Samba and NFS.
 *
 * Both tabs follow the router's draft → preview → apply shape: edits stay local
 * until saved, the rendered config is shown verbatim, and applying reports a
 * per-step result. The Samba tab also manages Samba accounts.
 */

import { nfsApi, sambaApi } from "../api";
import type { NfsState, SambaState, SmbConfig, SmbShare, StepResult } from "../api";
import { button, checkbox, el, emptyNote, field, panel, tableEditor, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function freshShare(): SmbShare {
  return {
    name: "",
    path: "",
    browsable: true,
    read_only: false,
    guest_ok: false,
    valid_users: [],
    force_user: null,
    create_mask: "0664",
    dir_mask: "0775",
    comment: "",
  };
}

export function createSharesPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 p-3" });

  let tab: "samba" | "nfs" = "samba";
  let samba: SambaState | null = null;
  let nfs: NfsState | null = null;
  let draft: SmbConfig | null = null;
  let steps: StepResult[] | null = null;
  let busy = false;
  let status = "";
  let loaded = false;

  const statusLine = el("span", { class: "tabular text-[11px] text-slate-500" });
  const tabBar = el("div", { class: "flex gap-1 border-b border-[#1e2733] pb-1" });
  const body = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2" });

  const refreshButton = button("refresh", "", () => void load());
  const saveButton = button("save", "", () => void saveDraft());
  const applyButton = button("apply", "text-emerald-400 hover:border-emerald-500", () =>
    void (tab === "samba" ? applyDraft() : applyNfs()),
  );

  const header = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-slate-300", text: "shares" }),
    refreshButton,
    saveButton,
    applyButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  root.append(header, tabBar, body);

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      samba = await sambaApi.state();
      nfs = await nfsApi.state();
      draft = structuredClone(samba.config);
      status = "";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      loaded = true;
      render();
    }
  }

  async function saveDraft(): Promise<void> {
    if (!draft || busy) return;
    busy = true;
    status = "saving…";
    render();
    try {
      samba = await sambaApi.save(draft);
      draft = structuredClone(samba.config);
      status = "saved";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function applyDraft(): Promise<void> {
    if (!draft || busy) return;
    busy = true;
    status = "applying…";
    render();
    try {
      const report = await sambaApi.apply(draft);
      samba = report.state;
      draft = structuredClone(samba.config);
      steps = report.steps;
      status = report.steps.every((step) => step.ok) ? "applied" : "applied with problems";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function applyNfs(): Promise<void> {
    if (!nfs || busy) return;
    busy = true;
    status = "applying…";
    render();
    try {
      const report = await nfsApi.apply(nfs.config);
      nfs = report.state;
      steps = report.steps;
      status = report.steps.every((step) => step.ok) ? "applied" : "applied with problems";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  function render(): void {
    statusLine.textContent = status;
    saveButton.disabled = busy || tab !== "samba";
    applyButton.disabled = busy;

    tabBar.replaceChildren();
    for (const entry of ["samba", "nfs"] as const) {
      tabBar.append(
        button(entry, entry === tab ? "text-emerald-400" : "", () => {
          tab = entry;
          steps = null;
          render();
        }),
      );
    }
    applyButton.textContent = tab === "samba" ? "apply samba" : "apply nfs";

    body.replaceChildren();
    if (!loaded) {
      body.append(emptyNote("loading…"));
      return;
    }

    if (tab === "samba") {
      if (!samba || !draft) {
        body.append(emptyNote(status || "the Samba API is unavailable"));
        return;
      }
      const problems = problemsPanel(samba.problems, samba.config_error);
      if (problems) body.append(problems);
      body.append(sambaSettings());
      body.append(sharesEditor());
      body.append(accountsPanel());
      body.append(previewPanel("smb.conf", samba.preview));
    } else {
      if (!nfs) {
        body.append(emptyNote(status || "the NFS API is unavailable"));
        return;
      }
      body.append(nfsSettings());
      body.append(exportsEditor());
      body.append(previewPanel("/etc/exports", nfs.preview));
      body.append(activeExports());
    }
    const report = reportPanel();
    if (report) body.append(report);
  }

  function problemsPanel(problems: { field: string; message: string }[], configError: string | null): HTMLElement | null {
    if (problems.length === 0 && !configError) return null;
    const box = el("div", { class: "border border-rose-800 bg-rose-950/40 px-3 py-2 text-[11px] text-rose-300" });
    if (configError) box.append(el("div", { text: configError }));
    for (const problem of problems) {
      box.append(el("div", { text: `${problem.field}: ${problem.message}` }));
    }
    return box;
  }

  function sambaSettings(): HTMLElement {
    const { root: card, body: cardBody } = panel("samba");
    cardBody.className = "flex flex-wrap items-end gap-3 px-3 py-2";
    cardBody.append(
      field("enabled", checkbox(draft!.enabled, (value) => (draft!.enabled = value), "run samba")),
      field("workgroup", textInput(draft!.workgroup, (value) => (draft!.workgroup = value))),
      field("server string", textInput(draft!.server_string, (value) => (draft!.server_string = value))),
      el("span", {
        class: "text-[11px] text-slate-600",
        text: samba!.lan_cidr ? `hosts allow = ${samba!.lan_cidr}` : "no LAN network detected",
      }),
    );
    return card;
  }

  function sharesEditor(): HTMLElement {
    const { root: card, body: cardBody } = panel("shares");
    cardBody.append(
      tableEditor(
        draft!.shares,
        [
          { header: "name", cell: (row) => textInput(row.name, (value) => (row.name = value)) },
          { header: "path", cell: (row) => textInput(row.path, (value) => (row.path = value)) },
          {
            header: "options",
            cell: (row) =>
              el("div", { class: "flex flex-wrap gap-2" }, [
                checkbox(row.browsable, (value) => (row.browsable = value), "browse"),
                checkbox(row.read_only, (value) => (row.read_only = value), "ro"),
                checkbox(row.guest_ok, (value) => (row.guest_ok = value), "guest"),
              ]),
          },
          {
            header: "valid users",
            cell: (row) =>
              textInput(row.valid_users.join(", "), (value) => {
                row.valid_users = value
                  .split(",")
                  .map((part) => part.trim())
                  .filter(Boolean);
              }),
          },
          {
            header: "masks",
            cell: (row) =>
              el("div", { class: "flex gap-1" }, [
                textInput(row.create_mask, (value) => (row.create_mask = value), { class: "w-16" }),
                textInput(row.dir_mask, (value) => (row.dir_mask = value), { class: "w-16" }),
              ]),
          },
        ],
        freshShare,
        render,
        "add share",
      ),
    );
    return card;
  }

  function accountsPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("samba accounts");
    cardBody.className = "flex flex-col gap-2 px-3 py-2";
    if (samba!.users_error) {
      cardBody.append(el("div", { class: "text-[11px] text-amber-400", text: samba!.users_error }));
    } else if (samba!.users.length === 0) {
      cardBody.append(emptyNote("no Samba accounts yet"));
    } else {
      const table = el("table", { class: "w-full text-[11px]" });
      table.append(
        el("thead", {}, [
          el("tr", {}, [
            el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-slate-600", text: "user" }),
            el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-slate-600", text: "uid" }),
            el("th", { class: "px-2 py-1" }),
          ]),
        ]),
      );
      const tbody = el("tbody");
      for (const user of samba!.users) {
        tbody.append(
          el("tr", { class: "border-b border-[#161d27]" }, [
            el("td", { class: "px-2 py-1 text-slate-300", text: user.username }),
            el("td", { class: "tabular px-2 py-1 text-slate-500", text: user.uid ?? "—" }),
            el("td", { class: "px-2 py-1 text-right" }, [
              button("set password", "mr-2 text-[10px]", () => void setPassword(user.username)),
              button("remove", "text-[10px] text-rose-400", async () => {
                await withBusy(() => sambaApi.removeUser(user.username));
                await load();
              }),
            ]),
          ]),
        );
      }
      table.append(tbody);
      cardBody.append(table);
    }
    return card;
  }

  async function setPassword(username: string): Promise<void> {
    const password = window.prompt(`New Samba password for ${username} (min 8 chars)`);
    if (!password) return;
    await withBusy(() => sambaApi.setUser(username, password, true));
    status = `password set for ${username}`;
    render();
  }

  function nfsSettings(): HTMLElement {
    const { root: card, body: cardBody } = panel("nfs");
    cardBody.className = "flex flex-wrap items-end gap-3 px-3 py-2";
    cardBody.append(
      field("enabled", checkbox(nfs!.config.enabled, (value) => (nfs!.config.enabled = value), "run nfs-server")),
      field("v4 only", checkbox(nfs!.config.v4, (value) => (nfs!.config.v4 = value), "NFSv4")),
    );
    return card;
  }

  function exportsEditor(): HTMLElement {
    const { root: card, body: cardBody } = panel("exports");
    cardBody.append(
      tableEditor(
        nfs!.config.exports,
        [
          { header: "path", cell: (row) => textInput(row.path, (value) => (row.path = value)) },
          {
            header: "clients",
            cell: (row) =>
              textInput(row.clients.join(", "), (value) => {
                row.clients = value
                  .split(",")
                  .map((part) => part.trim())
                  .filter(Boolean);
              }),
          },
          {
            header: "options",
            cell: (row) =>
              el("div", { class: "flex flex-wrap gap-2" }, [
                checkbox(row.ro, (value) => (row.ro = value), "ro"),
                checkbox(row.sync, (value) => (row.sync = value), "sync"),
                checkbox(row.no_subtree_check, (value) => (row.no_subtree_check = value), "no_subtree"),
                checkbox(row.root_squash, (value) => (row.root_squash = value), "root_squash"),
              ]),
          },
        ],
        () => ({
          path: "/srv/share",
          clients: ["192.168.1.0/24"],
          ro: false,
          sync: true,
          no_subtree_check: true,
          root_squash: true,
        }),
        render,
        "add export",
      ),
    );
    return card;
  }

  function activeExports(): HTMLElement {
    const { root: card, body: cardBody } = panel("active exports (host)");
    cardBody.className = "flex flex-col gap-1 px-3 py-2 text-[11px]";
    if (nfs?.active_error) {
      cardBody.append(el("div", { class: "text-amber-400", text: nfs.active_error }));
    } else if (!nfs || nfs.active.length === 0) {
      cardBody.append(emptyNote("nothing is currently exported"));
    } else {
      for (const entry of nfs.active) {
        cardBody.append(el("div", { class: "text-slate-400", text: `${entry.path} → ${entry.clients}` }));
      }
    }
    return card;
  }

  function previewPanel(title: string, content: string): HTMLElement {
    const { root: card, body: cardBody } = panel(title);
    const pre = el("pre", {
      class: "max-h-64 overflow-auto px-3 py-2 font-mono text-[10px] text-slate-400",
      text: content,
    });
    cardBody.append(pre);
    return card;
  }

  function reportPanel(): HTMLElement | null {
    if (!steps || steps.length === 0) return null;
    const { root: card, body: cardBody } = panel("last apply");
    cardBody.className = "flex flex-col gap-0.5 px-3 py-2 text-[11px]";
    for (const step of steps) {
      cardBody.append(
        el("div", {
          class: step.ok ? "text-emerald-400" : "text-rose-400",
          text: `${step.ok ? "ok" : "fail"} · ${step.subsystem}: ${step.detail}`,
        }),
      );
    }
    return card;
  }

  async function withBusy<T>(work: () => Promise<T>): Promise<T | null> {
    busy = true;
    render();
    try {
      return await work();
    } catch (err) {
      status = errorText(err);
      return null;
    } finally {
      busy = false;
      render();
    }
  }

  void load();
  return { root };
}
