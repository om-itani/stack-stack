import "./style.css";
import { Sparkline } from "./spark";
import { Telemetry, fetchVersion } from "./telemetry";
import type { Disk, LinkState, Network, Smart, Snapshot } from "./telemetry";

const EMERALD = "#34d399";
const SKY = "#38bdf8";

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

function bytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  let scaled = value;
  let unit = 0;
  while (scaled >= 1024 && unit < BYTE_UNITS.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  const decimals = unit > 0 && scaled < 10 ? 1 : 0;
  return `${scaled.toFixed(decimals)} ${BYTE_UNITS[unit]}`;
}

function rate(value: number): string {
  return `${bytes(value)}/s`;
}

function uptime(seconds: number): string {
  if (!seconds) {
    return "—";
  }
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) {
    return `${days}d ${hours}h ${minutes}m`;
  }
  return hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
}

function percent(value: number): string {
  return `${value.toFixed(value < 10 ? 1 : 0)}%`;
}

function fill(bar: HTMLElement, value: number): void {
  const clamped = Math.min(100, Math.max(0, value));
  bar.style.width = `${clamped}%`;
  bar.className =
    clamped >= 90 ? "h-full bg-rose-500" : clamped >= 75 ? "h-full bg-amber-400" : "h-full bg-emerald-500";
}

function must<T extends Element>(selector: string): T {
  const found = document.querySelector<T>(selector);
  if (!found) {
    throw new Error(`missing element ${selector}`);
  }
  return found;
}

