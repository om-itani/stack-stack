/**
 * System dashboard: CPU, memory, network, filesystems and SMART.
 *
 * The page owns only its panels; the header status (host, uptime, link) lives in
 * the shell so it stays visible on every page. It subscribes to the shared
 * telemetry client and paints on every frame.
 *
 * Each of the CPU and Network cards can switch from the 1 Hz live sparkline to a
 * server-side history window (1h/6h/24h), which is sampled once a minute and
 * therefore survives the browser being closed.
 */

import { metricsApi } from "../api";
import type { HistoryRange, HistoryPoint } from "../api";
import { Sparkline } from "../spark";
import type { Telemetry } from "../telemetry";
import type { Disk, Network, Smart, Snapshot } from "../telemetry";
import { animateNumber, bytes, emptyNote, fill, must, percent, rate } from "../ui";
import { buildSmartPanel } from "./smartPanel";

const EMERALD = "#34d399";
const SKY = "#38bdf8";

/** Live is 1 Hz; a range is the once-a-minute history, so the window is wider. */
type Range = HistoryRange | "live";

const RANGES: Array<{ id: Range; label: string }> = [
  { id: "live", label: "live" },
  { id: "1h", label: "1h" },
  { id: "6h", label: "6h" },
  { id: "24h", label: "24h" },
];

