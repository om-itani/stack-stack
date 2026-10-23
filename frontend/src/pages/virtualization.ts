/**
 * Virtualization page: KVM/QEMU virtual machines and LXC containers.
 *
 * Three jobs, mirroring how Proxmox splits them:
 *  - "guests": every VM and container with live status and lifecycle actions,
 *  - "new vm" / "new container": pick an image from the catalog, then set the
 *    essentials (hostname, password, IP, DNS) and create,
 *  - "host": what this box can actually run (KVM, QEMU accelerators, hugepages).
 *
 * The page keeps two mutable drafts (a VM and a container). Text fields write
 * straight into the draft so typing never loses focus; structural choices (image
 * or template selection) re-render.
 */

import { virtApi, hostApi, blankVm, blankCt } from "../api";
import type {
  ActionReport,
  ApiFailure,
  BindMount,
  CtConfig,
  CtPreview,
  GuestImage,
  GuestStatus,
  HostCapabilities,
  Problem,
  VirtState,
  VmConfig,
  VmPreview,
} from "../api";
// xterm is loaded only when a console is opened, keeping it out of the initial
// bundle.
const loadTerminal = () => import("../terminal");
import {
  button,
  checkbox,
  el,
  emptyNote,
  field,
  listInput,
  numberInput,
  select,
  textInput,
} from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

/**
 * The host's LAN network in `<a.b.c>.` form, used as the placeholder prefix for a
 * guest's static address so the form never suggests the wrong subnet (a
 * hardcoded `192.168.1.0/24` is wrong on a `192.168.0.0/24` host).
 *
 * Fetched once and cached; `null` until it resolves so callers fall back to a
 * generic hint rather than guessing a network.
 */
let lanPrefix: string | null = null;
let lanPrefixRequested = false;

function lanAddressPrefix(): string | null {
  if (lanPrefix !== null || lanPrefixRequested) {
    return lanPrefix;
  }
  lanPrefixRequested = true;
  void hostApi
    .ip()
    .then((host) => {
      const octets = host.ip.trim().split(".");
      if (octets.length === 4 && octets.every((o) => o !== "" && Number.isInteger(Number(o)))) {
        lanPrefix = `${octets[0]}.${octets[1]}.${octets[2]}.`;
      }
    })
    .catch(() => {
      // Leave the prefix null: the placeholder falls back to a neutral hint.
      lanPrefixRequested = false;
    });
  return lanPrefix;
}

/** `192.168.0.50/24` shaped hint from the host's own network, else a generic one. */
function addressHint(): string {
  return `${lanAddressPrefix() ?? "192.168.?.?"}.50/24`;
}

/** The address/gateway/DNS rows only apply to a static guest. */
function staticOnly(dependent: HTMLElement[], mode: string): void {
  const show = mode === "static";
  for (const node of dependent) {
    node.style.display = show ? "" : "none";
  }
}

type Tab = { id: string; label: string; build: () => HTMLElement };

const REFRESH_MS = 5000;

const IP_MODE_OPTIONS: [string, string][] = [
  ["dhcp", "dhcp"],
  ["static", "static"],
  ["none", "none (isolated)"],
];

const STATE_STYLES: Record<string, string> = {
  running: "text-emerald-400",
  stopped: "text-neutral-500",
  paused: "text-amber-400",
  frozen: "text-amber-400",
  starting: "text-sky-400",
  stopping: "text-amber-400",
  unknown: "text-neutral-600",
  unavailable: "text-neutral-600",
};