function emptyNote(text: string): HTMLElement {
  const note = document.createElement("div");
  note.className = "px-3 py-3 text-slate-600";
  note.textContent = text;
  return note;
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

const app = must<HTMLElement>("#app");
app.innerHTML = `
<header class="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-[#1e2733] bg-[#0d131b] px-4 py-2">
  <span class="font-bold tracking-[0.25em] text-emerald-400">STACK-STACK</span>
  <span id="host" class="text-slate-300">—</span>
  <span id="uptime" class="tabular text-slate-500">up —</span>
  <span id="load" class="tabular text-slate-500">load —</span>
  <span class="ml-auto flex items-center gap-2">
    <span id="frame" class="tabular text-slate-600"></span>
    <span id="version" class="text-slate-600"></span>
    <span id="dot" class="inline-block size-2 rounded-full bg-slate-600"></span>
    <span id="link" class="text-slate-400">connecting</span>
  </span>
</header>

<main class="grid grid-cols-1 gap-3 p-3 xl:grid-cols-2 2xl:grid-cols-3">
  <section class="panel">
    <div class="panel-title flex items-baseline justify-between gap-4 border-b border-[#1e2733] px-3 py-1.5 text-[10px]">
      <span>cpu</span>
      <span id="cpu-model" class="truncate text-[10px] normal-case tracking-normal text-slate-600"></span>
    </div>
    <div class="flex items-center gap-4 px-3 py-2">
      <div id="cpu-total" class="tabular text-3xl text-slate-100">—</div>
      <div class="min-w-0 flex-1">
        <div id="cpu-spark"></div>
        <div class="tabular mt-1 flex justify-between text-[10px] text-slate-500">
          <span id="cpu-user">usr —</span>
          <span id="cpu-sys">sys —</span>
          <span id="cpu-io">io —</span>
          <span id="cpu-idle">idle —</span>
        </div>
      </div>
    </div>
    <div id="cpu-cores" class="grid grid-cols-6 gap-x-2 gap-y-1 px-3 pb-3 sm:grid-cols-8 lg:grid-cols-10"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">memory</div>
    <div class="px-3 py-2">
      <div class="flex items-baseline justify-between">
        <span id="mem-used" class="tabular text-2xl text-slate-100">—</span>
        <span id="mem-total" class="tabular text-slate-500">—</span>
      </div>
      <div class="mt-2 h-2 bg-[#1e2733]"><div id="mem-bar" class="h-full bg-emerald-500" style="width:0%"></div></div>
      <dl class="tabular mt-2 grid grid-cols-2 gap-x-4 text-[11px] text-slate-500">
        <div class="flex justify-between"><dt>cached</dt><dd id="mem-cached">—</dd></div>
        <div class="flex justify-between"><dt>buffers</dt><dd id="mem-buffers">—</dd></div>
        <div class="flex justify-between"><dt>available</dt><dd id="mem-avail">—</dd></div>
        <div class="flex justify-between"><dt>swap</dt><dd id="mem-swap">—</dd></div>
      </dl>
    </div>
  </section>

  <section class="panel xl:col-span-2 2xl:col-span-1">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">network</div>
    <div id="nets"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">filesystems</div>
    <div id="disks"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">smart</div>
    <div id="smart"></div>
  </section>
</main>
`;

const hostEl = must<HTMLElement>("#host");
const uptimeEl = must<HTMLElement>("#uptime");
const loadEl = must<HTMLElement>("#load");
const frameEl = must<HTMLElement>("#frame");
const versionEl = must<HTMLElement>("#version");
const dotEl = must<HTMLElement>("#dot");
const linkEl = must<HTMLElement>("#link");
const cpuModelEl = must<HTMLElement>("#cpu-model");
const cpuTotalEl = must<HTMLElement>("#cpu-total");
const cpuUserEl = must<HTMLElement>("#cpu-user");
const cpuSysEl = must<HTMLElement>("#cpu-sys");
const cpuIoEl = must<HTMLElement>("#cpu-io");
const cpuIdleEl = must<HTMLElement>("#cpu-idle");
const cpuCoresEl = must<HTMLElement>("#cpu-cores");
const memUsedEl = must<HTMLElement>("#mem-used");
const memTotalEl = must<HTMLElement>("#mem-total");
const memBarEl = must<HTMLElement>("#mem-bar");
const memCachedEl = must<HTMLElement>("#mem-cached");
const memBuffersEl = must<HTMLElement>("#mem-buffers");
const memAvailEl = must<HTMLElement>("#mem-avail");
const memSwapEl = must<HTMLElement>("#mem-swap");
const netsEl = must<HTMLElement>("#nets");
const disksEl = must<HTMLElement>("#disks");
const smartEl = must<HTMLElement>("#smart");

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

const cpuSpark = new Sparkline(120, { max: 100, color: EMERALD });
must<HTMLElement>("#cpu-spark").append(cpuSpark.element);

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
      label.className = "tabular text-[9px] text-slate-600";
      const track = document.createElement("div");
      track.className = "h-1.5 bg-[#1e2733]";
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
    const ref = coreBars[index];
    if (!ref) {
      return;
    }
    fill(ref.bar, value);
    ref.label.textContent = `${index}·${value.toFixed(0)}`;
  });
}

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
      row.className = "border-b border-[#161d27] px-3 py-2 last:border-b-0";

      const head = document.createElement("div");
      head.className = "flex items-baseline justify-between";
      const name = document.createElement("span");
      name.className = "text-slate-300";
      name.textContent = net.iface;
      const total = document.createElement("span");
      total.className = "tabular text-[10px] text-slate-600";
      head.append(name, total);

      const split = document.createElement("div");
      split.className = "mt-1 grid grid-cols-2 gap-3";

      const rxLabel = document.createElement("div");
      rxLabel.className = "tabular text-[11px] text-slate-500";
      const rxValue = document.createElement("span");
      rxValue.className = "text-emerald-400";
      rxLabel.append("↓ ", rxValue);
      const rxSpark = new Sparkline(60, { color: EMERALD });

      const txLabel = document.createElement("div");
      txLabel.className = "tabular text-[11px] text-slate-500";
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
      row.className = "border-b border-[#161d27] px-3 py-2 last:border-b-0";

      const head = document.createElement("div");
      head.className = "flex items-baseline justify-between gap-3";
      const mount = document.createElement("span");
      mount.className = "truncate text-slate-300";
      mount.textContent = disk.mount;
      const usage = document.createElement("span");
      usage.className = "tabular shrink-0 text-[11px] text-slate-500";
      head.append(mount, usage);

      const track = document.createElement("div");
      track.className = "mt-1.5 h-1.5 bg-[#1e2733]";
      const bar = document.createElement("div");
      bar.className = "h-full bg-emerald-500";
      bar.style.width = "0%";
      track.append(bar);

      const meta = document.createElement("div");
      meta.className = "tabular mt-1 text-[10px] text-slate-600";
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
      row.className = "flex items-baseline justify-between gap-3 border-b border-[#161d27] px-3 py-2 last:border-b-0";

      const left = document.createElement("div");
      left.className = "min-w-0";
      const name = document.createElement("div");
      name.className = "truncate text-slate-300";
      name.textContent = device.device;
      const model = document.createElement("div");
      model.className = "truncate text-[10px] text-slate-600";
      model.textContent = device.model ?? "";
      left.append(name, model);

      const right = document.createElement("div");
      right.className = "tabular flex shrink-0 items-baseline gap-3 text-[11px]";
      const health = document.createElement("span");
      const temp = document.createElement("span");
      temp.className = "text-slate-500";
      const hours = document.createElement("span");
      hours.className = "text-slate-600";
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
      row.health.className = "text-slate-600";
    } else {
      row.health.textContent = device.healthy ? "OK" : "FAIL";
      row.health.className = device.healthy ? "text-emerald-400" : "text-rose-500";
    }

    row.temp.textContent = device.temperature_c === null ? "" : `${device.temperature_c}°C`;
    row.hours.textContent = device.power_on_hours === null ? "" : `${device.power_on_hours}h`;
  }
}

