/**
 * VPN page (WireGuard).
 *
 * Interface card with the server key lifecycle (generate once, never shown
 * again), the peer table, and a one-time client export with a QR code when
 * `qrencode` is installed.
 */

import { remoteAccessApi, wireguardApi } from "../api";
import type { ClientExport, RemoteAccessOverview, WgPeer, WgState } from "../api";
import { showToast } from "../toast";
import { bytes, uptime } from "../ui";
import { badge, button, checkbox, el, emptyNote, field, numberInput, openModal, panel, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
  onHide?: () => void;
}

type VpnTab = "wireguard" | "tailscale" | "netbird" | "cloudflared";

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function handshake(seconds: number): string {
  if (!seconds) return "never";
  return `${uptime(Math.max(0, Math.floor(Date.now() / 1000) - seconds))} ago`;
}

function freshPeer(): WgPeer {
  return {
    name: "",
    public_key: "",
    allowed_ips: [],
    dns: null,
    persistent_keepalive: 25,
    enabled: true,
  };
}

export function createVpnPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 overflow-auto p-3" });

  let activeTab: VpnTab = "wireguard";
  let state: WgState | null = null;
  let remoteOverview: RemoteAccessOverview | null = null;
  let steps: { subsystem: string; ok: boolean; detail: string }[] | null = null;
  let busy = false;
  let status = "";
  let loaded = false;

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-500" });
  const refreshButton = button("refresh", "", () => void load());
  const keyButton = button("generate server key", "text-amber-400 hover:border-amber-500", () =>
    void generateServerKey(),
  );
  const applyButton = button("apply", "text-emerald-400 hover:border-emerald-500", () =>
    void applyConfig(),
  );
  const addPeerButton = button("add peer", "", () => openPeerModal());

  const tabStrip = el("div", { class: "flex items-center gap-1 border-b border-white/10 pb-2" });
  const tabs: { id: VpnTab; label: string }[] = [
    { id: "wireguard", label: "WireGuard" },
    { id: "tailscale", label: "Tailscale" },
    { id: "netbird", label: "NetBird" },
    { id: "cloudflared", label: "Cloudflare Tunnel" },
  ];

  function renderTabs(): void {
    tabStrip.innerHTML = "";
    for (const t of tabs) {
      const active = t.id === activeTab;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.textContent = t.label;
      btn.className = active
        ? "rounded-md border border-emerald-500/30 bg-emerald-500/10 px-2.5 py-1 text-xs font-medium text-emerald-400"
        : "rounded-md border border-white/5 bg-white/[0.02] px-2.5 py-1 text-xs text-neutral-400 hover:border-white/10 hover:text-white transition-colors";
      btn.addEventListener("click", () => {
        if (activeTab !== t.id) {
          activeTab = t.id;
          render();
        }
      });
      tabStrip.append(btn);
    }
  }

  const wgActions = el("div", { class: "flex items-center gap-2" }, [
    keyButton,
    addPeerButton,
    applyButton,
  ]);

  const header = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-neutral-300 font-semibold text-xs tracking-wider uppercase", text: "remote access & vpn" }),
    refreshButton,
    wgActions,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  const body = el("div", { class: "flex flex-col gap-2" });
  root.append(header, tabStrip, body);

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      const [wgState, remote] = await Promise.all([
        wireguardApi.state(),
        remoteAccessApi.status().catch(() => null),
      ]);
      state = wgState;
      remoteOverview = remote;
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
    keyButton.disabled = busy;
    addPeerButton.disabled = busy;
    applyButton.disabled = busy || !state;
    applyButton.textContent = state?.config.enabled ? "apply (up)" : "apply (down)";
    wgActions.style.display = activeTab === "wireguard" ? "flex" : "none";
    renderTabs();
  }

  function render(): void {
    renderChrome();
    body.replaceChildren();
    if (!loaded) {
      body.append(emptyNote("loading…"));
      return;
    }

    if (activeTab === "tailscale") {
      body.append(renderTailscale());
      return;
    }
    if (activeTab === "netbird") {
      body.append(renderNetBird());
      return;
    }
    if (activeTab === "cloudflared") {
      body.append(renderCloudflared());
      return;
    }

    // Default: WireGuard
    if (!state) {
      body.append(emptyNote(status || "the wireguard API is unavailable"));
      return;
    }

    const problems = problemsPanel();
    if (problems) body.append(problems);
    body.append(interfaceCard());
    body.append(peersCard());
    body.append(previewPanel());
    if (steps) body.append(reportPanel());
  }

  function renderTailscale(): HTMLElement {
    const ts = remoteOverview?.tailscale;
    const { root: card, body: cardBody } = panel("tailscale overlay network");
    cardBody.className = "flex flex-col gap-3 p-4 text-xs";

    if (!ts || !ts.installed) {
      const installBox = el("div", {
        class: "flex flex-col gap-3 rounded-lg border border-emerald-500/30 bg-emerald-950/20 p-4 text-neutral-300",
      });
      installBox.append(
        el("div", { class: "flex items-center justify-between" }, [
          el("div", { class: "flex flex-col" }, [
            el("span", { class: "font-semibold text-emerald-300 text-sm", text: "Tailscale Zero-Config Mesh Network" }),
            el("span", { class: "text-neutral-400 text-xs mt-0.5", text: "Connect your home server securely to laptops, phones, and off-site machines with WireGuard encryption." }),
          ]),
        ]),
      );

      const installBtn = button(
        "Install Tailscale (1-Click)",
        "border-emerald-600 bg-emerald-500/20 font-semibold text-emerald-300 hover:border-emerald-500 hover:bg-emerald-500/30 px-3 py-1.5",
        async () => {
          installBtn.disabled = true;
          installBtn.textContent = "Installing Tailscale…";
          try {
            const res = await remoteAccessApi.tailscaleInstall();
            showToast({
              tone: "ok",
              title: "Tailscale Installation Started",
              message: res.message || "Downloading packages. Check activity drawer.",
            });
            window.dispatchEvent(new Event("stack-stack:activity-open"));
            const interval = setInterval(async () => {
              const overview = await remoteAccessApi.status().catch(() => null);
              if (overview?.tailscale.installed) {
                clearInterval(interval);
                showToast({ tone: "ok", title: "Tailscale Installed!", message: "Tailscale daemon is active and ready." });
                await load();
              }
            }, 3000);
          } catch (err) {
            installBtn.disabled = false;
            installBtn.textContent = "Install Tailscale (1-Click)";
            showToast({ tone: "error", title: "Installation Failed", message: errorText(err) });
          }
        },
      );

      installBox.append(
        el("div", { class: "flex items-center gap-3 mt-1" }, [
          installBtn,
          el("span", { class: "text-[11px] text-neutral-500", text: "Installs official package and enables systemd service automatically." }),
        ]),
        el("details", { class: "mt-2 text-[11px] text-neutral-500 cursor-pointer" }, [
          el("summary", { class: "hover:text-neutral-400", text: "View manual command alternative" }),
          el("pre", { class: "rounded bg-black/60 p-2 font-mono text-[10px] text-emerald-400 mt-1", text: "curl -fsSL https://tailscale.com/install.sh | sh\nsudo systemctl enable --now tailscaled" }),
        ]),
      );

      cardBody.append(installBox);
      return card;
    }

    // Status Header
    const statusBox = el("div", { class: "flex items-center justify-between rounded-lg border border-white/10 bg-white/[0.02] p-3" });
    const left = el("div", { class: "flex items-center gap-3" });
    const dot = el("span", { class: `size-2.5 rounded-full ${ts.running ? "bg-emerald-400 ring-2 ring-emerald-400/20" : "bg-neutral-600"}` });
    const info = el("div", { class: "flex flex-col" });
    info.append(
      el("span", { class: "font-semibold text-white", text: ts.running ? `Connected as ${ts.hostname || "host"}` : "Disconnected" }),
      el("span", { class: "text-[11px] text-neutral-400", text: ts.ip ? `Tailscale IP: ${ts.ip} · ${ts.peers_count} peer(s)` : "Not logged into a tailnet" }),
    );
    left.append(dot, info);
    statusBox.append(left);

    if (ts.running) {
      const disconnectBtn = button("Disconnect", "border-rose-500/40 text-rose-400 hover:bg-rose-500/10", async () => {
        disconnectBtn.disabled = true;
        await withBusy(async () => {
          await remoteAccessApi.tailscaleDown();
          await load();
        });
      });
      statusBox.append(disconnectBtn);
      cardBody.append(statusBox);
    } else {
      cardBody.append(statusBox);

      // Connect Form
      const form = el("div", { class: "flex flex-col gap-3 rounded-lg border border-white/10 bg-white/[0.02] p-3" });
      let authKey = "";
      let hostname = "";
      let exitNode = false;
      let acceptRoutes = true;

      const authKeyInput = textInput("", (v) => (authKey = v), { type: "password", placeholder: "tskey-auth-..." });
      const hostnameInput = textInput("", (v) => (hostname = v), { placeholder: "fujitsu-server (optional)" });
      const exitNodeBox = checkbox(exitNode, (v) => (exitNode = v));
      const acceptRoutesBox = checkbox(acceptRoutes, (v) => (acceptRoutes = v));

      form.append(
        field("Tailscale Auth Key", authKeyInput),
        field("Node Hostname (optional)", hostnameInput),
        el("div", { class: "flex items-center gap-4 text-xs text-neutral-400 mt-1" }, [
          el("label", { class: "flex items-center gap-1.5 cursor-pointer" }, [exitNodeBox, el("span", { text: "Advertise as exit node" })]),
          el("label", { class: "flex items-center gap-1.5 cursor-pointer" }, [acceptRoutesBox, el("span", { text: "Accept subnet routes" })]),
        ]),
      );

      const connectBtn = button("Connect to Tailnet", "border-emerald-600 text-emerald-400 hover:border-emerald-500", async () => {
        if (!authKey.trim()) return;
        connectBtn.disabled = true;
        await withBusy(async () => {
          await remoteAccessApi.tailscaleUp({
            auth_key: authKey.trim(),
            hostname: hostname.trim() || undefined,
            exit_node: exitNode,
            accept_routes: acceptRoutes,
          });
          await load();
        });
      });

      form.append(el("div", { class: "flex justify-end mt-1" }, [connectBtn]));
      cardBody.append(form);
    }

    return card;
  }

  function renderNetBird(): HTMLElement {
    const nb = remoteOverview?.netbird;
    const { root: card, body: cardBody } = panel("netbird mesh network");
    cardBody.className = "flex flex-col gap-3 p-4 text-xs";

    if (!nb || !nb.installed) {
      const installBox = el("div", {
        class: "flex flex-col gap-3 rounded-lg border border-sky-500/30 bg-sky-950/20 p-4 text-neutral-300",
      });
      installBox.append(
        el("div", { class: "flex items-center justify-between" }, [
          el("div", { class: "flex flex-col" }, [
            el("span", { class: "font-semibold text-sky-300 text-sm", text: "NetBird Fast Peer-to-Peer WireGuard Network" }),
            el("span", { class: "text-neutral-400 text-xs mt-0.5", text: "Open-source WireGuard overlay network with single sign-on (SSO), MFA, and direct peer routing." }),
          ]),
        ]),
      );

      const installBtn = button(
        "Install NetBird (1-Click)",
        "border-sky-600 bg-sky-500/20 font-semibold text-sky-300 hover:border-sky-500 hover:bg-sky-500/30 px-3 py-1.5",
        async () => {
          installBtn.disabled = true;
          installBtn.textContent = "Installing NetBird…";
          try {
            const res = await remoteAccessApi.netbirdInstall();
            showToast({
              tone: "ok",
              title: "NetBird Installation Started",
              message: res.message || "Downloading packages. Check activity drawer.",
            });
            window.dispatchEvent(new Event("stack-stack:activity-open"));
            const interval = setInterval(async () => {
              const overview = await remoteAccessApi.status().catch(() => null);
              if (overview?.netbird.installed) {
                clearInterval(interval);
                showToast({ tone: "ok", title: "NetBird Installed!", message: "NetBird daemon is active and ready." });
                await load();
              }
            }, 3000);
          } catch (err) {
            installBtn.disabled = false;
            installBtn.textContent = "Install NetBird (1-Click)";
            showToast({ tone: "error", title: "Installation Failed", message: errorText(err) });
          }
        },
      );

      installBox.append(
        el("div", { class: "flex items-center gap-3 mt-1" }, [
          installBtn,
          el("span", { class: "text-[11px] text-neutral-500", text: "Installs official NetBird repository package and starts daemon." }),
        ]),
        el("details", { class: "mt-2 text-[11px] text-neutral-500 cursor-pointer" }, [
          el("summary", { class: "hover:text-neutral-400", text: "View manual command alternative" }),
          el("pre", { class: "rounded bg-black/60 p-2 font-mono text-[10px] text-sky-400 mt-1", text: "curl -fsSL https://pkgs.netbird.io/install.sh | sh\nsudo systemctl enable --now netbird" }),
        ]),
      );

      cardBody.append(installBox);
      return card;
    }

    // Status Header
    const statusBox = el("div", { class: "flex items-center justify-between rounded-lg border border-white/10 bg-white/[0.02] p-3" });
    const left = el("div", { class: "flex items-center gap-3" });
    const dot = el("span", { class: `size-2.5 rounded-full ${nb.running ? "bg-emerald-400 ring-2 ring-emerald-400/20" : "bg-neutral-600"}` });
    const info = el("div", { class: "flex flex-col" });
    info.append(
      el("span", { class: "font-semibold text-white", text: nb.running ? "Connected to NetBird" : "Disconnected" }),
      el("span", { class: "text-[11px] text-neutral-400", text: nb.ip ? `NetBird IP: ${nb.ip} · ${nb.peers_count} peer(s)` : "Not connected to management server" }),
    );
    left.append(dot, info);
    statusBox.append(left);

    if (nb.running) {
      const disconnectBtn = button("Disconnect", "border-rose-500/40 text-rose-400 hover:bg-rose-500/10", async () => {
        disconnectBtn.disabled = true;
        await withBusy(async () => {
          await remoteAccessApi.netbirdDown();
          await load();
        });
      });
      statusBox.append(disconnectBtn);
      cardBody.append(statusBox);
    } else {
      cardBody.append(statusBox);

      // Connect Form
      const form = el("div", { class: "flex flex-col gap-3 rounded-lg border border-white/10 bg-white/[0.02] p-3" });
      let setupKey = "";
      let managementUrl = "";

      const setupKeyInput = textInput("", (v) => (setupKey = v), { type: "password", placeholder: "Setup key" });
      const mgmtInput = textInput("", (v) => (managementUrl = v), { placeholder: "https://api.netbird.io (or custom URL)" });

      form.append(
        field("Setup Key", setupKeyInput),
        field("Management URL (optional)", mgmtInput),
      );

      const connectBtn = button("Connect to NetBird", "border-emerald-600 text-emerald-400 hover:border-emerald-500", async () => {
        if (!setupKey.trim()) return;
        connectBtn.disabled = true;
        await withBusy(async () => {
          await remoteAccessApi.netbirdUp({
            setup_key: setupKey.trim(),
            management_url: managementUrl.trim() || undefined,
          });
          await load();
        });
      });

      form.append(el("div", { class: "flex justify-end mt-1" }, [connectBtn]));
      cardBody.append(form);
    }

    return card;
  }

  function renderCloudflared(): HTMLElement {
    const cf = remoteOverview?.cloudflared;
    const { root: card, body: cardBody } = panel("cloudflare tunnel");
    cardBody.className = "flex flex-col gap-3 p-4 text-xs";

    const statusBox = el("div", { class: "flex items-center justify-between rounded-lg border border-white/10 bg-white/[0.02] p-3" });
    const left = el("div", { class: "flex items-center gap-3" });
    const dot = el("span", { class: `size-2.5 rounded-full ${cf?.active ? "bg-emerald-400 ring-2 ring-emerald-400/20" : "bg-neutral-600"}` });
    const info = el("div", { class: "flex flex-col" });
    info.append(
      el("span", { class: "font-semibold text-white", text: cf?.active ? "Cloudflare Tunnel Active" : "Tunnel Inactive" }),
      el("span", { class: "text-[11px] text-neutral-400", text: cf?.installed ? `cloudflared ${cf.version || "installed"}` : "cloudflared binary is not installed" }),
    );
    left.append(dot, info);
    statusBox.append(left);

    const configBtn = button("Configure Tunnel Ingress ↗", "text-neutral-300 hover:text-white", () => {
      location.hash = "#/network";
    });
    statusBox.append(configBtn);

    cardBody.append(
      statusBox,
      el("div", { class: "text-neutral-400 leading-relaxed text-[11.5px]" }, [
        el("div", { text: "Cloudflare Tunnels create an encrypted, outbound-only connection between your server and Cloudflare's global edge. This allows you to securely access your internal services without opening any router ports or exposing your public home IP." }),
        el("div", { class: "mt-1.5 text-neutral-500", text: "Configure hostnames, credentials, and ingress routing from the Network → Tunnel page." }),
      ]),
    );

    return card;
  }

  function problemsPanel(): HTMLElement | null {
    if (!state) return null;
    const all = [...state.problems];
    if (state.config_error) {
      all.push({ field: "document", message: state.config_error });
    }
    if (all.length === 0) return null;
    const box = el("div", { class: "border border-rose-800 bg-rose-950/40 px-3 py-2 text-[11px] text-rose-300" });
    for (const problem of all) {
      box.append(el("div", { text: `${problem.field}: ${problem.message}` }));
    }
    return box;
  }

  function interfaceCard(): HTMLElement {
    const { root: card, body: cardBody } = panel("interface");
    cardBody.className = "flex flex-wrap items-end gap-3 px-3 py-2";
    const config = state!.config;

    cardBody.append(
      field("enabled", checkbox(config.enabled, (value) => (config.enabled = value), "run wireguard")),
      field("interface", textInput(config.interface, (value) => (config.interface = value))),
      field("address (cidr)", textInput(config.address_cidr, (value) => (config.address_cidr = value))),
      field(
        "listen port",
        textInput(String(config.listen_port), (value) => (config.listen_port = Number.parseInt(value, 10) || 0)),
      ),
      field("mtu", textInput(String(config.mtu), (value) => (config.mtu = Number.parseInt(value, 10) || 0))),
    );

    const keyState = el("div", { class: "flex flex-col gap-0.5 text-[11px]" });
    if (state!.has_key) {
      keyState.append(el("div", { class: "text-emerald-400", text: "server key: present" }));
      if (state!.public_key) {
        keyState.append(el("div", { class: "text-neutral-500", text: `public: ${state!.public_key}` }));
      }
      if (state!.public_key_error) {
        keyState.append(el("div", { class: "text-amber-400", text: state!.public_key_error }));
      }
      if (state!.endpoint) {
        keyState.append(el("div", { class: "text-neutral-500", text: `endpoint: ${state!.endpoint}` }));
      }
    } else {
      keyState.append(el("div", { class: "text-amber-400", text: "no server key yet — generate one" }));
    }
    cardBody.append(keyState);

    if (state!.status) {
      const live = el("div", { class: "flex flex-col gap-0.5 text-[11px]" });
      live.append(
        el("div", {
          class: "text-emerald-400",
          text: `up · ${state!.status.peers.length} peer(s) · port ${state!.status.listen_port}`,
        }),
      );
      for (const peer of state!.status.peers) {
        live.append(
          el("div", {
            class: "text-neutral-500",
            text: `peer ${peer.public_key.slice(0, 8)}… handshake ${handshake(peer.latest_handshake)} · rx ${bytes(peer.rx_bytes)} tx ${bytes(peer.tx_bytes)}`,
          }),
        );
      }
      cardBody.append(live);
    } else if (state!.status_error) {
      cardBody.append(el("div", { class: "text-[11px] text-amber-400", text: state!.status_error }));
    }
    return card;
  }

  function suggestNextIp(): string {
    const defaultSubnet = "10.10.0.";
    if (!state || !state.config.peers || state.config.peers.length === 0) {
      return `${defaultSubnet}2/32`;
    }
    const occupied = new Set<number>();
    for (const peer of state.config.peers) {
      for (const ip of peer.allowed_ips) {
        const match = ip.match(/10\.10\.0\.(\d+)/);
        if (match) {
          occupied.add(Number.parseInt(match[1], 10));
        }
      }
    }
    for (let host = 2; host < 254; host++) {
      if (!occupied.has(host)) {
        return `${defaultSubnet}${host}/32`;
      }
    }
    return `${defaultSubnet}2/32`;
  }

  function openPeerModal(existing?: WgPeer, idx?: number): void {
    const isNew = existing === undefined;
    const modal = openModal(isNew ? "Add WireGuard Peer" : `Edit Peer · ${existing.name}`, "max-w-md");
    modal.body.className = "flex flex-col gap-3";

    const row = existing
      ? structuredClone(existing)
      : {
          ...freshPeer(),
          allowed_ips: [suggestNextIp()],
        };

    let allowedIpsStr = row.allowed_ips.join(", ");
    const errorEl = el("div", { class: "text-xs text-rose-400 min-h-4" });

    const nameInput = textInput(row.name, (v) => { row.name = v; }, { placeholder: "e.g. phone, laptop-work, macbook" });
    const ipInput = textInput(allowedIpsStr, (v) => { allowedIpsStr = v; }, { placeholder: "10.10.0.2/32" });
    const pubKeyInput = textInput(row.public_key, (v) => { row.public_key = v; }, { placeholder: "Leave empty to auto-generate for client export" });
    const dnsInput = textInput(row.dns ?? "", (v) => { row.dns = v.trim() ? v.trim() : null; }, { placeholder: "e.g. 10.10.0.1 or 1.1.1.1" });
    const keepaliveInput = numberInput(row.persistent_keepalive, (v) => { row.persistent_keepalive = v; }, { min: 0, max: 3600 });
    const enabledCheck = checkbox(row.enabled, (v) => { row.enabled = v; }, "Enable peer immediately");

    modal.body.append(
      field("Peer Name", nameInput, "Friendly label for this client device."),
      field("Tunnel IP Address (Allowed IPs)", ipInput, "Assigned virtual IP within the WireGuard interface network."),
      field("Client Public Key (optional)", pubKeyInput, "If omitted, a keypair will be generated when clicking 'Client Conf'."),
      el("div", { class: "grid grid-cols-2 gap-2" }, [
        field("DNS Resolver (optional)", dnsInput),
        field("Persistent Keepalive (s)", keepaliveInput),
      ]),
      el("div", { class: "p-2 bg-black/20 rounded border border-white/5" }, [enabledCheck]),
      errorEl,
    );

    const saveBtn = button(isNew ? "Add Peer" : "Save Changes", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold hover:bg-emerald-500/30 px-4 py-1.5 text-xs shadow-sm", async () => {
      errorEl.textContent = "";
      if (!row.name.trim()) {
        errorEl.textContent = "Peer Name is required.";
        return;
      }
      const ips = allowedIpsStr.split(",").map((s) => s.trim()).filter(Boolean);
      if (ips.length === 0) {
        errorEl.textContent = "At least one tunnel address is required (e.g. 10.10.0.2/32).";
        return;
      }
      row.allowed_ips = ips;

      if (isNew) {
        try {
          saveBtn.disabled = true;
          saveBtn.textContent = "Adding…";
          const result = await wireguardApi.addPeer({ ...freshPeer(), ...row });
          state = result;
          showToast({ tone: "ok", title: "Peer Added", message: `WireGuard peer '${row.name}' added successfully` });
          modal.close();
          render();
        } catch (err) {
          errorEl.textContent = errorText(err);
          saveBtn.disabled = false;
          saveBtn.textContent = "Add Peer";
        }
      } else if (idx !== undefined && state) {
        state.config.peers[idx] = row;
        showToast({ tone: "ok", title: "Peer Updated", message: `WireGuard peer '${row.name}' updated` });
        modal.close();
        render();
      }
    });

    modal.body.append(
      el("div", { class: "flex items-center justify-end gap-2 pt-2 border-t border-white/10" }, [
        button("Cancel", "text-neutral-400 hover:text-white text-xs px-3 py-1", () => modal.close()),
        saveBtn,
      ]),
    );
  }

  function peersCard(): HTMLElement {
    const { root: card, body: cardBody } = panel("peers");
    cardBody.className = "flex flex-col gap-3 p-3";

    const topBar = el("div", { class: "flex items-center justify-between" });
    topBar.append(
      el("div", { class: "flex flex-col" }, [
        el("span", { class: "font-semibold text-xs text-neutral-200", text: "WireGuard Peers" }),
        el("span", { class: "text-[11px] text-neutral-400", text: "Client devices connected to this WireGuard tunnel." }),
      ]),
      button(
        "+ Add Peer",
        "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm",
        () => openPeerModal(),
      ),
    );
    cardBody.append(topBar);

    if (!state?.config.peers || state.config.peers.length === 0) {
      cardBody.append(emptyNote("No peers configured yet. Click '+ Add Peer' above to add your first device."));
      return card;
    }

    const table = el("table", { class: "w-full text-xs text-left" });
    table.append(
      el("thead", {}, [
        el("tr", { class: "border-b border-white/10 text-[10px] uppercase text-neutral-500 tracking-wider" }, [
          el("th", { class: "py-2 px-3", text: "Peer Name" }),
          el("th", { class: "py-2 px-3", text: "Tunnel Address" }),
          el("th", { class: "py-2 px-3", text: "Public Key" }),
          el("th", { class: "py-2 px-3", text: "DNS" }),
          el("th", { class: "py-2 px-3", text: "Keepalive" }),
          el("th", { class: "py-2 px-3", text: "Status" }),
          el("th", { class: "py-2 px-3 text-right", text: "Actions" }),
        ]),
      ]),
    );
    const tbody = el("tbody", { class: "divide-y divide-white/5 font-mono text-[11px]" });
    state.config.peers.forEach((peer, idx) => {
      const tr = el("tr", { class: "hover:bg-white/[0.02] transition-colors" });
      const pubKeyShort = peer.public_key
        ? `${peer.public_key.slice(0, 8)}…${peer.public_key.slice(-6)}`
        : "(Auto-generated)";

      tr.append(
        el("td", { class: "py-2 px-3 text-neutral-200 font-sans font-medium", text: peer.name }),
        el("td", { class: "py-2 px-3 text-emerald-400 font-semibold", text: peer.allowed_ips.join(", ") }),
        el("td", { class: "py-2 px-3 text-neutral-400 font-mono text-[10px]", text: pubKeyShort }),
        el("td", { class: "py-2 px-3 text-neutral-300", text: peer.dns || "—" }),
        el("td", { class: "py-2 px-3 text-neutral-400 font-sans", text: `${peer.persistent_keepalive}s` }),
        el("td", { class: "py-2 px-3 font-sans" }, [
          peer.enabled ? badge("Active", "ok") : badge("Disabled", "muted"),
        ]),
        el("td", { class: "py-2 px-3 text-right font-sans flex items-center justify-end gap-1" }, [
          button("📱 Client Conf", "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold px-2 py-0.5 text-xs hover:bg-sky-500/30", () => {
            void exportClient(peer);
          }),
          button("Edit", "text-neutral-300 hover:text-white px-2 py-0.5 text-xs", () => openPeerModal(peer, idx)),
          button("Delete", "text-rose-400 hover:text-rose-300 px-2 py-0.5 text-xs", () => {
            if (window.confirm(`Delete WireGuard peer '${peer.name}'?`)) {
              state?.config.peers.splice(idx, 1);
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

  function previewPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel(`${state!.config.interface}.conf`);
    cardBody.append(
      el("pre", {
        class: "max-h-56 overflow-auto px-3 py-2 font-mono text-[10px] text-neutral-400",
        text: state!.preview,
      }),
    );
    return card;
  }

  function reportPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("last apply");
    cardBody.className = "flex flex-col gap-0.5 px-3 py-2 text-[11px]";
    for (const step of steps ?? []) {
      cardBody.append(
        el("div", {
          class: step.ok ? "text-emerald-400" : "text-rose-400",
          text: `${step.ok ? "ok" : "fail"} · ${step.subsystem}: ${step.detail}`,
        }),
      );
    }
    return card;
  }

  async function generateServerKey(): Promise<void> {
    if (state?.has_key && !window.confirm("Replace the existing server key? Peers must be re-added.")) {
      return;
    }
    const key = await withBusy(() => wireguardApi.generateKey(true));
    if (key) {
      status = "server key generated";
      await load();
    }
  }

  async function applyConfig(): Promise<void> {
    if (!state) return;
    const report = await withBusy(() => wireguardApi.apply(state!.config));
    if (report) {
      state = report.state;
      steps = report.steps;
      status = report.steps.every((step) => step.ok) ? "applied" : "applied with problems";
      render();
    }
  }

  async function exportClient(peer: WgPeer): Promise<void> {
    const address = peer.allowed_ips[0] ?? "";
    if (!address) {
      status = `${peer.name} has no allowed IP to hand out`;
      renderChrome();
      return;
    }
    const exported = await withBusy(() => wireguardApi.exportClient(peer.name, address));
    if (!exported) return;
    showExport(exported, peer.name);
    status = "client configuration generated (private key shown once)";
    renderChrome();
  }

  function downloadConf(filename: string, content: string): void {
    const blob = new Blob([content], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }

  function showExport(exported: ClientExport, name: string): void {
    const modal = openModal(`Client Configuration · ${name}`, "max-w-2xl");
    modal.body.className = "flex flex-col gap-3";

    modal.body.append(
      el("div", {
        class: "p-2.5 bg-amber-950/30 rounded border border-amber-600/30 text-amber-300 text-xs leading-relaxed",
        text: "⚠️ Important: The client private key below was generated on demand and is never stored on the server. Download the .conf file or scan the QR code now before closing.",
      }),
    );

    if (exported.qr) {
      modal.body.append(
        el("div", { class: "flex flex-col items-center gap-1.5 p-2 bg-black/40 rounded border border-white/5" }, [
          el("span", { class: "text-[10px] uppercase text-neutral-400 tracking-wider", text: "Scan with WireGuard Mobile App" }),
          el("pre", { class: "font-mono text-[8px] leading-3 text-emerald-400 overflow-x-auto select-all", text: exported.qr }),
        ]),
      );
    } else if (exported.qr_error) {
      modal.body.append(
        el("div", { class: "text-[11px] text-neutral-500 italic", text: `QR code unavailable (${exported.qr_error}). Use configuration text below.` }),
      );
    }

    const confBox = el("pre", {
      class: "max-h-56 overflow-auto border border-white/5 bg-black/60 p-3 font-mono text-[11px] text-neutral-300 rounded",
      text: exported.config,
    });
    modal.body.append(confBox);

    const copyBtn = button("📋 Copy Config", "border-neutral-600 bg-white/5 text-neutral-200 hover:text-white px-3 py-1.5 text-xs", async () => {
      try {
        await navigator.clipboard.writeText(exported.config);
        showToast({ tone: "ok", title: "Copied", message: "WireGuard configuration copied to clipboard" });
      } catch {
        showToast({ tone: "error", title: "Copy Failed", message: "Could not access clipboard" });
      }
    });

    const downloadBtn = button("💾 Download .conf", "border-sky-600 bg-sky-500/20 text-sky-300 font-semibold hover:bg-sky-500/30 px-3 py-1.5 text-xs shadow-sm", () => {
      downloadConf(`wg0-${name}.conf`, exported.config);
      showToast({ tone: "ok", title: "File Downloaded", message: `Downloaded wg0-${name}.conf` });
    });

    modal.body.append(
      el("div", { class: "flex items-center justify-between pt-2 border-t border-white/10" }, [
        el("div", { class: "flex gap-2" }, [copyBtn, downloadBtn]),
        button("Done", "border-emerald-600 bg-emerald-500/20 text-emerald-300 font-semibold px-4 py-1.5 text-xs hover:bg-emerald-500/30", () => modal.close()),
      ]),
    );
  }

  void load();
  return {
    root,
    onShow: () => {
      void load();
    },
  };
}
