/**
 * Host systemd Services Management.
 */

import { servicesApi } from "../api";
import type { ServiceUnit } from "../api";
import { showToast } from "../toast";
import {
  button,
  dangerButton,
  el,
  emptyNote,
  field,
  openStepperModal,
  select,
  textInput,
} from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

type FilterKind = "all" | "active" | "inactive" | "failed";

export function createServicesPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-4 p-4 overflow-y-auto" });

  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-3 px-4 py-3" });
  const heading = el("span", {
    class: "panel-title text-xs font-semibold uppercase tracking-wider text-neutral-300",
    text: "Host Services (systemd)",
  });
  const reload = button("refresh", "px-3 py-1 text-xs");
  const createBtn = button(
    "+ Create Service",
    "px-3 py-1 text-xs font-semibold border-emerald-500/40 bg-emerald-500/15 text-emerald-300 hover:bg-emerald-500/25",
  );
  const status = el("span", { class: "tabular truncate text-[11px] text-neutral-500" });
  toolbar.append(heading, reload, createBtn, el("span", { class: "ml-auto" }), status);

  // Filter toolbar
  const filterRow = el("div", { class: "flex flex-wrap items-center gap-3 text-xs" });
  let searchQuery = "";
  let activeFilter: FilterKind = "all";

  const searchBox = textInput(
    "",
    (val) => {
      searchQuery = val.toLowerCase().trim();
      renderServices();
    },
    { placeholder: "Filter service name or description…", class: "w-64" },
  );

  const filterButtonsContainer = el("div", { class: "flex items-center gap-1.5" });
  const filterTabs: Array<{ id: FilterKind; label: string }> = [
    { id: "all", label: "All" },
    { id: "active", label: "Active" },
    { id: "inactive", label: "Inactive" },
    { id: "failed", label: "Failed" },
  ];

  const filterTabBtns: HTMLButtonElement[] = [];
  const updateFilterStyles = () => {
    filterTabs.forEach((tab, i) => {
      const btn = filterTabBtns[i];
      if (btn) {
        if (activeFilter === tab.id) {
          btn.className =
            "rounded-md border border-emerald-500/40 bg-emerald-500/20 px-2.5 py-1 text-xs font-medium text-emerald-300";
        } else {
          btn.className =
            "rounded-md border border-white/10 bg-white/5 px-2.5 py-1 text-xs text-neutral-400 hover:text-white hover:bg-white/10";
        }
      }
    });
  };

  filterTabs.forEach((tab) => {
    const btn = button(tab.label, "", () => {
      activeFilter = tab.id;
      updateFilterStyles();
      renderServices();
    });
    filterTabBtns.push(btn);
    filterButtonsContainer.append(btn);
  });
  updateFilterStyles();

  filterRow.append(field("Search", searchBox), el("span", { class: "ml-2" }), filterButtonsContainer);

  const tableContainer = el("section", { class: "panel flex flex-col p-4" });
  root.append(toolbar, filterRow, tableContainer);

  let servicesList: ServiceUnit[] = [];

  const renderServices = () => {
    tableContainer.replaceChildren();

    const filtered = servicesList.filter((s) => {
      const matchesSearch =
        searchQuery === "" ||
        s.name.toLowerCase().includes(searchQuery) ||
        s.description.toLowerCase().includes(searchQuery);

      if (!matchesSearch) return false;

      if (activeFilter === "active") {
        return s.active_state === "active";
      }
      if (activeFilter === "inactive") {
        return s.active_state === "inactive";
      }
      if (activeFilter === "failed") {
        return s.active_state === "failed";
      }
      return true;
    });

    if (filtered.length === 0) {
      tableContainer.append(
        emptyNote(searchQuery || activeFilter !== "all" ? "No matching services found." : "No services available."),
      );
      return;
    }

    const table = el("table", { class: "w-full text-left text-xs" });
    const thead = el("thead", {
      class: "border-b border-white/10 text-neutral-400 text-[11px] uppercase tracking-wider",
    });
    const trHead = el("tr");
    trHead.append(
      el("th", { class: "pb-2 font-medium", text: "Unit" }),
      el("th", { class: "pb-2 font-medium", text: "Description" }),
      el("th", { class: "pb-2 font-medium", text: "State" }),
      el("th", { class: "pb-2 font-medium text-right", text: "Actions" }),
    );
    thead.append(trHead);

    const tbody = el("tbody", { class: "divide-y divide-white/5" });

    for (const unit of filtered) {
      const row = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });

      // Unit name
      const nameCell = el("td", { class: "py-2.5 font-mono font-medium text-neutral-200" });
      nameCell.textContent = unit.name;

      // Description
      const descCell = el("td", { class: "py-2.5 text-neutral-300 max-w-xs truncate" });
      descCell.textContent = unit.description || "—";
      descCell.title = unit.description;

      // State badge
      const stateCell = el("td", { class: "py-2.5" });
      let badgeClass = "border-neutral-500/40 bg-neutral-500/15 text-neutral-300";
      if (unit.active_state === "active") {
        badgeClass = "border-emerald-500/40 bg-emerald-500/15 text-emerald-300";
      } else if (unit.active_state === "failed") {
        badgeClass = "border-rose-500/40 bg-rose-500/15 text-rose-300";
      }
      const stateBadge = el("span", {
        class: `inline-flex items-center rounded px-2 py-0.5 text-[11px] font-semibold border ${badgeClass}`,
        text: `${unit.active_state} (${unit.sub_state})`,
      });
      stateCell.append(stateBadge);

      // Actions
      const actionsCell = el("td", { class: "py-2.5 text-right whitespace-nowrap" });
      const actionsGroup = el("div", { class: "inline-flex items-center gap-2" });

      if (unit.active_state === "active") {
        const restartBtn = button("Restart", "px-2 py-0.5 text-[11px]", async () => {
          try {
            await servicesApi.action(unit.name, "restart");
            showToast({ title: `Restarted ${unit.name}`, tone: "ok" });
            load();
          } catch (err) {
            showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
          }
        });

        const stopBtn = dangerButton("Stop", "px-2 py-0.5 text-[11px]", async () => {
          try {
            await servicesApi.action(unit.name, "stop");
            showToast({ title: `Stopped ${unit.name}`, tone: "ok" });
            load();
          } catch (err) {
            showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
          }
        });
        actionsGroup.append(restartBtn, stopBtn);
      } else {
        const startBtn = button(
          "Start",
          "px-2 py-0.5 text-[11px] border-emerald-500/30 text-emerald-300 hover:bg-emerald-500/10",
          async () => {
            try {
              await servicesApi.action(unit.name, "start");
              showToast({ title: `Started ${unit.name}`, tone: "ok" });
              load();
            } catch (err) {
              showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
            }
          },
        );
        actionsGroup.append(startBtn);
      }

      // View Logs button
      const logsBtn = button("Journal Logs", "px-2 py-0.5 text-[11px] text-sky-400 hover:text-sky-300", () => {
        window.dispatchEvent(
          new CustomEvent("stack-stack:logs-unit", {
            detail: { unit: unit.name },
          }),
        );
        location.hash = "#logs";
      });
      actionsGroup.append(logsBtn);

      actionsCell.append(actionsGroup);
      row.append(nameCell, descCell, stateCell, actionsCell);
      tbody.append(row);
    }

    table.append(thead, tbody);
    tableContainer.append(table);
  };

  const openCreateModal = () => {
    let name = "";
    let description = "";
    let execStart = "";
    let workingDir = "";
    let restart = "always";
    let user = "root";

    openStepperModal(
      "Create Host Service",
      [
        {
          title: "Service Identity",
          description: "Name & purpose of this system daemon",
          render: (container) => {
            const nameInput = textInput(name, (v) => { name = v.trim(); }, {
              placeholder: "e.g. backup-worker",
            });
            const descInput = textInput(description, (v) => { description = v; }, {
              placeholder: "e.g. Background Backup Daemon",
            });
            container.append(
              field("Service Name", nameInput, "Will create <name>.service in /etc/systemd/system/"),
              field("Description", descInput),
            );
          },
          isValid: () => {
            if (!name) {
              showToast({ title: "Please provide a service name", tone: "error" });
              return false;
            }
            return true;
          },
        },
        {
          title: "Execution Command",
          description: "Binary to execute and working directory",
          render: (container) => {
            const execInput = textInput(execStart, (v) => { execStart = v; }, {
              placeholder: "e.g. /usr/local/bin/node /opt/app/index.js",
            });
            const workdirInput = textInput(workingDir, (v) => { workingDir = v.trim(); }, {
              placeholder: "Optional: /opt/app",
            });
            container.append(
              field("ExecStart", execInput, "Absolute binary path and arguments"),
              field("WorkingDirectory", workdirInput, "Optional execution root directory"),
            );
          },
          isValid: () => {
            if (!execStart) {
              showToast({ title: "Please provide an ExecStart command", tone: "error" });
              return false;
            }
            return true;
          },
        },
        {
          title: "Process Lifecycle",
          description: "User execution and restart policies",
          render: (container) => {
            const userInput = textInput(user, (v) => { user = v.trim(); }, {
              placeholder: "root",
            });
            const restartSelector = select(
              restart,
              [
                ["always", "always (restart automatically on exit or crash)"],
                ["on-failure", "on-failure (restart only if crashed)"],
                ["no", "no (do not restart)"],
              ],
              (v) => { restart = v; },
            );
            container.append(
              field("User", userInput, "System user to run this process as"),
              field("Restart Policy", restartSelector),
            );
          },
        },
      ],
      {
        finishLabel: "Create & Register",
        onFinish: async () => {
          try {
            await servicesApi.create({
              name,
              description,
              exec_start: execStart,
              working_dir: workingDir || undefined,
              restart,
              user,
            });
            showToast({ title: `Created service ${name}.service`, tone: "ok" });
            load();
          } catch (err) {
            showToast({ title: err instanceof Error ? err.message : String(err), tone: "error" });
          }
        },
      },
    );
  };

  const load = async () => {
    status.textContent = "Loading services…";
    try {
      servicesList = await servicesApi.list();
      renderServices();
      status.textContent = `${servicesList.length} services loaded`;
    } catch (err) {
      status.textContent = "Error loading services";
      tableContainer.replaceChildren(emptyNote(err instanceof Error ? err.message : String(err)));
    }
  };

  reload.addEventListener("click", () => void load());
  createBtn.addEventListener("click", openCreateModal);

  void load();

  return {
    root,
    onShow: () => void load(),
  };
}
