/**
 * Typed client for the non-telemetry API surface.
 *
 * The interfaces mirror the Rust wire types in `src/containers/model.rs`; keep
 * both sides in step when a field changes. Every call funnels through
 * `request`, so error bodies (`{"error": "..."}`) always surface as a thrown
 * `Error` with the server's message.
 */

export type ContainerSource = "app-store" | "manual";

export interface ContainerPort {
  private: number;
  public: number | null;
  protocol: string;
  ip: string | null;
}

export interface Container {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
  created: number;
  ports: ContainerPort[];
  source: ContainerSource;
  app_id: string | null;
  web_urls: string[];
}

export interface AppPort {
  container: number;
  host: number;
  protocol: string;
}

export interface App {
  id: string;
  name: string;
  description: string;
  category: string;
  tags: string[];
  icon: string;
  ports: AppPort[];
  web_port: number | null;
  compose: string;
  installed: boolean;
}

interface ActionResult {
  ok: boolean;
}

export interface OpResult {
  ok: boolean;
  output: string;
}

/** A hung or dead backend must not leave a button spinning forever. */
const REQUEST_TIMEOUT_MS = 15000;

/** Extra details our JSON APIs can attach to an error response. */
export interface Problem {
  field: string;
  message: string;
}

export interface ApiFailure extends Error {
  problems: Problem[];
}

function apiFailure(message: string, problems: Problem[] = []): ApiFailure {
  const error = new Error(message) as ApiFailure;
  error.problems = problems;
  return error;
}

/** POST/PUT with a JSON body, so callers never hand-roll headers. */
function jsonRequest<T>(path: string, method: "POST" | "PUT", body: unknown): Promise<T> {
  return request<T>(path, {
    method,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  let response: Response;
  try {
    response = await fetch(path, { ...init, signal: controller.signal });
  } catch (err) {
    throw new Error(
      err instanceof DOMException && err.name === "AbortError"
        ? "request timed out"
        : "network error",
    );
  } finally {
    window.clearTimeout(timer);
  }

  const text = await response.text();
  let body: unknown = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = null;
    }
  }

  if (!response.ok) {
    const record = body && typeof body === "object" ? (body as Record<string, unknown>) : null;
    const message = record && "error" in record
      ? String(record.error)
      : `request failed (${response.status})`;
    const problems =
      record && Array.isArray(record.problems) ? (record.problems as Problem[]) : [];
    throw apiFailure(message, problems);
  }

  return body as T;
}

function post<T = ActionResult>(path: string): Promise<T> {
  return request<T>(path, { method: "POST" });
}

// ---------------------------------------------------------------------------
// Router / network
// ---------------------------------------------------------------------------

export type InterfaceRole = "wan" | "lan" | "unused";
export type FirewallAction = "accept" | "drop" | "reject";
export type CertMode = "none" | "self-signed" | "imported";
export type InterfaceKind =
  | "ethernet"
  | "wifi"
  | "bridge"
  | "vlan"
  | "loopback"
  | "virtual";

export interface InterfaceConfig {
  name: string;
  role: InterfaceRole;
  address: string | null;
}

export interface VlanConfig {
  name: string;
  parent: string;
  id: number;
  address: string | null;
  comment: string;
}

export interface BridgeConfig {
  name: string;
  members: string[];
  address: string | null;
  stp: boolean;
  comment: string;
}

export interface WifiConfig {
  enabled: boolean;
  interface: string;
  ssid: string;
  passphrase: string;
  band: string;
  channel: number;
  country: string;
  bridge: string;
  security: string;
}

export interface FirewallRule {
  id: string;
  action: FirewallAction;
  protocol: string;
  port: number | null;
  source: string | null;
  comment: string;
  enabled: boolean;
}

export interface FirewallConfig {
  default_input: FirewallAction;
  allow_established: boolean;
  allow_icmp: boolean;
  management_ports: number[];
  rules: FirewallRule[];
}

export interface PortForward {
  id: string;
  protocol: string;
  external_port: number;
  internal_ip: string;
  internal_port: number;
  comment: string;
  enabled: boolean;
}

export interface DhcpReservation {
  mac: string;
  ip: string;
  name: string;
}

export interface DhcpConfig {
  enabled: boolean;
  interface: string;
  range_start: string;
  range_end: string;
  lease: string;
  gateway: string;
  dns_servers: string[];
  domain: string;
  reservations: DhcpReservation[];
}

export interface DnsRewrite {
  host: string;
  address: string;
  comment: string;
}

export interface DnsConfig {
  enabled: boolean;
  listen: string[];
  upstreams: string[];
  dnssec: boolean;
  qname_minimisation: boolean;
  rewrites: DnsRewrite[];
  blocked_domains: string[];
  blocklist_file: string | null;
  cache_min_ttl: number;
  cache_max_ttl: number;
  cache_size_mb: number;
  threads: number;
}

