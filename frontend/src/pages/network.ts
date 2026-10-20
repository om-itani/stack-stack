/**
 * Router page: interfaces, firewall, DHCP, DNS, shaping, tunnel, certificates.
 *
 * The page keeps a mutable *draft* of the configuration. Text fields write
 * straight into the draft (so typing never loses focus); only structural
 * changes — adding or removing a row — re-render. "Save" persists, "Preview"
 * renders every generated file, and "Apply" pushes it to the system.
 *
 * The firewall is reversible: after an apply the server holds the previous
 * ruleset for a short window and a countdown is shown until it is confirmed.
 */

import { networkApi } from "../api";
import type {
  ApiFailure,
  ApplyReport,
  CertInfo,
  CertMode,
  FirewallAction,
  InterfaceInfo,
  InterfaceRole,
  NetworkConfig,
  NetworkState,
  NetworkStatus,
  PendingInfo,
  PreviewBundle,
  Problem,
} from "../api";
import {
  button,
  checkbox,
  el,
  field,
  listInput,
  numberInput,
  select,
  tableEditor,
  textInput,
} from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

type Tab = {
  id: string;
  label: string;
  build: () => HTMLElement;
};

const ROLE_OPTIONS: [string, string][] = [
  ["unused", "unused"],
  ["lan", "lan"],
  ["wan", "wan"],
];

const ACTION_OPTIONS: [string, string][] = [
  ["accept", "accept"],
  ["drop", "drop"],
  ["reject", "reject"],
];

const PROTOCOL_OPTIONS: [string, string][] = [
  ["tcp", "tcp"],
  ["udp", "udp"],
  ["icmp", "icmp"],
  ["any", "any"],
];

const CERT_MODE_OPTIONS: [string, string][] = [
  ["none", "none (TLS terminated upstream)"],
  ["self-signed", "self-signed (generated locally)"],
  ["imported", "imported PEM pair"],
];

