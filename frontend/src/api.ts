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
    const message =
      body && typeof body === "object" && "error" in body
        ? String((body as { error: unknown }).error)
        : `request failed (${response.status})`;
    throw new Error(message);
  }

  return body as T;
}

function post(path: string): Promise<ActionResult> {
  return request<ActionResult>(path, { method: "POST" });
}

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
