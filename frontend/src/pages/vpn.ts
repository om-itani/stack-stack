/**
 * VPN page (WireGuard).
 *
 * Interface card with the server key lifecycle (generate once, never shown
 * again), the peer table, and a one-time client export with a QR code when
 * `qrencode` is installed.
 */

import { wireguardApi } from "../api";
import type { ClientExport, WgPeer, WgState } from "../api";
import { bytes, uptime } from "../ui";
import { button, checkbox, el, emptyNote, field, panel, tableEditor, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

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

  let state: WgState | null = null;
  let steps: { subsystem: string; ok: boolean; detail: string }[] | null = null;
  let busy = false;
  let status = "";
  let loaded = false;

  const statusLine = el("span", { class: "tabular text-[11px] text-slate-500" });
  const refreshButton = button("refresh", "", () => void load());
  const keyButton = button("generate server key", "text-amber-400 hover:border-amber-500", () =>
    void generateServerKey(),
  );
  const applyButton = button("apply", "text-emerald-400 hover:border-emerald-500", () =>
    void applyConfig(),
  );
  const addPeerButton = button("add peer", "", () => void addPeer());

  const header = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-slate-300", text: "vpn · wireguard" }),
    refreshButton,
    keyButton,
    addPeerButton,
    applyButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  const body = el("div", { class: "flex flex-col gap-2" });
  root.append(header, body);

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      state = await wireguardApi.state();
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
  }

  function render(): void {
    renderChrome();
    body.replaceChildren();
    if (!loaded) {
      body.append(emptyNote("loading…"));
      return;
    }
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
        keyState.append(el("div", { class: "text-slate-500", text: `public: ${state!.public_key}` }));
      }
      if (state!.public_key_error) {
        keyState.append(el("div", { class: "text-amber-400", text: state!.public_key_error }));
      }
      if (state!.endpoint) {
        keyState.append(el("div", { class: "text-slate-500", text: `endpoint: ${state!.endpoint}` }));
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
            class: "text-slate-500",
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

  function peersCard(): HTMLElement {
    const { root: card, body: cardBody } = panel("peers");
    cardBody.append(
      tableEditor(
        state!.config.peers,
        [
          { header: "name", cell: (row) => textInput(row.name, (value) => (row.name = value)) },
          {
            header: "public key",
            cell: (row) => textInput(row.public_key, (value) => (row.public_key = value)),
          },
          {
            header: "allowed ips",
            cell: (row) =>
              textInput(row.allowed_ips.join(", "), (value) => {
                row.allowed_ips = value
                  .split(",")
                  .map((part) => part.trim())
                  .filter(Boolean);
              }),
          },
          {
            header: "dns",
            cell: (row) => textInput(row.dns ?? "", (value) => (row.dns = value.trim() ? value.trim() : null)),
          },
          {
            header: "keepalive",
            cell: (row) =>
              textInput(String(row.persistent_keepalive), (value) => {
                row.persistent_keepalive = Number.parseInt(value, 10) || 0;
              }),
          },
          {
            header: "export",
            cell: (row) =>
              button("client conf", "text-[10px]", () => void exportClient(row)),
          },
        ],
        freshPeer,
        render,
        "add peer",
      ),
    );
    return card;
  }

  function previewPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel(`${state!.config.interface}.conf`);
    cardBody.append(
      el("pre", {
        class: "max-h-56 overflow-auto px-3 py-2 font-mono text-[10px] text-slate-400",
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

  async function addPeer(): Promise<void> {
    const name = window.prompt("Peer name (e.g. phone)");
    if (!name?.trim()) return;
    const address = window.prompt("Tunnel address for this peer (inside the interface network)", "10.10.0.2/32");
    if (!address?.trim()) return;
    const result = await withBusy(() =>
      wireguardApi.addPeer({ ...freshPeer(), name: name.trim(), allowed_ips: [address.trim()] }),
    );
    if (result) {
      state = result;
      status = `peer ${name.trim()} added`;
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

  function showExport(exported: ClientExport, name: string): void {
    const overlay = el("div", {
      class: "fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4",
    });
    const card = el("div", {
      class: "flex max-h-[85vh] w-full max-w-2xl flex-col gap-2 overflow-auto border border-[#1e2733] bg-[#0d131b] p-3",
    });
    const close = (): void => overlay.remove();
    card.append(
      el("div", { class: "flex items-center gap-2" }, [
        el("span", { class: "text-slate-300", text: `client configuration — ${name}` }),
        el("span", { class: "ml-auto" }),
        button("close", "text-[10px]", close),
      ]),
      el("div", {
        class: "text-[11px] text-amber-400",
        text: "The private key below is shown once and is not stored. Copy it now.",
      }),
      el("pre", {
        class: "overflow-auto border border-[#1e2733] bg-[#0b0f14] p-2 font-mono text-[10px] text-slate-300",
        text: exported.config,
      }),
    );
    if (exported.qr) {
      card.append(el("pre", { class: "font-mono text-[8px] leading-3 text-emerald-400", text: exported.qr }));
    } else if (exported.qr_error) {
      card.append(el("div", { class: "text-[10px] text-slate-600", text: `qr unavailable: ${exported.qr_error}` }));
    }
    overlay.append(card);
    overlay.addEventListener("mousedown", (event) => {
      if (event.target === overlay) close();
    });
    document.body.append(overlay);
  }

  void load();
  return { root };
}