export function createVirtualizationPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-col gap-3 p-3" });

  let state: VirtState | null = null;
  let vmDraft: VmConfig = blankVm();
  let ctDraft: CtConfig = blankCt();
  let activeTab = "guests";
  let busy = false;
  let vmPreview: VmPreview | null = null;
  let ctPreview: CtPreview | null = null;
  let previewFile = 0;
  let vmFilter = "";
  let ctFilter = "";

  const heading = el("span", { class: "text-neutral-300", text: "virtualization" });
  const statusLine = el("span", { class: "tabular truncate text-[11px] text-neutral-500" });
  const badges = el("span", { class: "tabular text-[10px] text-neutral-500" });
  const reload = button("refresh");
  const header = el("div", { class: "flex flex-wrap items-center gap-3" }, [
    heading,
    reload,
    el("span", { class: "ml-auto" }, [badges]),
    statusLine,
  ]);

  const noticeBox = el("div", { class: "flex flex-col gap-1" });
  const tabBar = el("div", { class: "flex flex-wrap gap-1 border-b border-white/5 pb-1" });
  const tabBody = el("div", { class: "min-h-0 flex-1" });
  root.append(header, noticeBox, tabBar, tabBody);

  const tabs: Tab[] = [
    { id: "guests", label: "guests", build: buildGuests },
    { id: "new-vm", label: "new vm", build: buildNewVm },
    { id: "new-ct", label: "new container", build: buildNewCt },
    { id: "preview", label: "preview", build: buildPreview },
    { id: "host", label: "host", build: buildHost },
  ];

  // --- data --------------------------------------------------------------

  function setStatus(text: string, tone: "muted" | "error" | "ok"): void {
    statusLine.textContent = text;
    statusLine.className =
      tone === "error"
        ? "tabular truncate text-[11px] text-rose-400"
        : tone === "ok"
          ? "tabular truncate text-[11px] text-emerald-400"
          : "tabular truncate text-[11px] text-neutral-500";
  }

  function errorText(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }

  async function load(): Promise<void> {
    if (busy) {
      return;
    }
    try {
      state = await virtApi.state();
      renderChrome();
      renderTabs();
    } catch (err) {
      setStatus(errorText(err), "error");
    }
  }

  function reportOutcome(report: ActionReport, note: string): void {
    const warnings = report.warnings.length > 0 ? ` (${report.warnings.join("; ")})` : "";
    setStatus(`${note}: ${report.output}${warnings}`, report.warnings.length > 0 ? "error" : "ok");
  }

  async function run(action: () => Promise<ActionReport>, note: string, reloadAfter = true): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    setStatus(`${note}…`, "muted");
    try {
      const report = await action();
      reportOutcome(report, note);
    } catch (err) {
      const failure = err as ApiFailure;
      if (Array.isArray(failure.problems) && failure.problems.length > 0) {
        setStatus(`${failure.problems.length} problem(s): ${failure.problems[0].field}`, "error");
      } else {
        setStatus(errorText(err), "error");
      }
    } finally {
      busy = false;
    }
    if (reloadAfter) {
      await load();
    } else {
      renderChrome();
      renderTabs();
    }
  }

  // --- chrome ------------------------------------------------------------

  function renderChrome(): void {
    badges.textContent = "";
    if (state) {
      const host = state.host;
      badges.textContent = [
        host.kvm ? "kvm" : "no-kvm",
        host.qemu ? "qemu" : "no-qemu",
        host.lxc ? "lxc" : "no-lxc",
        // Spelled out rather than `hp:0`, which reads like an error code.
        host.hugepages_2m_free > 0 ? `${host.hugepages_2m_free} hugepages` : "no hugepages",
      ].join(" · ");
    }

    noticeBox.replaceChildren();
    if (state?.config_error) {
      noticeBox.append(
        el("div", {
          class: "border border-rose-800 bg-rose-950/40 px-3 py-2 text-[11px] text-rose-300",
          text: state.config_error,
        }),
      );
    }
    const problems: Problem[] = state?.problems ?? [];
    if (problems.length > 0) {
      const list = el("div", {
        class: "border border-amber-800 bg-amber-950/30 px-3 py-2 text-[11px] text-amber-300",
      });
      list.append(el("div", { class: "font-semibold", text: "validation problems" }));
      for (const problem of problems.slice(0, 10)) {
        list.append(el("div", { text: `${problem.field}: ${problem.message}` }));
      }
      noticeBox.append(list);
    }
  }

  function renderTabs(): void {
    tabBar.replaceChildren();
    for (const tab of tabs) {
      const active = tab.id === activeTab;
      tabBar.append(
        button(tab.label, active ? "border-emerald-600 text-emerald-400" : "", () => {
          activeTab = tab.id;
          renderTabs();
        }),
      );
    }
    const tab = tabs.find((entry) => entry.id === activeTab) ?? tabs[0];
    tabBody.replaceChildren(tab.build());
  }

  const rerender = () => renderTabs();

  // --- tab: guests -------------------------------------------------------

  function buildGuests(): HTMLElement {
    const layout = el("div", { class: "flex min-h-0 flex-col gap-3" });
    if (!state) {
      layout.append(el("div", { class: "text-neutral-600", text: "loading…" }));
      return layout;
    }
    if (state.statuses.length === 0) {
      layout.append(
        emptyNote("no guests yet. Create a virtual machine or a container from the tabs above."),
      );
      return layout;
    }

    const grid = el("div", { class: "grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3" });
    for (const status of state.statuses) {
      grid.append(guestCard(status));
    }
    layout.append(grid);
    return layout;
  }

  function guestCard(status: GuestStatus): HTMLElement {
    const card = el("section", { class: "panel flex flex-col" });

    const head = el("div", {
      class: "flex items-baseline justify-between gap-3 border-b border-white/5 px-3 py-1.5",
    });
    head.append(
      el("span", { class: "truncate font-semibold text-neutral-200", text: status.name }),
      el("span", {
        class: `tabular text-[10px] uppercase ${STATE_STYLES[status.state] ?? "text-neutral-500"}`,
        text: status.state,
      }),
    );

    const kindLabel = status.kind === "vm" ? "KVM" : "LXC";
    const meta = el("div", {
      class: "tabular flex flex-wrap gap-x-3 px-3 py-1 text-[10px] text-neutral-600",
    });
    meta.append(
      el("span", { class: "text-neutral-500", text: kindLabel }),
      el("span", { text: status.pid ? `pid ${status.pid}` : "no pid" }),
      el("span", { text: status.memory_bytes ? `${mib(status.memory_bytes)} MiB` : "—" }),
      status.ip ? el("span", { text: status.ip }) : el("span", { text: "—" }),
    );

    const actions = el("div", { class: "flex flex-wrap gap-2 border-t border-white/5 px-3 py-2" });
    const running = status.state === "running";

    const startBtn = button("start");
    startBtn.disabled = running || busy;
    startBtn.addEventListener("click", () =>
      void run(
        () => (status.kind === "vm" ? virtApi.vmStart(status.id) : virtApi.ctStart(status.id)),
        `starting ${status.id}`,
      ),
    );
    actions.append(startBtn);

    const stopBtn = button("stop");
    stopBtn.disabled = !running || busy;
    stopBtn.addEventListener("click", () =>
      void run(
        () => (status.kind === "vm" ? virtApi.vmShutdown(status.id) : virtApi.ctStop(status.id)),
        `stopping ${status.id}`,
      ),
    );
    actions.append(stopBtn);

    if (status.kind === "vm") {
      const rebootBtn = button("reboot");
      rebootBtn.disabled = !running || busy;
      rebootBtn.addEventListener("click", () =>
        void run(() => virtApi.vmReboot(status.id), `rebooting ${status.id}`),
      );
      actions.append(rebootBtn);
    } else {
      const killBtn = button("kill");
      killBtn.disabled = !running || busy;
      killBtn.addEventListener("click", () =>
        void run(() => virtApi.ctKill(status.id), `killing ${status.id}`),
      );
      actions.append(killBtn);
    }

    const consoleBtn = button("console");
    consoleBtn.disabled = !running || busy;
    consoleBtn.addEventListener("click", () => {
      const path =
        status.kind === "vm"
          ? `/api/virt/vms/${encodeURIComponent(status.id)}/console/ws`
          : `/api/virt/cts/${encodeURIComponent(status.id)}/console/ws`;
      void loadTerminal().then((terminal) =>
      terminal.openConsole(path, status.kind === "vm" ? "serial console" : "shell", status.name),
    );
    });
    actions.append(consoleBtn);

    const deleteBtn = button("delete", "text-rose-400 hover:border-rose-500");
    deleteBtn.disabled = busy;
    deleteBtn.addEventListener("click", () => {
      if (
        window.confirm(
          `Delete ${status.name}? Its disk and configuration are removed. This cannot be undone.`,
        )
      ) {
        void run(
          () =>
            status.kind === "vm" ? virtApi.vmDelete(status.id) : virtApi.ctDelete(status.id),
          `deleting ${status.id}`,
        );
      }
    });
    actions.append(deleteBtn);

    card.append(head, meta, actions);
    return card;
  }

  // --- tab: new VM -------------------------------------------------------

  function buildNewVm(): HTMLElement {
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });
    const vm = vmDraft;

    layout.append(vmImagePicker());

    const form = el("section", { class: "panel" });
    form.append(sectionTitle("virtual machine"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3 xl:grid-cols-4" });
    const bridges = bridgeOptions();

    grid.append(
      field("id", textInput(vm.id, (value) => { vm.id = value; })),
      field("hostname", textInput(vm.cloud_init.hostname, (value) => { vm.cloud_init.hostname = value; })),
      field("image / iso path", textInput(vm.image ?? "", (value) => { vm.image = value.trim() === "" ? null : value; })),
      field("bridge", select(vm.bridge, bridges, (value) => { vm.bridge = value; })),
      field("vCPUs", numberInput(vm.vcpus, (value) => { vm.vcpus = value; }, { min: 1, max: 256 })),
      field("memory MiB", numberInput(vm.memory_mb, (value) => { vm.memory_mb = value; }, { min: 128 })),
      field("disk GiB", numberInput(vm.disk_gb, (value) => { vm.disk_gb = value; }, { min: 1 })),
      field("VNC port", numberInput(vm.vnc_port ?? 0, (value) => { vm.vnc_port = value > 0 ? value : null; }, { min: 0, max: 5999 })),
      field(
        "machine",
        select(vm.machine, [["q35", "q35 (modern)"], ["pc", "pc (legacy)"]], (value) => {
          vm.machine = value as VmConfig["machine"];
        }),
      ),
      field(
        "firmware",
        select(vm.firmware, [["bios", "BIOS"], ["uefi", "UEFI"]], (value) => {
          vm.firmware = value as VmConfig["firmware"];
        }),
      ),
      field(
        "cpu model",
        select(vm.cpu_model, [["host", "host (fastest)"], ["max", "max"], ["qemu64", "qemu64"]], (value) => {
          vm.cpu_model = value as VmConfig["cpu_model"];
        }),
      ),
      field(
        "disk format",
        select(vm.disk_format, [["qcow2", "qcow2"], ["raw", "raw"]], (value) => {
          vm.disk_format = value as VmConfig["disk_format"];
        }),
      ),
    );

    const toggles = el("div", { class: "col-span-full flex flex-wrap gap-4" }, [
      checkbox(
        vm.hugepages,
        (value) => { vm.hugepages = value; },
        "hugepages",
        !state?.host.hugepages_2m_free,
      ),
      checkbox(vm.discard, (value) => { vm.discard = value; }, "TRIM/discard"),
      checkbox(vm.multiqueue, (value) => { vm.multiqueue = value; }, "multiqueue net"),
      checkbox(vm.vnc_enabled, (value) => { vm.vnc_enabled = value; }, "VNC"),
      checkbox(vm.guest_agent, (value) => { vm.guest_agent = value; }, "guest agent"),
      checkbox(vm.autostart, (value) => { vm.autostart = value; }, "autostart"),
    ]);
    grid.append(toggles);
    form.append(grid);

    // Cloud-init.
    form.append(sectionTitle("provisioning (cloud-init)"));
    const ci = vm.cloud_init;
    const ciGrid = el("div", { class: "grid grid-cols-2 gap-2 p-3 xl:grid-cols-4" });
    const vmAddressField = field(
      "address (cidr)",
      nullableText(ci.address, (value) => { ci.address = value; }, { placeholder: addressHint() }),
    );
    const vmGatewayField = field("gateway", textInput(ci.gateway, (value) => { ci.gateway = value; }));
    const vmDnsField = field("DNS servers", listInput(ci.dns, (values) => { ci.dns = values; }));
    const vmStaticFields = [vmAddressField, vmGatewayField, vmDnsField];
    const vmIpField = field(
      "IP mode",
      select(ci.ip_mode, IP_MODE_OPTIONS, (value) => {
        ci.ip_mode = value as CloudInitIpMode;
        staticOnly(vmStaticFields, ci.ip_mode);
      }),
    );
    staticOnly(vmStaticFields, ci.ip_mode);
    ciGrid.append(
      el("div", { class: "col-span-full" }, [
        checkbox(ci.enabled, (value) => { ci.enabled = value; }, "enable cloud-init seed"),
      ]),
      field("username", textInput(ci.username, (value) => { ci.username = value; })),
      field("password", textInput(ci.password, (value) => { ci.password = value; }, { type: "password" })),
      vmIpField,
      // Address, gateway and DNS only make sense for a static guest; they are
      // hidden until the operator picks `static`, and the placeholder is derived
      // from this host's own subnet so it never suggests the wrong network.
      vmAddressField,
      vmGatewayField,
      vmDnsField,
      field("SSH keys", listInput(ci.ssh_keys, (values) => { ci.ssh_keys = values; }, { placeholder: "ssh-ed25519 AAAA…" })),
    );
    form.append(ciGrid);

    const actions = el("div", { class: "flex flex-wrap gap-2 border-t border-white/5 px-3 py-2" });
    actions.append(
      button("create vm", "text-emerald-400 hover:border-emerald-500", () => {
        void run(() => virtApi.createVm(cloneVm(vm)), `creating ${vm.id || "vm"}`);
      }),
      button("preview", "", () => void previewVm()),
      button("reset", "", () => {
        vmDraft = blankVm();
        vmPreview = null;
        rerender();
      }),
    );
    form.append(actions);
    layout.append(form);

    return layout;
  }

  function vmImagePicker(): HTMLElement {
    const section = el("section", { class: "panel" });
    section.append(sectionTitle("image catalog"));
    const bar = el("div", { class: "flex flex-wrap items-center gap-2 px-3 py-2" });
    bar.append(
      textInput(vmFilter, (value) => {
        vmFilter = value;
        rerender();
      }, { placeholder: "search images…", class: "w-48" }),
    );
    section.append(bar);

    const images = vmImages();
    const grid = el("div", { class: "grid max-h-[60vh] grid-cols-1 gap-2 overflow-auto px-3 pb-3 lg:grid-cols-2" });
    if (images.length === 0) {
      grid.append(emptyNote("no matching images"));
    }
    for (const image of images) {
      grid.append(imageCard(image, vmDraft.image === image.id, () => applyVmImage(image)));
    }
    section.append(grid);
    return section;
  }

  function applyVmImage(image: GuestImage): void {
    vmDraft.image = image.id;
    if (!vmDraft.cloud_init.password && image.default_user) {
      vmDraft.cloud_init.username = image.default_user;
    }
    if (image.min_disk_gb > vmDraft.disk_gb) {
      vmDraft.disk_gb = image.min_disk_gb;
    }
    if (image.min_memory_mb > vmDraft.memory_mb) {
      vmDraft.memory_mb = image.min_memory_mb;
    }
    if (image.arch) {
      vmDraft.arch = image.arch;
    }
    if (image.format === "iso") {
      vmDraft.cloud_init.enabled = false;
    }
    rerender();
  }

  // --- tab: new CT -------------------------------------------------------

  function buildNewCt(): HTMLElement {
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });
    const ct = ctDraft;

    layout.append(ctTemplatePicker());

    const form = el("section", { class: "panel" });
    form.append(sectionTitle("lxc container"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3 xl:grid-cols-4" });
    const bridges = bridgeOptions();

    const ctAddressField = field(
      "address (cidr)",
      nullableText(ct.address, (value) => { ct.address = value; }, { placeholder: addressHint() }),
    );
    const ctGatewayField = field("gateway", textInput(ct.gateway, (value) => { ct.gateway = value; }));
    const ctDnsField = field("DNS servers", listInput(ct.dns, (values) => { ct.dns = values; }));
    const ctStaticFields = [ctAddressField, ctGatewayField, ctDnsField];
    const ctIpField = field(
      "IP mode",
      select(ct.ip_mode, IP_MODE_OPTIONS, (value) => {
        ct.ip_mode = value as CloudInitIpMode;
        staticOnly(ctStaticFields, ct.ip_mode);
      }),
    );
    staticOnly(ctStaticFields, ct.ip_mode);

    grid.append(
      field("id", textInput(ct.id, (value) => { ct.id = value; })),
      field("hostname", textInput(ct.hostname, (value) => { ct.hostname = value; })),
      field("template", textInput(ct.template, (value) => { ct.template = value; })),
      field("bridge", select(ct.bridge, bridges, (value) => { ct.bridge = value; })),
      field("cores", numberInput(ct.cores, (value) => { ct.cores = value; }, { min: 0, max: 256 })),
      field("memory MiB", numberInput(ct.memory_mb, (value) => { ct.memory_mb = value; }, { min: 32 })),
      field("swap MiB", numberInput(ct.swap_mb, (value) => { ct.swap_mb = value; }, { min: 0 })),
      field("rootfs GiB", numberInput(ct.rootfs_gb, (value) => { ct.rootfs_gb = value; }, { min: 1 })),
      ctIpField,
      // Same rule as the VM form: the address rows only apply to a static guest.
      ctAddressField,
      ctGatewayField,
      ctDnsField,
      field("root password", textInput(ct.password, (value) => { ct.password = value; }, { type: "password" })),
      field("SSH keys", listInput(ct.ssh_keys, (values) => { ct.ssh_keys = values; }, { placeholder: "ssh-ed25519 AAAA…" })),
    );

    const toggles = el("div", { class: "col-span-full flex flex-wrap gap-4" }, [
      checkbox(ct.unprivileged, (value) => { ct.unprivileged = value; }, "unprivileged"),
      checkbox(ct.nesting, (value) => { ct.nesting = value; }, "nesting"),
      checkbox(ct.autostart, (value) => { ct.autostart = value; }, "autostart"),
    ]);
    grid.append(toggles);
    form.append(grid);

    // Bind mounts.
    form.append(sectionTitle("bind mounts"));
    const bindBox = el("div", { class: "flex flex-col gap-1 p-3" });
    ct.binds.forEach((bind, index) => {
      bindBox.append(bindRow(bind, index));
    });
    bindBox.append(
      button("add mount", "", () => {
        ct.binds.push({ source: "", target: "", readonly: false });
        rerender();
      }),
    );
    form.append(bindBox);

    const actions = el("div", { class: "flex flex-wrap gap-2 border-t border-white/5 px-3 py-2" });
    actions.append(
      button("create container", "text-emerald-400 hover:border-emerald-500", () => {
        void run(() => virtApi.createCt(cloneCt(ct)), `creating ${ct.id || "container"}`);
      }),
      button("preview", "", () => void previewCt()),
      button("reset", "", () => {
        ctDraft = blankCt();
        ctPreview = null;
        rerender();
      }),
    );
    form.append(actions);
    layout.append(form);

    return layout;
  }

  function bindRow(bind: BindMount, index: number): HTMLElement {
    const row = el("div", { class: "flex flex-wrap items-center gap-2 text-[11px]" });
    row.append(
      textInput(bind.source, (value) => { bind.source = value; }, { placeholder: "/srv/data" }),
      el("span", { class: "text-neutral-600", text: "→" }),
      textInput(bind.target, (value) => { bind.target = value; }, { placeholder: "/mnt/data" }),
      checkbox(bind.readonly, (value) => { bind.readonly = value; }, "ro"),
      button("remove", "text-rose-400", () => {
        ctDraft.binds.splice(index, 1);
        rerender();
      }),
    );
    return row;
  }

  function ctTemplatePicker(): HTMLElement {
    const section = el("section", { class: "panel" });
    section.append(sectionTitle("container templates"));
    const bar = el("div", { class: "flex flex-wrap items-center gap-2 px-3 py-2" });
    bar.append(
      textInput(ctFilter, (value) => {
        ctFilter = value;
        rerender();
      }, { placeholder: "search templates…", class: "w-48" }),
    );
    section.append(bar);

    const templates = lxcTemplates();
    const grid = el("div", { class: "grid max-h-[60vh] grid-cols-1 gap-2 overflow-auto px-3 pb-3 lg:grid-cols-2" });
    if (templates.length === 0) {
      grid.append(emptyNote("no matching templates"));
    }
    for (const image of templates) {
      grid.append(imageCard(image, ctDraft.template === image.id, () => applyCtTemplate(image)));
    }
    section.append(grid);
    return section;
  }

  function applyCtTemplate(image: GuestImage): void {
    ctDraft.template = image.id;
    if (image.min_disk_gb > ctDraft.rootfs_gb) {
      ctDraft.rootfs_gb = image.min_disk_gb;
    }
    if (image.min_memory_mb > ctDraft.memory_mb) {
      ctDraft.memory_mb = image.min_memory_mb;
    }
    rerender();
  }

  function imageCard(image: GuestImage, selected: boolean, onPick: () => void): HTMLElement {
    const card = el("button", {
      type: "button",
      class: `flex flex-col items-start gap-1 border px-2 py-1.5 text-left transition-colors ${
        selected
          ? "border-emerald-600 bg-emerald-950/30"
          : "border-white/5 hover:border-neutral-500"
      }`,
    });
    const top = el("div", { class: "flex w-full items-baseline gap-2" });
    top.append(
      el("span", { text: image.icon || "▢" }),
      el("span", { class: "font-semibold text-neutral-200", text: image.name }),
      el("span", { class: "tabular ml-auto text-[10px] text-neutral-600", text: image.category }),
    );
    // A rolling release cannot be checksum-pinned, so it is badged as unverified
    // rather than failing the download with a checksum it can never match.
    if (image.verified === false) {
      top.append(
        el("span", {
          class: "border border-amber-700 px-1 text-[9px] text-amber-500",
          text: "unverified",
        }),
      );
    }
    card.append(
      top,
      el("span", { class: "text-[10px] text-neutral-500", text: image.description }),
      el("span", {
        class: "tabular text-[10px] text-neutral-600",
        text: [
          image.arch,
          image.format ?? image.lxc_release ?? "",
          image.min_memory_mb ? `≥${image.min_memory_mb}MiB` : "",
          image.cached ? "cached" : "",
        ]
          .filter(Boolean)
          .join(" · "),
      }),
    );
    card.addEventListener("click", onPick);
    return card;
  }

  // --- tab: preview ------------------------------------------------------

  function buildPreview(): HTMLElement {
    const section = el("section", { class: "panel" });
    section.append(sectionTitle("generated configuration"));

    const files: string[] = [];
    if (vmPreview) {
      files.push(vmPreview.qemu, vmPreview.user_data ?? "", vmPreview.meta_data ?? "", vmPreview.network_config ?? "");
    } else if (ctPreview) {
      files.push(ctPreview.lxc, ctPreview.resolv_conf ?? "");
    }

    if (files.length === 0) {
      section.append(
        el("div", {
          class: "px-3 py-2 text-neutral-600",
          text: "press preview on a VM or container form to render its QEMU command line, LXC config and cloud-init files",
        }),
      );
      return section;
    }

    const labels = vmPreview
      ? ["qemu", "user-data", "meta-data", "network-config"]
      : ["lxc config", "resolv.conf"];
    const bar = el("div", { class: "flex flex-wrap gap-1 px-3 py-2" });
    labels.forEach((label, index) => {
      if (files[index]) {
        bar.append(
          button(label, index === previewFile ? "border-emerald-600 text-emerald-400" : "", () => {
            previewFile = index;
            rerender();
          }),
        );
      }
    });
    section.append(bar);
    section.append(
      el("pre", {
        class:
          "mt-1 max-h-[60vh] overflow-auto border-t border-white/5 bg-black/40 p-3 text-[11px] leading-relaxed text-neutral-400",
        text: files[previewFile] ?? "",
      }),
    );

    if (vmPreview) {
      section.append(
        el("div", {
          class: "tabular px-3 py-2 text-[10px] text-neutral-600",
          text: `uuid ${vmPreview.uuid}${vmPreview.vnc ? ` · vnc ${vmPreview.vnc}` : ""}`,
        }),
      );
    }
    return section;
  }

  async function previewVm(): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    setStatus("rendering…", "muted");
    try {
      vmPreview = await virtApi.previewVm(cloneVm(vmDraft));
      ctPreview = null;
      previewFile = 0;
      activeTab = "preview";
      setStatus("preview ready", "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
      renderTabs();
    }
  }

  async function previewCt(): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    setStatus("rendering…", "muted");
    try {
      ctPreview = await virtApi.previewCt(cloneCt(ctDraft));
      vmPreview = null;
      previewFile = 0;
      activeTab = "preview";
      setStatus("preview ready", "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
      renderTabs();
    }
  }

  // --- tab: host ---------------------------------------------------------

  function buildHost(): HTMLElement {
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });
    if (!state) {
      layout.append(el("div", { class: "text-neutral-600", text: "loading…" }));
      return layout;
    }
    const host: HostCapabilities = state.host;

    const virt = el("section", { class: "panel" });
    virt.append(sectionTitle("virtualization capabilities"));
    const virtBody = el("div", { class: "flex flex-col gap-1 px-3 py-2 text-[11px]" });
    virtBody.append(
      row("architecture", host.arch),
      row("KVM (/dev/kvm)", host.kvm ? "available" : "not available", host.kvm),
      row("nested virtualisation", host.nested ? "enabled" : "disabled", host.nested),
      row("QEMU", host.qemu ? host.qemu.version : "not installed", !!host.qemu),
      row("accelerators", host.accelerators.join(", ") || "none"),
      row("UEFI firmware", host.uefi ? "installed" : "not found", host.uefi),
      row("LXC", host.lxc ? host.lxc.version : "not installed", !!host.lxc),
      row("unprivileged ids (/etc/subuid)", host.subuid_mapped ? "mapped" : "not mapped", host.subuid_mapped),
    );
    virt.append(virtBody);
    layout.append(virt);

    const hw = el("section", { class: "panel" });
    hw.append(sectionTitle("hardware"));
    const hwBody = el("div", { class: "flex flex-col gap-1 px-3 py-2 text-[11px]" });
    hwBody.append(
      row("CPU", host.cpu.model ?? host.cpu.vendor ?? "unknown"),
      row("logical CPUs", String(host.cpu.logical_cpus)),
      row("virtualisation flag", host.cpu.virtualization_flag ?? "none"),
      row("AES-NI / AVX2", `${host.cpu.has_aes ? "aes" : "no-aes"} / ${host.cpu.has_avx2 ? "avx2" : "no-avx2"}`),
      row("memory", `${host.memory_mb} MiB`),
      row("2 MiB hugepages", `${host.hugepages_2m_free} free / ${host.hugepages_2m_total} total`),
      row("bridges", host.bridges.join(", ") || "none"),
    );
    hw.append(hwBody);
    layout.append(hw);

    return layout;
  }

  function row(label: string, value: string, ok?: boolean): HTMLElement {
    return el("div", { class: "flex justify-between gap-3" }, [
      el("span", { class: "text-neutral-500", text: label }),
      el("span", {
        class: ok === undefined ? "text-neutral-300" : ok ? "text-emerald-400" : "text-neutral-600",
        text: value,
      }),
    ]);
  }

  // --- helpers -----------------------------------------------------------

  function vmImages(): GuestImage[] {
    const query = vmFilter.trim().toLowerCase();
    return (state?.catalog ?? [])
      .filter((image) => image.kind === "vm")
      .filter((image) => matches(image, query));
  }

  function lxcTemplates(): GuestImage[] {
    const query = ctFilter.trim().toLowerCase();
    return (state?.catalog ?? [])
      .filter((image) => image.kind === "lxc")
      .filter((image) => matches(image, query));
  }

  function matches(image: GuestImage, query: string): boolean {
    if (!query) {
      return true;
    }
    const haystack =
      `${image.name} ${image.id} ${image.category} ${image.description} ${image.tags.join(" ")}`.toLowerCase();
    return haystack.includes(query);
  }

  function bridgeOptions(): [string, string][] {
    const bridges = state?.host.bridges ?? ["br0", "user"];
    return bridges.map((name) => [name, name === "user" ? "user (slirp/NAT)" : name]);
  }

  renderChrome();
  renderTabs();

  // The page lives as long as the tab; only refresh while it is visible.
  window.setInterval(() => {
    if (root.style.display !== "none" && !busy && activeTab === "guests") {
      void load();
    }
  }, REFRESH_MS);

  return {
    root,
    onShow: () => void load(),
  };
}

type CloudInitIpMode = VmConfig["cloud_init"]["ip_mode"];

function sectionTitle(text: string): HTMLElement {
  return el("div", {
    class: "panel-title border-b border-white/5 px-3 py-1.5 text-[10px]",
    text,
  });
}

function nullableText(
  value: string | null,
  onChange: (value: string | null) => void,
  options: { placeholder?: string } = {},
): HTMLInputElement {
  return textInput(
    value ?? "",
    (raw) => {
      const trimmed = raw.trim();
      onChange(trimmed === "" ? null : raw);
    },
    options,
  );
}

/** Deep copies so the request body can never alias the live draft. */
function cloneVm(vm: VmConfig): VmConfig {
  return JSON.parse(JSON.stringify(vm)) as VmConfig;
}

function cloneCt(ct: CtConfig): CtConfig {
  return JSON.parse(JSON.stringify(ct)) as CtConfig;
}

function mib(bytes: number): string {
  return String(Math.round(bytes / (1024 * 1024)));
}
