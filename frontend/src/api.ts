/**
 * Typed client for the non-telemetry API surface.
 *
 * The interfaces mirror the Rust wire types in `src/containers/model.rs`; keep
 * both sides in step when a field changes. Every call funnels through
 * `request`, so error bodies (`{"error": "..."}`) always surface as a thrown
 * `Error` with the server's message.
 */

import { showToast } from "./toast";

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
  /** Compose project this container belongs to, when Docker reports one. */
  stack: string | null;
  web_urls: string[];
}

/** An operator-authored compose stack and the exact YAML on disk. */
export interface StackInfo {
  name: string;
  compose: string;
}

export interface StackResult {
  ok: boolean;
  output: string;
}

export const stacksApi = {
  list: () => request<StackInfo[]>("/api/containers/stacks"),
  create: (name: string, compose: string) =>
    jsonRequest<StackResult>("/api/containers/stacks", "POST", { name, compose }),
  remove: (name: string, force = false) =>
    request<StackResult>(
      `/api/containers/stacks/${encodeURIComponent(name)}${force ? "?force=true" : ""}`,
      { method: "DELETE" },
    ),
};

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

export interface TestResult {
  ok: boolean;
  detail: string;
}

export interface OkDetail {
  ok: boolean;
  detail?: string;
  count?: number;
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
  /** Machine code from the backend (`error`), e.g. "NotFound". */
  code?: string;
  /** Extra diagnostic context from the backend (`details`). */
  details?: string;
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
  // Fold a caller-supplied signal (used to cancel a superseded search) into the
  // timeout signal, so an external abort and the deadline are distinguishable.
  const external = init?.signal ?? null;
  const signal = external ? AbortSignal.any([external, controller.signal]) : controller.signal;