export function createNetworkPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-col gap-3 p-3" });

  let draft: NetworkConfig | null = null;
  let discovered: InterfaceInfo[] = [];
  let status: NetworkStatus | null = null;
  let certificates: CertInfo[] = [];
  let problems: Problem[] = [];
  let configError: string | null = null;
  let pending: PendingInfo | null = null;
  let pendingSince = 0;
  let dirty = false;
  let busy = false;
  let activeTab = "interfaces";
  let preview: PreviewBundle | null = null;
  let previewFile: keyof PreviewBundle = "nftables";
  let report: ApplyReport | null = null;

  // --- shell -------------------------------------------------------------
  const heading = el("span", { class: "text-slate-300", text: "network" });
  const dirtyBadge = el("span", { class: "tabular text-[10px] text-amber-400" });
  const statusLine = el("span", { class: "tabular truncate text-[11px] text-slate-500" });
  const badges = el("span", { class: "flex flex-wrap items-center gap-2" });

  const saveButton = button("save");
  const previewButton = button("preview");
  const applyButton = button("apply", "text-emerald-400 hover:border-emerald-500");
  const confirmButton = button("confirm firewall", "text-amber-400 hover:border-amber-500");
  const reloadButton = button("refresh");

  const header = el("div", { class: "flex flex-wrap items-center gap-3" }, [
    heading,
    dirtyBadge,
    saveButton,
    previewButton,
    applyButton,
    confirmButton,
    reloadButton,
    el("span", { class: "ml-auto" }, [badges]),
    statusLine,
  ]);

  const noticeBox = el("div", { class: "flex flex-col gap-1" });
  const pendingBox = el("div", {
    class:
      "border border-amber-700 bg-amber-950/40 px-3 py-2 text-[11px] text-amber-300",
  });
  const tabBar = el("div", { class: "flex flex-wrap gap-1 border-b border-[#1e2733] pb-1" });
  const tabBody = el("div", { class: "min-h-0 flex-1" });

  root.append(header, noticeBox, pendingBox, tabBar, tabBody);

  const tabs: Tab[] = [
    { id: "interfaces", label: "interfaces", build: buildInterfaces },
    { id: "firewall", label: "firewall", build: buildFirewall },
    { id: "dhcp", label: "dhcp", build: buildDhcp },
    { id: "dns", label: "dns", build: buildDns },
    { id: "sqm", label: "sqm", build: buildSqm },
    { id: "tunnel", label: "tunnel", build: buildTunnel },
    { id: "certs", label: "certificates", build: buildCerts },
    { id: "preview", label: "preview", build: buildPreview },
    { id: "status", label: "status", build: buildStatus },
  ];

  // --- data --------------------------------------------------------------

  function markDirty(): void {
    dirty = true;
    renderChrome();
  }

  async function load(): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    setStatus("loading…", "muted");
    try {
      const state = await networkApi.state();
      applyState(state);
      setStatus("", "muted");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
      renderAll();
    }
  }

  function applyState(state: NetworkState): void {
    draft = state.config;
    discovered = state.interfaces;
    status = state.status;
    certificates = state.certificates;
    problems = state.problems;
    configError = state.config_error;
    pending = state.pending;
    pendingSince = Date.now();
    report = null;
    preview = null;
    dirty = false;
  }

  async function save(): Promise<void> {
    if (!draft || busy) {
      return;
    }
    busy = true;
    setStatus("saving…", "muted");
    try {
      applyState(await networkApi.save(draft));
      setStatus("saved", "ok");
    } catch (err) {
      showProblems(err);
    } finally {
      busy = false;
      renderAll();
    }
  }

  async function doPreview(): Promise<void> {
    if (!draft || busy) {
      return;
    }
    busy = true;
    setStatus("rendering…", "muted");
    try {
      preview = await networkApi.preview(draft);
      activeTab = "preview";
      setStatus("", "muted");
    } catch (err) {
      showProblems(err);
    } finally {
      busy = false;
      renderAll();
    }
  }

  async function apply(): Promise<void> {
    if (!draft || busy) {
      return;
    }
    if (!window.confirm("Apply this configuration to the system now?")) {
      return;
    }
    busy = true;
    setStatus("applying…", "muted");
    try {
      report = await networkApi.apply(draft);
      dirty = false;
      const failed = report.steps.filter((step) => !step.ok).length;
      setStatus(failed === 0 ? "applied" : `${failed} subsystem(s) failed`, failed === 0 ? "ok" : "error");
      if (report.pending) {
        pending = report.pending;
        pendingSince = Date.now();
      } else {
        pending = null;
      }
    } catch (err) {
      showProblems(err);
    } finally {
      busy = false;
      renderAll();
    }
  }

  async function confirm(): Promise<void> {
    if (busy) {
      return;
    }
    busy = true;
    try {
      await networkApi.confirm();
      pending = null;
      setStatus("firewall confirmed", "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
      // The window may have closed on its own; refresh to see the truth.
      await load();
    } finally {
      busy = false;
      renderAll();
    }
  }

  function showProblems(err: unknown): void {
    const failure = err as ApiFailure;
    if (Array.isArray(failure.problems) && failure.problems.length > 0) {
      problems = failure.problems;
      setStatus(`${failure.problems.length} problem(s)`, "error");
    } else {
      setStatus(errorText(err), "error");
    }
  }

  function setStatus(text: string, tone: "muted" | "error" | "ok"): void {
    statusLine.textContent = text;
    statusLine.className =
      tone === "error"
        ? "tabular truncate text-[11px] text-rose-400"
        : tone === "ok"
          ? "tabular truncate text-[11px] text-emerald-400"
          : "tabular truncate text-[11px] text-slate-500";
  }

  // --- chrome ------------------------------------------------------------

  function renderChrome(): void {
    dirtyBadge.textContent = dirty ? "unsaved changes" : "";
    confirmButton.style.display = pending ? "" : "none";

    badges.replaceChildren();
    if (configError) {
      badges.append(el("span", { class: "text-[10px] text-rose-400", text: "config file unreadable" }));
    }
    if (status) {
      for (const service of status.services) {
        badges.append(
          el("span", {
            class: `tabular text-[10px] ${service.active ? "text-emerald-500" : "text-slate-600"}`,
            text: `${service.name}:${service.state}`,
            title: service.unit,
          }),
        );
      }
      badges.append(
        el("span", {
          class: `tabular text-[10px] ${status.firewall.loaded ? "text-emerald-500" : "text-slate-600"}`,
          text: `nft:${status.firewall.rules}`,
          title: status.firewall.detail,
        }),
      );
      badges.append(
        el("span", {
          class: `tabular text-[10px] ${status.sqm.active ? "text-emerald-500" : "text-slate-600"}`,
          text: status.sqm.active ? "sqm:on" : "sqm:off",
          title: status.sqm.detail,
        }),
      );
    }

    noticeBox.replaceChildren();
    if (configError) {
      noticeBox.append(
        el("div", {
          class: "border border-rose-800 bg-rose-950/40 px-3 py-2 text-[11px] text-rose-300",
          text: configError,
        }),
      );
    }
    if (problems.length > 0) {
      const list = el("div", {
        class: "border border-amber-800 bg-amber-950/30 px-3 py-2 text-[11px] text-amber-300",
      });
      list.append(el("div", { class: "font-semibold", text: "validation problems" }));
      for (const problem of problems.slice(0, 12)) {
        list.append(el("div", { text: `${problem.field}: ${problem.message}` }));
      }
      if (problems.length > 12) {
        list.append(el("div", { text: `…and ${problems.length - 12} more` }));
      }
      noticeBox.append(list);
    }
    if (report) {
      const panel = el("div", { class: "flex flex-col gap-0.5 border border-[#1e2733] px-3 py-2 text-[11px]" });
      panel.append(el("div", { class: "font-semibold text-slate-300", text: "apply result" }));
      for (const step of report.steps) {
        panel.append(
          el("div", { class: step.ok ? "text-emerald-400" : "text-rose-400" }, [
            el("span", { class: "tabular", text: `${step.subsystem} ` }),
            el("span", { class: "text-slate-500", text: step.detail }),
          ]),
        );
      }
      noticeBox.append(panel);
    }

    // Pending firewall window with a live countdown.
    pendingBox.replaceChildren();
    if (pending) {
      const elapsed = Math.floor((Date.now() - pendingSince) / 1000);
      const remaining = Math.max(0, pending.expires_in_s - elapsed);
      pendingBox.style.display = "";
      pendingBox.replaceChildren(
        el("span", { text: "firewall change is live and reversible: " }),
        el("span", { class: "tabular font-semibold", text: `${remaining}s` }),
        el("span", {
          text: " remaining before the previous ruleset is restored. Confirm if the box is still reachable.",
        }),
      );
      if (remaining === 0 && !busy) {
        // The server has rolled back; refresh once so the UI stops claiming
        // there is anything pending.
        pending = null;
        void load();
      }
    } else {
      pendingBox.style.display = "none";
    }
  }

  function renderTabs(): void {
    tabBar.replaceChildren();
    for (const tab of tabs) {
      const active = tab.id === activeTab;
      tabBar.append(
        button(tab.label, active ? "border-emerald-600 text-emerald-400" : "", () => {
          activeTab = tab.id;
          renderAll();
        }),
      );
    }

    const tab = tabs.find((entry) => entry.id === activeTab) ?? tabs[0];
    tabBody.replaceChildren(tab.build());
  }

  function renderAll(): void {
    if (!draft) {
      tabBody.replaceChildren(el("div", { class: "text-slate-600", text: "loading…" }));
      renderChrome();
      return;
    }
    renderChrome();
    renderTabs();
  }

  const rerender = () => renderAll();

  // --- tab: interfaces ---------------------------------------------------

  function buildInterfaces(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const config = draft;

    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    // Configured interfaces.
    const interfacesSection = el("section", { class: "panel" });
    interfacesSection.append(sectionTitle("interfaces (role + address)"));
    const nameList = el("datalist", { id: "net-iface-names" });
    for (const iface of discovered) {
      nameList.append(el("option", { value: iface.name }));
    }
    interfacesSection.append(nameList);
    interfacesSection.append(
      tableEditor<NetworkConfig["interfaces"][number]>(
        config.interfaces,
        [
          {
            header: "name",
            cell: (row) =>
              el("input", {
                class:
                  "border border-[#1e2733] bg-[#0b0f14] px-1.5 py-0.5 text-slate-200 outline-none focus:border-slate-500",
                value: row.name,
                list: "net-iface-names",
                oninput: (event: Event) => {
                  row.name = (event.target as HTMLInputElement).value;
                  markDirty();
                },
              }),
          },
          {
            header: "role",
            cell: (row) =>
              select(row.role, ROLE_OPTIONS, (value) => {
                row.role = value as InterfaceRole;
                markDirty();
              }),
          },
          {
            header: "address (ip/prefix, blank = dhcp)",
            cell: (row) =>
              nullableText(row.address, (value) => {
                row.address = value;
                markDirty();
              }),
          },
        ],
        () => ({ name: "", role: "unused" as InterfaceRole, address: null }),
        rerender,
        "add interface",
      ),
    );
    layout.append(interfacesSection);

    // Bridges.
    const bridgeSection = el("section", { class: "panel" });
    bridgeSection.append(sectionTitle("bridges"));
    bridgeSection.append(
      tableEditor<NetworkConfig["bridges"][number]>(
        config.bridges,
        [
          {
            header: "name",
            cell: (row) => textInput(row.name, (value) => { row.name = value; markDirty(); }),
          },
          {
            header: "members (comma separated)",
            cell: (row) =>
              listInput(row.members, (values) => {
                row.members = values;
                markDirty();
              }),
          },
          {
            header: "address",
            cell: (row) =>
              nullableText(row.address, (value) => {
                row.address = value;
                markDirty();
              }),
          },
          {
            header: "stp",
            cell: (row) =>
              checkbox(row.stp, (value) => {
                row.stp = value;
                markDirty();
              }),
          },
        ],
        () => ({ name: "br0", members: [], address: null, stp: false, comment: "" }),
        rerender,
        "add bridge",
      ),
    );
    layout.append(bridgeSection);

    // VLANs.
    const vlanSection = el("section", { class: "panel" });
    vlanSection.append(sectionTitle("vlans (802.1q)"));
    vlanSection.append(
      tableEditor<NetworkConfig["vlans"][number]>(
        config.vlans,
        [
          { header: "name", cell: (row) => textInput(row.name, (value) => { row.name = value; markDirty(); }) },
          { header: "parent", cell: (row) => textInput(row.parent, (value) => { row.parent = value; markDirty(); }) },
          {
            header: "id",
            cell: (row) =>
              numberInput(row.id, (value) => {
                row.id = value;
                markDirty();
              }, { min: 1, max: 4094, class: "w-20" }),
          },
          {
            header: "address",
            cell: (row) =>
              nullableText(row.address, (value) => {
                row.address = value;
                markDirty();
              }),
          },
        ],
        () => ({ name: "", parent: "", id: 0, address: null, comment: "" }),
        rerender,
        "add vlan",
      ),
    );
    layout.append(vlanSection);

    // WiFi access point.
    const wifi = config.wifi;
    const wifiSection = el("section", { class: "panel" });
    wifiSection.append(sectionTitle("wifi access point"));
    const wifiGrid = el("div", { class: "grid grid-cols-2 gap-2 p-3" });
    wifiGrid.append(
      el("div", { class: "col-span-2" }, [
        checkbox(wifi.enabled, (value) => { wifi.enabled = value; markDirty(); }, "enable hotspot"),
      ]),
      field("radio interface", textInput(wifi.interface, (value) => { wifi.interface = value; markDirty(); })),
      field("ssid", textInput(wifi.ssid, (value) => { wifi.ssid = value; markDirty(); })),
      field("passphrase", textInput(wifi.passphrase, (value) => { wifi.passphrase = value; markDirty(); }, { type: "password" })),
      field("security", select(wifi.security, [["wpa2", "wpa2"], ["wpa3", "wpa3"], ["open", "open"]], (value) => { wifi.security = value; markDirty(); })),
      field("band", select(wifi.band, [["2g", "2.4 GHz"], ["5g", "5 GHz"]], (value) => { wifi.band = value; markDirty(); })),
      field("channel (0 = auto)", numberInput(wifi.channel, (value) => { wifi.channel = value; markDirty(); }, { min: 0, max: 196 })),
      field("country", textInput(wifi.country, (value) => { wifi.country = value; markDirty(); })),
      field("bridge", textInput(wifi.bridge, (value) => { wifi.bridge = value; markDirty(); })),
    );
    wifiSection.append(wifiGrid);
    layout.append(wifiSection);

    // Discovered hardware, for reference.
    const liveSection = el("section", { class: "panel xl:col-span-2" });
    liveSection.append(sectionTitle("detected on this host"));
    if (discovered.length === 0) {
      liveSection.append(
        el("div", {
          class: "px-3 py-2 text-slate-600",
          text: "no interfaces detected (this host has no /sys/class/net or no permissions)",
        }),
      );
    } else {
      const table = el("div", { class: "overflow-x-auto" });
      const node = document.createElement("table");
      node.className = "w-full text-[11px]";
      node.append(
        el("thead", {}, [
          el("tr", {}, ["name", "kind", "state", "mac", "mtu", "speed", "addresses", "wifi"].map((header) =>
            el("th", {
              class:
                "border-b border-[#1e2733] px-2 py-1 text-left text-[10px] uppercase tracking-wide text-slate-600",
              text: header,
            }),
          )),
        ]),
      );
      const body = document.createElement("tbody");
      for (const iface of discovered) {
        const wifiText = iface.wifi
          ? `ssid ${iface.wifi.ssid ?? "—"} · ${iface.wifi.mode ?? "?"} · ch ${iface.wifi.channel ?? "?"}${iface.wifi.ap_capable ? " · AP" : ""}`
          : "";
        body.append(
          el("tr", {}, [
            el("td", { class: "border-b border-[#161d27] px-2 py-1 text-slate-300", text: iface.name }),
            el("td", { class: "border-b border-[#161d27] px-2 py-1 text-slate-500", text: iface.kind }),
            el("td", {
              class: `border-b border-[#161d27] px-2 py-1 ${iface.operstate === "up" ? "text-emerald-400" : "text-slate-500"}`,
              text: iface.operstate,
            }),
            el("td", { class: "border-b border-[#161d27] px-2 py-1 text-slate-500", text: iface.mac ?? "—" }),
            el("td", { class: "border-b border-[#161d27] px-2 py-1 text-slate-500", text: iface.mtu === null ? "—" : String(iface.mtu) }),
            el("td", { class: "border-b border-[#161d27] px-2 py-1 text-slate-500", text: iface.speed_mbps === null ? "—" : `${iface.speed_mbps}M` }),
            el("td", {
              class: "border-b border-[#161d27] px-2 py-1 text-slate-500",
              text: iface.addresses.map((address) => address.cidr).join(", ") || "—",
            }),
            el("td", { class: "border-b border-[#161d27] px-2 py-1 text-slate-500", text: wifiText }),
          ]),
        );
      }
      node.append(body);
      table.append(node);
      liveSection.append(table);
    }
    layout.append(liveSection);

    return layout;
  }

  // --- tab: firewall -----------------------------------------------------

  function buildFirewall(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const firewall = draft.firewall;
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    const policy = el("section", { class: "panel" });
    policy.append(sectionTitle("policy"));
    const policyGrid = el("div", { class: "grid grid-cols-2 gap-2 p-3" });
    policyGrid.append(
      field(
        "default input policy",
        select(firewall.default_input, ACTION_OPTIONS, (value) => {
          firewall.default_input = value as FirewallAction;
          markDirty();
        }),
      ),
      field(
        "management ports",
        listInput(firewall.management_ports.map(String), (values) => {
          firewall.management_ports = values.map((value) => Number.parseInt(value, 10) || 0);
          markDirty();
        }, { numeric: true }),
      ),
      el("div", { class: "col-span-2 flex gap-4" }, [
        checkbox(firewall.allow_established, (value) => { firewall.allow_established = value; markDirty(); }, "allow established/related"),
        checkbox(firewall.allow_icmp, (value) => { firewall.allow_icmp = value; markDirty(); }, "allow ICMP"),
      ]),
    );
    policy.append(policyGrid);
    layout.append(policy);

    const forwardsSection = el("section", { class: "panel" });
    forwardsSection.append(sectionTitle("port forwards (dnat)"));
    forwardsSection.append(
      tableEditor<NetworkConfig["port_forwards"][number]>(
        draft.port_forwards,
        [
          { header: "id", cell: (row) => textInput(row.id, (value) => { row.id = value; markDirty(); }) },
          {
            header: "proto",
            cell: (row) =>
              select(row.protocol, [["tcp", "tcp"], ["udp", "udp"]], (value) => {
                row.protocol = value;
                markDirty();
              }),
          },
          { header: "ext", cell: (row) => numberInput(row.external_port, (value) => { row.external_port = value; markDirty(); }, { min: 1, max: 65535, class: "w-20" }) },
          { header: "to ip", cell: (row) => textInput(row.internal_ip, (value) => { row.internal_ip = value; markDirty(); }) },
          { header: "int", cell: (row) => numberInput(row.internal_port, (value) => { row.internal_port = value; markDirty(); }, { min: 1, max: 65535, class: "w-20" }) },
          { header: "comment", cell: (row) => textInput(row.comment, (value) => { row.comment = value; markDirty(); }) },
          { header: "on", cell: (row) => checkbox(row.enabled, (value) => { row.enabled = value; markDirty(); }) },
        ],
        () => ({ id: "", protocol: "tcp", external_port: 0, internal_ip: "", internal_port: 0, comment: "", enabled: true }),
        rerender,
        "add forward",
      ),
    );
    layout.append(forwardsSection);

    const rulesSection = el("section", { class: "panel xl:col-span-2" });
    rulesSection.append(sectionTitle("input rules"));
    rulesSection.append(
      tableEditor<NetworkConfig["firewall"]["rules"][number]>(
        firewall.rules,
        [
          { header: "id", cell: (row) => textInput(row.id, (value) => { row.id = value; markDirty(); }) },
          {
            header: "action",
            cell: (row) =>
              select(row.action, ACTION_OPTIONS, (value) => {
                row.action = value as FirewallAction;
                markDirty();
              }),
          },
          {
            header: "proto",
            cell: (row) =>
              select(row.protocol, PROTOCOL_OPTIONS, (value) => {
                row.protocol = value;
                markDirty();
              }),
          },
          {
            header: "port",
            cell: (row) =>
              nullableNumber(row.port, (value) => {
                row.port = value;
                markDirty();
              }),
          },
          {
            header: "source (blank = LAN)",
            cell: (row) =>
              nullableText(row.source, (value) => {
                row.source = value;
                markDirty();
              }),
          },
          { header: "comment", cell: (row) => textInput(row.comment, (value) => { row.comment = value; markDirty(); }) },
          { header: "on", cell: (row) => checkbox(row.enabled, (value) => { row.enabled = value; markDirty(); }) },
        ],
        () => ({ id: "", action: "accept" as FirewallAction, protocol: "tcp", port: null, source: null, comment: "", enabled: true }),
        rerender,
        "add rule",
      ),
    );
    layout.append(rulesSection);

    return layout;
  }

  // --- tab: dhcp ---------------------------------------------------------

  function buildDhcp(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const dhcp = draft.dhcp;
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    const main = el("section", { class: "panel" });
    main.append(sectionTitle("dhcp server"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3" });
    grid.append(
      el("div", { class: "col-span-2" }, [
        checkbox(dhcp.enabled, (value) => { dhcp.enabled = value; markDirty(); }, "serve DHCP"),
      ]),
      field("interface", textInput(dhcp.interface, (value) => { dhcp.interface = value; markDirty(); })),
      field("domain", textInput(dhcp.domain, (value) => { dhcp.domain = value; markDirty(); })),
      field("range start", textInput(dhcp.range_start, (value) => { dhcp.range_start = value; markDirty(); })),
      field("range end", textInput(dhcp.range_end, (value) => { dhcp.range_end = value; markDirty(); })),
      field("gateway", textInput(dhcp.gateway, (value) => { dhcp.gateway = value; markDirty(); })),
      field("lease", textInput(dhcp.lease, (value) => { dhcp.lease = value; markDirty(); })),
      field("dns servers", listInput(dhcp.dns_servers, (values) => { dhcp.dns_servers = values; markDirty(); })),
    );
    main.append(grid);
    layout.append(main);

    const reservations = el("section", { class: "panel" });
    reservations.append(sectionTitle("static reservations"));
    reservations.append(
      tableEditor<NetworkConfig["dhcp"]["reservations"][number]>(
        dhcp.reservations,
        [
          { header: "mac", cell: (row) => textInput(row.mac, (value) => { row.mac = value; markDirty(); }) },
          { header: "ip", cell: (row) => textInput(row.ip, (value) => { row.ip = value; markDirty(); }) },
          { header: "name", cell: (row) => textInput(row.name, (value) => { row.name = value; markDirty(); }) },
        ],
        () => ({ mac: "", ip: "", name: "" }),
        rerender,
        "add reservation",
      ),
    );
    layout.append(reservations);

    return layout;
  }

  // --- tab: dns ----------------------------------------------------------

  function buildDns(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const dns = draft.dns;
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    const server = el("section", { class: "panel" });
    server.append(sectionTitle("unbound resolver"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3" });
    grid.append(
      el("div", { class: "col-span-2" }, [
        checkbox(dns.enabled, (value) => { dns.enabled = value; markDirty(); }, "enable unbound"),
      ]),
      field("listen addresses", listInput(dns.listen, (values) => { dns.listen = values; markDirty(); })),
      field("upstreams (blank = full recursion)", listInput(dns.upstreams, (values) => { dns.upstreams = values; markDirty(); })),
      field("cache min ttl", numberInput(dns.cache_min_ttl, (value) => { dns.cache_min_ttl = value; markDirty(); }, { min: 0 })),
      field("cache max ttl", numberInput(dns.cache_max_ttl, (value) => { dns.cache_max_ttl = value; markDirty(); })),
      field("cache size (MiB)", numberInput(dns.cache_size_mb, (value) => { dns.cache_size_mb = value; markDirty(); }, { min: 1 })),
      field("threads", numberInput(dns.threads, (value) => { dns.threads = value; markDirty(); }, { min: 1, max: 32 })),
      field("blocklist file (optional)", nullableText(dns.blocklist_file, (value) => { dns.blocklist_file = value; markDirty(); })),
      el("div", { class: "col-span-2 flex gap-4" }, [
        checkbox(dns.dnssec, (value) => { dns.dnssec = value; markDirty(); }, "DNSSEC validation"),
        checkbox(dns.qname_minimisation, (value) => { dns.qname_minimisation = value; markDirty(); }, "QNAME minimisation"),
      ]),
    );
    server.append(grid);
    layout.append(server);

    const rewrites = el("section", { class: "panel" });
    rewrites.append(sectionTitle("local rewrites (*.domain at the apex)"));
    rewrites.append(
      tableEditor<NetworkConfig["dns"]["rewrites"][number]>(
        dns.rewrites,
        [
          { header: "host", cell: (row) => textInput(row.host, (value) => { row.host = value; markDirty(); }, { placeholder: "app.example.com or *.example.com" }) },
          { header: "address", cell: (row) => textInput(row.address, (value) => { row.address = value; markDirty(); }) },
          { header: "comment", cell: (row) => textInput(row.comment, (value) => { row.comment = value; markDirty(); }) },
        ],
        () => ({ host: "", address: "", comment: "" }),
        rerender,
        "add rewrite",
      ),
    );
    layout.append(rewrites);

    const blocked = el("section", { class: "panel xl:col-span-2" });
    blocked.append(sectionTitle("blocked domains (answered NXDOMAIN, subtree included)"));
    const blockedGrid = el("div", { class: "grid grid-cols-1 gap-2 p-3" });
    blockedGrid.append(
      field("domains", listInput(dns.blocked_domains, (values) => { dns.blocked_domains = values; markDirty(); }, { placeholder: "ads.example.com, tracker.example.net" })),
    );
    blocked.append(blockedGrid);
    layout.append(blocked);

    return layout;
  }

  // --- tab: sqm ----------------------------------------------------------

  function buildSqm(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const sqm = draft.sqm;
    const section = el("section", { class: "panel" });
    section.append(sectionTitle("bufferbloat control (cake)"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3 xl:grid-cols-4" });
    grid.append(
      el("div", { class: "col-span-full" }, [
        checkbox(sqm.enabled, (value) => { sqm.enabled = value; markDirty(); }, "enable SQM"),
      ]),
      field("wan interface", textInput(sqm.interface, (value) => { sqm.interface = value; markDirty(); })),
      field("download kbit/s", numberInput(sqm.download_kbps, (value) => { sqm.download_kbps = value; markDirty(); }, { min: 1 })),
      field("upload kbit/s", numberInput(sqm.upload_kbps, (value) => { sqm.upload_kbps = value; markDirty(); }, { min: 1 })),
      field("link layer", select(sqm.link_layer, [["ethernet", "ethernet"], ["atm", "atm"], ["none", "none"]], (value) => { sqm.link_layer = value; markDirty(); })),
      field("overhead (bytes)", numberInput(sqm.overhead, (value) => { sqm.overhead = value; markDirty(); })),
      field("ingress ifb device", textInput(sqm.ifb, (value) => { sqm.ifb = value; markDirty(); })),
      el("div", { class: "col-span-full" }, [
        checkbox(sqm.diffserv4, (value) => { sqm.diffserv4 = value; markDirty(); }, "diffserv4"),
      ]),
    );
    section.append(grid);
    section.append(
      el("div", {
        class: "px-3 pb-3 text-[10px] text-slate-600",
        text: "Set the bandwidth to about 85-95% of what a speed test reports; that is what keeps the queue draining.",
      }),
    );
    return section;
  }

  // --- tab: tunnel -------------------------------------------------------

  function buildTunnel(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const tunnel = draft.tunnel;
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    const main = el("section", { class: "panel" });
    main.append(sectionTitle("cloudflare tunnel"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3" });
    grid.append(
      el("div", { class: "col-span-2 flex gap-4" }, [
        checkbox(tunnel.enabled, (value) => { tunnel.enabled = value; markDirty(); }, "enable cloudflared"),
        checkbox(tunnel.expose_ui, (value) => { tunnel.expose_ui = value; markDirty(); }, "fall back to the stack-stack UI"),
      ]),
      field("tunnel id or name", textInput(tunnel.tunnel_id, (value) => { tunnel.tunnel_id = value; markDirty(); })),
      field("credentials file", textInput(tunnel.credentials_file, (value) => { tunnel.credentials_file = value; markDirty(); })),
    );
    main.append(grid);
    main.append(
      el("div", {
        class: "px-3 pb-3 text-[10px] text-slate-600",
        text: "Cloudflare terminates TLS at the edge, so *.yourdomain over HTTPS needs no local certificate: create the tunnel and add a public hostname per service below.",
      }),
    );
    layout.append(main);

    const ingress = el("section", { class: "panel" });
    ingress.append(sectionTitle("public hostnames"));
    ingress.append(
      tableEditor<NetworkConfig["tunnel"]["ingress"][number]>(
        tunnel.ingress,
        [
          { header: "hostname", cell: (row) => textInput(row.hostname, (value) => { row.hostname = value; markDirty(); }, { placeholder: "app.example.com" }) },
          { header: "service", cell: (row) => textInput(row.service, (value) => { row.service = value; markDirty(); }, { placeholder: "http://192.168.1.10:8080" }) },
          { header: "comment", cell: (row) => textInput(row.comment, (value) => { row.comment = value; markDirty(); }) },
        ],
        () => ({ hostname: "", service: "", comment: "" }),
        rerender,
        "add hostname",
      ),
    );
    layout.append(ingress);

    return layout;
  }

  // --- tab: certificates -------------------------------------------------

  function buildCerts(): HTMLElement {
    if (!draft) {
      return el("div");
    }
    const certs = draft.certs;
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    const config = el("section", { class: "panel" });
    config.append(sectionTitle("certificate settings"));
    const grid = el("div", { class: "grid grid-cols-2 gap-2 p-3" });
    grid.append(
      field(
        "mode",
        select(certs.mode, CERT_MODE_OPTIONS, (value) => {
          certs.mode = value as CertMode;
          markDirty();
        }),
      ),
      field("directory", textInput(certs.directory, (value) => { certs.directory = value; markDirty(); })),
      field("domains", listInput(certs.domains, (values) => { certs.domains = values; markDirty(); }, { placeholder: "example.com, *.example.com" })),
      field("active certificate", nullableText(certs.active, (value) => { certs.active = value; markDirty(); })),
    );
    config.append(grid);

    const actions = el("div", { class: "flex flex-wrap gap-2 px-3 pb-3" });
    const nameInput = textInput("local", () => {});
    actions.append(
      el("span", { class: "flex items-center gap-2" }, [
        el("span", { class: "text-[10px] uppercase text-slate-600", text: "name" }),
        nameInput,
      ]),
      button("generate self-signed", "text-emerald-400 hover:border-emerald-500", () => {
        void generate(nameInput.value);
      }),
      button("import PEM", "", () => {
        void importPair(nameInput.value);
      }),
    );
    config.append(actions);
    config.append(
      el("div", {
        class: "px-3 pb-3 text-[10px] text-slate-600",
        text: "Import accepts a Cloudflare Origin certificate and key (or any PEM pair). Generation needs openssl on the host.",
      }),
    );
    layout.append(config);

    const list = el("section", { class: "panel" });
    list.append(sectionTitle("installed certificates"));
    if (certificates.length === 0) {
      list.append(el("div", { class: "px-3 py-2 text-slate-600", text: "no certificates found" }));
    }
    for (const cert of certificates) {
      const row = el("div", { class: "border-b border-[#161d27] px-3 py-2 text-[11px]" });
      row.append(
        el("div", { class: "flex items-baseline gap-2" }, [
          el("span", { class: "text-slate-200", text: cert.name }),
          cert.active ? el("span", { class: "text-[10px] text-emerald-400", text: "active" }) : el("span"),
          cert.key_matches === false
            ? el("span", { class: "text-[10px] text-rose-400", text: "key mismatch" })
            : el("span"),
          el("span", { class: "ml-auto" }, [
            button("remove", "text-rose-400 hover:border-rose-500", () => {
              void removeCert(cert.name);
            }),
          ]),
        ]),
        el("div", { class: "text-slate-500", text: cert.subject ?? "—" }),
        el("div", { class: "text-slate-600", text: cert.sans.join(", ") || "no SANs" }),
        el("div", {
          class: "text-slate-600",
          text: cert.days_remaining === null
            ? "expiry unknown"
            : `${cert.days_remaining} day(s) remaining (${cert.not_after ?? ""})`,
        }),
      );
      list.append(row);
    }
    layout.append(list);

    return layout;
  }

  // --- tab: preview ------------------------------------------------------

  function buildPreview(): HTMLElement {
    const section = el("section", { class: "panel" });
    section.append(sectionTitle("generated configuration"));
    if (!preview) {
      section.append(
        el("div", {
          class: "px-3 py-2 text-slate-600",
          text: "press preview to render nftables, dnsmasq, unbound, cloudflared and the SQM script",
        }),
      );
      return section;
    }

    const files: (keyof PreviewBundle)[] = ["nftables", "dnsmasq", "unbound", "cloudflared", "sqm"];
    const bar = el("div", { class: "flex flex-wrap gap-1 px-3 py-2" });
    for (const file of files) {
      bar.append(
        button(file, file === previewFile ? "border-emerald-600 text-emerald-400" : "", () => {
          previewFile = file;
          renderAll();
        }),
      );
    }
    section.append(bar, el("pre", {
      class:
        "mt-1 max-h-[60vh] overflow-auto border-t border-[#1e2733] bg-[#0b0f14] p-3 text-[11px] leading-relaxed text-slate-400",
      text: preview[previewFile],
    }));
    return section;
  }

  // --- tab: status -------------------------------------------------------

  function buildStatus(): HTMLElement {
    const layout = el("div", { class: "grid gap-4 xl:grid-cols-2" });

    const services = el("section", { class: "panel" });
    services.append(sectionTitle("daemons"));
    if (!status) {
      services.append(el("div", { class: "px-3 py-2 text-slate-600", text: "no status yet" }));
    } else {
      for (const service of status.services) {
        services.append(
          el("div", { class: "flex items-baseline justify-between border-b border-[#161d27] px-3 py-2 text-[11px]" }, [
            el("span", { class: "text-slate-300", text: service.unit }),
            el("span", {
              class: service.active ? "text-emerald-400" : "text-slate-500",
              text: service.state,
            }),
          ]),
        );
      }
    }
    layout.append(services);

    const runtime = el("section", { class: "panel" });
    runtime.append(sectionTitle("runtime"));
    const body = el("div", { class: "flex flex-col gap-2 px-3 py-2 text-[11px]" });
    body.append(
      el("div", { class: "flex justify-between" }, [
        el("span", { class: "text-slate-500", text: "firewall" }),
        el("span", {
          class: status?.firewall.loaded ? "text-emerald-400" : "text-slate-500",
          text: status?.firewall.detail ?? "unknown",
        }),
      ]),
      el("div", { class: "flex justify-between" }, [
        el("span", { class: "text-slate-500", text: "traffic shaping" }),
        el("span", {
          class: status?.sqm.active ? "text-emerald-400" : "text-slate-500",
          text: status?.sqm.detail ?? "unknown",
        }),
      ]),
    );
    runtime.append(body);
    layout.append(runtime);

    return layout;
  }

  // --- small helpers -----------------------------------------------------

  function nullableText(
    value: string | null,
    onChange: (value: string | null) => void,
  ): HTMLInputElement {
    return textInput(value ?? "", (raw) => {
      const trimmed = raw.trim();
      onChange(trimmed === "" ? null : raw);
    });
  }

  function nullableNumber(
    value: number | null,
    onChange: (value: number | null) => void,
  ): HTMLInputElement {
    return textInput(value === null ? "" : String(value), (raw) => {
      const trimmed = raw.trim();
      if (trimmed === "") {
        onChange(null);
        return;
      }
      const parsed = Number.parseInt(trimmed, 10);
      onChange(Number.isFinite(parsed) ? parsed : null);
    }, { type: "number", class: "w-20" });
  }

  async function generate(name: string): Promise<void> {
    if (!draft || busy) {
      return;
    }
    busy = true;
    setStatus("generating…", "muted");
    try {
      const info = await networkApi.generateCertificate(name || "local");
      certificates = await networkApi.certificates();
      setStatus(`generated ${info.name}`, "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
      renderAll();
    }
  }

  async function importPair(name: string): Promise<void> {
    if (busy) {
      return;
    }
    const cert = window.prompt("Paste the certificate PEM:");
    if (!cert) {
      return;
    }
    const key = window.prompt("Paste the private key PEM:");
    if (!key) {
      return;
    }
    busy = true;
    setStatus("importing…", "muted");
    try {
      const info = await networkApi.importCertificate({
        name: name || "imported",
        cert_pem: cert,
        key_pem: key,
      });
      certificates = await networkApi.certificates();
      setStatus(`imported ${info.name}`, "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
      renderAll();
    }
  }

  async function removeCert(name: string): Promise<void> {
    if (busy) {
      return;
    }
    if (!window.confirm(`Delete certificate ${name}?`)) {
      return;
    }
    busy = true;
    try {
      await networkApi.removeCertificate(name);
      certificates = await networkApi.certificates();
      setStatus(`removed ${name}`, "ok");
    } catch (err) {
      setStatus(errorText(err), "error");
    } finally {
      busy = false;
      renderAll();
    }
  }

  // --- wiring ------------------------------------------------------------

  saveButton.addEventListener("click", () => void save());
  previewButton.addEventListener("click", () => void doPreview());
  applyButton.addEventListener("click", () => void apply());
  confirmButton.addEventListener("click", () => void confirm());
  reloadButton.addEventListener("click", () => void load());

  // Countdown refresh while a firewall change is reversible.
  window.setInterval(() => {
    if (root.style.display !== "none" && pending) {
      renderChrome();
    }
  }, 1000);

  return {
    root,
    onShow: () => void load(),
  };
}

function sectionTitle(text: string): HTMLElement {
  return el("div", {
    class: "panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]",
    text,
  });
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
