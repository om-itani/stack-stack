/**
 * User Management & RBAC administration.
 */

import { authApi, usersApi } from "../api";
import type { UserRole, UserSummary } from "../api";
import { showToast } from "../toast";
import { button, dangerButton, el, emptyNote, field, openModal, select, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

export function createUsersPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-4 p-4 overflow-y-auto" });

  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-3 px-4 py-3" });
  const heading = el("span", { class: "panel-title text-xs font-semibold uppercase tracking-wider text-neutral-300", text: "User Management (RBAC)" });
  const reload = button("refresh", "px-3 py-1 text-xs");
  const addUserBtn = button("+ Add User", "px-3 py-1 text-xs font-semibold border-emerald-500/40 bg-emerald-500/15 text-emerald-300 hover:bg-emerald-500/25");
  const status = el("span", { class: "tabular truncate text-[11px] text-neutral-500" });
  toolbar.append(heading, reload, addUserBtn, el("span", { class: "ml-auto" }), status);

  // Role info banner
  const roleBanner = el("div", {
    class: "grid grid-cols-1 md:grid-cols-3 gap-3 text-xs",
  });

  const makeRoleCard = (title: string, color: string, desc: string) => {
    const card = el("div", { class: "rounded-lg border border-white/10 bg-white/5 p-3 flex flex-col gap-1" });
    const h = el("div", { class: `font-semibold text-xs ${color}`, text: title });
    const p = el("div", { class: "text-neutral-400 text-[11px]", text: desc });
    card.append(h, p);
    return card;
  };

  roleBanner.append(
    makeRoleCard("Admin", "text-purple-400", "Full root system access: manages users, disks, network, VMs, and system recovery."),
    makeRoleCard("Operator", "text-emerald-400", "Can deploy & manage containers, view logs, run backups. Blocked from user mgmt & disk formatting."),
    makeRoleCard("Viewer", "text-neutral-400", "Strict read-only access to metrics, dashboards, and live logs."),
  );

  const tableContainer = el("section", { class: "panel flex flex-col p-4" });
  root.append(toolbar, roleBanner, tableContainer);

  let currentUsername = "";
  let currentUserRole: UserRole = "admin";

  const renderUsers = (users: UserSummary[]) => {
    tableContainer.replaceChildren();

    if (users.length === 0) {
      tableContainer.append(emptyNote("No users registered yet."));
      return;
    }

    const table = el("table", { class: "w-full text-left text-xs" });
    const thead = el("thead", { class: "border-b border-white/10 text-neutral-400 text-[11px] uppercase tracking-wider" });
    const trHead = el("tr");
    trHead.append(
      el("th", { class: "pb-2 font-medium", text: "Username" }),
      el("th", { class: "pb-2 font-medium", text: "Role" }),
      el("th", { class: "pb-2 font-medium", text: "Created" }),
      el("th", { class: "pb-2 font-medium text-right", text: "Actions" }),
    );
    thead.append(trHead);

    const tbody = el("tbody", { class: "divide-y divide-white/5" });

    for (const u of users) {
      const row = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });

      // Username column
      const userCell = el("td", { class: "py-2.5 font-mono font-medium text-neutral-200" });
      userCell.textContent = u.username;
      if (u.username === currentUsername) {
        const youBadge = el("span", {
          class: "ml-2 text-[10px] rounded bg-white/10 px-1.5 py-0.5 text-neutral-300 font-sans",
          text: "you",
        });
        userCell.append(youBadge);
      }

      // Role badge column
      const roleCell = el("td", { class: "py-2.5" });
      let badgeClass = "border-neutral-500/40 bg-neutral-500/15 text-neutral-300";
      if (u.role === "admin") {
        badgeClass = "border-purple-500/40 bg-purple-500/15 text-purple-300";
      } else if (u.role === "operator") {
        badgeClass = "border-emerald-500/40 bg-emerald-500/15 text-emerald-300";
      }
      const roleTag = el("span", {
        class: `inline-flex items-center rounded px-2 py-0.5 text-[11px] font-semibold border ${badgeClass}`,
        text: u.role.toUpperCase(),
      });
      roleCell.append(roleTag);

      // Created date column
      const createdCell = el("td", { class: "py-2.5 text-neutral-400 font-mono text-[11px]" });
      createdCell.textContent = u.created_at > 0 ? new Date(u.created_at * 1000).toLocaleString() : "Initial Setup";

      // Actions column
      const actionsCell = el("td", { class: "py-2.5 text-right whitespace-nowrap" });
      const actionsGroup = el("div", { class: "inline-flex items-center gap-2" });

      // Role change dropdown
      const roleSelect = select(
        u.role,
        [
          ["admin", "Admin"],
          ["operator", "Operator"],
          ["viewer", "Viewer"],
        ],
        async (newRole) => {
          try {
            await usersApi.updateRole(u.username, newRole as UserRole);
            showToast({ title: `Role for ${u.username} updated to ${newRole}`, tone: "ok" });
            load();
          } catch (err) {
            showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
            load();
          }
        },
      );
      roleSelect.className = "rounded border border-white/10 bg-[#161a26] px-2 py-0.5 text-xs text-neutral-200 cursor-pointer";
      if (currentUserRole !== "admin") {
        roleSelect.disabled = true;
      }

      // Reset password button
      const resetPassBtn = button("Reset Password", "px-2 py-0.5 text-[11px]", () => {
        openPasswordResetModal(u.username);
      });

      // Delete button
      const deleteBtn = dangerButton("Delete", "px-2 py-0.5 text-[11px]", async () => {
        if (!confirm(`Are you sure you want to permanently delete user account '${u.username}'?`)) {
          return;
        }
        try {
          await usersApi.delete(u.username);
          showToast({ title: `User ${u.username} deleted`, tone: "ok" });
          load();
        } catch (err) {
          showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
        }
      });

      if (u.username === currentUsername || currentUserRole !== "admin") {
        deleteBtn.disabled = true;
      }

      actionsGroup.append(roleSelect, resetPassBtn, deleteBtn);
      actionsCell.append(actionsGroup);

      row.append(userCell, roleCell, createdCell, actionsCell);
      tbody.append(row);
    }

    table.append(thead, tbody);
    tableContainer.append(table);
  };

  const openAddUserModal = () => {
    const modal = openModal("Create New Account", "max-w-md");
    let username = "";
    let password = "";
    let role: UserRole = "operator";

    const userInput = textInput("", (v) => { username = v.trim(); }, { placeholder: "e.g. operator1" });
    const passInput = textInput("", (v) => { password = v; }, { type: "password", placeholder: "Minimum 12 characters" });
    const roleSelector = select(
      role,
      [
        ["operator", "Operator (Can manage containers, views logs)"],
        ["viewer", "Viewer (Read-only dashboard access)"],
        ["admin", "Admin (Full control)"],
      ],
      (v) => { role = v as UserRole; },
    );

    const submitBtn = button("Create Account", "w-full py-1.5 text-xs font-semibold border-emerald-500/40 bg-emerald-500/20 text-emerald-300 hover:bg-emerald-500/30", async () => {
      if (!username) {
        showToast({ title: "Please enter a username", tone: "error" });
        return;
      }
      if (!password || password.length < 12) {
        showToast({ title: "Password must be at least 12 characters", tone: "error" });
        return;
      }
      submitBtn.disabled = true;
      try {
        await usersApi.create({ username, password, role });
        showToast({ title: `User '${username}' created successfully`, tone: "ok" });
        modal.close();
        load();
      } catch (err) {
        showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
      } finally {
        submitBtn.disabled = false;
      }
    });

    modal.body.append(
      field("Username", userInput, "Allowed: alphanumeric, hyphens, and underscores"),
      field("Password", passInput, "Must be at least 12 characters"),
      field("Role", roleSelector),
      submitBtn,
    );
  };

  const openPasswordResetModal = (targetUser: string) => {
    const modal = openModal(`Reset Password: ${targetUser}`, "max-w-md");
    let newPassword = "";

    const passInput = textInput("", (v) => { newPassword = v; }, { type: "password", placeholder: "Minimum 12 characters" });
    const confirmBtn = button("Set New Password", "w-full py-1.5 text-xs font-semibold border-emerald-500/40 bg-emerald-500/20 text-emerald-300 hover:bg-emerald-500/30", async () => {
      if (!newPassword || newPassword.length < 12) {
        showToast({ title: "Password must be at least 12 characters", tone: "error" });
        return;
      }
      confirmBtn.disabled = true;
      try {
        await usersApi.setPassword(targetUser, newPassword);
        showToast({ title: `Password updated for ${targetUser}`, tone: "ok" });
        modal.close();
      } catch (err) {
        showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
      } finally {
        confirmBtn.disabled = false;
      }
    });

    modal.body.append(
      field("New Password", passInput, "Must be at least 12 characters"),
      confirmBtn,
    );
  };

  const load = async () => {
    status.textContent = "Loading...";
    try {
      const sess = await authApi.session();
      if (sess.username) {
        currentUsername = sess.username;
      }
      if (sess.role) {
        currentUserRole = sess.role;
      }
      if (currentUserRole !== "admin") {
        addUserBtn.disabled = true;
      }

      const users = await usersApi.list();
      renderUsers(users);
      status.textContent = `${users.length} account${users.length === 1 ? "" : "s"}`;
    } catch (err) {
      status.textContent = "Error loading users";
      tableContainer.replaceChildren(emptyNote(err instanceof Error ? err.message : String(err)));
    }
  };

  reload.addEventListener("click", () => void load());
  addUserBtn.addEventListener("click", openAddUserModal);

  void load();

  return {
    root,
    onShow: () => void load(),
  };
}