export interface SqmConfig {
  enabled: boolean;
  interface: string;
  download_kbps: number;
  upload_kbps: number;
  link_layer: string;
  overhead: number;
  diffserv4: boolean;
  ifb: string;
}

export interface TunnelIngress {
  hostname: string;
  service: string;
  comment: string;
}

export interface TunnelConfig {
  enabled: boolean;
  tunnel_id: string;
  credentials_file: string;
  ingress: TunnelIngress[];
  expose_ui: boolean;
}

export interface CertConfig {
  mode: CertMode;
  domains: string[];
  directory: string;
  active: string | null;
}

export interface NetworkConfig {
  interfaces: InterfaceConfig[];
  vlans: VlanConfig[];
  bridges: BridgeConfig[];
  wifi: WifiConfig;
  firewall: FirewallConfig;
  port_forwards: PortForward[];
  dhcp: DhcpConfig;
  dns: DnsConfig;
  sqm: SqmConfig;
  tunnel: TunnelConfig;
  certs: CertConfig;
}

export interface AddressInfo {
  cidr: string;
  scope: string;
}

export interface WifiInfo {
  ssid: string | null;
  mode: string | null;
  channel: number | null;
  frequency_mhz: number | null;
  ap_capable: boolean;
}

export interface InterfaceInfo {
  name: string;
  kind: InterfaceKind;
  operstate: string;
  carrier: boolean | null;
  speed_mbps: number | null;
  mac: string | null;
  mtu: number | null;
  addresses: AddressInfo[];
  wifi: WifiInfo | null;
}

export interface ServiceStatus {
  name: string;
  unit: string;
  state: string;
  active: boolean;
}

export interface FirewallStatus {
  loaded: boolean;
  rules: number;
  detail: string;
}

export interface SqmStatus {
  active: boolean;
  detail: string;
}

export interface NetworkStatus {
  services: ServiceStatus[];
  firewall: FirewallStatus;
  sqm: SqmStatus;
}

export interface CertInfo {
  name: string;
  cert_path: string;
  key_path: string;
  active: boolean;
  has_key: boolean;
  subject: string | null;
  issuer: string | null;
  sans: string[];
  not_after: string | null;
  days_remaining: number | null;
  key_matches: boolean | null;
}

export interface PendingInfo {
  generation: number;
  expires_in_s: number;
}

export interface NetworkState {
  config: NetworkConfig;
  problems: Problem[];
  config_error: string | null;
  interfaces: InterfaceInfo[];
  status: NetworkStatus;
  certificates: CertInfo[];
  pending: PendingInfo | null;
}

export interface PreviewBundle {
  nftables: string;
  dnsmasq: string;
  unbound: string;
  cloudflared: string;
  sqm: string;
}

export interface StepResult {
  subsystem: string;
  ok: boolean;
  detail: string;
}

export interface ApplyReport {
  steps: StepResult[];
  pending: PendingInfo | null;
}

export interface ConfirmResult {
  ok: boolean;
  generation: number;
}

export interface CertUpload {
  name: string;
  cert_pem: string;
  key_pem: string;
}

export const networkApi = {
  state: () => request<NetworkState>("/api/network"),
  save: (config: NetworkConfig) => jsonRequest<NetworkState>("/api/network", "PUT", config),
  preview: (config: NetworkConfig) =>
    jsonRequest<PreviewBundle>("/api/network/preview", "POST", config),
  apply: (config: NetworkConfig) =>
    jsonRequest<ApplyReport>("/api/network/apply", "POST", config),
  confirm: () => post<ConfirmResult>("/api/network/confirm"),
  certificates: () => request<CertInfo[]>("/api/network/certificates"),
  importCertificate: (upload: CertUpload) =>
    jsonRequest<CertInfo>("/api/network/certificates", "POST", upload),
  generateCertificate: (name: string) =>
    jsonRequest<CertInfo>("/api/network/certificates/self-signed", "POST", { name }),
  removeCertificate: (name: string) =>
    request<{ ok: boolean }>(`/api/network/certificates/${encodeURIComponent(name)}`, {
      method: "DELETE",
    }),
};

export const api = {
  containers: () => request<Container[]>("/api/containers"),

  start: (id: string) => post(`/api/containers/${encodeURIComponent(id)}/start`),
  stop: (id: string) => post(`/api/containers/${encodeURIComponent(id)}/stop`),
  restart: (id: string) => post(`/api/containers/${encodeURIComponent(id)}/restart`),
  remove: (id: string) =>
    request<ActionResult>(`/api/containers/${encodeURIComponent(id)}`, { method: "DELETE" }),

  catalog: () => request<App[]>("/api/apps/catalog"),
  install: (id: string) =>
    request<OpResult>(`/api/apps/${encodeURIComponent(id)}/install`, { method: "POST" }),
  uninstall: (id: string) =>
    request<OpResult>(`/api/apps/${encodeURIComponent(id)}`, { method: "DELETE" }),
};

