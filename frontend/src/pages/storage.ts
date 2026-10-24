/**
 * Storage page.
 *
 * Lists the physical block devices and lets an unmounted disk be formatted and
 * mounted. Every destructive action goes through a confirmation that names the
 * exact device, and the backend re-validates it: a device that is mounted, backs
 * the root filesystem, or is not a disk or partition is refused there as well.
 */

import { storageApi } from "../api";
import type { BlockDevice, StorageState } from "../api";
import { badge, button, bytes, el, emptyNote, field, openModal, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

/** Mount points the backend accepts; the suggestion field is a starting point. */
const MOUNT_PREFIX = "/mnt/";

/** The `dmesg`-style set the backend refuses to unmount; mirrored for the UI. */
const PROTECTED_MOUNTS = ["/", "/boot", "/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/run"];

export function createStoragePage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-3 p-4" });

  const toolbar = el("section", { class: "panel flex flex-wrap items-center gap-3 px-3 py-2" });
  const heading = el("span", { class: "panel-title text-[10px]", text: "storage" });
  const reload = button("refresh");
  const status = el("span", { class: "tabular truncate text-[11px] text-neutral-500" });
  toolbar.append(heading, reload, status);

  const host = el("div", { class: "flex flex-col gap-3" });
  root.append(toolbar, host);

  let busy = false;

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
    if (busy) {
      return;
    }
    setStatus("loading…");
    try {
      const state = await storageApi.state();
      render(state);
      setStatus("");
    } catch (err) {
      host.replaceChildren(emptyNote(`could not read the disk list: ${errorText(err)}`));
      setStatus(errorText(err), "error");
    }
  }

  function render(state: StorageState): void {
    host.replaceChildren();
    if (state.devices.length === 0) {
      host.append(emptyNote("lsblk reported no block devices"));
      return;
    }
    for (const device of state.devices) {
      host.append(diskPanel(device, state));
    }
  }

  function diskPanel(disk: BlockDevice, state: StorageState): HTMLElement {
    const panel = el("section", { class: "panel flex flex-col" });

    const head = el("div", {
      class: "flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-white/5 px-3 py-2",
    });
    head.append(
      el("span", { class: "font-medium text-neutral-100", text: disk.path }),
      el("span", {
        class: "truncate text-[11px] text-neutral-500",
        text: disk.model ?? disk.kind,
      }),
      el("span", { class: "tabular ml-auto text-[11px] text-neutral-500", text: bytes(disk.size_bytes) }),
    );
    panel.append(head);

    const rows = el("div", { class: "flex flex-col divide-y divide-white/5" });
    rows.append(deviceRow(disk, state, disk.children.length === 0));
    for (const child of disk.children) {
      rows.append(deviceRow(child, state, true));
    }
    panel.append(rows);
    return panel;
  }

  /** One device line with its state badges and the actions it may offer. */
  function deviceRow(device: BlockDevice, state: StorageState, actionsAllowed: boolean): HTMLElement {
    const row = el("div", { class: "flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-[11px]" });

    row.append(
      el("span", { class: "tabular min-w-24 text-neutral-200", text: device.path }),
      el("span", { class: "tabular text-neutral-500", text: bytes(device.size_bytes) }),
    );

    const mounted = Boolean(device.mountpoint);
    const isRoot = state.root_device === device.path;
    if (device.fstype) {
      row.append(badge(device.fstype, "muted"));
    } else if (device.kind === "disk" || device.kind === "part") {
      row.append(badge("unformatted", "warn"));
    }
    if (mounted) {
      row.append(badge(`mounted at ${device.mountpoint}`, "ok"));
    }
    if (device.label) {
      row.append(el("span", { class: "text-neutral-600", text: device.label }));
    }
    if (isRoot) {
      row.append(badge("root filesystem", "err"));
    }

    const actions = el("span", { class: "ml-auto flex gap-2" });
    if (actionsAllowed) {
      const canFormat =
        !mounted &&
        !isRoot &&
        !hasMountedDescendant(device) &&
        (device.kind === "part" || (device.kind === "disk" && device.children.length === 0));
      if (canFormat) {
        actions.append(
          button("format…", "text-[10px] text-amber-400", () => openFormat(device, state)),
        );
      }
      if (!mounted && !isRoot && device.fstype && !hasMountedDescendant(device)) {
        actions.append(
          button("mount…", "text-[10px] text-emerald-400", () => openMount(device)),
        );
      }
      if (mounted && !isProtected(device.mountpoint ?? "")) {
        actions.append(
          button("unmount", "text-[10px] text-rose-400", () => void unmount(device)),
        );
      }
    }
    if (actions.childElementCount > 0) {
      row.append(actions);
    }
    return row;
  }

  function openFormat(device: BlockDevice, state: StorageState): void {
    const modal = openModal("format disk", "max-w-md");
    let fs = state.formats[0] ?? "ext4";
    let confirmation = "";

    modal.body.append(
      el("div", {
        class: "border border-rose-500/30 bg-rose-500/10 px-3 py-2 text-[11px] text-rose-300",
        text:
          `This erases everything on ${device.path} (${bytes(device.size_bytes)}` +
          `${device.model ? `, ${device.model}` : ""}). There is no undo.`,
      }),
    );
    const select = el(
      "select",
      { class: "border border-white/5 bg-black/40 px-2 py-1 text-neutral-200" },
      state.formats.map((name) => el("option", { value: name, text: name })),
    ) as HTMLSelectElement;
    fs = select.value || fs;
    select.addEventListener("change", () => {
      fs = select.value;
    });
    modal.body.append(field("filesystem", select));

    const typed = textInput(
      "",
      (value) => {
        confirmation = value;
      },
      { placeholder: device.path },
    );
    modal.body.append(
      field(`type ${device.path} to confirm`, typed),
    );

    const feedback = el("div", { class: "tabular text-[11px] text-neutral-500" });
    const confirm = button("format", "text-rose-400 hover:border-rose-500");
    const cancel = button("cancel", "", () => modal.close());
    confirm.addEventListener("click", async () => {
      if (confirmation.trim() !== device.path) {
        feedback.className = "tabular text-[11px] text-amber-400";
        feedback.textContent = `type ${device.path} exactly to confirm`;
        return;
      }
      confirm.disabled = true;
      busy = true;
      feedback.className = "tabular text-[11px] text-neutral-500";
      feedback.textContent = "formatting…";
      try {
        const result = await storageApi.format(device.path, fs);
        modal.close();
        setStatus(result.message, "ok");
      } catch (err) {
        feedback.className = "tabular text-[11px] text-rose-400";
        feedback.textContent = errorText(err);
      } finally {
        busy = false;
        confirm.disabled = false;
      }
      await load();
    });
    modal.body.append(el("div", { class: "flex flex-wrap items-center gap-2" }, [confirm, cancel, feedback]));
  }

  function openMount(device: BlockDevice): void {
    const modal = openModal("mount disk", "max-w-md");
    let path = `${MOUNT_PREFIX}${device.name}`;
    let persist = true;

    modal.body.append(
      el("div", {
        class: "text-[11px] text-neutral-400",
        text: `${device.path} (${device.fstype ?? "unknown"}) will be mounted and can be shown on the dashboard.`,
      }),
    );
    modal.body.append(
      field(
        "mount point",
        textInput(path, (value) => {
          path = value;
        }, { placeholder: "/mnt/disk1" }),
      ),
    );
    modal.body.append(
      el("div", { class: "text-[10px] text-neutral-600", text: `must be under ${MOUNT_PREFIX}` }),
    );

    const persistLabel = el("label", { class: "flex items-center gap-2 text-[11px] text-neutral-400" });
    const persistBox = document.createElement("input");
    persistBox.type = "checkbox";
    persistBox.checked = true;
    persistBox.addEventListener("change", () => {
      persist = persistBox.checked;
    });
    persistLabel.append(persistBox, el("span", { text: "add to /etc/fstab so it mounts at boot" }));
    modal.body.append(persistLabel);

    const feedback = el("div", { class: "tabular text-[11px] text-neutral-500" });
    const confirm = button("mount", "text-emerald-400 hover:border-emerald-500");
    const cancel = button("cancel", "", () => modal.close());
    confirm.addEventListener("click", async () => {
      confirm.disabled = true;
      busy = true;
      feedback.textContent = "mounting…";
      try {
        const result = await storageApi.mount(device.path, path.trim(), persist);
        modal.close();
        setStatus(result.message, "ok");
      } catch (err) {
        feedback.className = "tabular text-[11px] text-rose-400";
        feedback.textContent = errorText(err);
      } finally {
        busy = false;
        confirm.disabled = false;
      }
      await load();
    });
    modal.body.append(el("div", { class: "flex flex-wrap items-center gap-2" }, [confirm, cancel, feedback]));
  }

  async function unmount(device: BlockDevice): Promise<void> {
    const mountpoint = device.mountpoint ?? device.path;
    if (!window.confirm(`Unmount ${mountpoint}? Running programs using it will see I/O errors.`)) {
      return;
    }
    busy = true;
    setStatus(`unmounting ${mountpoint}…`);
    try {
      const result = await storageApi.unmount(mountpoint);
      setStatus(result.message, "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
    }
    await load();
  }

  reload.addEventListener("click", () => void load());

  return {
    root,
    onShow: () => void load(),
  };
}

function hasMountedDescendant(device: BlockDevice): boolean {
  if (device.mountpoint) {
    return true;
  }
  return device.children.some(hasMountedDescendant);
}

function isProtected(mountpoint: string): boolean {
  const normalized = mountpoint.replace(/\/+$/, "") || "/";
  return PROTECTED_MOUNTS.includes(normalized);
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