  let response: Response;
  try {
    response = await fetch(path, { ...init, signal });
  } catch (err) {
    // A caller cancellation is reported as an AbortError so callers can ignore
    // it; the internal deadline is a timeout; anything else is a network fault.
    if (external?.aborted) {
      const abort = new Error("aborted");
      abort.name = "AbortError";
      throw abort;
    }
    const message =
      err instanceof DOMException && err.name === "AbortError"
        ? "request timed out"
        : "network error";
    console.error("[API Error]", { path, message, error: err });
    showToast({ tone: "error", title: "Backend unreachable", message: `${path}: ${message}` });
    throw new Error(message);
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
    const isAuthPath = path.startsWith("/api/auth/");
    // A 401 anywhere other than the auth endpoints means the session expired or
    // was dropped; the shell listens for this and returns to the login gate.
    if (response.status === 401 && !isAuthPath) {
      window.dispatchEvent(new CustomEvent("stack-stack:unauthorized"));
    }

    const record = body && typeof body === "object" ? (body as Record<string, unknown>) : null;
    // Standard shape is { error: code, message, details }; older/manual error
    // bodies carry only { error: <text> }, so `message` falls back to it.
    const code = record && typeof record.error === "string" ? record.error : undefined;
    const message =
      record && typeof record.message === "string"
        ? String(record.message)
        : code ?? `request failed (${response.status})`;
    const details =
      record && typeof record.details === "string" ? String(record.details) : undefined;
    const problems =
      record && Array.isArray(record.problems) ? (record.problems as Problem[]) : [];

    // Every failure is logged with enough context to debug from the console.
    console.error("[API Error]", {
      path,
      status: response.status,
      code,
      message,
      details,
      body,
    });
    // Auth failures are shown inline by the login gate, and a 401 is handled by
    // the session-expiry redirect; neither needs a toast.
    if (!isAuthPath && response.status !== 401) {
      showToast({
        tone: "error",
        title: `${message}`,
        message: details ?? `${response.status} ${response.statusText}`.trim(),
      });
    }

    const failure = apiFailure(message, problems);
    failure.code = code;
    failure.details = details;
    throw failure;
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

// ---------------------------------------------------------------------------
// Auth / login gate
// ---------------------------------------------------------------------------

export interface AuthStatus {
  setup_required: boolean;
  authenticated: boolean;
  must_change: boolean;
  error?: string;
}

export const authApi = {
  /** Exempt from the gate, so it doubles as the boot check. */
  session: () => request<AuthStatus>("/api/auth/session"),
  setup: (password: string) =>
    jsonRequest<AuthStatus>("/api/auth/setup", "POST", { password }),
  login: (password: string) =>
    jsonRequest<AuthStatus>("/api/auth/login", "POST", { password }),
  logout: () => post<AuthStatus>("/api/auth/logout"),
  changePassword: (current: string, newPassword: string) =>
    jsonRequest<AuthStatus>("/api/auth/password", "POST", {
      current,
      new_password: newPassword,
    }),
};

// ---------------------------------------------------------------------------
// WireGuard
// ---------------------------------------------------------------------------

export interface WgPeer {
  name: string;
  public_key: string;
  allowed_ips: string[];
  dns: string | null;
  persistent_keepalive: number;
  enabled: boolean;
}

export interface WgConfig {
  enabled: boolean;
  interface: string;
  address_cidr: string;
  listen_port: number;
  mtu: number;
  peers: WgPeer[];
}

export interface WgPeerStatus {
  public_key: string;
  endpoint: string;
  allowed_ips: string;
  latest_handshake: number;
  rx_bytes: number;
  tx_bytes: number;
}

export interface WgInterfaceStatus {
  public_key: string;
  listen_port: number;
  peers: WgPeerStatus[];
}

export interface WgState {
  config: WgConfig;
  problems: Problem[];
  config_error: string | null;
  preview: string;
  has_key: boolean;
  public_key: string | null;
  public_key_error: string | null;
  status: WgInterfaceStatus | null;
  status_error: string | null;
  endpoint: string | null;
}

export interface WgApplyReport {
  state: WgState;
  steps: StepResult[];
}

export interface GeneratedKey {
  private_key: string;
  public_key: string;
}

export interface ClientExport {
  config: string;
  qr: string | null;
  qr_error: string | null;
}

export const wireguardApi = {
  state: (live = true) => request<WgState>(`/api/wireguard?live=${live}`),
  save: (config: WgConfig) => jsonRequest<WgState>("/api/wireguard/config", "PUT", config),
  apply: (config: WgConfig) =>
    jsonRequest<WgApplyReport>("/api/wireguard/apply", "POST", config),
  generateKey: (asServer: boolean) =>
    jsonRequest<GeneratedKey>("/api/wireguard/key", "POST", { as_server: asServer }),
  setServerKey: (privateKey: string) =>
    jsonRequest<{ ok: boolean }>("/api/wireguard/key/server", "POST", privateKey),
  exportClient: (peerName: string, address: string) =>
    jsonRequest<ClientExport>("/api/wireguard/export", "POST", {
      peer_name: peerName,
      address,
    }),
  addPeer: (peer: WgPeer) => jsonRequest<WgState>("/api/wireguard/peers", "POST", peer),
  removePeer: (name: string) =>
    request<WgState>(`/api/wireguard/peers/${encodeURIComponent(name)}`, { method: "DELETE" }),
};

// ---------------------------------------------------------------------------
// Logs (journald)
// ---------------------------------------------------------------------------

export interface UnitInfo {
  unit: string;
  active: string;
  sub: string;
  description: string;
}

export interface LogEntry {
  ts_ms: number;
  priority: number;
  unit: string;
  message: string;
}

export interface LogsState {
  units: UnitInfo[];
  units_error: string | null;
  entries: LogEntry[];
  entries_error: string | null;
  available: boolean;
  priorities: [string, string][];
}

export const logsApi = {
  state: (unit = "", lines = 200, priority = "") =>
    request<LogsState>(
      `/api/logs?unit=${encodeURIComponent(unit)}&lines=${lines}&priority=${encodeURIComponent(priority)}`,
    ),
  followUrl: (unit: string, priority: string) =>
    wsUrl(
      `/api/logs/ws?unit=${encodeURIComponent(unit)}&priority=${encodeURIComponent(priority)}`,
    ),
};

// ---------------------------------------------------------------------------
// Dynamic DNS (Cloudflare)
// ---------------------------------------------------------------------------

export interface DdnsConfig {
  enabled: boolean;
  zone_id: string;
  record_names: string[];
  proxied: boolean;
  interval_min: number;
  ttl: number;
  last_seen_ip: string | null;
  last_ok_at: number | null;
}

export interface DdnsState {
  config: DdnsConfig;
  problems: Problem[];
  config_error: string | null;
  has_token: boolean;
  wan_ip: string | null;
}

export interface UpdateOutcome {
  name: string;
  changed: boolean;
  ok: boolean;
  detail: string;
}

export interface UpdateReport {
  outcomes: UpdateOutcome[];
  state: DdnsState;
}

export const ddnsApi = {
  state: () => request<DdnsState>("/api/ddns"),
  saveConfig: (config: DdnsConfig) =>
    jsonRequest<DdnsState>("/api/ddns/config", "PUT", config),
  saveToken: (apiToken: string) =>
    jsonRequest<{ ok: boolean }>("/api/ddns/token", "POST", { api_token: apiToken }),
  update: () => post<UpdateReport>("/api/ddns/update"),
};

// ---------------------------------------------------------------------------
// SMART self-tests
// ---------------------------------------------------------------------------

export type SmartKind = "short" | "long";

export interface DevicePlan {
  device: string;
  /** `smartctl -d` type, e.g. `sat`; detected on first use. */
  device_type: string | null;
  short_weekday: number | null;
  short_at: string;
  long_day: number | null;
  long_at: string;
}

export interface SmartConfig {
  devices: DevicePlan[];
}

export interface TestProgress {
  running: boolean;
  /** `short`, `long`, or the raw description, when a test is in flight. */
  running_kind: string | null;
  percent_remaining: number | null;
  percent_complete: number | null;
  detail: string;
  last_result: string | null;
  last_percent: string | null;
}

export interface DeviceProgress extends TestProgress {
  device: string;
}

export interface TestRecord {
  device: string;
  kind: SmartKind;
  started_ms: number;
  accepted: boolean;
  detail: string;
}

export interface SmartState {
  config: SmartConfig;
  problems: Problem[];
  config_error: string | null;
  history: TestRecord[];
  progress: DeviceProgress[];
  available: boolean;
  running: boolean;
  /** Disks smartctl can see that are not scheduled yet. */
  detected: string[];
}

/** A start attempt plus the state read back afterwards. */
export interface SmartStartResult extends TestProgress {
  accepted: boolean;
  detail: string;
}

export interface SmartAbortResult extends TestProgress {
  ok: boolean;
  detail: string;
}

export const smartApi = {
  state: () => request<SmartState>("/api/smart"),
  saveConfig: (config: SmartConfig) =>
    jsonRequest<SmartState>("/api/smart/config", "PUT", config),
  start: (device: string, kind: SmartKind) =>
    jsonRequest<SmartStartResult>(`/api/smart/${encodeURIComponent(device)}`, "POST", kind),
  /** The running self-test's state and the last completed one. */
  selftest: (device: string) =>
    request<TestProgress>(`/api/smart/selftest?device=${encodeURIComponent(device)}`),
  abort: (device: string) =>
    jsonRequest<SmartAbortResult>("/api/smart/selftest/abort", "POST", { device }),
  progress: (device: string) =>
    request<TestProgress>(`/api/smart/progress?device=${encodeURIComponent(device)}`),
};

// ---------------------------------------------------------------------------
// Telemetry history (1h / 6h / 24h graphs)
// ---------------------------------------------------------------------------

export type HistoryRange = "1h" | "6h" | "24h";

export interface HistoryPoint {
  ts_ms: number;
  cpu_pct: number;
  mem_pct: number;
  rx_bps: number;
  tx_bps: number;
}

export interface HistoryResponse {
  range: string;
  interval_ms: number;
  total: number;
  points: HistoryPoint[];
}

export const metricsApi = {
  history: (range: HistoryRange) =>
    request<HistoryResponse>(`/api/metrics/history?range=${range}`),
};

// ---------------------------------------------------------------------------
// Backups (restic)
// ---------------------------------------------------------------------------

export type Cadence = "hourly" | "daily" | "weekly";

export interface Schedule {
  cadence: Cadence;
  at: string;
  weekday: number;
}

export interface Retention {
  keep_last: number;
  keep_daily: number;
  keep_weekly: number;
  keep_monthly: number;
  keep_yearly: number;
}

export interface JobSources {
  docker_volumes: boolean;
  compose_apps: boolean;
  virt_guests: boolean;
  documents: boolean;
  certs_ca: boolean;
  file_roots: boolean;
  custom: string[];
}

export type BackupBackend =
  | { kind: "local"; path: string }
  | { kind: "s3"; endpoint: string; bucket: string }
  | { kind: "sftp"; target: string };

export interface BackupJob {
  id: string;
  name: string;
  enabled: boolean;
  sources: JobSources;
  excludes: string[];
  schedule: Schedule;
  retention: Retention;
  backend: BackupBackend;
  secret_ref: string;
}

export interface BackupRun {
  job_id: string;
  name: string;
  ts_ms: number;
  ok: boolean;
  duration_ms: number;
  snapshot_id: string | null;
  bytes: number | null;
  detail: string;
}

export interface BackupState {
  config: { jobs: BackupJob[] };
  problems: Problem[];
  config_error: string | null;
  history: BackupRun[];
  secrets_configured: string[];
  next_runs: Record<string, number>;
  running: boolean;
}

export interface Snapshot {
  id: string;
  short_id: string;
  time: string;
  hostname: string;
  paths: string[];
  tags: string[];
}

export interface LsEntry {
  path: string;
  name: string;
  kind: string;
  size: number;
  mtime: string;
}

export const backupApi = {
  state: () => request<BackupState>("/api/backup"),
  saveConfig: (config: { jobs: BackupJob[] }) =>
    jsonRequest<BackupState>("/api/backup/config", "PUT", config),
  run: (id: string) => post<BackupState>(`/api/backup/jobs/${encodeURIComponent(id)}/run`),
  verify: (id: string) =>
    post<OkDetail>(`/api/backup/jobs/${encodeURIComponent(id)}/verify`),
  saveSecrets: (
    id: string,
    resticPassword: string,
    awsAccessKeyId: string | null,
    awsSecretAccessKey: string | null,
    replace: boolean,
  ) =>
    jsonRequest<OkDetail>(`/api/backup/jobs/${encodeURIComponent(id)}/secrets`, "POST", {
      restic_password: resticPassword,
      aws_access_key_id: awsAccessKeyId,
      aws_secret_access_key: awsSecretAccessKey,
      replace,
    }),
  snapshots: (id: string) =>
    request<Snapshot[]>(`/api/backup/jobs/${encodeURIComponent(id)}/snapshots`),
  browse: (id: string, snapshot: string, path: string) =>
    request<LsEntry[]>(
      `/api/backup/jobs/${encodeURIComponent(id)}/snapshots/${encodeURIComponent(snapshot)}/ls?path=${encodePathSegment(path)}`,
    ),
  restore: (id: string, snapshot: string, target: string, include: string | null) =>
    jsonRequest<OkDetail>(`/api/backup/jobs/${encodeURIComponent(id)}/restore`, "POST", {
      snapshot,
      target,
      include,
    }),
  downloadUrl: (id: string, snapshot: string, path: string) =>
    `/api/backup/jobs/${encodeURIComponent(id)}/download?snapshot=${encodeURIComponent(snapshot)}&path=${encodePathSegment(path)}`,
};

// ---------------------------------------------------------------------------
// Shares (Samba + NFS)
// ---------------------------------------------------------------------------

export interface SmbShare {
  name: string;
  path: string;
  browsable: boolean;
  read_only: boolean;
  guest_ok: boolean;
  valid_users: string[];
  force_user: string | null;
  create_mask: string;
  dir_mask: string;
  comment: string;
}

export interface SmbUser {
  username: string;
  enabled: boolean;
}

export interface SmbConfig {
  enabled: boolean;
  workgroup: string;
  server_string: string;
  shares: SmbShare[];
  users: SmbUser[];
}

export interface SmbUserStatus {
  username: string;
  uid: number | null;
  full_name: string;
}

export interface SambaState {
  config: SmbConfig;
  problems: Problem[];
  config_error: string | null;
  preview: string;
  lan_cidr: string | null;
  users: SmbUserStatus[];
  users_error: string | null;
}

export interface NfsExport {
  path: string;
  clients: string[];
  ro: boolean;
  sync: boolean;
  no_subtree_check: boolean;
  root_squash: boolean;
}

export interface NfsConfig {
  enabled: boolean;
  v4: boolean;
  exports: NfsExport[];
}

export interface ActiveExport {
  path: string;
  clients: string;
}

export interface NfsState {
  config: NfsConfig;
  problems: Problem[];
  config_error: string | null;
  preview: string;
  active: ActiveExport[];
  active_error: string | null;
}

export interface SharesReport<T> {
  state: T;
  steps: StepResult[];
}

export const sambaApi = {
  state: () => request<SambaState>("/api/samba"),
  save: (config: SmbConfig) => jsonRequest<SambaState>("/api/samba/config", "PUT", config),
  apply: (config: SmbConfig) =>
    jsonRequest<SharesReport<SambaState>>("/api/samba/apply", "POST", config),
  setUser: (username: string, password: string | null, enabled: boolean | null) =>
    jsonRequest<{ ok: boolean }>("/api/samba/users", "POST", { username, password, enabled }),
  removeUser: (username: string) =>
    request<{ ok: boolean }>(`/api/samba/users/${encodeURIComponent(username)}`, {
      method: "DELETE",
    }),
};

export const nfsApi = {
  state: () => request<NfsState>("/api/nfs"),
  save: (config: NfsConfig) => jsonRequest<NfsState>("/api/nfs/config", "PUT", config),
  apply: (config: NfsConfig) =>
    jsonRequest<SharesReport<NfsState>>("/api/nfs/apply", "POST", config),
};

/** Encode a path for use in a query string, preserving forward slashes. Unlike
 * encodeURIComponent, this does not turn "/" into "%2F" — some HTTP stacks reject
 * encoded slashes in query strings, and "/" is a legal sub-delim there. */
export function encodePathSegment(path: string): string {
  return encodeURIComponent(path).replace(/%2F/g, "/").replace(/%2f/g, "/");
}

// ---------------------------------------------------------------------------
// File manager
// ---------------------------------------------------------------------------

export type FileKind = "file" | "dir" | "symlink" | "other";

export interface FileEntry {
  name: string;
  path: string;
  kind: FileKind;
  size: number;
  modified_ms: number | null;
  mode: string | null;
  owner: string | null;
  group: string | null;
  symlink_target: string | null;
}

export interface FileListing {
  path: string;
  parent: string | null;
  entries: FileEntry[];
  truncated: boolean;
}

export interface FilesConfig {
  roots: string[];
  show_hidden: boolean;
  upload_max_bytes: number;
  listing_cap: number;
}

export interface FilesState {
  config: FilesConfig;
  problems: Problem[];
  config_error: string | null;
  roots: string[];
  listing: FileListing;
}

export interface FileText {
  path: string;
  size: number;
  truncated: boolean;
  mime: string;
  content: string;
}

export interface Accounts {
  users: string[];
  groups: string[];
}

export const filesApi = {
  state: (path = "") =>
    request<FilesState>(`/api/files?path=${encodePathSegment(path)}`),
  read: (path: string) =>
    request<FileText>(`/api/files/read?path=${encodePathSegment(path)}`),
  save: (path: string, content: string) =>
    jsonRequest<ActionResult>("/api/files/save", "POST", { path, content }),
  mkdir: (path: string) => jsonRequest<ActionResult>("/api/files/mkdir", "POST", { path }),
  createFile: (path: string) =>
    jsonRequest<ActionResult>("/api/files/file", "POST", { path }),
  rename: (from: string, to: string) =>
    jsonRequest<ActionResult>("/api/files/rename", "POST", { from, to }),
  remove: (path: string, recursive: boolean) =>
    jsonRequest<ActionResult>("/api/files/delete", "POST", { path, recursive }),
  chmod: (path: string, mode: string) =>
    jsonRequest<ActionResult>("/api/files/chmod", "POST", { path, mode }),
  chown: (path: string, user: string | null, group: string | null) =>
    jsonRequest<ActionResult>("/api/files/chown", "POST", { path, user, group }),
  extract: (path: string) =>
    jsonRequest<ActionResult>("/api/files/extract", "POST", { path }),
  accounts: () => request<Accounts>("/api/files/accounts"),
  saveConfig: (config: FilesConfig) =>
    jsonRequest<{ ok: boolean; roots: string[] }>("/api/files/config", "PUT", config),
  downloadUrl: (path: string) => `/api/files/download?path=${encodePathSegment(path)}`,
  archiveUrl: (path: string) => `/api/files/archive?path=${encodePathSegment(path)}`,
  uploadUrl: (dir: string) => `/api/files/upload?path=${encodePathSegment(dir)}`,
};

// ---------------------------------------------------------------------------
// Reverse proxy + internal CA
// ---------------------------------------------------------------------------

export type ProxyTls = "ca-managed" | "cert" | "off";

export interface ProxyUpstreamContainer {
  kind: "container";
  id: string;
  port: number | null;
}

export interface ProxyUpstreamUrl {
  kind: "url";
  url: string;
}

export type ProxyUpstream = ProxyUpstreamContainer | ProxyUpstreamUrl;

export interface ProxyRoute {
  id: string;
  hostname: string;
  upstream: ProxyUpstream;
  tls: ProxyTls;
  tls_cert: string | null;
  enabled: boolean;
  comment: string;
}

export interface PortCheck {
  port: number;
  role: string;
  available: boolean;
  detail: string;
}

export interface CaInfo {
  configured: boolean;
  directory: string;
  cert_path: string;
  key_path: string;
  subject: string | null;
  not_after: string | null;
  days_remaining: number | null;
  fingerprint: string | null;
  error?: string | null;
}

export interface LeafInfo {
  name: string;
  hostname: string;
  cert_path: string;
  key_path: string;
  subject: string | null;
  issuer: string | null;
  not_after: string | null;
  days_remaining: number | null;
  sans: string[];
  ca_signed: boolean;
  needs_renewal: boolean;
}

export interface ProxyState {
  config: { routes: ProxyRoute[] };
  problems: Problem[];
  config_error: string | null;
  running: boolean;
  admin_hostname: string;
  http_port: number;
  https_port: number;
  preflight: PortCheck[];
  tls_ready: boolean;
  ca: CaInfo;
  certificates: LeafInfo[];
}

export interface RouteReport {
  state: ProxyState;
  steps: StepResult[];
}

export interface TrustInstruction {
  os: string;
  steps: string[];
}

export interface CaState {
  info: CaInfo;
  leaves: LeafInfo[];
  trust: TrustInstruction[];
}

export const proxyApi = {
  state: () => request<ProxyState>("/api/proxy"),
  save: (config: { routes: ProxyRoute[] }) =>
    jsonRequest<ProxyState>("/api/proxy", "PUT", config),
  createRoute: (route: ProxyRoute) =>
    jsonRequest<RouteReport>("/api/proxy/routes", "POST", route),
  updateRoute: (id: string, route: ProxyRoute) =>
    jsonRequest<RouteReport>(`/api/proxy/routes/${encodeURIComponent(id)}`, "PUT", route),
  deleteRoute: (id: string) =>
    request<RouteReport>(`/api/proxy/routes/${encodeURIComponent(id)}`, { method: "DELETE" }),
  preflight: () => post<PortCheck[]>("/api/proxy/preflight"),
  enable: (enabled: boolean) =>
    jsonRequest<ProxyState>("/api/proxy/enable", "POST", { enabled }),
};

export const caApi = {
  state: () => request<CaState>("/api/ca"),
  init: () => post<CaInfo>("/api/ca/init"),
  downloadPath: "/api/ca/download",
  issue: (hostname: string, extraDns: string[] = [], ip: string | null = null) =>
    jsonRequest<LeafInfo>("/api/ca/issue", "POST", { hostname, extra_dns: extraDns, ip }),
  remove: (name: string) =>
    request<{ ok: boolean }>(`/api/ca/leaves/${encodeURIComponent(name)}`, { method: "DELETE" }),
};

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

export type NotifyLevel = "info" | "warning" | "error";

export interface Notification {
  id: number;
  ts_ms: number;
  level: NotifyLevel;
  event: string;
  title: string;
  message: string;
}

export interface NotifyConfig {
  enabled: boolean;
  backup_failed: boolean;
  cert_expiring: boolean;
  cert_expiring_days: number;
  firewall_rollback: boolean;
  smart_prefail: boolean;
  disk_used_pct: number;
  service_down: boolean;
  webhook_url: string | null;
}

export interface NotifyState {
  items: Notification[];
  config: NotifyConfig;
  config_error: string | null;
}

export const notifyApi = {
  feed: (limit?: number) =>
    request<NotifyState>(
      limit ? `/api/notifications?limit=${limit}` : "/api/notifications",
    ),
  clear: () => post<{ ok: boolean }>("/api/notifications/clear"),
  saveConfig: (config: NotifyConfig) =>
    jsonRequest<NotifyState>("/api/notifications/config", "PUT", config),
  test: () => post<TestResult>("/api/notifications/test"),
};

export interface HostIp {
  ip: string;
}

export const hostApi = {
  /** Primary IPv4 of this host; used to pre-fill guest address placeholders. */
  ip: () => request<HostIp>("/api/host/ip"),
};

export const api = {
  containers: () => request<Container[]>("/api/containers"),

  start: (id: string) => post(`/api/containers/${encodeURIComponent(id)}/start`),
  stop: (id: string) => post(`/api/containers/${encodeURIComponent(id)}/stop`),
  restart: (id: string) => post(`/api/containers/${encodeURIComponent(id)}/restart`),
  remove: (id: string) =>
    request<ActionResult>(`/api/containers/${encodeURIComponent(id)}`, { method: "DELETE" }),
  stats: (id: string) =>
    request<ContainerStats>(`/api/containers/${encodeURIComponent(id)}/stats`),

  catalog: () => request<App[]>("/api/apps/catalog"),
  install: (id: string) =>
    request<OpResult>(`/api/apps/${encodeURIComponent(id)}/install`, { method: "POST" }),
  uninstall: (id: string) =>
    request<OpResult>(`/api/apps/${encodeURIComponent(id)}`, { method: "DELETE" }),
};

/** A Docker Hub repository search result. */
export interface HubRepo {
  name: string;
  description: string;
  star_count: number;
  pull_count: number;
  is_official: boolean;
}

export interface HubResults {
  /** False when the backend's transport (curl) is missing. */
  available: boolean;
  results: HubRepo[];
}

export const hubApi = {
  search: (query: string, signal?: AbortSignal) =>
    request<HubResults>(`/api/apps/hub-search?q=${encodeURIComponent(query)}`, { signal }),
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
  verified: boolean;
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

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/** One live sample for a container, as the Stats modal shows it. */
export interface ContainerStats {
  id: string;
  cpu_pct: number;
  memory_used_bytes: number;
  memory_limit_bytes: number;
  memory_pct: number;
  network_rx_bytes: number;
  network_tx_bytes: number;
}

/** One node of the physical block-device tree. */
export interface BlockDevice {
  name: string;
  path: string;
  kind: string;
  size_bytes: number;
  model: string | null;
  fstype: string | null;
  mountpoint: string | null;
  label: string | null;
  children: BlockDevice[];
}

export interface StorageState {
  devices: BlockDevice[];
  /** Device backing `/`, which the UI marks as untouchable. */
  root_device: string | null;
  formats: string[];
}

export interface StorageResult {
  ok: boolean;
  message: string;
}

export const storageApi = {
  state: () => request<StorageState>("/api/storage"),
  format: (device: string, fs: string) =>
    jsonRequest<StorageResult>("/api/storage/format", "POST", { device, fs }),
  mount: (device: string, path: string, persist: boolean) =>
    jsonRequest<StorageResult>("/api/storage/mount", "POST", { device, path, persist }),
  unmount: (target: string) =>
    jsonRequest<StorageResult>("/api/storage/unmount", "POST", { target }),
};

// ---------------------------------------------------------------------------
// DNS blocklists (adblock)
// ---------------------------------------------------------------------------

/** One subscription plus the outcome of its last fetch. */
export interface BlocklistRow {
  name: string;
  url: string;
  enabled: boolean;
  builtin: boolean;
  status: string | null;
  error: string | null;
  domains: number | null;
  fetched_at: number | null;
}

export interface BlocklistState {
  sources: BlocklistRow[];
  allowlist: string[];
  total: number;
  updated_at: number | null;
  compiled_file: string;
  unbound_enabled: boolean;
}

export interface BlocklistUpdateResult {
  ok: boolean;
  message: string;
  state: BlocklistState;
}

export interface BlocklistLookup {
  domain: string;
  blocked: boolean;
  allowed: boolean;
}

export const blocklistsApi = {
  state: () => request<BlocklistState>("/api/network/blocklists"),
  saveSources: (
    sources: { name: string; url: string; enabled: boolean; builtin: boolean }[],
  ) => jsonRequest<BlocklistState>("/api/network/blocklists/sources", "POST", { sources }),
  saveAllowlist: (allowlist: string[]) =>
    jsonRequest<BlocklistState>("/api/network/blocklists/allowlist", "POST", { allowlist }),
  update: () =>
    request<BlocklistUpdateResult>("/api/network/blocklists/update", { method: "POST" }),
  lookup: (domain: string) =>
    request<BlocklistLookup>(
      `/api/network/blocklists/lookup?domain=${encodeURIComponent(domain)}`,
    ),
};

// ---------------------------------------------------------------------------
// Container / stack auto-update
// ---------------------------------------------------------------------------

export type UpdateSchedule = "manual" | "daily" | "weekly";

export interface UpdateEntry {
  id: string;
  /** `stack` or `container`; only a stack can be redeployed and rolled back. */
  kind: string;
  auto_update: boolean;
  schedule: UpdateSchedule;
  pinned_digest: string | null;
  previous_digest: string | null;
  last_checked: number | null;
  last_result: string | null;
  update_available: boolean;
}

export interface UpdatesState {
  entries: UpdateEntry[];
  doc_error: string | null;
}

export interface UpdateResult {
  ok: boolean;
  message: string;
  entries: UpdateEntry[];
}

export const updatesApi = {
  state: () => request<UpdatesState>("/api/updates"),
  configure: (id: string, auto_update: boolean, schedule: UpdateSchedule, kind = "stack") =>
    jsonRequest<UpdatesState>(`/api/updates/${encodeURIComponent(id)}`, "POST", {
      auto_update,
      schedule,
      kind,
    }),
  check: (id: string) =>
    request<UpdateResult>(`/api/updates/${encodeURIComponent(id)}/check`, { method: "POST" }),
  apply: (id: string) =>
    request<UpdateResult>(`/api/updates/${encodeURIComponent(id)}/apply`, { method: "POST" }),
  rollback: (id: string) =>
    request<UpdateResult>(`/api/updates/${encodeURIComponent(id)}/rollback`, { method: "POST" }),
};

// ---------------------------------------------------------------------------
// Guest image sources and cache
// ---------------------------------------------------------------------------

export interface CachedImage {
  name: string;
  size_bytes: number;
  modified: number;
}

export interface ImageActionResult {
  ok: boolean;
  message: string;
  image: { name: string; path: string; size_bytes: number };
}

export const imagesApi = {
  list: () => request<CachedImage[]>("/api/virt/images"),
  fetch: (url: string, name?: string) =>
    jsonRequest<ImageActionResult>("/api/virt/images/fetch", "POST", { url, name: name ?? null }),
  import: (path: string) =>
    jsonRequest<ImageActionResult>("/api/virt/images/import", "POST", { path }),
  remove: (name: string) =>
    request<{ ok: boolean }>(`/api/virt/images/${encodeURIComponent(name)}`, {
      method: "DELETE",
    }),
};

// ---------------------------------------------------------------------------
// Scheduled tasks
// ---------------------------------------------------------------------------

export interface ScheduledTask {
  id: string;
  name: string;
  schedule: string;
  command: string;
  last_run: number | null;
  /** `ok`, `failed` or null when it has not run. */
  last_status: string | null;
  last_output: string | null;
}

export interface TasksState {
  tasks: ScheduledTask[];
  doc_error: string | null;
}

export interface TaskRunResult {
  ok: boolean;
  status: string;
  output: string;
  tasks: ScheduledTask[];
}

export const tasksApi = {
  list: () => request<TasksState>("/api/tasks"),
  save: (task: { id?: string; name: string; schedule: string; command: string }) =>
    jsonRequest<TasksState>("/api/tasks", "POST", task),
  remove: (id: string) =>
    request<TasksState>(`/api/tasks/${encodeURIComponent(id)}`, { method: "DELETE" }),
  run: (id: string) =>
    request<TaskRunResult>(`/api/tasks/${encodeURIComponent(id)}/run`, { method: "POST" }),
};

// ---------------------------------------------------------------------------
// Background jobs (activity drawer)
// ---------------------------------------------------------------------------

export type JobStatus = "pending" | "running" | "success" | "failed";

export interface Job {
  id: string;
  name: string;
  status: JobStatus;
  progress: number;
  logs: string[];
  started_ms: number;
  finished_ms: number | null;
  message: string | null;
  error: string | null;
}

export interface JobsState {
  jobs: Job[];
}

export const jobsApi = {
  list: () => request<JobsState>("/api/jobs"),
  remove: (id: string) =>
    request<{ ok: boolean }>(`/api/jobs/${encodeURIComponent(id)}`, { method: "DELETE" }),
};

// ---------------------------------------------------------------------------
// Alerting rules + notification providers
// ---------------------------------------------------------------------------

export type AlertComparison = ">" | "<" | "==" | "!=";
export type ProviderKind = "discord" | "slack" | "email" | "webhook";

export interface AlertRule {
  id: string;
  name: string;
  /** cpu_usage, memory_usage, load1, disk_usage, disk_usage:/mnt, container_status:<name>. */
  metric: string;
  condition: AlertComparison;
  threshold: number;
  duration_s: number;
  /** Provider id. */
  action: string;
  enabled: boolean;
  cooldown_s: number;
  last_fired: number | null;
}

export interface ProviderConfig {
  id: string;
  name: string;
  kind: ProviderKind;
  url: string | null;
  to: string | null;
  enabled: boolean;
}

export interface AlertsState {
  providers: ProviderConfig[];
  rules: AlertRule[];
  doc_error: string | null;
}

export const alertsApi = {
  state: () => request<AlertsState>("/api/alerts"),
  save: (doc: { providers: ProviderConfig[]; rules: AlertRule[] }) =>
    jsonRequest<AlertsState>("/api/alerts", "PUT", doc),
  test: (id: string) =>
    post<TestResult>(`/api/alerts/test/${encodeURIComponent(id)}`),
};

// ---------------------------------------------------------------------------
// Container / stack health checks
// ---------------------------------------------------------------------------

export type ProbeKind = "http" | "tcp";

export interface HealthCheck {
  stack: string;
  enabled: boolean;
  kind: ProbeKind;
  /** An http(s):// URL, or host:port for a TCP connect. */
  target: string;
  timeout_s: number;
  retries: number;
  cooldown_s: number;
  last_restart: number | null;
}

export interface HealthState {
  checks: HealthCheck[];
  doc_error: string | null;
}

export const healthApi = {
  state: () => request<HealthState>("/api/healthchecks"),
  save: (doc: { checks: HealthCheck[] }) =>
    jsonRequest<HealthState>("/api/healthchecks", "PUT", doc),
};

// ---------------------------------------------------------------------------
// Developer / diagnostics
// ---------------------------------------------------------------------------

export interface DebugRoute {
  method: string;
  path: string;
}

export interface PingResult {
  method: string;
  path: string;
  /** HTTP status, or 0 when the request could not be completed. */
  status: number;
  ok: boolean;
  ms: number;
  error: string | null;
}

export interface PingReport {
  results: PingResult[];
  ok_count: number;
  fail_count: number;
}

export interface ConfigRef {
  name: string;
  path: string;
  exists: boolean;
  bytes: number | null;
  modified_ms: number | null;
}

export interface ConfigBody {
  name: string;
  path: string;
  /** "file" or "defaults (no file)". */
  source: string;
  /** A loader error, if the on-disk document could not be parsed. */
  error: string | null;
  json: unknown;
}

export interface SystemLog {
  available: boolean;
  unit: string;
  /** Plain journal lines, shown verbatim. */
  raw: string;
  error: string | null;
}

export const debugApi = {
  routes: () => request<{ routes: DebugRoute[] }>("/api/debug/routes"),
  ping: () => request<PingReport>("/api/debug/ping"),
  configs: () => request<{ configs: ConfigRef[] }>("/api/debug/configs"),
  config: (name: string) =>
    request<ConfigBody>(`/api/debug/config?name=${encodeURIComponent(name)}`),
  systemLog: () => request<SystemLog>("/api/logs/system"),
  /** Deliberately hits a route that does not exist, to exercise the error toast. */
  forceError: () => request<unknown>("/api/debug/force-error", { method: "POST" }),
};