// ---------------------------------------------------------------------------
// Frame handling
// ---------------------------------------------------------------------------

function render(snapshot: Snapshot): void {
  document.title = `${snapshot.hostname} · stack-stack`;
  hostEl.textContent = snapshot.hostname;
  uptimeEl.textContent = `up ${uptime(snapshot.uptime_s)}`;
  loadEl.textContent = `load ${snapshot.load.map((v) => v.toFixed(2)).join(" ")}`;
  frameEl.textContent = new Date(snapshot.ts_ms).toLocaleTimeString();

  cpuTotalEl.textContent = percent(snapshot.cpu.usage_pct);
  cpuSpark.push(snapshot.cpu.usage_pct);
  cpuModelEl.textContent = snapshot.cpu.model ?? "";
  cpuUserEl.textContent = `usr ${snapshot.cpu.user_pct.toFixed(0)}`;
  cpuSysEl.textContent = `sys ${snapshot.cpu.system_pct.toFixed(0)}`;
  cpuIoEl.textContent = `io ${snapshot.cpu.iowait_pct.toFixed(0)}`;
  cpuIdleEl.textContent = `idle ${snapshot.cpu.idle_pct.toFixed(0)}`;
  renderCores(snapshot.cpu.per_core);

  const memory = snapshot.memory;
  memUsedEl.textContent = bytes(memory.used_bytes);
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

function setLinkState(state: LinkState): void {
  linkEl.textContent = state;
  dotEl.className =
    state === "live"
      ? "inline-block size-2 rounded-full bg-emerald-400"
      : state === "connecting"
        ? "inline-block size-2 rounded-full bg-amber-400"
        : "inline-block size-2 rounded-full bg-rose-500";
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

const telemetry = new Telemetry(setLinkState);
telemetry.onSnapshot(render);

void fetchVersion().then((version) => {
  versionEl.textContent = version;
});

void telemetry.currentSnapshot().then((snapshot) => {
  if (snapshot) {
    render(snapshot);
  }
});

telemetry.start();