/** WebSocket URL with the current page's scheme (http -> ws, https -> wss). */
export function wsUrl(path: string): string {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}${path}`;
}

// ---------------------------------------------------------------------------
// Virtualization (KVM/QEMU + LXC)
// ---------------------------------------------------------------------------

export type GuestImageKind = "vm" | "lxc";
export type NetMode = "dhcp" | "static" | "none";
export type Machine = "q35" | "pc";
export type Firmware = "bios" | "uefi";
export type DiskFormat = "qcow2" | "raw";
export type DiskCache = "none" | "writeback" | "directsync" | "unsafe";
export type AioMode = "io_uring" | "native" | "threads";
export type CpuModel = "host" | "max" | "qemu64";
export type GuestKind = "vm" | "ct";

export interface GuestImage {
  id: string;
  name: string;
  kind: GuestImageKind;
  category: string;
  description: string;
  arch: string;
  url: string | null;
  sha256: string | null;
  format: string | null;
  default_user: string | null;
  min_disk_gb: number;
  min_memory_mb: number;
  lxc_dist: string | null;
  lxc_release: string | null;
  lxc_mirror: string | null;
  icon: string;
  tags: string[];
  cached: boolean;
}

export interface CloudInitConfig {
  enabled: boolean;
  hostname: string;
  username: string;
  password: string;
  ssh_keys: string[];
  ip_mode: NetMode;
  address: string | null;
  gateway: string;
  dns: string[];
}

export interface CpuTopology {
  sockets: number;
  cores: number;
  threads: number;
}

export interface VmConfig {
  id: string;
  name: string;
  description: string;
  image: string | null;
  arch: string;
  machine: Machine;
  firmware: Firmware;
  cpu_model: CpuModel;
  cpu_features: string[];
  vcpus: number;
  cpu_topology: CpuTopology | null;
  memory_mb: number;
  balloon_mb: number | null;
  hugepages: boolean;
  mem_prealloc: boolean;
  disk_gb: number;
  disk_format: DiskFormat;
  disk_cache: DiskCache;
  aio: AioMode;
  discard: boolean;
  bridge: string;
  mac: string | null;
  multiqueue: boolean;
  vnc_enabled: boolean;
  vnc_port: number | null;
  vnc_bind: string;
  serial_console: boolean;
  nested: boolean;
  tpm: boolean;
  guest_agent: boolean;
  rng: boolean;
  cpuset: string | null;
  autostart: boolean;
  cloud_init: CloudInitConfig;
  extra_args: string[];
  tags: string[];
  comment: string;
}

export interface BindMount {
  source: string;
  target: string;
  readonly: boolean;
}

export interface CtConfig {
  id: string;
  name: string;
  hostname: string;
  description: string;
  template: string;
  arch: string;
  cores: number;
  memory_mb: number;
  swap_mb: number;
  rootfs_gb: number;
  unprivileged: boolean;
  nesting: boolean;
  bridge: string;
  ip_mode: NetMode;
  address: string | null;
  gateway: string;
  dns: string[];
  mac: string | null;
  password: string;
  ssh_keys: string[];
  autostart: boolean;
  startup_order: number;
  startup_delay: number;
  binds: BindMount[];
  cap_drop: string[];
  tags: string[];
  comment: string;
}

export interface GuestDefaults {
  storage_dir: string;
  images_dir: string;
  bridge: string;
  arch: string;
  disk_format: DiskFormat;
  vnc_bind: string;
  vnc_port_start: number;
}

export interface VirtualizationConfig {
  defaults: GuestDefaults;
  vms: VmConfig[];
  cts: CtConfig[];
}

export interface GuestStatus {
  id: string;
  kind: GuestKind;
  name: string;
  state: string;
  pid: number | null;
  memory_bytes: number | null;
  cpu_seconds: number | null;
  ip: string | null;
  autostart: boolean;
}

export interface CpuInfo {
  vendor: string | null;
  model: string | null;
  logical_cpus: number;
  physical_cores: number | null;
  virtualization_flag: string | null;
  has_aes: boolean;
  has_avx2: boolean;
}

export interface ToolInfo {
  program: string;
  version: string;
}

export interface HostCapabilities {
  arch: string;
  kvm: boolean;
  nested: boolean;
  cpu: CpuInfo;
  memory_mb: number;
  hugepages_2m_total: number;
  hugepages_2m_free: number;
  qemu: ToolInfo | null;
  accelerators: string[];
  lxc: ToolInfo | null;
  subuid_mapped: boolean;
  bridges: string[];
  uefi: boolean;
}

export interface VirtState {
  config: VirtualizationConfig;
  problems: Problem[];
  config_error: string | null;
  catalog: GuestImage[];
  host: HostCapabilities;
  statuses: GuestStatus[];
}

export interface VmPreview {
  qemu: string;
  uuid: string;
  vnc: string | null;
  user_data: string | null;
  meta_data: string | null;
  network_config: string | null;
}

export interface CtPreview {
  lxc: string;
  resolv_conf: string | null;
}

export interface ActionReport {
  ok: boolean;
  output: string;
  warnings: string[];
  disk: string | null;
  seed: string | null;
}

export const virtApi = {
  state: () => request<VirtState>("/api/virt"),
  save: (config: VirtualizationConfig) => jsonRequest<VirtState>("/api/virt", "PUT", config),

  previewVm: (vm: VmConfig) => jsonRequest<VmPreview>("/api/virt/preview/vm", "POST", vm),
  previewCt: (ct: CtConfig) => jsonRequest<CtPreview>("/api/virt/preview/ct", "POST", ct),

  createVm: (vm: VmConfig) => jsonRequest<ActionReport>("/api/virt/vms", "POST", vm),
  createCt: (ct: CtConfig) => jsonRequest<ActionReport>("/api/virt/cts", "POST", ct),

  vmStart: (id: string) => post<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/start`),
  vmStop: (id: string) => post<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/stop`),
  vmShutdown: (id: string) => post<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/shutdown`),
  vmReboot: (id: string) => post<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/reboot`),
  vmDestroy: (id: string) => post<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/destroy`),
  vmDelete: (id: string) =>
    request<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}`, { method: "DELETE" }),
  vmSnapshot: (id: string, action: string, name?: string) =>
    jsonRequest<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/snapshot`, "POST", {
      action,
      name: name ?? null,
    }),
  vmResize: (id: string, diskGb: number) =>
    jsonRequest<ActionReport>(`/api/virt/vms/${encodeURIComponent(id)}/resize`, "POST", {
      disk_gb: diskGb,
    }),

  ctStart: (id: string) => post<ActionReport>(`/api/virt/cts/${encodeURIComponent(id)}/start`),
  ctStop: (id: string) => post<ActionReport>(`/api/virt/cts/${encodeURIComponent(id)}/stop`),
  ctKill: (id: string) => post<ActionReport>(`/api/virt/cts/${encodeURIComponent(id)}/kill`),
  ctDelete: (id: string) =>
    request<ActionReport>(`/api/virt/cts/${encodeURIComponent(id)}`, { method: "DELETE" }),
  ctPassword: (id: string, password: string) =>
    jsonRequest<ActionReport>(`/api/virt/cts/${encodeURIComponent(id)}/password`, "POST", {
      password,
    }),
};

