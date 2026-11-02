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

import { virtApi, hostApi, imagesApi, blankVm, blankCt } from "../api";
import { showToast } from "../toast";
import type {
  ActionReport,
  ApiFailure,
  BindMount,
  CpuModel,
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
  bytes,
  checkbox,
  el,
  emptyNote,
  field,
  listInput,
  numberInput,
  openModal,
  select,
  textInput,
} from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
  onHide?: () => void;
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
  const tabBar = el("div", { class: "flex flex-wrap gap-1.5 border-b border-white/10 pb-2" });
  const tabBody = el("div", { class: "min-h-0 flex-1" });
  root.append(header, noticeBox, tabBar, tabBody);

  const tabs: Tab[] = [
    { id: "guests", label: "guests", build: buildGuests },
    // Named in caps and kept near the front: this is where custom OS images and
    // the download cache live, and it is easy to miss in a long tab strip.
    { id: "images", label: "Images", build: buildImages },
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

  // --- images tab: custom sources and the download cache ------------------

  function buildImages(): HTMLElement {
    const layout = el("div", { class: "flex flex-col gap-3" });
    const host = el("div", {
      class: "flex flex-col divide-y divide-white/10 border border-white/10 bg-[#141414]",
    });

    const intro = el("section", { class: "panel flex flex-col gap-1 px-3 py-2.5" });
    intro.append(
      el("div", {
        class: "text-[12px] font-medium text-neutral-100",
        text: "Custom OS images",
      }),
      el("div", {
        class: "text-[11px] text-neutral-400",
        text: "Download an OS image once and create as many VMs from it as you like, or point a VM at any image already on this host.",
      }),
      el("div", {
        class: "text-[10px] text-neutral-500",
        text: "Add any image by URL or import one from this server (validated with qemu-img). Images without a pinned checksum show a warning icon.",
      }),
    );

    const actions = el("section", { class: "panel flex flex-wrap items-center gap-2 px-3 py-2.5" });
    const urlBtn = button(
      "+ Add from URL",
      "border-emerald-600/60 bg-emerald-500/10 font-medium text-emerald-300 hover:border-emerald-500",
    );
    urlBtn.addEventListener("click", openAddUrl);
    const pathBtn = button(
      "Import from Server Path",
      "border-white/20 bg-white/5 font-medium text-neutral-100",
    );
    pathBtn.addEventListener("click", openImportPath);
    const refreshBtn = button("refresh");
    refreshBtn.addEventListener("click", () => void refreshImages(host));
    actions.append(
      el("span", { class: "panel-title text-[10px]", text: "download cache" }),
      urlBtn,
      pathBtn,
      el("span", { class: "ml-auto" }),
      refreshBtn,
    );

    layout.append(intro, actions, host);
    void refreshImages(host);
    return layout;
  }

  async function refreshImages(host: HTMLElement): Promise<void> {
    host.replaceChildren(el("div", { class: "px-3 py-2 text-[11px] text-neutral-500", text: "loading…" }));
    try {
      const images = await imagesApi.list();
      host.replaceChildren();
      if (images.length === 0) {
        host.append(emptyNote("no downloaded images yet"));
        return;
      }
      for (const image of images) {
        const row = el("div", { class: "flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-[11px]" });
        row.append(
          el("span", { class: "min-w-0 flex-1 truncate text-neutral-200", text: image.name }),
          el("span", { class: "tabular text-neutral-500", text: bytes(image.size_bytes) }),
          el("span", {
            class: "tabular text-neutral-600",
            text: image.modified ? new Date(image.modified * 1000).toLocaleDateString() : "—",
          }),
        );
        const removeBtn = button("delete", "text-[10px] text-rose-400 hover:border-rose-500");
        removeBtn.addEventListener("click", async () => {
          if (!window.confirm(`Delete ${image.name} from the images directory?`)) {
            return;
          }
          try {
            await imagesApi.remove(image.name);
            setStatus(`${image.name} deleted`, "ok");
          } catch (err) {
            setStatus(errorText(err), "error");
          }
          await refreshImages(host);
        });
        row.append(removeBtn);
        host.append(row);
      }
    } catch (err) {
      host.replaceChildren(emptyNote(errorText(err)));
    }
  }

  function openAddUrl(): void {
    const modal = openModal("add image from url", "max-w-lg");
    let url = "";
    let name = "";
    const feedback = el("div", { class: "tabular text-[11px] text-neutral-500" });
    const submit = button("download", "text-emerald-400 hover:border-emerald-500");
    const cancel = button("cancel", "", () => modal.close());
    submit.addEventListener("click", async () => {
      if (!url.trim()) {
        feedback.textContent = "a url is required";
        return;
      }
      submit.disabled = true;
      busy = true;
      feedback.className = "tabular text-[11px] text-neutral-500";
      feedback.textContent = "downloading… this can take a while for an ISO";
      try {
        const result = await imagesApi.fetch(url.trim(), name.trim() || undefined);
        setStatus(result.message ?? (result.job_id ? "download started" : "image downloaded"), "ok");
        showToast({ tone: "ok", title: "Image download started", message: result.job_id ? "Track progress in Activity." : result.message });
        if (result.job_id) window.dispatchEvent(new Event("stack-stack:activity-open"));
        modal.close();
      } catch (err) {
        feedback.className = "tabular text-[11px] text-rose-400";
        feedback.textContent = errorText(err);
      } finally {
        busy = false;
        submit.disabled = false;
      }
    });
    modal.body.append(
      field("url", textInput("", (value) => { url = value; }, { placeholder: "https://example.com/kali.iso", autocomplete: "off" })),
      field("file name (optional)", textInput("", (value) => { name = value; }, { placeholder: "derived from the url (e.g. kali.iso)", autocomplete: "off" })),
      el("div", { class: "flex flex-wrap items-center gap-2" }, [submit, cancel, feedback]),
    );
  }

  function openImportPath(): void {
    const modal = openModal("import from server path", "max-w-lg");
    let path = "";
    const feedback = el("div", { class: "tabular text-[11px] text-neutral-500" });
    const submit = button("import", "text-emerald-400 hover:border-emerald-500");
    const cancel = button("cancel", "", () => modal.close());
    submit.addEventListener("click", async () => {
      if (!path.trim()) {
        feedback.textContent = "a path is required";
        return;
      }
      submit.disabled = true;
      busy = true;
      feedback.className = "tabular text-[11px] text-neutral-500";
      feedback.textContent = "copying…";
      try {
        const result = await imagesApi.import(path.trim());
        setStatus(result.message ?? (result.job_id ? "import started" : "image imported"), "ok");
        showToast({ tone: "ok", title: "Image import started", message: result.job_id ? "Track progress in Activity." : result.message });
        if (result.job_id) window.dispatchEvent(new Event("stack-stack:activity-open"));
        modal.close();
      } catch (err) {
        feedback.className = "tabular text-[11px] text-rose-400";
        feedback.textContent = errorText(err);
      } finally {
        busy = false;
        submit.disabled = false;
      }
    });
    modal.body.append(
      el("div", {
        class: "text-[11px] text-neutral-400",
        text: "This imports a file already on the server. To upload from your computer, use Files → Upload first, then import that path.",
      }),
      field("server path", textInput("", (value) => { path = value; }, { placeholder: "/home/you/kali.iso", autocomplete: "off" })),
      el("div", { class: "flex flex-wrap items-center gap-2" }, [submit, cancel, feedback]),
    );
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
    showToast({
      tone: report.warnings.length > 0 ? "error" : "ok",
      title: `${note.charAt(0).toUpperCase() + note.slice(1)} ${report.warnings.length > 0 ? "failed" : "ready"}`,
      message: report.output,
    });
  }

  async function run(action: () => Promise<ActionReport>, note: string, reloadAfter = true): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    setStatus(`${note}…`, "muted");
    showToast({ tone: "info", title: `${note.charAt(0).toUpperCase() + note.slice(1)}…` });
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
    const focused = document.activeElement instanceof HTMLInputElement
      ? { placeholder: document.activeElement.placeholder, start: document.activeElement.selectionStart, end: document.activeElement.selectionEnd }
      : null;
    tabBar.replaceChildren();
    for (const tab of tabs) {
      const active = tab.id === activeTab;
      const style = active
        ? "border-emerald-500 bg-emerald-500/15 font-semibold text-emerald-300"
        : "text-neutral-400";
      tabBar.append(
        button(tab.label, style, () => {
          activeTab = tab.id;
          renderTabs();
        }),
      );
    }
    const tab = tabs.find((entry) => entry.id === activeTab) ?? tabs[0];
    tabBody.replaceChildren(tab.build());
    if (focused && focused.placeholder) {
      const replacement = [...tabBody.querySelectorAll("input")].find((input) => input.placeholder === focused.placeholder);
      if (replacement instanceof HTMLInputElement) {
        replacement.focus();
        if (focused.start !== null && focused.end !== null) {
          replacement.setSelectionRange(focused.start, focused.end);
        }
      }
    }
  }

  const rerender = () => renderTabs();

  // --- Proxmox-Style Creation Wizards ------------------------------------

  function nextFreeId(kind: "vm" | "ct"): string {
    const ids = (state?.statuses ?? [])
      .filter((s) => s.kind === kind)
      .map((s) => Number.parseInt(s.id, 10))
      .filter((n) => Number.isFinite(n) && n >= 100);
    const max = ids.length > 0 ? Math.max(...ids) : 99;
    return String(max + 1);
  }

  function renderStepper(steps: string[], currentStep: number, onJump: (step: number) => void): HTMLElement {
    const bar = el("div", { class: "flex items-center gap-1 overflow-x-auto pb-2 border-b border-white/10" });
    steps.forEach((label, idx) => {
      const isCurrent = idx === currentStep;
      const isPast = idx < currentStep;
      const stepBtn = el("button", {
        type: "button",
        class: `flex items-center gap-1.5 px-2.5 py-1 text-xs rounded transition-all whitespace-nowrap ${
          isCurrent
            ? "bg-emerald-500/20 text-emerald-300 border border-emerald-500/50 font-medium shadow-sm"
            : isPast
              ? "text-neutral-300 hover:text-white hover:bg-white/5 cursor-pointer"
              : "text-neutral-500 cursor-not-allowed opacity-60"
        }`,
      });
      const num = el("span", {
        class: `flex items-center justify-center size-4 rounded-full text-[10px] font-mono ${
          isCurrent ? "bg-emerald-500 text-black font-bold" : isPast ? "bg-white/20 text-white" : "bg-white/5 text-neutral-500"
        }`,
        text: String(idx + 1),
      });
      stepBtn.append(num, el("span", { text: label }));
      if (isPast) {
        stepBtn.addEventListener("click", () => onJump(idx));
      }
      bar.append(stepBtn);
      if (idx < steps.length - 1) {
        bar.append(el("span", { class: "text-neutral-600 text-xs select-none", text: "›" }));
      }
    });
    return bar;
  }

  function applySelectedVmImage(targetVm: VmConfig, image: GuestImage): void {
    targetVm.image = image.id;
    if (image.default_user) {
      targetVm.cloud_init.username = image.default_user;
    }
    if (image.min_disk_gb > targetVm.disk_gb) {
      targetVm.disk_gb = image.min_disk_gb;
    }
    if (image.min_memory_mb > targetVm.memory_mb) {
      targetVm.memory_mb = image.min_memory_mb;
    }
    if (image.arch) {
      targetVm.arch = image.arch;
    }
    if (image.format === "iso") {
      targetVm.cloud_init.enabled = false;
    }
  }

  function applySelectedCtTemplate(targetCt: CtConfig, image: GuestImage): void {
    targetCt.template = image.id;
    if (image.min_disk_gb > targetCt.rootfs_gb) {
      targetCt.rootfs_gb = image.min_disk_gb;
    }
    if (image.min_memory_mb > targetCt.memory_mb) {
      targetCt.memory_mb = image.min_memory_mb;
    }
  }

  function openVmWizard(): void {
    const modal = openModal("Create: Virtual Machine", "max-w-4xl");
    modal.body.className = "flex flex-col gap-4";

    const vm = blankVm();
    vm.id = nextFreeId("vm");
    vm.cloud_init.hostname = `vm-${vm.id}`;
    let activeStep = 0;
    let startAfterCreate = true;
    let imageTab: "catalog" | "custom" = "catalog";
    let imageSearch = "";
    let selectedImageObj: GuestImage | null = null;
    let sshKeysRaw = "";

    const steps = ["General", "OS Image", "System & CPU", "Disks & Memory", "Network", "Cloud-Init", "Confirm"];

    const stepperContainer = el("div");
    const stepContent = el("div", { class: "min-h-[360px] flex flex-col gap-4" });
    const footer = el("div", { class: "flex items-center justify-between border-t border-white/10 pt-3" });

    modal.body.append(stepperContainer, stepContent, footer);

    function renderCurrentStep(): void {
      stepperContainer.replaceChildren(renderStepper(steps, activeStep, (step) => {
        activeStep = step;
        renderCurrentStep();
      }));

      stepContent.replaceChildren();

      if (activeStep === 0) {
        // Step 0: General
        const generalBox = el("div", { class: "flex flex-col gap-3" });
        const hostInfo = el("div", { class: "flex items-center gap-2 text-xs text-neutral-400 bg-white/[0.02] p-2.5 rounded border border-white/5" });
        hostInfo.append(
          el("span", { class: "text-neutral-500", text: "Target Node:" }),
          el("span", { class: "font-semibold text-emerald-400 font-mono", text: "host (fujitsu)" }),
          el("span", { class: "ml-auto text-[11px] text-neutral-500", text: state?.host.kvm ? "KVM Acceleration: Available" : "KVM: Not detected" }),
        );

        const formGrid = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3" });
        const idInput = textInput(vm.id, (v) => { vm.id = v; }, { placeholder: "100" });
        const nameInput = textInput(vm.cloud_init.hostname, (v) => {
          vm.cloud_init.hostname = v;
          vm.name = v;
        }, { placeholder: "ubuntu-server" });
        const autostartBox = checkbox(vm.autostart, (v) => { vm.autostart = v; }, "Start at boot (Autostart VM when host system boots)");
        const commentInput = textInput(vm.comment, (v) => { vm.comment = v; }, { placeholder: "Production web server, database, test lab…" });

        formGrid.append(
          field("VM ID", idInput, "Numerical identifier for this VM. Suggested next available ID is automatically assigned."),
          field("VM Name / Hostname", nameInput, "Name of the VM and guest system hostname."),
          field("Description / Notes", commentInput, "Optional notes or operator tags."),
          el("div", { class: "col-span-full pt-1" }, [autostartBox]),
        );

        generalBox.append(hostInfo, formGrid);
        stepContent.append(generalBox);
      } else if (activeStep === 1) {
        // Step 1: OS Image
        const imageBox = el("div", { class: "flex flex-col gap-3" });

        const topBar = el("div", { class: "flex items-center justify-between gap-3" });
        const tabToggle = el("div", { class: "flex items-center gap-1 bg-black/40 p-1 rounded border border-white/5 text-xs" });
        const catBtn = button(
          "Cloud Images Catalog",
          imageTab === "catalog" ? "bg-emerald-500/20 text-emerald-300 font-medium" : "text-neutral-400",
          () => {
            imageTab = "catalog";
            renderCurrentStep();
          },
        );
        const customBtn = button(
          "Custom ISO / Image Path",
          imageTab === "custom" ? "bg-emerald-500/20 text-emerald-300 font-medium" : "text-neutral-400",
          () => {
            imageTab = "custom";
            renderCurrentStep();
          },
        );
        tabToggle.append(catBtn, customBtn);

        const searchInput = textInput(imageSearch, (v) => {
          imageSearch = v;
          renderCatalogGrid();
        }, { placeholder: "Search distributions (Debian, Ubuntu, Alpine…)", class: "w-64" });

        topBar.append(tabToggle);
        if (imageTab === "catalog") {
          topBar.append(searchInput);
        }

        imageBox.append(topBar);

        const contentArea = el("div", { class: "flex-1" });
        const renderCatalogGrid = () => {
          contentArea.replaceChildren();
          const query = imageSearch.toLowerCase().trim();
          const images = (state?.catalog ?? [])
            .filter((img) => img.kind === "vm")
            .filter((img) => !query || `${img.name} ${img.id} ${img.category} ${img.description}`.toLowerCase().includes(query));

          if (images.length === 0) {
            contentArea.append(emptyNote("No matching operating system images found."));
            return;
          }

          const grid = el("div", { class: "grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-2.5 max-h-[340px] overflow-y-auto pr-1" });
          for (const img of images) {
            const isSelected = vm.image === img.id;
            const card = el("button", {
              type: "button",
              class: `flex flex-col items-start gap-1 p-3 rounded-lg border text-left transition-all ${
                isSelected
                  ? "border-emerald-500 bg-emerald-950/40 ring-1 ring-emerald-500/50 shadow-md"
                  : "border-white/10 bg-white/[0.02] hover:border-white/25 hover:bg-white/[0.04]"
              }`,
            });

            const cardHeader = el("div", { class: "flex w-full items-center justify-between" });
            const titleGroup = el("div", { class: "flex items-center gap-2" });
            titleGroup.append(
              el("span", { class: "text-base", text: img.icon || "💿" }),
              el("span", { class: "font-semibold text-neutral-100 text-xs", text: img.name }),
            );
            cardHeader.append(titleGroup);
            if (isSelected) {
              cardHeader.append(el("span", { class: "text-[10px] text-emerald-400 font-bold uppercase", text: "Selected" }));
            }

            const desc = el("span", { class: "text-[11px] text-neutral-400 line-clamp-2 leading-relaxed", text: img.description });
            const specs = el("div", { class: "flex items-center gap-2 text-[10px] text-neutral-500 mt-1 font-mono" });
            specs.append(
              el("span", { text: img.arch }),
              el("span", { text: "·" }),
              el("span", { text: `≥${img.min_memory_mb}MB RAM` }),
              el("span", { text: "·" }),
              el("span", { text: `${img.min_disk_gb}GB disk` }),
            );

            card.append(cardHeader, desc, specs);
            card.addEventListener("click", () => {
              applySelectedVmImage(vm, img);
              selectedImageObj = img;
              renderCurrentStep();
            });
            grid.append(card);
          }
          contentArea.append(grid);
        };

        if (imageTab === "catalog") {
          renderCatalogGrid();
        } else {
          const customForm = el("div", { class: "flex flex-col gap-3 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
          const pathInput = textInput(vm.image ?? "", (v) => { vm.image = v.trim() || null; }, {
            placeholder: "/var/lib/stack-stack/images/custom-guest.qcow2 or https://.../image.iso",
          });
          customForm.append(
            field("Image Path or URL", pathInput, "Absolute file path on host or direct URL to download."),
            el("div", { class: "text-xs text-neutral-400 leading-relaxed" }, [
              el("p", { text: "You can point this VM to any ISO installer or QCOW2 cloud image residing on the host." }),
              el("p", { class: "text-neutral-500 mt-1", text: "Tip: For manual CD/DVD installers, Cloud-Init will be bypassed automatically." }),
            ]),
          );
          contentArea.append(customForm);
        }

        imageBox.append(contentArea);
        stepContent.append(imageBox);
      } else if (activeStep === 2) {
        // Step 2: System & CPU
        const sysBox = el("div", { class: "flex flex-col gap-4" });

        const formGrid = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3" });
        const vcpusInput = numberInput(vm.vcpus, (v) => { vm.vcpus = Math.max(1, v); }, { min: 1, max: 256 });
        const cpuSelect = select(vm.cpu_model, [
          ["host", "host (fastest / native host CPU passthrough)"],
          ["max", "max (maximum virtualized feature set)"],
          ["qemu64", "qemu64 (standard generic x86_64)"],
        ], (v) => { vm.cpu_model = v as CpuModel; });

        const machineSelect = select(vm.machine, [
          ["q35", "q35 (Modern PCIe architecture - Recommended)"],
          ["pc", "pc / i440fx (Legacy PCI architecture)"],
        ], (v) => { vm.machine = v as VmConfig["machine"]; });

        const firmwareSelect = select(vm.firmware, [
          ["uefi", "UEFI / OVMF (Modern GPT partitions & EFI)"],
          ["bios", "BIOS / SeaBIOS (Standard MBR boot)"],
        ], (v) => { vm.firmware = v as VmConfig["firmware"]; });

        formGrid.append(
          field("vCPU Cores", vcpusInput, "Number of virtual CPU execution threads assigned to this virtual machine."),
          field("CPU Architecture Model", cpuSelect, "'host' matches the physical CPU model and flags for near bare-metal compute performance."),
          field("Machine Type", machineSelect, "Q35 provides native PCIe topology, ideal for modern operating systems."),
          field("BIOS Firmware", firmwareSelect, "UEFI (OVMF) is recommended for modern distributions and Windows."),
        );

        const optionsSection = el("div", { class: "flex flex-col gap-2 pt-2 border-t border-white/5" });
        optionsSection.append(
          el("span", { class: "text-xs font-semibold text-neutral-300 uppercase tracking-wide", text: "Virtualization Features" }),
          el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-2" }, [
            checkbox(vm.guest_agent, (v) => { vm.guest_agent = v; }, "QEMU Guest Agent (Reports memory & IP stats, allows graceful shutdown)"),
            checkbox(vm.vnc_enabled, (v) => { vm.vnc_enabled = v; }, "VNC Graphics Console (Enable web desktop viewing)"),
          ]),
        );

        sysBox.append(formGrid, optionsSection);
        stepContent.append(sysBox);
      } else if (activeStep === 3) {
        // Step 3: Disks & Memory
        const storageBox = el("div", { class: "flex flex-col gap-4" });

        // Memory row
        const memCard = el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
        const memTop = el("div", { class: "flex items-center justify-between" });
        memTop.append(
          el("span", { class: "text-xs font-semibold text-neutral-200", text: "Memory (RAM)" }),
          el("span", { class: "text-xs font-mono text-emerald-400", text: `${vm.memory_mb} MiB (${(vm.memory_mb / 1024).toFixed(1)} GiB)` }),
        );
        const memInput = numberInput(vm.memory_mb, (v) => {
          vm.memory_mb = Math.max(128, v);
          renderCurrentStep();
        }, { min: 128 });
        const memPills = el("div", { class: "flex items-center gap-1.5 mt-1" });
        [1024, 2048, 4096, 8192, 16384].forEach((mb) => {
          const pill = button(
            `${mb / 1024} GiB`,
            vm.memory_mb === mb ? "bg-emerald-500/25 text-emerald-300 border-emerald-500" : "text-neutral-400 hover:text-white",
            () => {
              vm.memory_mb = mb;
              renderCurrentStep();
            },
          );
          memPills.append(pill);
        });
        memCard.append(memTop, field("RAM in MiB", memInput), memPills);

        // Disk row
        const diskCard = el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
        const diskTop = el("div", { class: "flex items-center justify-between" });
        diskTop.append(
          el("span", { class: "text-xs font-semibold text-neutral-200", text: "Primary Virtual Disk" }),
          el("span", { class: "text-xs font-mono text-emerald-400", text: `${vm.disk_gb} GiB` }),
        );
        const diskInput = numberInput(vm.disk_gb, (v) => {
          vm.disk_gb = Math.max(1, v);
          renderCurrentStep();
        }, { min: 1 });
        const diskPills = el("div", { class: "flex items-center gap-1.5 mt-1" });
        [10, 20, 40, 80, 160].forEach((gb) => {
          const pill = button(
            `${gb} GiB`,
            vm.disk_gb === gb ? "bg-emerald-500/25 text-emerald-300 border-emerald-500" : "text-neutral-400 hover:text-white",
            () => {
              vm.disk_gb = gb;
              renderCurrentStep();
            },
          );
          diskPills.append(pill);
        });
        const diskFormatSelect = select(vm.disk_format, [
          ["qcow2", "qcow2 (Thin-provisioned, snapshots supported - Recommended)"],
          ["raw", "raw (Direct fixed storage allocation)"],
        ], (v) => { vm.disk_format = v as VmConfig["disk_format"]; });

        diskCard.append(
          diskTop,
          el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3" }, [
            field("Disk Size in GiB", diskInput),
            field("Disk Image Format", diskFormatSelect, "qcow2 only allocates physical host space as data is written."),
          ]),
          diskPills,
          el("div", { class: "pt-2 flex flex-wrap gap-4 border-t border-white/5" }, [
            checkbox(vm.discard, (v) => { vm.discard = v; }, "TRIM / Discard (Frees unused guest blocks back to host SSD)"),
            checkbox(vm.hugepages, (v) => { vm.hugepages = v; }, "Hugepages 2M", !state?.host.hugepages_2m_free),
          ]),
        );

        storageBox.append(memCard, diskCard);
        stepContent.append(storageBox);
      } else if (activeStep === 4) {
        // Step 4: Network
        const netBox = el("div", { class: "flex flex-col gap-4" });
        const bridges = bridgeOptions();
        const ci = vm.cloud_init;

        const netCard = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
        const bridgeSelect = select(vm.bridge, bridges, (v) => { vm.bridge = v; });
        const ipModeSelect = select(ci.ip_mode, [
          ["dhcp", "DHCP (Automatic IP address from LAN gateway)"],
          ["static", "Static (Manual IPv4 configuration)"],
          ["none", "None (Isolated guest, no network)"],
        ], (v) => {
          ci.ip_mode = v as CloudInitIpMode;
          renderCurrentStep();
        });

        netCard.append(
          field("Bridge Interface", bridgeSelect, "The host network bridge that the virtual NIC connects to."),
          field("IP Addressing Mode", ipModeSelect, "DHCP is standard for home servers and automatically assigns LAN IPs."),
        );

        netBox.append(netCard);

        if (ci.ip_mode === "static") {
          const staticCard = el("div", { class: "grid grid-cols-1 md:grid-cols-3 gap-3 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
          const addrInput = textInput(ci.address ?? "", (v) => { ci.address = v.trim() || null; }, { placeholder: addressHint() });
          const gwInput = textInput(ci.gateway, (v) => { ci.gateway = v; }, { placeholder: "192.168.0.1" });
          const dnsInput = listInput(ci.dns, (v) => { ci.dns = v; }, { placeholder: "1.1.1.1, 8.8.8.8" });

          staticCard.append(
            field("IPv4 Address & CIDR", addrInput, "e.g. 192.168.0.120/24"),
            field("Default Gateway", gwInput, "Router or gateway IP address"),
            field("DNS Nameservers", dnsInput, "Comma-separated upstream resolvers"),
          );
          netBox.append(staticCard);
        }

        const toggles = el("div", { class: "flex items-center gap-3" }, [
          checkbox(vm.multiqueue, (v) => { vm.multiqueue = v; }, "Multiqueue Network (Distribute NIC traffic across multiple vCPUs)"),
        ]);
        netBox.append(toggles);

        stepContent.append(netBox);
      } else if (activeStep === 5) {
        // Step 5: Cloud-Init
        const ciBox = el("div", { class: "flex flex-col gap-3" });
        const ci = vm.cloud_init;

        const enableBox = checkbox(ci.enabled, (v) => {
          ci.enabled = v;
          renderCurrentStep();
        }, "Enable automated Cloud-Init provisioning on first boot");

        ciBox.append(
          el("div", { class: "p-3 bg-white/[0.02] border border-white/5 rounded-lg flex items-center justify-between" }, [
            enableBox,
            el("span", { class: "text-[11px] text-neutral-400", text: "Sets up credentials, SSH keys, and network automatically." }),
          ]),
        );

        if (ci.enabled) {
          const credsGrid = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3" });
          const userInput = textInput(ci.username, (v) => { ci.username = v; }, { placeholder: "debian" });
          const passInput = textInput(ci.password, (v) => { ci.password = v; }, { type: "password", placeholder: "Leave blank for SSH key only" });

          credsGrid.append(
            field("Default User Account", userInput, "System login user created inside the virtual machine."),
            field("User Password", passInput, "Console and sudo password for this user."),
          );

          const sshBox = el("div", { class: "flex flex-col gap-1.5" });
          const sshHeader = el("div", { class: "flex items-center justify-between text-[11px] text-neutral-400" });
          sshHeader.append(
            el("span", { text: "SSH Authorized Public Keys" }),
            el("span", { class: "text-[10px] text-neutral-500", text: "Paste id_ed25519.pub or id_rsa.pub" }),
          );

          const sshArea = el("textarea", {
            class: "w-full h-28 font-mono text-[11px] p-2.5 bg-[#141722] border border-white/10 rounded-lg text-neutral-200 outline-none focus:border-emerald-500 resize-none",
            placeholder: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI... user@machine\nssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC... user@work",
          }) as HTMLTextAreaElement;
          sshArea.value = ci.ssh_keys.join("\n") || sshKeysRaw;
          sshArea.addEventListener("input", () => {
            sshKeysRaw = sshArea.value;
            ci.ssh_keys = sshArea.value.split("\n").map((s) => s.trim()).filter(Boolean);
          });

          sshBox.append(sshHeader, sshArea);
          ciBox.append(credsGrid, sshBox);
        }

        stepContent.append(ciBox);
      } else if (activeStep === 6) {
        // Step 6: Confirm & Deploy
        const confirmBox = el("div", { class: "flex flex-col gap-4" });

        const reviewCard = el("div", { class: "rounded-lg border border-white/10 bg-white/[0.02] overflow-hidden" });
        const table = el("table", { class: "w-full text-xs text-left" });
        const tbody = el("tbody", { class: "divide-y divide-white/5" });

        const rows: [string, string][] = [
          ["Target Node", "host (fujitsu)"],
          ["VM ID & Name", `${vm.id} (${vm.cloud_init.hostname || "vm"})`],
          ["Operating System", selectedImageObj?.name || vm.image || "Empty / Custom Disk"],
          ["Architecture & Firmware", `${vm.arch} · ${vm.machine} · ${vm.firmware.toUpperCase()}`],
          ["Processor (vCPUs)", `${vm.vcpus} Core(s) · ${vm.cpu_model}`],
          ["Memory (RAM)", `${vm.memory_mb} MiB (${(vm.memory_mb / 1024).toFixed(1)} GiB)`],
          ["Virtual Disk", `${vm.disk_gb} GiB · ${vm.disk_format} ${vm.discard ? "(TRIM enabled)" : ""}`],
          ["Network Interface", `${vm.bridge} (${vm.cloud_init.ip_mode})`],
          ["Provisioning", vm.cloud_init.enabled ? `User: ${vm.cloud_init.username || "root"} · ${vm.cloud_init.ssh_keys.length} SSH key(s)` : "Disabled"],
        ];

        for (const [k, v] of rows) {
          const tr = el("tr", { class: "hover:bg-white/[0.02]" });
          tr.append(
            el("td", { class: "py-2 px-3 text-neutral-400 font-medium w-48", text: k }),
            el("td", { class: "py-2 px-3 text-neutral-200 font-mono", text: v }),
          );
          tbody.append(tr);
        }
        table.append(tbody);
        reviewCard.append(table);

        const startToggle = checkbox(startAfterCreate, (v) => { startAfterCreate = v; }, "Start virtual machine immediately after creation");

        confirmBox.append(reviewCard, el("div", { class: "p-2 bg-emerald-950/20 border border-emerald-500/20 rounded-lg" }, [startToggle]));
        stepContent.append(confirmBox);
      }

      // Update footer
      footer.replaceChildren();
      const cancelBtn = button("Cancel", "text-neutral-400 hover:text-white", () => modal.close());

      const navGroup = el("div", { class: "flex items-center gap-2" });
      const backBtn = button("‹ Back", "text-neutral-300 hover:text-white", () => {
        if (activeStep > 0) {
          activeStep--;
          renderCurrentStep();
        }
      });
      backBtn.disabled = activeStep === 0;

      navGroup.append(backBtn);

      if (activeStep < steps.length - 1) {
        const nextBtn = button("Next ›", "border-emerald-600 bg-emerald-500/20 text-emerald-300 hover:bg-emerald-500/30 font-semibold px-4 py-1.5", () => {
          activeStep++;
          renderCurrentStep();
        });
        navGroup.append(nextBtn);
      } else {
        const finishBtn = button("Finish & Create VM", "border-emerald-600 bg-emerald-500/25 text-emerald-300 hover:bg-emerald-500/35 font-semibold px-5 py-1.5 shadow-md", async () => {
          finishBtn.disabled = true;
          finishBtn.textContent = "Creating Virtual Machine…";
          try {
            const report = await virtApi.createVm(cloneVm(vm));
            showToast({
              tone: "ok",
              title: "VM Created Successfully",
              message: report.output || `VM ${vm.id} is configured.`,
            });
            if (startAfterCreate && report.id) {
              await virtApi.vmStart(report.id).catch(() => null);
              showToast({
                tone: "ok",
                title: "VM Started",
                message: `VM ${report.id} is now running.`,
              });
            }
            modal.close();
            await load();
          } catch (err) {
            finishBtn.disabled = false;
            finishBtn.textContent = "Finish & Create VM";
            showToast({ tone: "error", title: "Creation Failed", message: errorText(err) });
          }
        });
        navGroup.append(finishBtn);
      }

      footer.append(cancelBtn, navGroup);
    }

    renderCurrentStep();
  }

  function openCtWizard(): void {
    const modal = openModal("Create: LXC Container", "max-w-3xl");
    modal.body.className = "flex flex-col gap-4";

    const ct = blankCt();
    ct.id = nextFreeId("ct");
    ct.hostname = `ct-${ct.id}`;
    let activeStep = 0;
    let startAfterCreate = true;
    let templateSearch = "";
    let selectedTemplateObj: GuestImage | null = null;
    let sshKeysRaw = "";

    const steps = ["General", "Template", "Disks & Mounts", "CPU & Memory", "Network", "Confirm"];

    const stepperContainer = el("div");
    const stepContent = el("div", { class: "min-h-[360px] flex flex-col gap-4" });
    const footer = el("div", { class: "flex items-center justify-between border-t border-white/10 pt-3" });

    modal.body.append(stepperContainer, stepContent, footer);

    function renderCurrentStep(): void {
      stepperContainer.replaceChildren(renderStepper(steps, activeStep, (step) => {
        activeStep = step;
        renderCurrentStep();
      }));

      stepContent.replaceChildren();

      if (activeStep === 0) {
        // Step 0: General
        const generalBox = el("div", { class: "flex flex-col gap-3" });
        const formGrid = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3" });

        const idInput = textInput(ct.id, (v) => { ct.id = v; }, { placeholder: "100" });
        const hostInput = textInput(ct.hostname, (v) => { ct.hostname = v; ct.name = v; }, { placeholder: "web-container" });
        const passInput = textInput(ct.password, (v) => { ct.password = v; }, { type: "password", placeholder: "Root account password" });

        formGrid.append(
          field("Container ID", idInput, "Numeric identifier for this container."),
          field("Hostname", hostInput, "Container hostname and local DNS name."),
          field("Root Password", passInput, "Root password for console or SSH access."),
        );

        const sshBox = el("div", { class: "flex flex-col gap-1.5" });
        const sshArea = el("textarea", {
          class: "w-full h-20 font-mono text-[11px] p-2 bg-[#141722] border border-white/10 rounded-lg text-neutral-200 outline-none focus:border-emerald-500 resize-none",
          placeholder: "Paste SSH public keys here (optional)",
        }) as HTMLTextAreaElement;
        sshArea.value = ct.ssh_keys.join("\n") || sshKeysRaw;
        sshArea.addEventListener("input", () => {
          sshKeysRaw = sshArea.value;
          ct.ssh_keys = sshArea.value.split("\n").map((s) => s.trim()).filter(Boolean);
        });
        sshBox.append(field("SSH Public Keys", sshArea));

        const toggles = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-2 pt-2 border-t border-white/5" }, [
          checkbox(ct.unprivileged, (v) => { ct.unprivileged = v; }, "Unprivileged container (Recommended security isolation)"),
          checkbox(ct.nesting, (v) => { ct.nesting = v; }, "Nesting (Allows running Docker or LXC inside container)"),
        ]);

        generalBox.append(formGrid, sshBox, toggles);
        stepContent.append(generalBox);
      } else if (activeStep === 1) {
        // Step 1: Template
        const templateBox = el("div", { class: "flex flex-col gap-3" });
        const searchInput = textInput(templateSearch, (v) => {
          templateSearch = v;
          renderTemplates();
        }, { placeholder: "Search container templates (Alpine, Debian, Ubuntu…)", class: "w-full" });

        const listContainer = el("div", { class: "flex-1" });
        const renderTemplates = () => {
          listContainer.replaceChildren();
          const query = templateSearch.toLowerCase().trim();
          const templates = (state?.catalog ?? [])
            .filter((img) => img.kind === "lxc")
            .filter((img) => !query || `${img.name} ${img.id} ${img.description}`.toLowerCase().includes(query));

          if (templates.length === 0) {
            listContainer.append(emptyNote("No matching LXC templates found."));
            return;
          }

          const grid = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-2 max-h-[320px] overflow-y-auto pr-1" });
          for (const tmpl of templates) {
            const isSelected = ct.template === tmpl.id;
            const card = el("button", {
              type: "button",
              class: `flex flex-col items-start gap-1 p-2.5 rounded-lg border text-left transition-all ${
                isSelected
                  ? "border-sky-500 bg-sky-950/40 ring-1 ring-sky-500/50 shadow-md"
                  : "border-white/10 bg-white/[0.02] hover:border-white/25 hover:bg-white/[0.04]"
              }`,
            });

            const top = el("div", { class: "flex w-full items-center justify-between" });
            top.append(
              el("span", { class: "font-semibold text-xs text-neutral-100", text: tmpl.name }),
              el("span", { class: "text-[10px] font-mono text-neutral-500", text: tmpl.arch }),
            );
            card.append(top, el("span", { class: "text-[11px] text-neutral-400 line-clamp-1", text: tmpl.description }));
            card.addEventListener("click", () => {
              applySelectedCtTemplate(ct, tmpl);
              selectedTemplateObj = tmpl;
              renderCurrentStep();
            });
            grid.append(card);
          }
          listContainer.append(grid);
        };

        renderTemplates();
        templateBox.append(searchInput, listContainer);
        stepContent.append(templateBox);
      } else if (activeStep === 2) {
        // Step 2: Disks & Mounts
        const diskBox = el("div", { class: "flex flex-col gap-4" });
        const rootfsInput = numberInput(ct.rootfs_gb, (v) => { ct.rootfs_gb = Math.max(1, v); }, { min: 1 });
        const pills = el("div", { class: "flex items-center gap-1.5" });
        [4, 8, 16, 32, 64].forEach((gb) => {
          const pill = button(
            `${gb} GiB`,
            ct.rootfs_gb === gb ? "bg-sky-500/25 text-sky-300 border-sky-500" : "text-neutral-400 hover:text-white",
            () => {
              ct.rootfs_gb = gb;
              renderCurrentStep();
            },
          );
          pills.append(pill);
        });

        const rootCard = el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" }, [
          field("Root Filesystem Size (GiB)", rootfsInput),
          pills,
        ]);

        const bindCard = el("div", { class: "flex flex-col gap-2 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
        bindCard.append(el("span", { class: "text-xs font-semibold text-neutral-200", text: "Host Bind Mounts" }));
        ct.binds.forEach((bind, idx) => {
          const row = el("div", { class: "flex flex-wrap items-center gap-2 text-xs" });
          row.append(
            textInput(bind.source, (v) => { bind.source = v; }, { placeholder: "/srv/data (host)" }),
            el("span", { class: "text-neutral-500", text: "→" }),
            textInput(bind.target, (v) => { bind.target = v; }, { placeholder: "/mnt/data (container)" }),
            checkbox(bind.readonly, (v) => { bind.readonly = v; }, "read-only"),
            button("✕", "text-rose-400 hover:text-rose-300 px-1.5", () => {
              ct.binds.splice(idx, 1);
              renderCurrentStep();
            }),
          );
          bindCard.append(row);
        });
        bindCard.append(
          button("+ Add Bind Mount", "text-sky-300 hover:border-sky-500 text-xs", () => {
            ct.binds.push({ source: "", target: "", readonly: false });
            renderCurrentStep();
          }),
        );

        diskBox.append(rootCard, bindCard);
        stepContent.append(diskBox);
      } else if (activeStep === 3) {
        // Step 3: CPU & Memory
        const cpuMemBox = el("div", { class: "flex flex-col gap-4" });
        const grid = el("div", { class: "grid grid-cols-1 md:grid-cols-3 gap-3 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });

        const coresInput = numberInput(ct.cores, (v) => { ct.cores = Math.max(1, v); }, { min: 1, max: 256 });
        const memInput = numberInput(ct.memory_mb, (v) => { ct.memory_mb = Math.max(64, v); }, { min: 64 });
        const swapInput = numberInput(ct.swap_mb, (v) => { ct.swap_mb = Math.max(0, v); }, { min: 0 });

        grid.append(
          field("CPU Cores", coresInput),
          field("Memory (MiB)", memInput),
          field("Swap (MiB)", swapInput),
        );
        cpuMemBox.append(grid);
        stepContent.append(cpuMemBox);
      } else if (activeStep === 4) {
        // Step 4: Network
        const netBox = el("div", { class: "flex flex-col gap-3" });
        const bridges = bridgeOptions();
        const bridgeSelect = select(ct.bridge, bridges, (v) => { ct.bridge = v; });
        const ipModeSelect = select(ct.ip_mode, [
          ["dhcp", "DHCP (Automatic)"],
          ["static", "Static IPv4"],
          ["none", "None (Isolated)"],
        ], (v) => {
          ct.ip_mode = v as CloudInitIpMode;
          renderCurrentStep();
        });

        const netCard = el("div", { class: "grid grid-cols-1 md:grid-cols-2 gap-3 p-3 bg-white/[0.02] border border-white/5 rounded-lg" }, [
          field("Bridge Interface", bridgeSelect),
          field("IP Mode", ipModeSelect),
        ]);
        netBox.append(netCard);

        if (ct.ip_mode === "static") {
          const staticCard = el("div", { class: "grid grid-cols-1 md:grid-cols-3 gap-3 p-3 bg-white/[0.02] border border-white/5 rounded-lg" });
          const addrInput = textInput(ct.address ?? "", (v) => { ct.address = v.trim() || null; }, { placeholder: addressHint() });
          const gwInput = textInput(ct.gateway, (v) => { ct.gateway = v; }, { placeholder: "192.168.0.1" });
          const dnsInput = listInput(ct.dns, (v) => { ct.dns = v; }, { placeholder: "1.1.1.1, 8.8.8.8" });

          staticCard.append(
            field("IPv4 Address & CIDR", addrInput),
            field("Default Gateway", gwInput),
            field("DNS Servers", dnsInput),
          );
          netBox.append(staticCard);
        }

        stepContent.append(netBox);
      } else if (activeStep === 5) {
        // Step 5: Confirm
        const confirmBox = el("div", { class: "flex flex-col gap-4" });
        const reviewCard = el("div", { class: "rounded-lg border border-white/10 bg-white/[0.02] overflow-hidden" });
        const table = el("table", { class: "w-full text-xs text-left" });
        const tbody = el("tbody", { class: "divide-y divide-white/5" });

        const rows: [string, string][] = [
          ["Container ID & Name", `${ct.id} (${ct.hostname})`],
          ["Template", selectedTemplateObj?.name || ct.template || "Standard LXC"],
          ["CPU & Memory", `${ct.cores} Core(s) · ${ct.memory_mb} MiB RAM · ${ct.swap_mb} MiB Swap`],
          ["Root Filesystem", `${ct.rootfs_gb} GiB`],
          ["Network", `${ct.bridge} (${ct.ip_mode})`],
          ["Isolation", ct.unprivileged ? "Unprivileged (Safe)" : "Privileged"],
        ];

        for (const [k, v] of rows) {
          const tr = el("tr", { class: "hover:bg-white/[0.02]" });
          tr.append(
            el("td", { class: "py-2 px-3 text-neutral-400 font-medium w-48 text-xs", text: k }),
            el("td", { class: "py-2 px-3 text-neutral-200 font-mono text-xs", text: v }),
          );
          tbody.append(tr);
        }
        table.append(tbody);
        reviewCard.append(table);

        const startToggle = checkbox(startAfterCreate, (v) => { startAfterCreate = v; }, "Start container immediately after creation");
        confirmBox.append(reviewCard, el("div", { class: "p-2 bg-sky-950/20 border border-sky-500/20 rounded-lg" }, [startToggle]));
        stepContent.append(confirmBox);
      }

      // Update footer
      footer.replaceChildren();
      const cancelBtn = button("Cancel", "text-neutral-400 hover:text-white", () => modal.close());
      const navGroup = el("div", { class: "flex items-center gap-2" });
      const backBtn = button("‹ Back", "text-neutral-300 hover:text-white", () => {
        if (activeStep > 0) {
          activeStep--;
          renderCurrentStep();
        }
      });
      backBtn.disabled = activeStep === 0;
      navGroup.append(backBtn);

      if (activeStep < steps.length - 1) {
        const nextBtn = button("Next ›", "border-sky-600 bg-sky-500/20 text-sky-300 hover:bg-sky-500/30 font-semibold px-4 py-1.5", () => {
          activeStep++;
          renderCurrentStep();
        });
        navGroup.append(nextBtn);
      } else {
        const finishBtn = button("Finish & Create Container", "border-sky-600 bg-sky-500/25 text-sky-300 hover:bg-sky-500/35 font-semibold px-5 py-1.5 shadow-md", async () => {
          finishBtn.disabled = true;
          finishBtn.textContent = "Creating Container…";
          try {
            const report = await virtApi.createCt(cloneCt(ct));
            showToast({
              tone: "ok",
              title: "Container Created Successfully",
              message: report.output || `Container ${ct.id} is configured.`,
            });
            if (startAfterCreate && report.id) {
              await virtApi.ctStart(report.id).catch(() => null);
              showToast({
                tone: "ok",
                title: "Container Started",
                message: `Container ${report.id} is now running.`,
              });
            }
            modal.close();
            await load();
          } catch (err) {
            finishBtn.disabled = false;
            finishBtn.textContent = "Finish & Create Container";
            showToast({ tone: "error", title: "Creation Failed", message: errorText(err) });
          }
        });
        navGroup.append(finishBtn);
      }

      footer.append(cancelBtn, navGroup);
    }

    renderCurrentStep();
  }

  // --- tab: guests -------------------------------------------------------

  function buildGuests(): HTMLElement {
    const layout = el("div", { class: "flex min-h-0 flex-col gap-3" });
    if (!state) {
      layout.append(el("div", { class: "text-neutral-600", text: "loading…" }));
      return layout;
    }

    const guestToolbar = el("div", {
      class: "flex flex-wrap items-center justify-between gap-3 rounded-lg border border-white/10 bg-white/[0.02] p-2.5",
    });
    const actionsGroup = el("div", { class: "flex items-center gap-2" });
    const createVmBtn = button(
      "+ Create Virtual Machine",
      "border-emerald-600 bg-emerald-500/20 font-semibold text-emerald-300 hover:border-emerald-500 hover:bg-emerald-500/30 px-3 py-1.5 shadow-sm text-xs",
      () => openVmWizard(),
    );
    const createCtBtn = button(
      "+ Create LXC Container",
      "border-sky-600 bg-sky-500/20 font-semibold text-sky-300 hover:border-sky-500 hover:bg-sky-500/30 px-3 py-1.5 shadow-sm text-xs",
      () => openCtWizard(),
    );
    actionsGroup.append(createVmBtn, createCtBtn);

    const statsInfo = el("div", { class: "flex items-center gap-3 text-xs text-neutral-400" });
    const vmCount = state.statuses.filter((s) => s.kind === "vm").length;
    const ctCount = state.statuses.filter((s) => s.kind === "ct").length;
    const runningCount = state.statuses.filter((s) => s.state === "running").length;
    statsInfo.append(
      el("span", { text: `${vmCount} VM(s) · ${ctCount} CT(s)` }),
      el("span", { class: "text-neutral-600", text: "·" }),
      el("span", { class: "text-emerald-400 font-medium", text: `${runningCount} running` }),
    );

    guestToolbar.append(actionsGroup, statsInfo);
    layout.append(guestToolbar);

    if (state.statuses.length === 0) {
      layout.append(
        emptyNote("No virtual machines or containers yet. Click '+ Create Virtual Machine' or '+ Create LXC Container' above to get started."),
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

    const wizardBanner = el("div", {
      class: "col-span-full flex flex-wrap items-center justify-between gap-3 p-3.5 rounded-lg border border-emerald-500/30 bg-emerald-950/20 text-neutral-200",
    });
    wizardBanner.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-emerald-300 text-sm", text: "Proxmox-Style Step-by-Step Creation Wizard" }),
        el("span", { class: "text-xs text-neutral-400 mt-0.5", text: "Guided step-by-step setup for node, cloud image catalog, vCPUs, SSD TRIM discard, and cloud-init." }),
      ]),
      button(
        "Launch Creation Wizard Modal",
        "border-emerald-600 bg-emerald-500/25 font-semibold text-emerald-300 hover:bg-emerald-500/35 px-4 py-1.5 shadow-sm text-xs",
        () => openVmWizard(),
      ),
    );
    layout.append(wizardBanner);

    layout.append(vmImagePicker());

    const form = el("section", { class: "panel" });
    form.append(sectionTitle("virtual machine"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3 xl:grid-cols-4" });
    const bridges = bridgeOptions();

    grid.append(
       field("id", textInput(vm.id, (value) => { vm.id = value; }, { autocomplete: "off", placeholder: "(auto)" })),
       field("hostname", textInput(vm.cloud_init.hostname, (value) => { vm.cloud_init.hostname = value; }, { autocomplete: "off" })),
       field("image / iso path", textInput(vm.image ?? "", (value) => { vm.image = value.trim() === "" ? null : value; }, { autocomplete: "off" })),
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
      nullableText(ci.address, (value) => { ci.address = value; }, { placeholder: addressHint(), autocomplete: "off" }),
    );
    const vmGatewayField = field("gateway", textInput(ci.gateway, (value) => { ci.gateway = value; }, { autocomplete: "off" }));
    const vmDnsField = field("DNS servers", listInput(ci.dns, (values) => { ci.dns = values; }, { autocomplete: "off" }));
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
       field("username", textInput(ci.username, (value) => { ci.username = value; }, { autocomplete: "off" })),
       field("password", textInput(ci.password, (value) => { ci.password = value; }, { type: "password", autocomplete: "new-password" })),
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
        void run(async () => {
          const report = await virtApi.createVm(cloneVm(vm));
          if (report.id) vmDraft.id = report.id;
          return report;
        }, `creating ${vm.cloud_init.hostname || "vm"}`);
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
    const grid = el("div", { class: "grid max-h-[60vh] grid-cols-1 gap-2 overflow-auto px-3 pb-3 lg:grid-cols-2" });
    const updateGrid = () => {
      grid.replaceChildren();
      const images = vmImages();
      if (images.length === 0) {
        grid.append(emptyNote("no matching images"));
      }
      for (const image of images) {
        grid.append(imageCard(image, vmDraft.image === image.id, () => {
          applyVmImage(image);
        }));
      }
    };
    updateGrid();

    const bar = el("div", { class: "flex flex-wrap items-center gap-2 px-3 py-2" });
    bar.append(
      textInput(vmFilter, (value) => {
        vmFilter = value;
        updateGrid();
      }, { placeholder: "search images…", class: "w-48", autocomplete: "off" }),
    );
    section.append(bar, grid);
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

    const wizardBanner = el("div", {
      class: "col-span-full flex flex-wrap items-center justify-between gap-3 p-3.5 rounded-lg border border-sky-500/30 bg-sky-950/20 text-neutral-200",
    });
    wizardBanner.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-sky-300 text-sm", text: "Proxmox-Style Step-by-Step Container Wizard" }),
        el("span", { class: "text-xs text-neutral-400 mt-0.5", text: "Guided step-by-step setup for container hostname, templates, mounts, and network." }),
      ]),
      button(
        "Launch Container Wizard Modal",
        "border-sky-600 bg-sky-500/25 font-semibold text-sky-300 hover:bg-sky-500/35 px-4 py-1.5 shadow-sm text-xs",
        () => openCtWizard(),
      ),
    );
    layout.append(wizardBanner);

    layout.append(ctTemplatePicker());

    const form = el("section", { class: "panel" });
    form.append(sectionTitle("lxc container"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3 xl:grid-cols-4" });
    const bridges = bridgeOptions();

    const ctAddressField = field(
      "address (cidr)",
      nullableText(ct.address, (value) => { ct.address = value; }, { placeholder: addressHint(), autocomplete: "off" }),
    );
    const ctGatewayField = field("gateway", textInput(ct.gateway, (value) => { ct.gateway = value; }, { autocomplete: "off" }));
    const ctDnsField = field("DNS servers", listInput(ct.dns, (values) => { ct.dns = values; }, { autocomplete: "off" }));
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
       field("id", textInput(ct.id, (value) => { ct.id = value; }, { autocomplete: "off", placeholder: "(auto)" })),
       field("hostname", textInput(ct.hostname, (value) => { ct.hostname = value; }, { autocomplete: "off" })),
       field("template", textInput(ct.template, (value) => { ct.template = value; }, { autocomplete: "off" })),
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
       field("root password", textInput(ct.password, (value) => { ct.password = value; }, { type: "password", autocomplete: "new-password" })),
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
        void run(async () => {
          const report = await virtApi.createCt(cloneCt(ct));
          if (report.id) ctDraft.id = report.id;
          return report;
        }, `creating ${ct.hostname || "container"}`);
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
    const grid = el("div", { class: "grid max-h-[60vh] grid-cols-1 gap-2 overflow-auto px-3 pb-3 lg:grid-cols-2" });
    const updateGrid = () => {
      grid.replaceChildren();
      const templates = lxcTemplates();
      if (templates.length === 0) {
        grid.append(emptyNote("no matching templates"));
      }
      for (const image of templates) {
        grid.append(imageCard(image, ctDraft.template === image.id, () => {
          applyCtTemplate(image);
        }));
      }
    };
    updateGrid();

    const bar = el("div", { class: "flex flex-wrap items-center gap-2 px-3 py-2" });
    bar.append(
      textInput(ctFilter, (value) => {
        ctFilter = value;
        updateGrid();
      }, { placeholder: "search templates…", class: "w-48", autocomplete: "off" }),
    );
    section.append(bar, grid);
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
      class: `group flex flex-col items-start gap-1 border px-2 py-1.5 text-left transition-colors ${
        selected
          ? "border-emerald-600 bg-emerald-950/30"
          : "border-white/5 hover:border-neutral-500"
      }`,
    });
    const top = el("div", { class: "flex w-full items-baseline gap-2" });
    top.append(
      el("span", { text: image.icon || "💿", title: "image icon" }),
      el("span", { class: "font-semibold text-neutral-200", text: image.name }),
      el("span", { class: "tabular ml-auto text-[10px] text-neutral-600", text: image.category }),
    );
    // A rolling release cannot be checksum-pinned, so it is badged as unverified
    // rather than failing the download with a checksum it can never match.
    if (image.verified === false) {
      top.append(
        el("span", {
          class:
            "rounded-full border border-amber-500/40 bg-amber-500/10 px-1.5 text-[9px] font-medium uppercase tracking-wide text-amber-400 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100",
          text: "⚠",
          title: "Unverified image: no pinned checksum",
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
          text: "# enter a hostname to preview before generating configuration",
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

    if (vmPreview && vmPreview.uuid) {
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
    if (!vmDraft.cloud_init.hostname.trim()) {
      setStatus("enter a hostname to preview", "error");
      vmPreview = {
        qemu: "# enter a hostname to preview",
        uuid: "",
        vnc: null,
        user_data: null,
        meta_data: null,
        network_config: null,
      };
      ctPreview = null;
      previewFile = 0;
      busy = false;
      activeTab = "preview";
      renderTabs();
      return;
    }
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
    if (!ctDraft.hostname.trim()) {
      setStatus("enter a hostname to preview", "error");
      ctPreview = {
        lxc: "# enter a hostname to preview",
        resolv_conf: null,
      };
      vmPreview = null;
      previewFile = 0;
      busy = false;
      activeTab = "preview";
      renderTabs();
      return;
    }
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

  let pollTimer: number | null = null;
  const startPolling = () => {
    if (pollTimer !== null) return;
    pollTimer = window.setInterval(() => {
      if (root.style.display !== "none" && !busy && activeTab === "guests" && !document.hidden) {
        void load();
      }
    }, REFRESH_MS);
  };

  const stopPolling = () => {
    if (pollTimer !== null) {
      window.clearInterval(pollTimer);
      pollTimer = null;
    }
  };

  return {
    root,
    onShow: () => {
      startPolling();
      void load();
    },
    onHide: () => {
      stopPolling();
    },
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
  options: { placeholder?: string; autocomplete?: string } = {},
): HTMLInputElement {
  return textInput(
    value ?? "",
    (raw) => {
      const trimmed = raw.trim();
      onChange(trimmed === "" ? null : raw);
    },
    { autocomplete: "off", ...options },
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
