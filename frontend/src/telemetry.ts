/**
 * Telemetry client.
 *
 * Mirrors `src/system/metrics.rs::Snapshot` field for field. The socket is
 * server-push only: after the first REST fetch the client never polls, it just
 * applies the frame the backend already computed.
 */

export interface Cpu {
  usage_pct: number;
  user_pct: number;
  system_pct: number;
  iowait_pct: number;
  idle_pct: number;
  cores: number;
  per_core: number[];
  model: string | null;
}

export interface Memory {
  total_bytes: number;
  used_bytes: number;
  available_bytes: number;
  cached_bytes: number;
  buffers_bytes: number;
  swap_total_bytes: number;
  swap_used_bytes: number;
  used_pct: number;
}

export interface Disk {
  device: string;
  mount: string;
  fs: string;
  total_bytes: number;
  used_bytes: number;
  available_bytes: number;
  used_pct: number;
}

export interface Network {
  iface: string;
  rx_bytes: number;
  tx_bytes: number;
  rx_bps: number;
  tx_bps: number;
}

export interface Smart {
  device: string;
  model: string | null;
  healthy: boolean | null;
  temperature_c: number | null;
  power_on_hours: number | null;
}

export interface Snapshot {
  ts_ms: number;
  hostname: string;
  uptime_s: number;
  load: [number, number, number];
  cpu: Cpu;
  memory: Memory;
  disks: Disk[];
  networks: Network[];
  smart: Smart[];
}

export type LinkState = "connecting" | "live" | "down";
export type SnapshotHandler = (snapshot: Snapshot) => void;

/** A hung backend must not wedge the first paint or the version tag forever. */
const FETCH_TIMEOUT_MS = 5000;

/**
 * Shallow shape check. `JSON.parse` only proves the frame is valid JSON, not that
 * it is a `Snapshot`; without this a wrong-shape frame renders `undefined`/`NaN`
 * silently. Checking the top-level skeleton is enough to reject anything that
 * would break the renderers.
 */
function isSnapshot(value: unknown): value is Snapshot {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const s = value as Record<string, unknown>;
  return (
    typeof s.ts_ms === "number" &&
    typeof s.hostname === "string" &&
    typeof s.uptime_s === "number" &&
    Array.isArray(s.load) &&
    typeof s.cpu === "object" &&
    s.cpu !== null &&
    typeof s.memory === "object" &&
    s.memory !== null &&
    Array.isArray(s.disks) &&
    Array.isArray(s.networks) &&
    Array.isArray(s.smart)
  );
}

/** `fetch` with a hard timeout, returning `null` on any failure or timeout. */
async function fetchWithTimeout(path: string): Promise<Response | null> {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  try {
    return await fetch(path, { signal: controller.signal });
  } catch {
    return null;
  } finally {
    window.clearTimeout(timer);
  }
}

export class Telemetry {
  private readonly handlers = new Set<SnapshotHandler>();
  private socket: WebSocket | null = null;
  private retries = 0;
  private stopped = false;

  constructor(private readonly onState: (state: LinkState) => void) {}

  onSnapshot(handler: SnapshotHandler): void {
    this.handlers.add(handler);
  }

  start(): void {
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    this.socket?.close();
  }

  /** One-shot fetch so the first paint does not wait for a socket round trip. */
  async currentSnapshot(): Promise<Snapshot | null> {
    const response = await fetchWithTimeout("/api/system/snapshot");
    if (!response?.ok) {
      return null;
    }
    try {
      const body: unknown = await response.json();
      return isSnapshot(body) ? body : null;
    } catch {
      return null;
    }
  }

  private connect(): void {
    if (this.stopped) {
      return;
    }
    this.onState(this.retries === 0 ? "connecting" : "down");

    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const socket = new WebSocket(`${scheme}://${location.host}/api/ws/telemetry`);
    this.socket = socket;

    socket.onopen = () => {
      this.retries = 0;
      this.onState("live");
    };

    socket.onmessage = (event: MessageEvent<string>) => {
      if (typeof event.data !== "string") {
        return;
      }
      let parsed: unknown = null;
      try {
        parsed = JSON.parse(event.data);
      } catch {
        return;
      }
      if (!isSnapshot(parsed)) {
        return;
      }
      for (const handler of this.handlers) {
        handler(parsed);
      }
    };

    socket.onclose = () => {
      this.onState("down");
      this.scheduleReconnect();
    };

    // onerror is always followed by onclose; closing here just speeds it up.
    socket.onerror = () => socket.close();
  }

  private scheduleReconnect(): void {
    if (this.stopped) {
      return;
    }
    const delay = Math.min(1000 * 2 ** this.retries, 15000);
    this.retries += 1;
    window.setTimeout(() => this.connect(), delay);
  }
}

export async function fetchVersion(): Promise<string> {
  const response = await fetchWithTimeout("/api/health");
  if (!response?.ok) {
    return "";
  }
  try {
    const body = (await response.json()) as { version?: unknown };
    return typeof body.version === "string" && body.version ? `v${body.version}` : "";
  } catch {
    return "";
  }
}
