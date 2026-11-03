/**
 * Shares page: Samba and NFS.
 *
 * Both tabs follow the router's draft → preview → apply shape: edits stay local
 * until saved, the rendered config is shown verbatim, and applying reports a
 * per-step result. The Samba tab also manages Samba accounts.
 */

import { filesApi, nfsApi, sambaApi } from "../api";
import type { NfsState, SambaState, SmbConfig, SmbShare, StepResult } from "../api";
import { showToast } from "../toast";
import { button, checkbox, el, emptyNote, field, openModal, panel, textInput } from "../ui";

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

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-500" });
  const tabBar = el("div", { class: "flex gap-1 border-b border-white/5 pb-1" });
  const body = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2" });

  const refreshButton = button("refresh", "", () => void load());
  const saveButton = button("save", "", () => void saveDraft());
  const applyButton = button("apply", "text-emerald-400 hover:border-emerald-500", () =>
    void (tab === "samba" ? applyDraft() : applyNfs()),
  );

  const header = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-neutral-300", text: "shares" }),
    refreshButton,
    saveButton,
    applyButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  root.append(header, tabBar, body);

  function syncDraftUsers(): void {
    if (!draft) return;
    if (!draft.users) draft.users = [];
    const known = new Set(draft.users.map((u) => u.username));
    if (samba?.users) {
      for (const u of samba.users) {
        if (!known.has(u.username)) {
          draft.users.push({ username: u.username, enabled: true });
          known.add(u.username);
        }
      }
    }
    for (const s of draft.shares) {
      for (const vu of s.valid_users) {
        if (!known.has(vu)) {
          draft.users.push({ username: vu, enabled: true });
          known.add(vu);
        }
      }
      if (s.force_user && !known.has(s.force_user)) {
        draft.users.push({ username: s.force_user, enabled: true });
        known.add(s.force_user);
      }
    }
  }

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      samba = await sambaApi.state();
      nfs = await nfsApi.state();
      draft = structuredClone(samba.config);
      syncDraftUsers();
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
    syncDraftUsers();
    busy = true;
    status = "saving…";
    render();
    try {
      samba = await sambaApi.save(draft);
      draft = structuredClone(samba.config);
      syncDraftUsers();
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
    syncDraftUsers();
    busy = true;
    status = "applying…";
    render();
    try {
      const report = await sambaApi.apply(draft);
      samba = report.state;
      draft = structuredClone(samba.config);
      syncDraftUsers();
      steps = report.steps;
      status = report.steps.every((step) => step.ok) ? "applied" : "applied with problems";
    } catch (err) {
      status = errorText(err);
      steps = [{
        subsystem: "samba",
        ok: false,
        detail: errorText(err),
      }];
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
      steps = [{
        subsystem: "nfs",
        ok: false,
        detail: errorText(err),
      }];
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
      const report = reportPanel();
      if (report) body.append(report);
      body.append(sambaSettings());
      body.append(sharesEditor());
      body.append(accountsPanel());
      body.append(previewPanel("smb.conf", samba.preview));
    } else {
      if (!nfs) {
        body.append(emptyNote(status || "the NFS API is unavailable"));
        return;
      }
      const problems = problemsPanel(nfs.problems, nfs.config_error);
      if (problems) body.append(problems);
      const report = reportPanel();
      if (report) body.append(report);
      body.append(nfsSettings());
      body.append(exportsEditor());
      body.append(previewPanel("/etc/exports.d/stack-stack.exports", nfs.preview));
      body.append(activeExports());
    }
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
        class: "text-[11px] text-neutral-600",
        text: samba!.lan_cidr ? `hosts allow = ${samba!.lan_cidr}` : "no LAN network detected",
      }),
    );
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
          const parentBtn = button("📁 .. (parent directory)", "w-full justify-start text-left text-neutral-400 hover:text-white text-xs py-1 px-1.5", () => {
            void navigateTo(fileState.listing.parent!);
          });
          dirList.append(parentBtn);
        }

        const dirs = fileState.listing.entries.filter((e) => e.kind === "dir");
        if (dirs.length === 0) {
          dirList.append(el("span", { class: "p-2 italic text-neutral-500 text-xs", text: "no subdirectories" }));
        } else {
          for (const d of dirs) {
            const btn = button(`📁 ${d.name}/`, "w-full justify-start text-left text-emerald-400 hover:underline text-xs py-1 px-1.5", () => {
              void navigateTo(d.path);
            });
            dirList.append(btn);
          }
        }
      } catch (err) {
        dirList.replaceChildren(el("span", { class: "p-2 text-rose-400 text-xs", text: errorText(err) }));
      }
    }

    void navigateTo("/srv");
  }

  function openAddUserModal(): void {
    const modal = openModal("Create Samba User Account", "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    let username = "";
    let password = "";
    let confirm = "";
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const userInput = textInput("", (v) => { username = v; }, { placeholder: "username (e.g. omar, media, backup)" });
    const passInput = textInput("", (v) => { password = v; }, { type: "password", placeholder: "Minimum 8 characters" });
    const confirmInput = textInput("", (v) => { confirm = v; }, { type: "password", placeholder: "Confirm password" });

    modal.body.append(
      el("div", { class: "text-xs text-neutral-400 leading-relaxed", text: "Create an authenticated SMB user account to access private or restricted network shares from Windows, macOS, or Linux." }),
      field("Username", userInput, "Samba authentication user name."),
      field("Password", passInput, "Samba user password (minimum 8 characters)."),
      field("Confirm Password", confirmInput),
      errorEl,
    );

    const submitBtn = button("Create Account", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", async () => {
      errorEl.textContent = "";
      if (!username.trim()) {
        errorEl.textContent = "Username is required.";
        return;
      }
      if (password.length < 8) {
        errorEl.textContent = "Password must be at least 8 characters long.";
        return;
      }
      if (password !== confirm) {
        errorEl.textContent = "Passwords do not match.";
        return;
      }

      submitBtn.disabled = true;
      submitBtn.textContent = "Creating Account…";
      try {
        await sambaApi.setUser(username.trim(), password, true);
        if (draft && !draft.users.some((u) => u.username === username.trim())) {
          draft.users.push({ username: username.trim(), enabled: true });
        }
        showToast({ tone: "ok", title: "Samba Account Created", message: `Account '${username.trim()}' is active.` });
        modal.close();
        await load();
      } catch (err) {
        submitBtn.disabled = false;
        submitBtn.textContent = "Create Account";
        errorEl.textContent = errorText(err);
      }
    });

    const footer = el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
      button("Cancel", "text-neutral-400 hover:text-white text-xs", () => modal.close()),
      submitBtn,
    ]);
    modal.body.append(footer);
  }

  function openSetPasswordModal(username: string): void {
    const modal = openModal(`Change Samba Password: ${username}`, "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    let password = "";
    let confirm = "";
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const passInput = textInput("", (v) => { password = v; }, { type: "password", placeholder: "New password (min 8 characters)" });
    const confirmInput = textInput("", (v) => { confirm = v; }, { type: "password", placeholder: "Confirm new password" });

    modal.body.append(
      field("New Password", passInput),
      field("Confirm Password", confirmInput),
      errorEl,
    );

    const submitBtn = button("Update Password", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", async () => {
      errorEl.textContent = "";
      if (password.length < 8) {
        errorEl.textContent = "Password must be at least 8 characters long.";
        return;
      }
      if (password !== confirm) {
        errorEl.textContent = "Passwords do not match.";
        return;
      }

      submitBtn.disabled = true;
      submitBtn.textContent = "Updating…";
      try {
        await sambaApi.setUser(username, password, true);
        showToast({ tone: "ok", title: "Password Updated", message: `Password for '${username}' has been updated.` });
        modal.close();
        await load();
      } catch (err) {
        submitBtn.disabled = false;
        submitBtn.textContent = "Update Password";
        errorEl.textContent = errorText(err);
      }
    });

    const footer = el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
      button("Cancel", "text-neutral-400 hover:text-white text-xs", () => modal.close()),
      submitBtn,
    ]);
    modal.body.append(footer);
  }

  function openShareModal(share?: SmbShare, editIdx?: number): void {
    const modal = openModal(share ? `Edit Share: ${share.name}` : "Create Samba Network Share", "max-w-xl");
    modal.body.className = "flex flex-col gap-4";

    const s: SmbShare = share ? structuredClone(share) : freshShare();
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const nameInput = textInput(s.name, (v) => { s.name = v; }, { placeholder: "e.g. media, backups, docs" });
    const pathInput = textInput(s.path, (v) => { s.path = v; }, { placeholder: "/srv/media or /home/omar/share" });
    const browseBtn = button("📁 Browse…", "border-white/10 hover:border-white/30 text-xs px-2.5", () => {
      openFolderPicker("Choose Share Directory", (chosen) => {
        s.path = chosen;
        pathInput.value = chosen;
      });
    });
    const pathRow = el("div", { class: "flex items-center gap-1.5" }, [pathInput, browseBtn]);

    const commentInput = textInput(s.comment, (v) => { s.comment = v; }, { placeholder: "Windows/Mac network share" });

    const browsableBox = checkbox(s.browsable, (v) => { s.browsable = v; }, "Browsable (Visible in network browsers / Finder)");
    const roBox = checkbox(s.read_only, (v) => { s.read_only = v; }, "Read-Only (Protect share against writes & deletes)");
    const guestBox = checkbox(s.guest_ok, (v) => { s.guest_ok = v; }, "Guest Access (Allow connection without user login)");

    const permissionsSection = el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" }, [
      el("span", { class: "text-xs font-semibold text-neutral-300", text: "Access & Visibility" }),
      el("div", { class: "grid grid-cols-1 md:grid-cols-3 gap-2" }, [browsableBox, roBox, guestBox]),
    ]);

    // Valid Users
    const usersBox = el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
    usersBox.append(el("span", { class: "text-xs font-semibold text-neutral-300", text: "Authorized Users (Empty allows all logged-in accounts)" }));

    const userPills = el("div", { class: "flex flex-wrap gap-2" });
    if (samba?.users && samba.users.length > 0) {
      for (const u of samba.users) {
        const isSelected = s.valid_users.includes(u.username);
        const pill = checkbox(isSelected, (checked) => {
          if (checked) {
            if (!s.valid_users.includes(u.username)) s.valid_users.push(u.username);
          } else {
            s.valid_users = s.valid_users.filter((x) => x !== u.username);
          }
        }, u.username);
        userPills.append(pill);
      }
    } else {
      userPills.append(el("span", { class: "text-xs text-neutral-500 italic", text: "No Samba accounts created yet." }));
    }
    usersBox.append(userPills);

    modal.body.append(
      field("Share Name", nameInput, "Name of the network share as shown on Windows / macOS."),
      field("Host Path", pathRow, "Directory on the physical host filesystem to share."),
      field("Comment / Description", commentInput),
      permissionsSection,
      usersBox,
      errorEl,
    );

    const submitBtn = button("Save Share", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", () => {
      errorEl.textContent = "";
      if (!s.name.trim()) {
        errorEl.textContent = "Share name is required.";
        return;
      }
      if (!s.path.trim()) {
        errorEl.textContent = "Host path is required.";
        return;
      }

      s.name = s.name.trim();
      s.path = s.path.trim();

      if (editIdx !== undefined && draft) {
        draft.shares[editIdx] = s;
      } else if (draft) {
        draft.shares.push(s);
      }
      syncDraftUsers();
      modal.close();
      render();
    });

    const footer = el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
      button("Cancel", "text-neutral-400 hover:text-white text-xs", () => modal.close()),
      submitBtn,
    ]);
    modal.body.append(footer);
  }

  function openAddExportModal(existing?: NfsState["config"]["exports"][number], editIdx?: number): void {
    const modal = openModal(existing ? "Edit NFS Export" : "Create NFS Export", "max-w-lg");
    modal.body.className = "flex flex-col gap-3";

    let path = existing?.path || "/srv/share";
    let clients = existing?.clients.join(", ") || "192.168.0.0/24";
    let ro = existing?.ro ?? false;
    let sync = existing?.sync ?? true;
    let no_subtree_check = existing?.no_subtree_check ?? true;
    let root_squash = existing?.root_squash ?? true;

    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const pathInput = textInput(path, (v) => { path = v; }, { placeholder: "/srv/nfs/share" });
    const browseBtn = button("📁 Browse…", "border-white/10 hover:border-white/30 text-xs px-2.5", () => {
      openFolderPicker("Choose NFS Export Directory", (chosen) => {
        path = chosen;
        pathInput.value = chosen;
      });
    });
    const pathRow = el("div", { class: "flex items-center gap-1.5" }, [pathInput, browseBtn]);
    const clientsInput = textInput(clients, (v) => { clients = v; }, { placeholder: "* or 192.168.0.0/24 or 192.168.0.50" });

    const roBox = checkbox(ro, (v) => { ro = v; }, "Read-Only (ro)");
    const syncBox = checkbox(sync, (v) => { sync = v; }, "Synchronous (sync - safe against host power loss)");
    const subtreeBox = checkbox(no_subtree_check, (v) => { no_subtree_check = v; }, "No Subtree Check (recommended for speed)");
    const squashBox = checkbox(root_squash, (v) => { root_squash = v; }, "Root Squash (map remote root to nobody for security)");

    modal.body.append(
      field("Export Path", pathRow, "Directory on host to export over NFS."),
      field("Allowed Clients", clientsInput, "IP address, subnet CIDR, or '*' for all LAN clients."),
      el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" }, [
        el("span", { class: "text-xs font-semibold text-neutral-300", text: "NFS Export Options" }),
        el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-2" }, [roBox, syncBox, subtreeBox, squashBox]),
      ]),
      errorEl,
    );

    const submitBtn = button("Save Export", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", () => {
      errorEl.textContent = "";
      if (!path.trim()) {
        errorEl.textContent = "Export path is required.";
        return;
      }
      const clientList = clients.split(",").map((s) => s.trim()).filter(Boolean);
      if (clientList.length === 0) clientList.push("*");

      const entry = {
        path: path.trim(),
        clients: clientList,
        ro,
        sync,
        no_subtree_check,
        root_squash,
      };

      if (editIdx !== undefined && nfs) {
        nfs.config.exports[editIdx] = entry;
      } else if (nfs) {
        nfs.config.exports.push(entry);
      }
      modal.close();
      render();
    });

    const footer = el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
      button("Cancel", "text-neutral-400 hover:text-white text-xs", () => modal.close()),
      submitBtn,
    ]);
    modal.body.append(footer);
  }

  function sharesEditor(): HTMLElement {
    const { root: card, body: cardBody } = panel("samba shares");
    cardBody.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "Windows & macOS Network Shares" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Shared folders accessible over SMB/CIFS across your home network." }),
      ]),
      button(
        "+ Add Share",
        "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openShareModal(),
      ),
    );
    cardBody.append(topBar);

    if (!draft?.shares || draft.shares.length === 0) {
      cardBody.append(emptyNote("No Samba network shares configured yet. Click '+ Add Share' above to create one."));
      return card;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "Share Name" }),
          el("th", { class: "py-2 px-3", text: "Host Path" }),
          el("th", { class: "py-2 px-3", text: "Access Permissions" }),
          el("th", { class: "py-2 px-3", text: "Allowed Users" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );

    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    draft.shares.forEach((share, idx) => {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });

      const nameTd = el("td", { class: "py-2 px-3 font-semibold text-neutral-100 font-sans" });
      nameTd.append(
        el("span", { text: share.name }),
        share.comment ? el("span", { class: "block text-[10px] text-neutral-500 font-normal", text: share.comment }) : el("span"),
      );

      const pathTd = el("td", { class: "py-2 px-3 text-emerald-400" }, [
        el("span", { text: `📁 ${share.path}` }),
      ]);

      const accessTd = el("td", { class: "py-2 px-3 font-sans" });
      const badges = el("div", { class: "flex items-center gap-1.5 text-[10px]" });
      badges.append(
        el("span", { class: `px-1.5 py-0.5 rounded border ${share.browsable ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-300" : "border-neutral-700 text-neutral-500"}`, text: share.browsable ? "Browsable" : "Hidden" }),
        el("span", { class: `px-1.5 py-0.5 rounded border ${share.read_only ? "border-amber-500/30 bg-amber-500/10 text-amber-300" : "border-emerald-500/30 bg-emerald-500/10 text-emerald-300"}`, text: share.read_only ? "Read-Only" : "Read/Write" }),
        el("span", { class: `px-1.5 py-0.5 rounded border ${share.guest_ok ? "border-sky-500/30 bg-sky-500/10 text-sky-300" : "border-neutral-700 text-neutral-400"}`, text: share.guest_ok ? "Guest Allowed" : "Auth Required" }),
      );
      accessTd.append(badges);

      const usersTd = el("td", { class: "py-2 px-3 font-sans text-[11px] text-neutral-300" });
      usersTd.textContent = share.valid_users.length > 0 ? share.valid_users.join(", ") : "All authenticated users";

      const actionsTd = el("td", { class: "py-2 px-3 text-right font-sans" });
      const editBtn = button("Edit", "mr-1 text-[11px] text-neutral-300 hover:text-white px-2 py-0.5", () => openShareModal(share, idx));
      const delBtn = button("Delete", "text-[11px] text-rose-400 hover:text-rose-300 px-2 py-0.5", () => {
        if (window.confirm(`Delete share '${share.name}'?`)) {
          draft?.shares.splice(idx, 1);
          render();
        }
      });
      actionsTd.append(editBtn, delBtn);

      tr.append(nameTd, pathTd, accessTd, usersTd, actionsTd);
      tbody.append(tr);
    });

    table.append(tbody);
    cardBody.append(table);
    return card;
  }

  function accountsPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("samba accounts");
    cardBody.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "Samba User Accounts" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Authenticated accounts used to log in to protected network shares." }),
      ]),
      button(
        "+ New Samba Account",
        "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openAddUserModal(),
      ),
    );
    cardBody.append(topBar);

    if (samba!.users_error) {
      cardBody.append(el("div", { class: "text-xs text-amber-400 p-2 rounded bg-amber-500/10 border border-amber-500/20", text: samba!.users_error }));
      return card;
    }
    if (samba!.users.length === 0) {
      cardBody.append(emptyNote("No Samba user accounts configured yet. Click '+ New Samba Account' above to create one."));
      return card;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "User" }),
          el("th", { class: "py-2 px-3", text: "UID" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5" });
    for (const user of samba!.users) {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
      tr.append(
        el("td", { class: "py-2 px-3 font-semibold text-neutral-200 flex items-center gap-2" }, [
          el("span", { text: "👤" }),
          el("span", { text: user.username }),
        ]),
        el("td", { class: "py-2 px-3 text-neutral-500 font-mono", text: user.uid ? String(user.uid) : "—" }),
        el("td", { class: "py-2 px-3 text-right" }, [
          button("Change Password", "mr-2 text-xs text-neutral-300 hover:text-white px-2 py-0.5", () => openSetPasswordModal(user.username)),
          button("Delete", "text-xs text-rose-400 hover:text-rose-300 px-2 py-0.5", async () => {
            if (window.confirm(`Delete Samba user '${user.username}'?`)) {
              await withBusy(() => sambaApi.removeUser(user.username));
              if (draft) {
                draft.users = draft.users.filter((u) => u.username !== user.username);
                for (const s of draft.shares) {
                  s.valid_users = s.valid_users.filter((u) => u !== user.username);
                  if (s.force_user === user.username) s.force_user = null;
                }
              }
              showToast({ tone: "ok", title: "User Removed", message: `Samba user '${user.username}' deleted.` });
              await load();
            }
          }),
        ]),
      );
      tbody.append(tr);
    }
    table.append(tbody);
    cardBody.append(table);
    return card;
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
    const { root: card, body: cardBody } = panel("nfs exports");
    cardBody.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "NFS Shared Directories" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Network File System exports for high-performance Linux, Proxmox, and VMware storage." }),
      ]),
      button(
        "+ Add Export",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openAddExportModal(),
      ),
    );
    cardBody.append(topBar);

    if (!nfs?.config.exports || nfs.config.exports.length === 0) {
      cardBody.append(emptyNote("No NFS exports configured yet. Click '+ Add Export' above to create one."));
      return card;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "Export Path" }),
          el("th", { class: "py-2 px-3", text: "Allowed Clients" }),
          el("th", { class: "py-2 px-3", text: "Options" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    nfs.config.exports.forEach((exp, idx) => {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
      tr.append(
        el("td", { class: "py-2 px-3 text-sky-300 font-mono", text: `📁 ${exp.path}` }),
        el("td", { class: "py-2 px-3 text-neutral-300", text: exp.clients.join(", ") }),
        el("td", { class: "py-2 px-3 text-neutral-400 text-[10px]" }, [
          el("span", { text: `${exp.ro ? "ro" : "rw"}, ${exp.sync ? "sync" : "async"}${exp.root_squash ? ", root_squash" : ", no_root_squash"}` }),
        ]),
        el("td", { class: "py-2 px-3 text-right font-sans" }, [
          button("Edit", "mr-1 text-xs text-neutral-300 hover:text-white px-2 py-0.5", () => openAddExportModal(exp, idx)),
          button("Delete", "text-xs text-rose-400 hover:text-rose-300 px-2 py-0.5", () => {
            if (window.confirm(`Delete NFS export '${exp.path}'?`)) {
              nfs?.config.exports.splice(idx, 1);
              render();
            }
          }),
        ]),
      );
      tbody.append(tr);
    });
    table.append(tbody);
    cardBody.append(table);
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
        cardBody.append(el("div", { class: "text-neutral-400", text: `${entry.path} → ${entry.clients}` }));
      }
    }
    return card;
  }

  function previewPanel(title: string, content: string): HTMLElement {
    const { root: card, body: cardBody } = panel(title);
    const pre = el("pre", {
      class: "max-h-64 overflow-auto px-3 py-2 font-mono text-[10px] text-neutral-400",
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