const MARKUP = `
  <section class="panel col-span-full">
    <div class="panel-title flex items-center justify-between gap-4 border-b border-white/5 px-3 py-1.5 text-[10px]">
      <span>network</span>
      <span class="flex items-center gap-1" data-ref="net-range"></span>
    </div>
    <div data-ref="nets"></div>
    <div data-ref="net-history" class="hidden px-3 py-2">
      <div class="tabular mb-1 flex flex-wrap items-baseline gap-4 text-[11px] text-neutral-500">
        <span class="text-neutral-400">all interfaces</span>
        <span class="text-emerald-400">↓ <span data-ref="hist-rx">—</span></span>
        <span class="text-sky-400">↑ <span data-ref="hist-tx">—</span></span>
        <span class="ml-auto text-neutral-600" data-ref="hist-note"></span>
      </div>
      <div data-ref="net-history-sparks" class="flex flex-col gap-1"></div>
    </div>
  </section>

  <section class="panel flex h-[140px] flex-col overflow-hidden">
    <div class="panel-title flex shrink-0 items-baseline justify-between gap-3 border-b border-white/5 px-3 py-1.5 text-[10px]">
      <span>cpu</span>
      <span class="flex items-center gap-1" data-ref="cpu-range"></span>
      <span data-ref="cpu-model" class="truncate text-[10px] normal-case tracking-normal text-neutral-600"></span>
    </div>
    <div class="flex shrink-0 items-center gap-3 px-3 py-1.5">
      <div class="shrink-0">
        <div data-ref="cpu-total" class="tabular stat text-2xl leading-none">—</div>
        <div data-ref="cpu-load" class="tabular mt-0.5 text-[10px] text-neutral-500">load —</div>
      </div>
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
    <div class="flex shrink-0 items-center gap-3 px-3 text-[10px] text-neutral-600">
      <span data-ref="cpu-freq"></span>
      <span data-ref="cpu-temp"></span>
      <span class="ml-auto" data-ref="cpu-note"></span>
    </div>
    <div data-ref="cpu-cores" class="mt-1 flex min-h-0 flex-1 flex-wrap content-start gap-x-1 gap-y-0.5 overflow-hidden px-3 pb-1.5"></div>
  </section>

  <section class="panel flex h-[140px] flex-col overflow-hidden">
    <div class="panel-title shrink-0 border-b border-white/5 px-3 py-1.5 text-[10px]">memory</div>
    <div class="min-h-0 flex-1 px-3 py-1.5">
      <div class="flex items-baseline justify-between">
        <span data-ref="mem-used" class="tabular stat text-2xl leading-none">—</span>
        <span data-ref="mem-total" class="tabular text-neutral-500">—</span>
      </div>
      <div class="mt-1.5 h-2 bg-white/10"><div data-ref="mem-bar" class="h-full bg-emerald-500" style="width:0%"></div></div>
      <dl class="tabular mt-1.5 grid grid-cols-2 gap-x-4 text-[11px] text-neutral-500">
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
  const cpuLoadEl = ref<HTMLElement>("cpu-load");
  const cpuFreqEl = ref<HTMLElement>("cpu-freq");
  const cpuTempEl = ref<HTMLElement>("cpu-temp");
  const cpuNoteEl = ref<HTMLElement>("cpu-note");
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
  const netHistoryEl = ref<HTMLElement>("net-history");
  const disksEl = ref<HTMLElement>("disks");
  const smartEl = ref<HTMLElement>("smart");

  // 1440 samples so a 24h window (one point a minute) fits without truncation.
  const cpuSpark = new Sparkline(1440, { max: 100, color: EMERALD });
  ref("cpu-spark").append(cpuSpark.element);

  // --- range controls ----------------------------------------------------
  let cpuRange: Range = "live";
  let netRange: Range = "live";

  function rangeControl(
    host: HTMLElement,
    current: Range,
    onSelect: (range: Range) => void,
  ): void {
    host.replaceChildren();
    for (const range of RANGES) {
      const active = range.id === current;
      const control = document.createElement("button");
      control.type = "button";
      control.textContent = range.label;
      control.className = active
        ? "rounded border border-emerald-600 px-1.5 py-0.5 text-[10px] text-emerald-400"
        : "rounded border border-white/10 px-1.5 py-0.5 text-[10px] text-neutral-500 transition-colors duration-200 hover:text-neutral-200";
      control.addEventListener("click", () => {
        if (range.id !== current) {
          onSelect(range.id);
        }
      });
      host.append(control);
    }
  }

  const netHistoryRx = new Sparkline(1440, { color: EMERALD });
  const netHistoryTx = new Sparkline(1440, { color: SKY });
  const netHistorySparks = ref<HTMLElement>("net-history-sparks");
  netHistorySparks.append(netHistoryRx.element, netHistoryTx.element);
  const histRxEl = ref<HTMLElement>("hist-rx");
  const histTxEl = ref<HTMLElement>("hist-tx");
  const histNoteEl = ref<HTMLElement>("hist-note");

  function selectCpuRange(range: Range): void {
    cpuRange = range;
    rangeControl(ref("cpu-range"), cpuRange, selectCpuRange);
    cpuNoteEl.textContent = range === "live" ? "" : `history · ${range}`;
    if (range === "live") {
      cpuSpark.clear();
    } else {
      void loadHistory(range);
    }
  }

  function selectNetRange(range: Range): void {
    netRange = range;
    rangeControl(ref("net-range"), netRange, selectNetRange);
    netsEl.classList.toggle("hidden", range !== "live");
    netHistoryEl.classList.toggle("hidden", range === "live");
    if (range === "live") {
      netHistoryRx.clear();
      netHistoryTx.clear();
    } else {
      void loadHistory(range);
    }
  }

  let historyToken = 0;
  async function loadHistory(range: HistoryRange): Promise<void> {
    const token = ++historyToken;
    try {
      const body = await metricsApi.history(range);
      // A newer selection may have landed while this request was in flight.
      if (token !== historyToken) {
        return;
      }
      applyHistory(body.points, body.interval_ms, range);
    } catch {
      // The API layer already logged and toasted; leave the previous curve.
    }
  }

  function applyHistory(
    points: HistoryPoint[],
    intervalMs: number,
    range: HistoryRange,
  ): void {
    if (cpuRange !== "live" && cpuRange === range) {
      cpuSpark.set(points.map((point) => point.cpu_pct));
    }
    if (netRange !== "live" && netRange === range) {
      netHistoryRx.set(points.map((point) => point.rx_bps));
      netHistoryTx.set(points.map((point) => point.tx_bps));
      const mean = (pick: (point: HistoryPoint) => number): number =>
        points.length === 0 ? 0 : points.reduce((sum, point) => sum + pick(point), 0) / points.length;
      histRxEl.textContent = rate(mean((point) => point.rx_bps));
      histTxEl.textContent = rate(mean((point) => point.tx_bps));
      histNoteEl.textContent =
        points.length === 0
          ? "no history yet"
          : `${points.length} pts · ${Math.round(intervalMs / 1000)}s avg`;
      histNoteEl.classList.toggle("text-amber-400", points.length === 0);
    }
  }

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
        // `flex: 1 1 0` with a hard minimum keeps every core legible instead of
        // letting a high core count squeeze the bars into hairlines.
        cell.className = "min-w-[8px] flex-1 basis-0";
        const label = document.createElement("div");
        label.className = "tabular text-[9px] leading-none text-neutral-500";
        const track = document.createElement("div");
        track.className = "mt-0.5 h-1.5 rounded-sm bg-white/10";
        const bar = document.createElement("div");
        bar.className = "h-full rounded-sm bg-emerald-500";
        bar.style.width = "0%";
        track.append(bar);
        cell.append(track, label);
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
      entry.label.textContent = `${index}`;
      entry.label.title = `core ${index}: ${value.toFixed(0)}%`;
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
      // Only the live view appends to the per-interface sparklines; a range view
      // draws the aggregate history instead.
      if (netRange === "live") {
        row.rxSpark.push(net.rx_bps);
        row.txSpark.push(net.tx_bps);
      }
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
    cpuModelEl.textContent = snapshot.cpu.model ?? "";
    cpuLoadEl.textContent = `load ${snapshot.load.map((value) => value.toFixed(2)).join(" ")}`;
    cpuUserEl.textContent = `usr ${snapshot.cpu.user_pct.toFixed(0)}`;
    cpuSysEl.textContent = `sys ${snapshot.cpu.system_pct.toFixed(0)}`;
    cpuIoEl.textContent = `io ${snapshot.cpu.iowait_pct.toFixed(0)}`;
    cpuIdleEl.textContent = `idle ${snapshot.cpu.idle_pct.toFixed(0)}`;
    cpuFreqEl.textContent = snapshot.cpu.freq_mhz === null ? "" : `${snapshot.cpu.freq_mhz} MHz`;
    cpuTempEl.textContent = snapshot.cpu.temp_c === null ? "" : `${snapshot.cpu.temp_c}°C`;
    renderCores(snapshot.cpu.per_core);
    // Only the live view appends; a selected range owns the curve until it is
    // switched back to live.
    if (cpuRange === "live") {
      cpuSpark.push(snapshot.cpu.usage_pct);
    }

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

  rangeControl(ref("cpu-range"), cpuRange, selectCpuRange);
  rangeControl(ref("net-range"), netRange, selectNetRange);

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
