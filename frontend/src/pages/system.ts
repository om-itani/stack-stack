/**
 * System dashboard: CPU, memory, network, filesystems and SMART.
 *
 * The page owns only its panels; the header status (host, uptime, link) lives in
 * the shell so it stays visible on every page. It subscribes to the shared
 * telemetry client and paints on every frame.
 */

import { Sparkline } from "../spark";
import type { Telemetry } from "../telemetry";
import type { Disk, Network, Smart, Snapshot } from "../telemetry";
import { animateNumber, bytes, emptyNote, fill, must, percent, rate } from "../ui";
import { buildSmartPanel } from "./smartPanel";

const EMERALD = "#34d399";
const SKY = "#38bdf8";

const MARKUP = `
  <section class="panel col-span-full">
    <div class="panel-title border-b border-white/5 px-3 py-1.5 text-[10px]">network</div>
    <div data-ref="nets"></div>
  </section>

  <section class="panel">
    <div class="panel-title flex items-baseline justify-between gap-4 border-b border-white/5 px-3 py-1.5 text-[10px]">
      <span>cpu</span>
      <span data-ref="cpu-model" class="truncate text-[10px] normal-case tracking-normal text-neutral-600"></span>
    </div>
    <div class="flex items-center gap-4 px-3 py-2">
      <div data-ref="cpu-total" class="tabular stat text-3xl">—</div>
      <div class="min-w-0 flex-1">
        <div data-ref="cpu-spark"></div>
        <div class="tabular mt-1 flex justify-between text-[10px] text-neutral-500">
          <span data-ref="cpu-user">usr —</span>
          <span data-ref="cpu-sys">sys —</span>
          <span data-ref="cpu-io">io —</span>
          <span data-ref="cpu-idle">idle —</span>
        </div>
      </div>
    </div>
    <div data-ref="cpu-cores" class="grid grid-cols-6 gap-x-2 gap-y-1 px-3 pb-3 sm:grid-cols-8 lg:grid-cols-10"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-white/5 px-3 py-1.5 text-[10px]">memory</div>
    <div class="px-3 py-2">
      <div class="flex items-baseline justify-between">
        <span data-ref="mem-used" class="tabular stat text-2xl">—</span>
        <span data-ref="mem-total" class="tabular text-neutral-500">—</span>
      </div>
      <div class="mt-2 h-2 bg-white/10"><div data-ref="mem-bar" class="h-full bg-emerald-500" style="width:0%"></div></div>
      <dl class="tabular mt-2 grid grid-cols-2 gap-x-4 text-[11px] text-neutral-500">
        <div class="flex justify-between"><dt>cached</dt><dd data-ref="mem-cached">—</dd></div>
        <div class="flex justify-between"><dt>buffers</dt><dd data-ref="mem-buffers">—</dd></div>
        <div class="flex justify-between"><dt>available</dt><dd data-ref="mem-avail">—</dd></div>
        <div class="flex justify-between"><dt>swap</dt><dd data-ref="mem-swap">—</dd></div>
      </dl>
    </div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-white/5 px-3 py-1.5 text-[10px]">filesystems</div>
    <div data-ref="disks"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-white/5 px-3 py-1.5 text-[10px]">smart</div>
    <div data-ref="smart"></div>
  </section>
`;