/** A blank VM with the same defaults the backend uses. */
export function blankVm(): VmConfig {
  return {
    id: "",
    name: "",
    description: "",
    image: null,
    arch: "amd64",
    machine: "q35",
    firmware: "bios",
    cpu_model: "host",
    cpu_features: [],
    vcpus: 2,
    cpu_topology: null,
    memory_mb: 2048,
    balloon_mb: null,
    hugepages: false,
    mem_prealloc: false,
    disk_gb: 20,
    disk_format: "qcow2",
    disk_cache: "none",
    aio: "io_uring",
    discard: true,
    bridge: "br0",
    mac: null,
    multiqueue: true,
    vnc_enabled: true,
    vnc_port: null,
    vnc_bind: "127.0.0.1",
    serial_console: true,
    nested: false,
    tpm: false,
    guest_agent: true,
    rng: true,
    cpuset: null,
    autostart: false,
    cloud_init: {
      enabled: true,
      hostname: "",
      username: "stack",
      password: "",
      ssh_keys: [],
      ip_mode: "dhcp",
      address: null,
      gateway: "",
      dns: [],
    },
    extra_args: [],
    tags: [],
    comment: "",
  };
}

/** A blank LXC container with the same defaults the backend uses. */
export function blankCt(): CtConfig {
  return {
    id: "",
    name: "",
    hostname: "",
    description: "",
    template: "",
    arch: "amd64",
    cores: 2,
    memory_mb: 1024,
    swap_mb: 512,
    rootfs_gb: 8,
    unprivileged: true,
    nesting: false,
    bridge: "br0",
    ip_mode: "dhcp",
    address: null,
    gateway: "",
    dns: [],
    mac: null,
    password: "",
    ssh_keys: [],
    autostart: false,
    startup_order: 0,
    startup_delay: 0,
    binds: [],
    cap_drop: [],
    tags: [],
    comment: "",
  };
}
