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
    try {
      const response = await fetch("/api/system/snapshot");
      if (!response.ok) {
        return null;
      }
      return (await response.json()) as Snapshot | null;
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
      let snapshot: Snapshot;
      try {
        snapshot = JSON.parse(event.data) as Snapshot;
      } catch {
        return;
      }
      for (const handler of this.handlers) {
        handler(snapshot);
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
  try {
    const response = await fetch("/api/health");
    if (!response.ok) {
      return "";
    }
    const body = (await response.json()) as { version?: string };
    return body.version ? `v${body.version}` : "";
  } catch {
    return "";
  }
}
