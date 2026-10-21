/**
 * Reverse-proxy tab (mounted inside the Network page).
 *
 * Self-contained: it talks only to `/api/proxy` and `/api/ca`, so the Network
 * page's draft/apply machinery is untouched. Route edits go through the
 * dedicated create/update/delete endpoints so the backend can issue a
 * certificate and rewrite DNS as part of the same operation.
 */

import { caApi, proxyApi } from "../api";
import type { CaState, LeafInfo, ProxyRoute, ProxyState, StepResult } from "../api";
import { button, checkbox, el, emptyNote, field, panel, select, textInput } from "../ui";

type UpstreamKind = "container" | "url";

function blankRoute(): ProxyRoute {
  return {
    id: "",
    hostname: "",
    upstream: { kind: "container", id: "", port: null },
    tls: "ca-managed",
    tls_cert: null,
    enabled: true,
    comment: "",
  };
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function buildProxyTab(): HTMLElement {
  const root = el("div", { class: "flex flex-col gap-2" });

  let state: ProxyState | null = null;
  let ca: CaState | null = null;
  let editing: ProxyRoute | null = null;
  let isNew = false;
  let busy = false;
  let status = "";
  let report: StepResult[] | null = null;
  let loaded = false;

  const statusLine = el("span", { class: "tabular text-[11px] text-slate-500" });
  const runningBadge = el("span", { class: "tabular text-[10px]" });
  const body = el("div", { class: "flex flex-col gap-2" });

  const refreshButton = button("refresh", "", () => void load());
  const preflightButton = button("check ports", "", () => void runPreflight());
  const toggleButton = button("", "", () => void toggleFrontDoor());
  const addButton = button("add route", "text-emerald-400 hover:border-emerald-500", () => {
    editing = blankRoute();
    isNew = true;
    render();
  });

  const header = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-slate-300", text: "reverse proxy" }),
    runningBadge,
    refreshButton,
    preflightButton,
    toggleButton,
    addButton,
    el("span", { class: "ml-auto" }, [statusLine]),
  ]);

  root.append(header, body);

  async function load(): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    render();
    try {
      const [proxyState, caState] = await Promise.all([proxyApi.state(), caApi.state()]);
      state = proxyState;
      ca = caState;
      status = "";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      loaded = true;
      render();
    }
  }

  async function runPreflight(): Promise<void> {
    try {
      const checks = await proxyApi.preflight();
      if (state) state.preflight = checks;
      status = checks.every((c) => c.available) ? "ports are free" : "a port is in use";
    } catch (err) {
      status = errorText(err);
    }
    render();
  }

  async function toggleFrontDoor(): Promise<void> {
    if (!state || busy) return;
    busy = true;
    status = state.running ? "stopping…" : "starting…";
    render();
    try {
      state = await proxyApi.enable(!state.running);
      ca = await caApi.state();
      status = state.running ? "front door is up" : "front door is down";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function submitRoute(): Promise<void> {
    if (!editing) return;
    busy = true;
    status = "saving…";
    render();
    try {
      const result = isNew
        ? await proxyApi.createRoute(editing)
        : await proxyApi.updateRoute(editing.id, editing);
      state = result.state;
      report = result.steps;
      editing = null;
      isNew = false;
      status = "saved";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  async function removeRoute(id: string): Promise<void> {
    if (busy) return;
    busy = true;
    status = "removing…";
    render();
    try {
      const result = await proxyApi.deleteRoute(id);
      state = result.state;
      report = result.steps;
      status = "removed";
    } catch (err) {
      status = errorText(err);
    } finally {
      busy = false;
      render();
    }
  }

  function render(): void {
    statusLine.textContent = status;
    if (state) {
      runningBadge.textContent = state.running ? "on" : "off";
      runningBadge.className = state.running
        ? "tabular text-[10px] text-emerald-400"
        : "tabular text-[10px] text-slate-500";
      toggleButton.textContent = state.running ? "stop :80/:443" : "start :80/:443";
      toggleButton.disabled = busy;
    }
    toggleButton.disabled = busy || !state;

    body.replaceChildren();

    if (!loaded) {
      body.append(emptyNote("loading…"));
      return;
    }
    if (!state) {
      body.append(emptyNote(status || "the proxy API is unavailable"));
      return;
    }

    body.append(frontDoorPanel(), caPanel());
    const changes = reportPanel();
    if (changes) body.append(changes);
    body.append(routesPanel());
    if (editing) body.append(editorPanel());
  }

  function frontDoorPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("front door");
    cardBody.className = "flex flex-col gap-1 px-3 py-2 text-[11px]";

    cardBody.append(
      el("div", {
        class: "flex flex-wrap gap-x-4 gap-y-1 text-slate-400",
        text: `admin host: ${state!.admin_hostname}   ·   http :${state!.http_port}   ·   https :${state!.https_port}   ·   certs loaded: ${state!.tls_ready ? "yes" : "no"}`,
      }),
    );
    for (const check of state!.preflight) {
      cardBody.append(
        el("div", {
          class: check.available ? "text-emerald-400" : "text-amber-400",
          text: `${check.role} :${check.port} — ${check.detail}`,
        }),
      );
    }
    return card;
  }

  function caPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("internal CA");
    cardBody.className = "flex flex-col gap-2 px-3 py-2 text-[11px]";
    const info = ca?.info;

    if (!info?.configured) {
      cardBody.append(
        el("div", { class: "text-slate-400", text: "No internal CA yet." }),
        button("create CA", "", () => void initCa()),
      );
      if (info?.error) {
        cardBody.append(el("div", { class: "text-rose-400", text: info.error }));
      }
      return card;
    }

    cardBody.append(
      el("div", {
        class: "text-slate-400",
        text: `${info.subject ?? "CA"} · expires ${info.not_after ?? "unknown"} (${info.days_remaining ?? "?"} days)`,
      }),
    );
    const link = el("a", {
      href: caApi.downloadPath,
      class: "text-emerald-400 underline",
      text: "download ca.crt",
    });
    cardBody.append(el("div", { class: "flex gap-3" }, [link]));

    if (ca?.trust.length) {
      const details = document.createElement("details");
      details.className = "text-slate-400";
      details.append(el("summary", { text: "trust instructions" }));
      for (const entry of ca.trust) {
        const block = el("div", { class: "mt-1" });
        block.append(el("div", { class: "text-slate-300", text: entry.os }));
        for (const line of entry.steps) {
          block.append(el("div", { class: "pl-3 text-slate-500", text: `• ${line}` }));
        }
        details.append(block);
      }
      cardBody.append(details);
    }
    return card;
  }

  async function initCa(): Promise<void> {
    try {
      status = "creating CA…";
      render();
      await caApi.init();
      ca = await caApi.state();
      if (state) state = await proxyApi.state();
      status = "CA created";
    } catch (err) {
      status = errorText(err);
    }
    render();
  }

  function reportPanel(): HTMLElement | null {
    if (!report || report.length === 0) return null;
    const { root: card, body: cardBody } = panel("last change");
    cardBody.className = "flex flex-col gap-0.5 px-3 py-2 text-[11px]";
    for (const step of report) {
      cardBody.append(
        el("div", {
          class: step.ok ? "text-emerald-400" : "text-rose-400",
          text: `${step.ok ? "ok" : "fail"} · ${step.subsystem}: ${step.detail}`,
        }),
      );
    }
    return card;
  }

  function certFor(hostname: string): LeafInfo | undefined {
    return state!.certificates.find((leaf) => leaf.hostname.toLowerCase() === hostname.toLowerCase());
  }

  function routesPanel(): HTMLElement {
    const { root: card, body: cardBody } = panel("routes");
    cardBody.className = "flex flex-col gap-1 px-3 py-2";

    if (state!.config.routes.length === 0) {
      cardBody.append(emptyNote("no routes yet — add one to publish an app on a hostname"));
      return card;
    }

    const table = el("table", { class: "w-full text-[11px]" });
    const head = el("tr", {}, [
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-slate-600", text: "hostname" }),
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-slate-600", text: "upstream" }),
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-slate-600", text: "tls" }),
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-slate-600", text: "cert" }),
      el("th", { class: "px-2 py-1" }),
    ]);
    table.append(el("thead", {}, [head]));

    const tbody = el("tbody");
    for (const route of state!.config.routes) {
      const upstream =
        route.upstream.kind === "container"
          ? `container ${route.upstream.id}${route.upstream.port ? `:${route.upstream.port}` : ""}`
          : route.upstream.url;
      const leaf = certFor(route.hostname);
      const certText = !route.enabled
        ? "—"
        : route.tls === "off"
          ? "http only"
          : leaf
            ? `${leaf.ca_signed ? "CA" : "extern"} · ${leaf.days_remaining ?? "?"}d${leaf.needs_renewal ? " (renew)" : ""}`
            : "missing";

      const openUrl = `${route.tls === "off" ? "http" : "https"}://${route.hostname}`;
      const row = el("tr", {}, [
        el("td", { class: "px-2 py-1 text-slate-300", text: route.hostname }),
        el("td", { class: "px-2 py-1 text-slate-500", text: upstream }),
        el("td", { class: "px-2 py-1 text-slate-500", text: `${route.tls}${route.enabled ? "" : " (off)"}` }),
        el("td", {
          class: leaf?.needs_renewal ? "px-2 py-1 text-amber-400" : "px-2 py-1 text-slate-500",
          text: certText,
        }),
        el("td", { class: "px-2 py-1 text-right" }, [
          el("a", { href: openUrl, target: "_blank", class: "mr-2 text-emerald-400 underline", text: "open" }),
          button("copy", "mr-2 text-[10px]", () => void copyUrl(openUrl)),
          button("renew", "mr-2 text-[10px]", () => void renew(route)),
          button("edit", "mr-2 text-[10px]", () => {
            editing = structuredClone(route);
            isNew = false;
            render();
          }),
          button("delete", "text-[10px] text-rose-400 hover:border-rose-500", () => void removeRoute(route.id)),
        ]),
      ]);
      tbody.append(row);
    }
    table.append(tbody);
    cardBody.append(table);
    return card;
  }

  async function copyUrl(url: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(url);
      status = `copied ${url}`;
    } catch {
      status = url;
    }
    render();
  }

  async function renew(route: ProxyRoute): Promise<void> {
    try {
      status = "issuing…";
      render();
      await caApi.issue(route.hostname);
      state = await proxyApi.state();
      status = `certificate issued for ${route.hostname}`;
    } catch (err) {
      status = errorText(err);
    }
    render();
  }

  function editorPanel(): HTMLElement {
    const route = editing!;
    const { root: card, body: cardBody } = panel(isNew ? "new route" : `edit ${route.id}`);
    cardBody.className = "flex flex-col gap-2 px-3 py-2";

    const grid = el("div", { class: "grid grid-cols-2 gap-2 md:grid-cols-3" });
    grid.append(
      field("id", textInput(route.id, (value) => (route.id = value), { placeholder: "auto" })),
      field("hostname", textInput(route.hostname, (value) => (route.hostname = value), { placeholder: "app.lan" })),
      field(
        "enabled",
        checkbox(route.enabled, (value) => (route.enabled = value), "route enabled"),
      ),
    );

    let upstreamKind: UpstreamKind = route.upstream.kind;
    const kindSelect = select(
      upstreamKind,
      [
        ["container", "container"],
        ["url", "url"],
      ],
      (value) => {
        upstreamKind = value as UpstreamKind;
        route.upstream =
          upstreamKind === "container"
            ? { kind: "container", id: "", port: null }
            : { kind: "url", url: "" };
        render();
      },
    );
    grid.append(field("upstream", kindSelect));

    if (route.upstream.kind === "container") {
      grid.append(
        field("container id", textInput(route.upstream.id, (value) => {
          if (route.upstream.kind === "container") route.upstream.id = value;
        }, { placeholder: "jellyfin" })),
      );
      grid.append(
        field("port (optional)", textInput(String(route.upstream.port ?? ""), (value) => {
          if (route.upstream.kind === "container") {
            route.upstream.port = value.trim() === "" ? null : Number.parseInt(value, 10) || null;
          }
        }, { placeholder: "private port" })),
      );
    } else {
      grid.append(
        field("url", textInput(route.upstream.url, (value) => {
          if (route.upstream.kind === "url") route.upstream.url = value;
        }, { placeholder: "http://127.0.0.1:8096" })),
      );
    }

    grid.append(
      field(
        "tls mode",
        select(
          route.tls,
          [
            ["ca-managed", "ca-managed (internal CA)"],
            ["cert", "named certificate"],
            ["off", "off (plain http)"],
          ],
          (value) => {
            route.tls = value as ProxyRoute["tls"];
            render();
          },
        ),
      ),
    );
    if (route.tls === "cert") {
      grid.append(
        field("cert name", textInput(route.tls_cert ?? "", (value) => (route.tls_cert = value), { placeholder: "origin" })),
      );
    }
    grid.append(field("comment", textInput(route.comment, (value) => (route.comment = value))));

    const actions = el("div", { class: "flex gap-2" }, [
      button("save route", "text-emerald-400 hover:border-emerald-500", () => void submitRoute()),
      button("cancel", "", () => {
        editing = null;
        isNew = false;
        render();
      }),
    ]);

    cardBody.append(grid, actions);
    return card;
  }

  void load();
  return root;
}