export function createSystemPage(telemetry: Telemetry): { root: HTMLElement } {
  const root = document.createElement("div");
  root.className = "grid grid-cols-1 gap-4 p-4 xl:grid-cols-2 2xl:grid-cols-3";
  root.innerHTML = MARKUP;

  const ref = <T extends Element>(name: string): T => must<T>(`[data-ref="${name}"]`, root);

  const cpuModelEl = ref<HTMLElement>("cpu-model");
  const cpuTotalEl = ref<HTMLElement>("cpu-total");
  const cpuUserEl = ref<HTMLElement>("cpu-user");
  const cpuSysEl = ref<HTMLElement>("cpu-sys");
  const cpuIoEl = ref<HTMLElement>("cpu-io");
  const cpuIdleEl = ref<HTMLElement>("cpu-idle");
  const cpuCoresEl = ref<HTMLElement>("cpu-cores");
  const memUsedEl = ref<HTMLElement>("mem-used");
  const memTotalEl = ref<HTMLElement>("mem-total");
  const memBarEl = ref<HTMLElement>("mem-bar");
  const memCachedEl = ref<HTMLElement>("mem-cached");
  const memBuffersEl = ref<HTMLElement>("mem-buffers");
  const memAvailEl = ref<HTMLElement>("mem-avail");
  const memSwapEl = ref<HTMLElement>("mem-swap");
  const netsEl = ref<HTMLElement>("nets");
  const disksEl = ref<HTMLElement>("disks");
  const smartEl = ref<HTMLElement>("smart");

  const cpuSpark = new Sparkline(120, { max: 100, color: EMERALD });
  ref("cpu-spark").append(cpuSpark.element);

  // --- CPU cores ---------------------------------------------------------
  const coreBars: Array<{ bar: HTMLElement; label: HTMLElement }> = [];
  let coreCount = -1;

  function renderCores(perCore: number[]): void {
    if (perCore.length !== coreCount) {
      coreCount = perCore.length;
      coreBars.length = 0;
      cpuCoresEl.replaceChildren();
      for (let index = 0; index < perCore.length; index++) {
        const cell = document.createElement("div");
        const label = document.createElement("div");
        label.className = "tabular text-[9px] text-neutral-600";
        const track = document.createElement("div");
        track.className = "h-1.5 bg-white/10";
        const bar = document.createElement("div");
        bar.className = "h-full bg-emerald-500";
        bar.style.width = "0%";
        track.append(bar);
        cell.append(label, track);
        cpuCoresEl.append(cell);
        coreBars.push({ bar, label });
      }
    }

    perCore.forEach((value, index) => {
      const entry = coreBars[index];
      if (!entry) {
        return;
      }
      fill(entry.bar, value);
      entry.label.textContent = `${index}·${value.toFixed(0)}`;
    });
  }

  // --- Network -----------------------------------------------------------
  interface NetRow {
    rx: HTMLElement;
    tx: HTMLElement;
    total: HTMLElement;
    rxSpark: Sparkline;
    txSpark: Sparkline;
  }

  const netRows = new Map<string, NetRow>();
  let netSignature = "";

  function renderNetworks(networks: Network[]): void {
    const signature = networks
      .map((net) => net.iface)
      .sort()
      .join(",");

    if (signature !== netSignature) {
      netSignature = signature;
      netRows.clear();
      netsEl.replaceChildren();

      if (networks.length === 0) {
        netsEl.append(emptyNote("no interfaces reported by /proc/net/dev"));
      }

      for (const net of networks) {
        const row = document.createElement("div");
        row.className = "border-b border-white/5 px-3 py-2 last:border-b-0";

        const head = document.createElement("div");
        head.className = "flex items-baseline justify-between";
        const name = document.createElement("span");
        name.className = "text-neutral-300";
        name.textContent = net.iface;
        const total = document.createElement("span");
        total.className = "tabular text-[10px] text-neutral-600";
        head.append(name, total);

        const split = document.createElement("div");
        split.className = "mt-1 grid grid-cols-2 gap-3";

        const rxLabel = document.createElement("div");
        rxLabel.className = "tabular text-[11px] text-neutral-500";
        const rxValue = document.createElement("span");
        rxValue.className = "text-emerald-400";
        rxLabel.append("↓ ", rxValue);
        const rxSpark = new Sparkline(60, { color: EMERALD });

        const txLabel = document.createElement("div");
        txLabel.className = "tabular text-[11px] text-neutral-500";
        const txValue = document.createElement("span");
        txValue.className = "text-sky-400";
        txLabel.append("↑ ", txValue);
        const txSpark = new Sparkline(60, { color: SKY });

        const rxCell = document.createElement("div");
        rxCell.append(rxLabel, rxSpark.element);
        const txCell = document.createElement("div");
        txCell.append(txLabel, txSpark.element);
        split.append(rxCell, txCell);

        row.append(head, split);
        netsEl.append(row);

        netRows.set(net.iface, { rx: rxValue, tx: txValue, total, rxSpark, txSpark });
      }
    }

    for (const net of networks) {
      const row = netRows.get(net.iface);
      if (!row) {
        continue;
      }
      row.rx.textContent = rate(net.rx_bps);
      row.tx.textContent = rate(net.tx_bps);
      row.total.textContent = `Σ ↓${bytes(net.rx_bytes)} ↑${bytes(net.tx_bytes)}`;
      row.rxSpark.push(net.rx_bps);
      row.txSpark.push(net.tx_bps);
    }
  }

  // --- Filesystems -------------------------------------------------------
  interface DiskRow {
    bar: HTMLElement;
    usage: HTMLElement;
  }

  const diskRows = new Map<string, DiskRow>();
  let diskSignature = "";

  function renderDisks(disks: Disk[]): void {
    const signature = disks
      .map((disk) => disk.mount)
      .sort()
      .join(",");

    if (signature !== diskSignature) {
      diskSignature = signature;
      diskRows.clear();
      disksEl.replaceChildren();

      if (disks.length === 0) {
        disksEl.append(emptyNote("no real filesystems mounted"));
      }

      for (const disk of disks) {
        const row = document.createElement("div");
        row.className = "border-b border-white/5 px-3 py-2 last:border-b-0";

        const head = document.createElement("div");
        head.className = "flex items-baseline justify-between gap-3";
        const mount = document.createElement("span");
        mount.className = "truncate text-neutral-300";
        mount.textContent = disk.mount;
        const usage = document.createElement("span");
        usage.className = "tabular shrink-0 text-[11px] text-neutral-500";
        head.append(mount, usage);

        const track = document.createElement("div");
        track.className = "mt-1.5 h-1.5 bg-white/10";
        const bar = document.createElement("div");
        bar.className = "h-full bg-emerald-500";
        bar.style.width = "0%";
        track.append(bar);

        const meta = document.createElement("div");
        meta.className = "tabular mt-1 text-[10px] text-neutral-600";
        meta.textContent = `${disk.device} · ${disk.fs}`;

        row.append(head, track, meta);
        disksEl.append(row);
        diskRows.set(disk.mount, { bar, usage });
      }
    }

    for (const disk of disks) {
      const row = diskRows.get(disk.mount);
      if (!row) {
        continue;
      }
      fill(row.bar, disk.used_pct);
      row.usage.textContent = `${bytes(disk.used_bytes)} / ${bytes(disk.total_bytes)} · ${percent(disk.used_pct)}`;
    }
  }

  // --- SMART -------------------------------------------------------------
  interface SmartRow {
    health: HTMLElement;
    temp: HTMLElement;
    hours: HTMLElement;
  }

  const smartRows = new Map<string, SmartRow>();
  let smartSignature = "";

  function renderSmart(devices: Smart[]): void {
    const signature = devices
      .map((device) => device.device)
      .sort()
      .join(",");

    if (signature !== smartSignature) {
      smartSignature = signature;
      smartRows.clear();
      smartEl.replaceChildren();

      if (devices.length === 0) {
        smartEl.append(emptyNote("no SMART devices (smartctl missing or no disks)"));
      }

      for (const device of devices) {
        const row = document.createElement("div");
        row.className =
          "flex items-baseline justify-between gap-3 border-b border-white/5 px-3 py-2 last:border-b-0";

        const left = document.createElement("div");
        left.className = "min-w-0";
        const name = document.createElement("div");
        name.className = "truncate text-neutral-300";
        name.textContent = device.device;
        const model = document.createElement("div");
        model.className = "truncate text-[10px] text-neutral-600";
        model.textContent = device.model ?? "";
        left.append(name, model);

        const right = document.createElement("div");
        right.className = "tabular flex shrink-0 items-baseline gap-3 text-[11px]";
        const health = document.createElement("span");
        const temp = document.createElement("span");
        temp.className = "text-neutral-500";
        const hours = document.createElement("span");
        hours.className = "text-neutral-600";
        right.append(health, temp, hours);

        row.append(left, right);
        smartEl.append(row);
        smartRows.set(device.device, { health, temp, hours });
      }
    }

    for (const device of devices) {
      const row = smartRows.get(device.device);
      if (!row) {
        continue;
      }

      if (device.healthy === null) {
        row.health.textContent = "?";
        row.health.className = "text-neutral-600";
      } else {
        row.health.textContent = device.healthy ? "OK" : "FAIL";
        row.health.className = device.healthy ? "text-emerald-400" : "text-rose-500";
      }

      row.temp.textContent =
        device.temperature_c === null ? "" : `${device.temperature_c}°C`;
      row.hours.textContent =
        device.power_on_hours === null ? "" : `${device.power_on_hours}h`;
    }
  }

  // --- Frame -------------------------------------------------------------
  function render(snapshot: Snapshot): void {
    // Rolled rather than snapped: a headline figure that jumps every tick flickers.
    animateNumber(cpuTotalEl, snapshot.cpu.usage_pct, percent, 500);
    cpuSpark.push(snapshot.cpu.usage_pct);
    cpuModelEl.textContent = snapshot.cpu.model ?? "";
    cpuUserEl.textContent = `usr ${snapshot.cpu.user_pct.toFixed(0)}`;
    cpuSysEl.textContent = `sys ${snapshot.cpu.system_pct.toFixed(0)}`;
    cpuIoEl.textContent = `io ${snapshot.cpu.iowait_pct.toFixed(0)}`;
    cpuIdleEl.textContent = `idle ${snapshot.cpu.idle_pct.toFixed(0)}`;
    renderCores(snapshot.cpu.per_core);

    const memory = snapshot.memory;
    animateNumber(memUsedEl, memory.used_bytes, bytes, 500);
    memTotalEl.textContent = `of ${bytes(memory.total_bytes)} · ${percent(memory.used_pct)}`;
    fill(memBarEl, memory.used_pct);
    memCachedEl.textContent = bytes(memory.cached_bytes);
    memBuffersEl.textContent = bytes(memory.buffers_bytes);
    memAvailEl.textContent = bytes(memory.available_bytes);
    memSwapEl.textContent =
      memory.swap_total_bytes === 0
        ? "none"
        : `${bytes(memory.swap_used_bytes)} / ${bytes(memory.swap_total_bytes)}`;

    renderNetworks(snapshot.networks);
    renderDisks(snapshot.disks);
    renderSmart(snapshot.smart);
  }

  telemetry.onSnapshot(render);
  void telemetry.currentSnapshot().then((snapshot) => {
    if (snapshot) {
      render(snapshot);
    }
  });

  // SMART self-tests: a schedule + history card next to the live health badge.
  root.append(buildSmartPanel());

  return { root };
}
