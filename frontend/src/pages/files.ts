/**
 * Files page — Modern File Explorer & Built-in Code Editor.
 *
 * Inspired by Windows 11 Explorer & Linear:
 * - Top command bar with New, Navigation, Upload, Archive, View modes, and Live Filter
 * - Omnibar-style address bar with interactive breadcrumbs and direct path jumping
 * - Left sidebar with Locations / Roots and Quick Shortcuts
 * - Sleek List & Grid views with bespoke file type icons
 * - Floating hover actions: clean rows with no visual clutter
 * - Windows 11 style right-click floating context menu with quick-actions strip
 * - Full-featured built-in Code Editor with line numbers, Tab support, and Ctrl+S
 * - Right-hand details & inspector pane for quick previews (images, text/code)
 * - Drag-and-drop upload with animated progress tracking
 * - Multi-selection with floating bulk-action toolbar
 */

import { filesApi } from "../api";
import type { FileEntry, FilesState } from "../api";
import { showToast } from "../toast";
import { bytes, el } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function formatTime(ms: number | null): string {
  if (!ms) return "—";
  const d = new Date(ms);
  return d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function parentOf(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  parts.pop();
  return parts.join("/") || "/";
}

function join(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

function isImage(name: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp|svg|ico)$/i.test(name);
}

function isText(name: string): boolean {
  return /\.(md|txt|log|json|ya?ml|toml|ini|conf|sh|bash|zsh|rs|ts|js|tsx|jsx|css|scss|html|xml|csv|sql|env|service|socket|target|timer)$/i.test(name);
}

function isArchive(name: string): boolean {
  return /\.(zip|tar\.gz|tgz|tar\.zst|tzst|tar\.xz|txz|tar\.bz2|tbz2|tar|gz|zst)$/i.test(name);
}

// ---------------------------------------------------------------------------
// SVG Icon Catalog (Clean, lightweight, bespoke vector assets)
// ---------------------------------------------------------------------------

const ICONS = {
  folder: `<svg class="size-4 shrink-0 text-amber-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.93a2 2 0 0 1-1.66-.9l-.82-1.2A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z"/></svg>`,
  fileCode: `<svg class="size-4 shrink-0 text-cyan-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>`,
  fileText: `<svg class="size-4 shrink-0 text-neutral-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/><line x1="10" y1="9" x2="8" y2="9"/></svg>`,
  fileImage: `<svg class="size-4 shrink-0 text-purple-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2" ry="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/></svg>`,
  fileArchive: `<svg class="size-4 shrink-0 text-rose-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="20" height="5" x="2" y="3" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"/><path d="M10 12h4"/></svg>`,
  fileGeneric: `<svg class="size-4 shrink-0 text-neutral-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>`,
  drive: `<svg class="size-4 shrink-0 text-emerald-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="22" x2="2" y1="12" y2="12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/><line x1="6" x2="6.01" y1="16" y2="16"/><line x1="10" x2="10.01" y1="16" y2="16"/></svg>`,
  shortcut: `<svg class="size-4 shrink-0 text-blue-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 14 20 9 15 4"/><path d="M4 20v-7a4 4 0 0 1 4-4h12"/></svg>`,
  up: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="18 15 12 9 6 15"/></svg>`,
  refresh: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16"/><path d="M16 16h5v5"/></svg>`,
  upload: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" x2="12" y1="3" y2="15"/></svg>`,
  download: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" x2="12" y1="15" y2="3"/></svg>`,
  plus: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="12" x2="12" y1="5" y2="19"/><line x1="5" x2="19" y1="12" y2="12"/></svg>`,
  grid: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/></svg>`,
  list: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="8" x2="21" y1="6" y2="6"/><line x1="8" x2="21" y1="12" y2="12"/><line x1="8" x2="21" y1="18" y2="18"/><line x1="3" x2="3.01" y1="6" y2="6"/><line x1="3" x2="3.01" y1="12" y2="12"/><line x1="3" x2="3.01" y1="18" y2="18"/></svg>`,
  search: `<svg class="size-3.5 shrink-0 text-neutral-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" x2="16.65" y1="21" y2="16.65"/></svg>`,
  eye: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/></svg>`,
  panel: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="18" x="3" y="3" rx="2"/><line x1="15" x2="15" y1="3" y2="21"/></svg>`,
  edit: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>`,
  trash: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/></svg>`,
  chevronRight: `<svg class="size-3 shrink-0 text-neutral-500" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>`,
  chevronDown: `<svg class="size-3 shrink-0 text-neutral-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9"/></svg>`,
  more: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></svg>`,
  back: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="15 18 9 12 15 6"/></svg>`,
  forward: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>`,
  copy: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>`,
  close: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" x2="6" y1="6" y2="18"/><line x1="6" x2="18" y1="6" y2="18"/></svg>`,
  check: `<svg class="size-3.5 shrink-0 text-emerald-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`,
  archive: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="20" height="5" x="2" y="3" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"/><path d="M10 12h4"/></svg>`,
  lock: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>`,
  user: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/></svg>`,
  rename: `<svg class="size-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"/></svg>`,
};

function getFileIcon(entry: FileEntry): string {
  if (entry.kind === "dir") return ICONS.folder;
  if (isImage(entry.name)) return ICONS.fileImage;
  if (isCode(entry.name)) return ICONS.fileCode;
  if (isText(entry.name)) return ICONS.fileText;
  if (isArchive(entry.name)) return ICONS.fileArchive;
  return ICONS.fileGeneric;
}

function isCode(name: string): boolean {
  return /\.(json|ya?ml|toml|ini|conf|sh|bash|zsh|rs|ts|js|tsx|jsx|css|scss|html|xml|sql|service|socket|target|timer)$/i.test(name);
}

function getFileTypeDescription(entry: FileEntry): string {
  if (entry.kind === "dir") return "File folder";
  const ext = entry.name.split(".").pop()?.toLowerCase() || "";
  switch (ext) {
    case "json": return "JSON File";
    case "yaml":
    case "yml": return "YAML Document";
    case "toml": return "TOML Document";
    case "sh":
    case "bash": return "Shell Script";
    case "rs": return "Rust Source";
    case "ts":
    case "tsx": return "TypeScript Source";
    case "js":
    case "jsx": return "JavaScript Source";
    case "css": return "CSS Stylesheet";
    case "html": return "HTML Document";
    case "md": return "Markdown Document";
    case "txt": return "Text Document";
    case "log": return "Log File";
    case "png":
    case "jpg":
    case "jpeg":
    case "webp":
    case "svg": return "Image";
    case "zip":
    case "tar":
    case "gz":
    case "zst": return "Archive";
    default: return ext ? `${ext.toUpperCase()} File` : "File";
  }
}

export function createFilesPage(): PageHandle {
  const root = el("div", { class: "flex h-full flex-col bg-[#0b0d13] text-neutral-200 select-none overflow-hidden" });

  let state: FilesState | null = null;
  let current = "";
  let busy = false;
  let status = "";
  let loaded = false;
  let loadError: string | null = null;
  let view: "list" | "grid" = "list";
  let showDetailsPane = false;
  let searchQuery = "";
  let isEditingPath = false;
  let preview: FileEntry | null = null;
  const selected = new Set<string>();

  // Browser-like navigation history
  const history: string[] = [];
  let historyIndex = -1;

  type SortKey = "name" | "modified" | "size" | "type";
  let sortKey: SortKey = "name";
  let sortAsc = true;

  // -------------------------------------------------------------------------
  // Clipboard helper
  // -------------------------------------------------------------------------
  async function copyToClipboard(text: string): Promise<boolean> {
    if (navigator.clipboard && window.isSecureContext) {
      try {
        await navigator.clipboard.writeText(text);
        return true;
      } catch {
        // fallback
      }
    }
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "fixed";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      const success = document.execCommand("copy");
      document.body.removeChild(ta);
      if (success) return true;
    } catch {
      // fallback
    }
    return false;
  }

  // -------------------------------------------------------------------------
  // Upload with real progress bar
  // -------------------------------------------------------------------------
  const uploadInput = el("input", { type: "file", multiple: true, class: "hidden" }) as HTMLInputElement;
  const progressTrack = el("div", { class: "hidden h-1 w-full bg-white/5 overflow-hidden" });
  const progressBar = el("div", { class: "h-full w-0 bg-emerald-500 transition-all duration-150" });
  progressTrack.append(progressBar);

  function setProgress(pct: number | null): void {
    if (pct === null) {
      progressTrack.classList.add("hidden");
      progressBar.style.width = "0%";
      return;
    }
    progressTrack.classList.remove("hidden");
    progressBar.style.width = `${Math.min(100, Math.max(0, pct))}%`;
  }

  function uploadOne(url: string, file: File, onProgress: (pct: number) => void): Promise<void> {
    return new Promise((resolve, reject) => {
      const request = new XMLHttpRequest();
      request.open("POST", url);
      request.upload.addEventListener("progress", (event) => {
        if (event.lengthComputable) {
          onProgress(Math.round((event.loaded / event.total) * 100));
        }
      });
      request.addEventListener("load", () => {
        if (request.status >= 200 && request.status < 300) {
          resolve();
        } else {
          let msg = `upload failed (${request.status})`;
          try {
            const body = JSON.parse(request.responseText) as { error?: string };
            if (body.error) msg = body.error;
          } catch {
            // keep default
          }
          reject(new Error(msg));
        }
      });
      request.addEventListener("error", () => reject(new Error("network error during upload")));
      request.addEventListener("abort", () => reject(new Error("upload cancelled")));
      request.addEventListener("timeout", () => reject(new Error("upload timed out")));

      const form = new FormData();
      form.append("file", file, file.name);
      request.send(form);
    });
  }

  async function uploadFiles(files: FileList | File[]): Promise<void> {
    busy = true;
    const list = Array.from(files);
    try {
      for (let i = 0; i < list.length; i++) {
        const file = list[i];
        status = `uploading ${i + 1}/${list.length}: ${file.name}`;
        renderChrome();
        setProgress(0);
        await uploadOne(filesApi.uploadUrl(current), file, setProgress);
      }
      status = `uploaded ${list.length} item(s)`;
    } catch (err) {
      status = errorText(err);
    } finally {
      setProgress(null);
      busy = false;
      await load(current, false);
    }
  }

  uploadInput.addEventListener("change", () => {
    if (uploadInput.files?.length) void uploadFiles(uploadInput.files);
    uploadInput.value = "";
  });

  // -------------------------------------------------------------------------
  // Main Load & Navigation
  // -------------------------------------------------------------------------
  async function load(path: string, pushHistory = true): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    loadError = null;
    selected.clear();
    isEditingPath = false;
    renderChrome();
    try {
      state = await filesApi.state(path);
      current = state.listing.path;
      if (pushHistory) {
        if (historyIndex === -1 || history[historyIndex] !== current) {
          history.splice(historyIndex + 1);
          history.push(current);
          historyIndex = history.length - 1;
        }
      }
      status = state.listing.truncated ? "listing truncated" : "";
    } catch (err) {
      status = errorText(err);
      current = path;
      loadError = errorText(err);
      state = null;
    } finally {
      busy = false;
      loaded = true;
      render();
    }
  }

  function goBack(): void {
    if (historyIndex > 0) {
      historyIndex--;
      void load(history[historyIndex], false);
    }
  }

  function goForward(): void {
    if (historyIndex < history.length - 1) {
      historyIndex++;
      void load(history[historyIndex], false);
    }
  }

  // -------------------------------------------------------------------------
  // Command Bar & Address Bar UI Nodes
  // -------------------------------------------------------------------------
  const commandBar = el("div", {
    class: "flex h-11 shrink-0 items-center gap-1.5 border-b border-white/10 bg-[#0f1219]/90 px-3 backdrop-blur-md",
  });
  const addressBarRow = el("div", {
    class: "flex h-10 shrink-0 items-center gap-2 border-b border-white/5 bg-[#0d1017]/90 px-3",
  });
  const mainWorkspace = el("div", { class: "flex min-h-0 flex-1 overflow-hidden" });
  const sidebar = el("aside", {
    class: "flex w-56 shrink-0 flex-col gap-4 overflow-y-auto border-r border-white/5 bg-[#0d1017]/40 p-3 text-xs",
  });
  const centerContent = el("div", {
    class: "relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-[#0b0d13]",
  });
  const tableHost = el("div", { class: "min-h-0 flex-1 overflow-auto" });
  const detailsPane = el("aside", {
    class: "hidden w-80 shrink-0 flex-col gap-4 overflow-y-auto border-l border-white/5 bg-[#0d1017]/70 p-4 text-xs",
  });
  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-400" });

  root.append(commandBar, addressBarRow, progressTrack, mainWorkspace);
  mainWorkspace.append(sidebar, centerContent, detailsPane);
  centerContent.append(tableHost, uploadInput);

  // Drag and drop overlay onto main workspace
  const dropOverlay = el("div", {
    class: "pointer-events-none absolute inset-0 z-40 hidden flex-col items-center justify-center gap-2 bg-[#0b0d13]/90 backdrop-blur-xs ring-2 ring-emerald-500/50",
  }, [
    el("span", { class: "size-10 flex items-center justify-center rounded-xl bg-emerald-500/10 text-emerald-400", html: ICONS.upload }),
    el("span", { class: "text-sm font-semibold text-white", text: "Drop files to upload here" }),
    el("span", { class: "text-xs text-neutral-400", text: `Destination: ${current || "current directory"}` }),
  ]);
  centerContent.append(dropOverlay);

  let dragCounter = 0;
  centerContent.addEventListener("dragenter", (e) => {
    e.preventDefault();
    dragCounter++;
    dropOverlay.classList.remove("hidden");
  });
  centerContent.addEventListener("dragleave", (e) => {
    e.preventDefault();
    dragCounter--;
    if (dragCounter <= 0) {
      dragCounter = 0;
      dropOverlay.classList.add("hidden");
    }
  });
  centerContent.addEventListener("dragover", (e) => {
    e.preventDefault();
  });
  centerContent.addEventListener("drop", (e) => {
    e.preventDefault();
    dragCounter = 0;
    dropOverlay.classList.add("hidden");
    if (e.dataTransfer?.files.length) {
      void uploadFiles(e.dataTransfer.files);
    }
  });

  // Background right-click menu on empty table space
  tableHost.addEventListener("contextmenu", (e) => {
    if (e.target === tableHost || (e.target as HTMLElement).tagName === "TABLE" || (e.target as HTMLElement).tagName === "TBODY") {
      e.preventDefault();
      openBlankContextMenu(e.clientX, e.clientY);
    }
  });

  // -------------------------------------------------------------------------
  // Render Chrome: Command Bar & Address Bar
  // -------------------------------------------------------------------------
  function renderChrome(): void {
    renderCommandBar();
    renderAddressBar();
  }

  function toolBtn(icon: string, label: string, onClick: () => void, opts: { title?: string; active?: boolean; disabled?: boolean; primary?: boolean } = {}): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-medium transition-colors ${
      opts.primary
        ? "bg-emerald-600 text-white hover:bg-emerald-500"
        : opts.active
          ? "bg-white/10 text-white"
          : "text-neutral-300 hover:bg-white/[0.06] hover:text-white"
    } ${opts.disabled ? "opacity-40 cursor-not-allowed" : "cursor-pointer"}`;
    if (opts.title ?? label) btn.title = (opts.title ?? label)!;
    btn.disabled = !!opts.disabled;
    const iconSpan = el("span", { class: "size-4 flex items-center justify-center shrink-0", html: icon });
    btn.append(iconSpan);
    if (label) {
      btn.append(el("span", { text: label }));
    }
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!opts.disabled) onClick();
    });
    return btn;
  }

  function renderCommandBar(): void {
    commandBar.replaceChildren();

    // New item dropdown button
    const newBtn = toolBtn(ICONS.plus, "New", () => {
      openNewMenu(newBtn);
    }, { primary: true });
    commandBar.append(newBtn);

    const divider = (): HTMLElement => el("div", { class: "h-4 w-px bg-white/10 mx-1 shrink-0" });
    commandBar.append(divider());

    // Navigation & Refresh
    const upBtn = toolBtn(ICONS.up, "Up", () => {
      if (state?.listing.parent) void load(state.listing.parent);
    }, { disabled: busy || !state?.listing.parent, title: "Go up to parent directory" });

    const refreshBtn = toolBtn(ICONS.refresh, "", () => {
      void load(current, false);
    }, { title: "Refresh directory" });

    commandBar.append(upBtn, refreshBtn, divider());

    // Actions: Upload, Archive
    const uploadBtn = toolBtn(ICONS.upload, "Upload", () => {
      uploadInput.click();
    }, { title: "Upload files into current directory" });

    const archiveBtn = toolBtn(ICONS.archive, "Compress", () => {
      openArchiveDialog(current);
    }, { title: "Archive / Compress current directory (.tar.zst)" });

    commandBar.append(uploadBtn, archiveBtn, divider());

    // View switch: List / Grid
    const viewBtn = toolBtn(view === "list" ? ICONS.grid : ICONS.list, view === "list" ? "Grid" : "List", () => {
      view = view === "list" ? "grid" : "list";
      render();
    }, { title: `Switch to ${view === "list" ? "Grid" : "List"} view` });

    // Details pane toggle
    const detailsBtn = toolBtn(ICONS.panel, "Details", () => {
      showDetailsPane = !showDetailsPane;
      render();
    }, { active: showDetailsPane, title: "Toggle Details & Inspector panel" });

    // Show/Hide hidden toggle
    const hiddenBtn = toolBtn(ICONS.eye, "Hidden", async () => {
      if (!state) return;
      state.config.show_hidden = !state.config.show_hidden;
      await filesApi.saveConfig(state.config);
      await load(current, false);
    }, { active: state?.config.show_hidden ?? false, title: "Toggle hidden files" });

    commandBar.append(viewBtn, detailsBtn, hiddenBtn, divider());

    // Real-time live search filter
    const searchWrap = el("div", {
      class: "flex items-center gap-2 rounded-lg bg-white/[0.04] px-2.5 py-1 text-xs text-neutral-300 ring-1 ring-white/10 focus-within:ring-emerald-500/50 focus-within:bg-white/[0.07] transition-all ml-1 w-44 sm:w-56",
    });
    const searchIcon = el("span", { class: "size-3.5 flex items-center justify-center shrink-0", html: ICONS.search });
    const searchInput = el("input", {
      type: "text",
      placeholder: "Filter files...",
      class: "w-full bg-transparent text-xs text-white placeholder-neutral-500 outline-none",
      value: searchQuery,
    }) as HTMLInputElement;
    searchInput.addEventListener("input", () => {
      searchQuery = searchInput.value;
      renderEntries();
      renderStatusLine();
    });
    searchWrap.append(searchIcon, searchInput);

    if (searchQuery) {
      const clearBtn = el("button", {
        type: "button",
        class: "text-neutral-500 hover:text-white shrink-0",
        html: ICONS.close,
      });
      clearBtn.addEventListener("click", () => {
        searchQuery = "";
        searchInput.value = "";
        renderEntries();
        renderStatusLine();
      });
      searchWrap.append(clearBtn);
    }
    commandBar.append(searchWrap);

    // Right-aligned status
    const statusWrap = el("div", { class: "ml-auto flex items-center gap-3 shrink-0" }, [statusLine]);
    commandBar.append(statusWrap);
  }

  function renderStatusLine(): void {
    if (status) {
      statusLine.textContent = status;
      return;
    }
    if (!state) {
      statusLine.textContent = "";
      return;
    }
    const total = state.listing.entries.length;
    const filtered = getVisibleEntries().length;
    if (searchQuery) {
      statusLine.textContent = `${filtered} of ${total} item(s)`;
    } else {
      statusLine.textContent = selected.size > 0 ? `${selected.size} selected (${total} items)` : `${total} items`;
    }
  }

  function renderAddressBar(): void {
    addressBarRow.replaceChildren();

    // Back & Forward history buttons
    const backBtn = document.createElement("button");
    backBtn.type = "button";
    backBtn.className = `flex size-7 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-neutral-400 transition-colors ${
      historyIndex > 0 ? "hover:border-white/20 hover:bg-white/10 hover:text-white cursor-pointer" : "opacity-30 cursor-not-allowed"
    }`;
    backBtn.innerHTML = ICONS.back;
    backBtn.title = "Back";
    backBtn.disabled = historyIndex <= 0;
    backBtn.addEventListener("click", goBack);

    const forwardBtn = document.createElement("button");
    forwardBtn.type = "button";
    forwardBtn.className = `flex size-7 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-neutral-400 transition-colors ${
      historyIndex < history.length - 1 ? "hover:border-white/20 hover:bg-white/10 hover:text-white cursor-pointer" : "opacity-30 cursor-not-allowed"
    }`;
    forwardBtn.innerHTML = ICONS.forward;
    forwardBtn.title = "Forward";
    forwardBtn.disabled = historyIndex >= history.length - 1;
    forwardBtn.addEventListener("click", goForward);

    addressBarRow.append(backBtn, forwardBtn);

    // Address Bar Pill Container
    const addressPill = el("div", {
      class: "flex min-w-0 flex-1 items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.03] px-2.5 py-1 text-xs text-neutral-300 transition-colors focus-within:border-emerald-500/50 focus-within:ring-1 focus-within:ring-emerald-500/30",
    });

    if (isEditingPath) {
      // Editable path input
      const pathInput = el("input", {
        type: "text",
        class: "w-full bg-transparent font-mono text-xs text-white outline-none",
        value: current,
      }) as HTMLInputElement;

      pathInput.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          const target = pathInput.value.trim();
          isEditingPath = false;
          if (target) void load(target);
          else renderAddressBar();
        } else if (e.key === "Escape") {
          isEditingPath = false;
          renderAddressBar();
        }
      });
      pathInput.addEventListener("blur", () => {
        isEditingPath = false;
        renderAddressBar();
      });

      addressPill.append(
        el("span", { class: "size-4 flex items-center justify-center shrink-0 text-emerald-400", html: ICONS.drive }),
        pathInput,
      );
      addressBarRow.append(addressPill);
      window.setTimeout(() => {
        pathInput.focus();
        pathInput.select();
      }, 0);
      return;
    }

    // Interactive Breadcrumbs Mode
    const driveIcon = el("span", { class: "size-4 flex items-center justify-center shrink-0 text-emerald-400", html: ICONS.drive });
    addressPill.append(driveIcon);

    if (state && state.roots.length > 0) {
      // Find matching root
      const matchingRoot = state.roots
        .filter((r) => current === r || current.startsWith(r + "/") || current.startsWith(r + "\\"))
        .sort((a, b) => b.length - a.length)[0] || state.roots[0];

      const rootBtn = el("button", {
        type: "button",
        class: "rounded px-1.5 py-0.5 font-medium text-white hover:bg-white/10 transition-colors cursor-pointer",
        text: matchingRoot,
      });
      rootBtn.addEventListener("click", () => void load(matchingRoot));
      addressPill.append(rootBtn);

      const remainder = current.slice(matchingRoot.length);
      const parts = remainder.split("/").filter(Boolean);
      let accumulated = matchingRoot.replace(/\/+$/, "");

      for (const part of parts) {
        accumulated += `/${part}`;
        const target = accumulated;
        addressPill.append(el("span", { class: "text-neutral-500", html: ICONS.chevronRight }));
        const segBtn = el("button", {
          type: "button",
          class: "rounded px-1.5 py-0.5 text-neutral-300 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
          text: part,
        });
        segBtn.addEventListener("click", () => void load(target));
        addressPill.append(segBtn);
      }
    } else {
      addressPill.append(el("span", { class: "text-neutral-400", text: current || "/" }));
    }

    // Empty space click switches to text edit mode
    addressPill.addEventListener("click", (e) => {
      if (e.target === addressPill) {
        isEditingPath = true;
        renderAddressBar();
      }
    });

    // Copy path button on right side of address bar
    const copyBtn = el("button", {
      type: "button",
      class: "ml-auto flex size-6 shrink-0 items-center justify-center rounded text-neutral-400 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
      html: ICONS.copy,
      title: "Copy current path to clipboard",
    });
    copyBtn.addEventListener("click", async (e) => {
      e.stopPropagation();
      const ok = await copyToClipboard(current);
      if (ok) {
        copyBtn.innerHTML = ICONS.check;
        window.setTimeout(() => (copyBtn.innerHTML = ICONS.copy), 1500);
      }
    });

    // Switch to edit path button
    const editBtn = el("button", {
      type: "button",
      class: "flex size-6 shrink-0 items-center justify-center rounded text-neutral-400 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
      html: ICONS.edit,
      title: "Edit path directly",
    });
    editBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      isEditingPath = true;
      renderAddressBar();
    });

    addressPill.append(copyBtn, editBtn);
    addressBarRow.append(addressPill);
  }

  // -------------------------------------------------------------------------
  // Left Sidebar: Locations / Volumes & Quick Shortcuts
  // -------------------------------------------------------------------------
  function renderSidebar(): void {
    sidebar.replaceChildren();

    // Section 1: Locations / Roots
    const rootsHeader = el("div", {
      class: "px-1 text-[10px] font-semibold uppercase tracking-wider text-neutral-500",
      text: "Locations",
    });
    sidebar.append(rootsHeader);

    if (!state || state.roots.length === 0) {
      sidebar.append(el("div", { class: "px-2 text-xs italic text-neutral-600", text: "No roots configured" }));
    } else {
      for (const rootPath of state.roots) {
        const isRootActive = current === rootPath || current.startsWith(rootPath + "/");
        const item = el("button", {
          type: "button",
          class: `group flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-xs transition-colors cursor-pointer ${
            isRootActive ? "bg-white/[0.08] font-medium text-white ring-1 ring-white/10" : "text-neutral-400 hover:bg-white/[0.04] hover:text-white"
          }`,
        });
        item.append(
          el("span", { class: "size-4 flex items-center justify-center shrink-0", html: ICONS.drive }),
          el("span", { class: "truncate flex-1 font-mono text-[11.5px]", text: rootPath }),
        );
        item.addEventListener("click", () => void load(rootPath));
        sidebar.append(item);
      }
    }

    // Section 2: Quick Shortcuts
    const shortcutsHeader = el("div", {
      class: "mt-2 px-1 text-[10px] font-semibold uppercase tracking-wider text-neutral-500",
      text: "Shortcuts",
    });
    sidebar.append(shortcutsHeader);

    const shortcuts = [
      { label: "Stacks", path: "/var/lib/stack-stack/stacks" },
      { label: "Images", path: "/var/lib/stack-stack/images" },
      { label: "Guests", path: "/var/lib/stack-stack/guests" },
      { label: "Backups", path: "/var/lib/stack-stack/deploy-backups" },
    ];

    for (const sc of shortcuts) {
      const isScActive = current === sc.path || current.startsWith(sc.path + "/");
      const item = el("button", {
        type: "button",
        class: `group flex items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-xs transition-colors cursor-pointer ${
          isScActive ? "bg-white/[0.08] font-medium text-white" : "text-neutral-400 hover:bg-white/[0.04] hover:text-white"
        }`,
      });
      item.append(
        el("span", { class: "size-4 flex items-center justify-center shrink-0 text-blue-400", html: ICONS.folder }),
        el("span", { class: "truncate flex-1", text: sc.label }),
      );
      item.addEventListener("click", () => void load(sc.path));
      sidebar.append(item);
    }
  }

  // -------------------------------------------------------------------------
  // Filtering & Sorting
  // -------------------------------------------------------------------------
  function getVisibleEntries(): FileEntry[] {
    if (!state) return [];
    let list = state.listing.entries;
    if (searchQuery.trim()) {
      const q = searchQuery.toLowerCase();
      list = list.filter((e) => e.name.toLowerCase().includes(q));
    }
    // Always sort folders first, then files by sortKey
    return list.slice().sort((a, b) => {
      if (a.kind !== b.kind) {
        return a.kind === "dir" ? -1 : 1;
      }
      let cmp = 0;
      switch (sortKey) {
        case "name":
          cmp = a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
          break;
        case "modified":
          cmp = (a.modified_ms ?? 0) - (b.modified_ms ?? 0);
          break;
        case "size":
          cmp = a.size - b.size;
          break;
        case "type":
          cmp = getFileTypeDescription(a).localeCompare(getFileTypeDescription(b));
          break;
      }
      return sortAsc ? cmp : -cmp;
    });
  }

  // -------------------------------------------------------------------------
  // Main Content & Entries Rendering
  // -------------------------------------------------------------------------
  function render(): void {
    renderChrome();
    renderSidebar();
    renderEntries();
    renderDetailsPane();
  }

  function renderEntries(): void {
    tableHost.replaceChildren();

    if (!loaded) {
      tableHost.append(
        el("div", { class: "flex h-64 items-center justify-center text-xs text-neutral-500", text: "Loading directory listing…" }),
      );
      return;
    }

    if (!state) {
      const detail = loadError ?? status ?? "The file API is unavailable";
      tableHost.append(
        el("div", { class: "m-4 rounded-xl border border-rose-500/20 bg-rose-950/20 p-4 text-xs text-rose-300" }, [
          el("div", { class: "font-semibold mb-1", text: `Could not list ${current}` }),
          el("div", { class: "text-rose-400 font-mono", text: detail }),
        ]),
      );
      return;
    }

    const entries = getVisibleEntries();

    if (entries.length === 0) {
      tableHost.append(
        el("div", { class: "flex h-64 flex-col items-center justify-center gap-2 text-center text-xs text-neutral-500" }, [
          el("span", { class: "size-8 flex items-center justify-center text-neutral-600", html: ICONS.folder }),
          el("span", { text: searchQuery ? "No files match your filter" : "This directory is empty" }),
        ]),
      );
      return;
    }

    if (view === "grid") {
      renderGrid(entries);
    } else {
      renderTable(entries);
    }

    // Multi-selection floating bottom action bar
    if (selected.size > 0) {
      centerContent.append(renderBulkBar());
    }
  }

  function renderBulkBar(): HTMLElement {
    // Remove previous bulk bar if any
    centerContent.querySelectorAll(".ss-bulk-bar").forEach((b) => b.remove());

    const bar = el("div", {
      class: "ss-bulk-bar absolute bottom-4 left-1/2 -translate-x-1/2 z-30 flex items-center gap-2 rounded-xl border border-white/10 bg-[#131722]/95 px-4 py-2 shadow-2xl backdrop-blur-xl text-xs",
    });

    bar.append(
      el("span", { class: "font-medium text-emerald-400 mr-2", text: `${selected.size} selected` }),
      toolBtn(ICONS.download, "Download Archive", () => {
        openArchiveDialog(Array.from(selected)[0]);
      }),
      toolBtn(ICONS.trash, `Delete (${selected.size})`, async () => {
        if (!window.confirm(`Delete ${selected.size} selected item(s)?`)) return;
        const targets = Array.from(selected);
        busy = true;
        status = "deleting…";
        renderChrome();
        try {
          for (const target of targets) {
            await filesApi.remove(target, true);
          }
          selected.clear();
          status = "deleted";
          await load(current, false);
        } catch (err) {
          status = errorText(err);
          renderChrome();
        } finally {
          busy = false;
        }
      }),
      toolBtn(ICONS.close, "Deselect", () => {
        selected.clear();
        render();
      }),
    );
    return bar;
  }

  // -------------------------------------------------------------------------
  // Table / List View
  // -------------------------------------------------------------------------
  function renderTable(entries: FileEntry[]): void {
    const table = el("table", { class: "w-full text-left text-xs border-collapse" });

    // Table Header
    const allSelected = entries.length > 0 && entries.every((e) => selected.has(e.path));
    const selectAllBox = el("input", {
      type: "checkbox",
      class: "size-4 rounded border-white/20 bg-white/5 accent-emerald-500 cursor-pointer",
    }) as HTMLInputElement;
    selectAllBox.checked = allSelected;
    selectAllBox.addEventListener("change", () => {
      if (selectAllBox.checked) {
        for (const e of entries) selected.add(e.path);
      } else {
        selected.clear();
      }
      render();
    });

    const th = (label: string, key: SortKey | null, extraClass = ""): HTMLElement => {
      const cell = el("th", {
        class: `sticky top-0 z-10 border-b border-white/10 bg-[#0f1219]/95 px-3 py-2 text-[11px] font-semibold text-neutral-400 backdrop-blur-md transition-colors ${
          key ? "cursor-pointer hover:text-white select-none" : ""
        } ${extraClass}`,
      });
      if (!key) {
        cell.append(label);
        return cell;
      }
      const isCurrentSort = sortKey === key;
      const textSpan = el("span", { text: label });
      const indicator = el("span", {
        class: `ml-1 inline-block text-[10px] ${isCurrentSort ? "text-emerald-400" : "text-transparent"}`,
        text: isCurrentSort ? (sortAsc ? "▲" : "▼") : "▲",
      });
      cell.append(textSpan, indicator);
      cell.addEventListener("click", () => {
        if (sortKey === key) {
          sortAsc = !sortAsc;
        } else {
          sortKey = key;
          sortAsc = true;
        }
        renderEntries();
      });
      return cell;
    };

    const thead = el("thead", {}, [
      el("tr", {}, [
        el("th", { class: "sticky top-0 z-10 w-9 border-b border-white/10 bg-[#0f1219]/95 px-3 py-2 text-center" }, [selectAllBox]),
        th("Name", "name", "min-w-48"),
        th("Date modified", "modified", "w-44"),
        th("Type", "type", "w-32 hidden sm:table-cell"),
        th("Size", "size", "w-24 text-right"),
        th("Mode", null, "w-24 hidden md:table-cell font-mono"),
        th("Owner", null, "w-28 hidden lg:table-cell"),
        el("th", { class: "sticky top-0 z-10 w-28 border-b border-white/10 bg-[#0f1219]/95 px-3 py-2 text-right" }),
      ]),
    ]);
    table.append(thead);

    // Table Rows
    const tbody = el("tbody", { class: "divide-y divide-white/[0.04]" });
    for (const entry of entries) {
      tbody.append(renderTableRow(entry));
    }
    table.append(tbody);
    tableHost.append(table);
  }

  function renderTableRow(entry: FileEntry): HTMLElement {
    const isSelected = selected.has(entry.path);
    const row = el("tr", {
      class: `group transition-colors cursor-pointer ${
        isSelected ? "bg-emerald-500/[0.08]" : "hover:bg-white/[0.04]"
      }`,
    });

    // Checkbox
    const check = el("input", {
      type: "checkbox",
      class: "size-4 rounded border-white/20 bg-white/5 accent-emerald-500 cursor-pointer",
    }) as HTMLInputElement;
    check.checked = isSelected;
    check.addEventListener("change", (e) => {
      e.stopPropagation();
      if (check.checked) selected.add(entry.path);
      else selected.delete(entry.path);
      render();
    });
    const checkTd = el("td", { class: "w-9 px-3 py-2 text-center" }, [check]);

    // Name + Icon
    const nameTd = el("td", { class: "px-3 py-2" });
    const nameWrap = el("div", { class: "flex items-center gap-2.5 min-w-0" });
    const iconEl = el("span", { class: "size-4 flex items-center justify-center shrink-0", html: getFileIcon(entry) });
    const label = el("span", {
      class: `truncate font-medium ${entry.kind === "dir" ? "text-neutral-100 group-hover:text-emerald-400" : "text-neutral-200"}`,
      text: entry.name,
    });
    nameWrap.append(iconEl, label);
    if (entry.symlink_target) {
      nameWrap.append(el("span", { class: "text-neutral-500 text-[11px] font-mono", text: `→ ${entry.symlink_target}` }));
    }
    nameTd.append(nameWrap);

    // Date modified
    const modTd = el("td", { class: "px-3 py-2 tabular text-neutral-400 whitespace-nowrap", text: formatTime(entry.modified_ms) });

    // Type description
    const typeTd = el("td", { class: "px-3 py-2 text-neutral-400 truncate hidden sm:table-cell", text: getFileTypeDescription(entry) });

    // Size
    const sizeTd = el("td", {
      class: "px-3 py-2 tabular text-right text-neutral-400 whitespace-nowrap",
      text: entry.kind === "dir" ? "—" : bytes(entry.size),
    });

    // Mode
    const modeTd = el("td", {
      class: "px-3 py-2 hidden md:table-cell font-mono text-[11px] text-neutral-500 whitespace-nowrap",
      text: entry.mode ?? "—",
    });

    // Owner
    const ownerTd = el("td", {
      class: "px-3 py-2 hidden lg:table-cell text-neutral-400 whitespace-nowrap",
      text: `${entry.owner ?? "—"}:${entry.group ?? "—"}`,
    });

    // Floating Hover Actions on right
    const actionsTd = el("td", { class: "px-3 py-2 text-right whitespace-nowrap" });
    const hoverActions = el("div", {
      class: "flex items-center justify-end gap-1 opacity-0 group-hover:opacity-100 transition-opacity",
    });

    if (entry.kind !== "dir") {
      const dlBtn = el("a", {
        href: filesApi.downloadUrl(entry.path),
        class: "flex size-6 items-center justify-center rounded text-neutral-400 hover:bg-white/10 hover:text-white transition-colors",
        title: "Download",
        html: ICONS.download,
      });
      dlBtn.addEventListener("click", (e) => e.stopPropagation());
      hoverActions.append(dlBtn);
    }

    if (entry.kind === "file" && (isText(entry.name) || isImage(entry.name))) {
      const editBtn = el("button", {
        type: "button",
        class: "flex size-6 items-center justify-center rounded text-neutral-400 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
        title: "Edit in Built-in Editor",
        html: ICONS.edit,
      });
      editBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        void openBuiltinEditor(entry);
      });
      hoverActions.append(editBtn);
    }

    const moreBtn = el("button", {
      type: "button",
      class: "flex size-6 items-center justify-center rounded text-neutral-400 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
      title: "More options",
      html: ICONS.more,
    });
    moreBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      const rect = moreBtn.getBoundingClientRect();
      openContextMenu(entry, rect.left, rect.bottom + 4);
    });
    hoverActions.append(moreBtn);
    actionsTd.append(hoverActions);

    row.append(checkTd, nameTd, modTd, typeTd, sizeTd, modeTd, ownerTd, actionsTd);

    // Row click interactions
    row.addEventListener("click", () => {
      preview = entry;
      renderDetailsPane();
    });
    row.addEventListener("dblclick", () => {
      if (entry.kind === "dir") {
        void load(entry.path);
      } else if (isText(entry.name)) {
        void openBuiltinEditor(entry);
      } else if (isImage(entry.name)) {
        preview = entry;
        showDetailsPane = true;
        render();
      }
    });
    row.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      openContextMenu(entry, e.clientX, e.clientY);
    });

    return row;
  }

  // -------------------------------------------------------------------------
  // Grid View
  // -------------------------------------------------------------------------
  function renderGrid(entries: FileEntry[]): void {
    const grid = el("div", {
      class: "grid grid-cols-2 gap-3 p-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 2xl:grid-cols-8",
    });

    for (const entry of entries) {
      const isSelected = selected.has(entry.path);
      const card = el("div", {
        class: `group relative flex flex-col items-center justify-between rounded-xl border p-3 text-center transition-all cursor-pointer ${
          isSelected
            ? "border-emerald-500/50 bg-emerald-500/[0.08]"
            : "border-white/5 bg-[#0f1219]/40 hover:border-white/20 hover:bg-white/[0.04]"
        }`,
      });

      // Checkbox top-left
      const check = el("input", {
        type: "checkbox",
        class: `absolute left-2.5 top-2.5 size-4 rounded border-white/20 bg-white/5 accent-emerald-500 cursor-pointer ${
          isSelected ? "opacity-100" : "opacity-0 group-hover:opacity-100"
        } transition-opacity`,
      }) as HTMLInputElement;
      check.checked = isSelected;
      check.addEventListener("change", (e) => {
        e.stopPropagation();
        if (check.checked) selected.add(entry.path);
        else selected.delete(entry.path);
        render();
      });

      // Icon container
      const iconWrap = el("div", {
        class: "my-2 flex size-12 items-center justify-center rounded-xl bg-white/[0.02] text-neutral-300",
      });
      const iconEl = el("span", { class: "size-8 flex items-center justify-center", html: getFileIcon(entry) });
      iconWrap.append(iconEl);

      // Name & Size
      const nameSpan = el("span", {
        class: `w-full truncate text-xs font-medium ${entry.kind === "dir" ? "text-neutral-100 group-hover:text-emerald-400" : "text-neutral-200"}`,
        text: entry.name,
        title: entry.name,
      });

      const sizeSpan = el("span", {
        class: "tabular text-[10px] text-neutral-500 mt-0.5",
        text: entry.kind === "dir" ? "Folder" : bytes(entry.size),
      });

      card.append(check, iconWrap, nameSpan, sizeSpan);

      card.addEventListener("click", () => {
        preview = entry;
        renderDetailsPane();
      });
      card.addEventListener("dblclick", () => {
        if (entry.kind === "dir") {
          void load(entry.path);
        } else if (isText(entry.name)) {
          void openBuiltinEditor(entry);
        } else if (isImage(entry.name)) {
          preview = entry;
          showDetailsPane = true;
          render();
        }
      });
      card.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        e.stopPropagation();
        openContextMenu(entry, e.clientX, e.clientY);
      });

      grid.append(card);
    }

    tableHost.append(grid);
  }

  // -------------------------------------------------------------------------
  // Right-hand Details & Inspector Pane
  // -------------------------------------------------------------------------
  function renderDetailsPane(): void {
    if (!showDetailsPane) {
      detailsPane.classList.add("hidden");
      detailsPane.replaceChildren();
      return;
    }
    detailsPane.classList.remove("hidden");
    detailsPane.replaceChildren();

    const header = el("div", { class: "flex items-center justify-between border-b border-white/10 pb-2" }, [
      el("span", { class: "text-xs font-semibold uppercase tracking-wider text-neutral-400", text: "Details" }),
      el("button", {
        type: "button",
        class: "text-neutral-400 hover:text-white cursor-pointer",
        html: ICONS.close,
        title: "Close details pane",
      }),
    ]);
    (header.querySelector("button") as HTMLElement).addEventListener("click", () => {
      showDetailsPane = false;
      render();
    });
    detailsPane.append(header);

    if (!preview) {
      detailsPane.append(
        el("div", { class: "py-12 text-center text-xs text-neutral-500", text: "Select a file or folder to view its properties." }),
      );
      return;
    }

    const entry = preview;

    // Preview Header: Icon + Name
    const previewHeader = el("div", { class: "flex flex-col items-center gap-2 p-2 text-center" }, [
      el("div", { class: "flex size-14 items-center justify-center rounded-2xl bg-white/[0.04]", html: getFileIcon(entry) }),
      el("div", { class: "w-full truncate font-semibold text-white", text: entry.name, title: entry.name }),
      el("div", { class: "text-[11px] text-neutral-400 font-mono break-all", text: entry.path }),
    ]);
    detailsPane.append(previewHeader);

    // Quick Action Buttons
    const actionsRow = el("div", { class: "flex gap-2" });
    if (entry.kind !== "dir") {
      actionsRow.append(
        el("a", {
          href: filesApi.downloadUrl(entry.path),
          class: "flex-1 flex items-center justify-center gap-1.5 rounded-lg border border-white/10 bg-white/5 py-1.5 text-xs font-medium text-neutral-200 hover:bg-white/10 hover:text-white transition-colors",
          text: "Download",
        }),
      );
    }
    if (entry.kind === "file" && isText(entry.name)) {
      const editBtn = el("button", {
        type: "button",
        class: "flex-1 flex items-center justify-center gap-1.5 rounded-lg bg-emerald-600 py-1.5 text-xs font-medium text-white hover:bg-emerald-500 transition-colors cursor-pointer",
        text: "Open Editor",
      });
      editBtn.addEventListener("click", () => void openBuiltinEditor(entry));
      actionsRow.append(editBtn);
    }
    detailsPane.append(actionsRow);

    // Visual Preview for Image or Text
    if (isImage(entry.name)) {
      const imgCard = el("div", { class: "overflow-hidden rounded-xl border border-white/10 bg-black/40 p-1" }, [
        el("img", {
          src: filesApi.downloadUrl(entry.path),
          alt: entry.name,
          class: "max-h-56 w-full rounded-lg object-contain",
        }),
      ]);
      detailsPane.append(imgCard);
    } else if (isText(entry.name)) {
      const previewCard = el("div", { class: "flex flex-col gap-1 rounded-xl border border-white/10 bg-black/40 p-2.5" }, [
        el("div", { class: "flex items-center justify-between text-[10px] text-neutral-500" }, [
          el("span", { text: "Preview (first 25 lines)" }),
          el("button", {
            type: "button",
            class: "text-emerald-400 hover:underline cursor-pointer",
            text: "Edit full",
          }),
        ]),
        el("pre", { class: "max-h-56 overflow-auto font-mono text-[10px] text-neutral-300 whitespace-pre-wrap break-all", text: "Loading…" }),
      ]);
      (previewCard.querySelector("button") as HTMLElement).addEventListener("click", () => void openBuiltinEditor(entry));
      const preEl = previewCard.querySelector("pre") as HTMLElement;
      void filesApi.read(entry.path).then((f) => {
        if (preview?.path === entry.path) {
          preEl.textContent = f.content.split("\n").slice(0, 25).join("\n");
        }
      }).catch((e) => {
        preEl.textContent = errorText(e);
      });
      detailsPane.append(previewCard);
    }

    // Metadata Properties Grid
    const metaBox = el("div", { class: "flex flex-col gap-2 rounded-xl border border-white/5 bg-white/[0.02] p-3 text-xs" });
    const row = (label: string, val: string): HTMLElement =>
      el("div", { class: "flex items-center justify-between" }, [
        el("span", { class: "text-neutral-500", text: label }),
        el("span", { class: "font-medium text-neutral-200 tabular", text: val }),
      ]);

    metaBox.append(
      row("Type", getFileTypeDescription(entry)),
      row("Size", entry.kind === "dir" ? "—" : bytes(entry.size)),
      row("Modified", formatTime(entry.modified_ms)),
      row("Permissions", entry.mode ?? "—"),
      row("Owner", `${entry.owner ?? "—"}:${entry.group ?? "—"}`),
    );
    detailsPane.append(metaBox);
  }

  // -------------------------------------------------------------------------
  // Windows 11 Style Context Menu
  // -------------------------------------------------------------------------
  function openContextMenu(entry: FileEntry, x: number, y: number): void {
    document.querySelectorAll(".ss-context-menu").forEach((m) => m.remove());

    const menu = el("div", {
      class: "ss-context-menu fixed z-50 flex min-w-56 flex-col gap-0.5 rounded-xl border border-white/10 bg-[#131722]/95 p-1.5 shadow-2xl backdrop-blur-xl text-xs",
    });

    // Positioning with bounds checking
    const menuWidth = 230;
    const menuHeight = 320;
    menu.style.left = `${Math.min(x, window.innerWidth - menuWidth - 10)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - menuHeight - 10)}px`;

    // 1. Top Quick Action Bar (Edit, Download, Rename, Copy, Delete)
    const quickBar = el("div", { class: "flex items-center justify-around border-b border-white/10 pb-1.5 pt-0.5 px-1" });
    const quickIcon = (icon: string, title: string, action: () => void, danger = false): HTMLElement => {
      const btn = el("button", {
        type: "button",
        class: `flex size-7 items-center justify-center rounded-lg ${
          danger ? "text-rose-400 hover:bg-rose-500/20" : "text-neutral-300 hover:bg-white/10 hover:text-white"
        } transition-colors cursor-pointer`,
        title,
        html: icon,
      });
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        close();
        action();
      });
      return btn;
    };

    if (entry.kind === "file" && isText(entry.name)) {
      quickBar.append(quickIcon(ICONS.edit, "Edit in built-in editor", () => void openBuiltinEditor(entry)));
    }
    if (entry.kind !== "dir") {
      quickBar.append(quickIcon(ICONS.download, "Download", () => (window.location.href = filesApi.downloadUrl(entry.path))));
    }
    quickBar.append(
      quickIcon(ICONS.rename, "Rename", () => void promptRename(entry)),
      quickIcon(ICONS.copy, "Copy path", () => {
        void copyToClipboard(entry.path).then((ok) => {
          if (ok) {
            status = `Copied ${entry.path}`;
            renderChrome();
          }
        });
      }),
      quickIcon(ICONS.trash, "Delete", () => void removeEntry(entry), true),
    );
    menu.append(quickBar);

    // 2. Menu Items
    const addItem = (icon: string, label: string, action: () => void, shortcut = "", danger = false): void => {
      const item = el("button", {
        type: "button",
        class: `flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-xs transition-colors cursor-pointer ${
          danger ? "text-rose-400 hover:bg-rose-500/10" : "text-neutral-200 hover:bg-white/[0.08] hover:text-white"
        }`,
      });
      const iconSpan = el("span", { class: "size-4 flex items-center justify-center shrink-0", html: icon });
      const labelSpan = el("span", { class: "flex-1", text: label });
      item.append(iconSpan, labelSpan);
      if (shortcut) {
        item.append(el("span", { class: "text-[10px] text-neutral-500", text: shortcut }));
      }
      item.addEventListener("click", (e) => {
        e.stopPropagation();
        close();
        action();
      });
      menu.append(item);
    };

    const separator = (): void => {
      menu.append(el("div", { class: "my-1 border-t border-white/10" }));
    };

    if (entry.kind === "dir") {
      addItem(ICONS.folder, "Open", () => void load(entry.path), "Enter");
      addItem(ICONS.archive, "Compress to .tar.zst", () => void promptName("Archive file name", (n) => filesApi.createArchive(entry.path, n)));
      addItem(ICONS.download, "Download as archive", () => (window.location.href = filesApi.archiveUrl(entry.path)));
    } else {
      if (isText(entry.name)) {
        addItem(ICONS.edit, "Edit with Code Editor", () => void openBuiltinEditor(entry));
      }
      addItem(ICONS.download, "Download", () => (window.location.href = filesApi.downloadUrl(entry.path)));
      addItem(ICONS.panel, "View details", () => {
        preview = entry;
        showDetailsPane = true;
        render();
      });
    }

    separator();

    if (entry.kind === "file" && isArchive(entry.name)) {
      addItem(ICONS.archive, "Extract archive", async () => {
        busy = true;
        status = "extracting…";
        renderChrome();
        try {
          await filesApi.extract(entry.path);
          status = "extracted";
          await load(current, false);
        } catch (err) {
          status = errorText(err);
          renderChrome();
        } finally {
          busy = false;
        }
      });
    }

    addItem(ICONS.rename, "Rename", () => void promptRename(entry), "F2");
    addItem(ICONS.copy, "Copy path", () => {
      void copyToClipboard(entry.path).then((ok) => {
        if (ok) {
          status = `Copied ${entry.path}`;
          renderChrome();
        }
      });
    });

    separator();

    if (entry.kind === "file") {
      addItem(ICONS.lock, "Permissions (chmod)", () => void promptPermissions(entry));
      addItem(ICONS.user, "Ownership (chown)", () => void promptOwner(entry));
      separator();
    }

    addItem(ICONS.trash, "Delete", () => void removeEntry(entry), "Del", true);

    const close = (): void => {
      menu.remove();
      document.removeEventListener("click", close);
      document.removeEventListener("keydown", onKey);
    };
    const onKey = (ev: KeyboardEvent): void => {
      if (ev.key === "Escape") close();
    };

    document.body.append(menu);
    window.setTimeout(() => {
      document.addEventListener("click", close);
      document.addEventListener("keydown", onKey);
    }, 0);
  }

  function openBlankContextMenu(x: number, y: number): void {
    document.querySelectorAll(".ss-context-menu").forEach((m) => m.remove());

    const menu = el("div", {
      class: "ss-context-menu fixed z-50 flex min-w-48 flex-col gap-0.5 rounded-xl border border-white/10 bg-[#131722]/95 p-1.5 shadow-2xl backdrop-blur-xl text-xs",
    });
    menu.style.left = `${Math.min(x, window.innerWidth - 210)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - 200)}px`;

    const addItem = (icon: string, label: string, action: () => void): void => {
      const item = el("button", {
        type: "button",
        class: "flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-xs text-neutral-200 hover:bg-white/[0.08] hover:text-white transition-colors cursor-pointer",
      });
      item.append(el("span", { class: "size-4 flex items-center justify-center shrink-0", html: icon }), el("span", { text: label }));
      item.addEventListener("click", (e) => {
        e.stopPropagation();
        close();
        action();
      });
      menu.append(item);
    };

    addItem(ICONS.plus, "New folder", () => void promptName("New folder", (n) => filesApi.mkdir(join(current, n))));
    addItem(ICONS.fileCode, "New file", () => void promptName("New file", (n) => filesApi.createFile(join(current, n))));
    addItem(ICONS.upload, "Upload files", () => uploadInput.click());
    menu.append(el("div", { class: "my-1 border-t border-white/10" }));
    addItem(ICONS.refresh, "Refresh", () => void load(current, false));
    addItem(ICONS.copy, "Copy folder path", () => void copyToClipboard(current));

    const close = (): void => {
      menu.remove();
      document.removeEventListener("click", close);
      document.removeEventListener("keydown", onKey);
    };
    const onKey = (ev: KeyboardEvent): void => {
      if (ev.key === "Escape") close();
    };

    document.body.append(menu);
    window.setTimeout(() => {
      document.addEventListener("click", close);
      document.addEventListener("keydown", onKey);
    }, 0);
  }

  function openNewMenu(anchor: HTMLElement): void {
    document.querySelectorAll(".ss-context-menu").forEach((m) => m.remove());
    const rect = anchor.getBoundingClientRect();
    const menu = el("div", {
      class: "ss-context-menu fixed z-50 flex min-w-44 flex-col gap-0.5 rounded-xl border border-white/10 bg-[#131722]/95 p-1.5 shadow-2xl backdrop-blur-xl text-xs",
    });
    menu.style.left = `${rect.left}px`;
    menu.style.top = `${rect.bottom + 4}px`;

    const addItem = (icon: string, label: string, action: () => void): void => {
      const item = el("button", {
        type: "button",
        class: "flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-xs text-neutral-200 hover:bg-white/[0.08] hover:text-white transition-colors cursor-pointer",
      });
      item.append(el("span", { class: "size-4 flex items-center justify-center shrink-0", html: icon }), el("span", { text: label }));
      item.addEventListener("click", (e) => {
        e.stopPropagation();
        close();
        action();
      });
      menu.append(item);
    };

    addItem(ICONS.folder, "New folder", () => void promptName("New folder", (n) => filesApi.mkdir(join(current, n))));
    addItem(ICONS.fileCode, "New file", () => void promptName("New file", (n) => filesApi.createFile(join(current, n))));

    const close = (): void => {
      menu.remove();
      document.removeEventListener("click", close);
    };
    document.body.append(menu);
    window.setTimeout(() => document.addEventListener("click", close), 0);
  }

  // -------------------------------------------------------------------------
  // Built-in Code Editor (IDE Style with line numbers and Ctrl+S)
  // -------------------------------------------------------------------------
  async function openBuiltinEditor(entry: FileEntry): Promise<void> {
    let file;
    try {
      file = await filesApi.read(entry.path);
    } catch (err) {
      status = errorText(err);
      renderChrome();
      return;
    }

    let isDirty = false;
    let wordWrap = false;
    let savedContent = file.content;

    const overlay = el("div", {
      class: "fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm p-3 md:p-6",
    });

    const modalWindow = el("div", {
      class: "flex h-full max-h-[92vh] w-full max-w-5xl flex-col rounded-2xl border border-white/10 bg-[#0e1118] shadow-2xl overflow-hidden",
    });

    // Editor Header
    const dirtyBadge = el("span", {
      class: "hidden rounded-full bg-amber-400/20 px-2 py-0.5 text-[10px] font-semibold text-amber-300 ring-1 ring-amber-400/30",
      text: "Unsaved",
    });

    const saveStatusNotice = el("span", {
      class: "hidden items-center gap-1 text-[11px] font-medium text-emerald-400",
      html: `${ICONS.check} <span>Saved</span>`,
    });

    const header = el("div", {
      class: "flex h-12 shrink-0 items-center justify-between border-b border-white/10 bg-[#131722]/90 px-4",
    });

    const fileMeta = el("div", { class: "flex items-center gap-3 min-w-0" }, [
      el("span", { class: "size-5 flex items-center justify-center text-cyan-400 shrink-0", html: getFileIcon(entry) }),
      el("div", { class: "flex flex-col min-w-0" }, [
        el("div", { class: "flex items-center gap-2" }, [
          el("span", { class: "font-semibold text-white text-xs truncate", text: entry.name }),
          dirtyBadge,
        ]),
        el("span", { class: "font-mono text-[10px] text-neutral-500 truncate", text: entry.path }),
      ]),
    ]);

    const headerActions = el("div", { class: "flex items-center gap-2 shrink-0" });

    const statsInfo = el("span", {
      class: "hidden sm:inline-block tabular text-[11px] text-neutral-400 font-mono mr-2",
      text: `${file.mime} · ${bytes(file.size)}`,
    });

    const wrapBtn = el("button", {
      type: "button",
      class: "rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-xs text-neutral-300 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
      text: "Wrap",
      title: "Toggle Word Wrap",
    });

    const saveBtn = document.createElement("button");
    saveBtn.type = "button";
    saveBtn.className = "flex items-center gap-1.5 rounded-lg bg-emerald-600 px-3 py-1 text-xs font-semibold text-white hover:bg-emerald-500 shadow-sm transition-colors cursor-pointer";
    saveBtn.innerHTML = `<span>Save</span> <span class="text-[10px] opacity-75 font-mono">(Ctrl+S)</span>`;

    const closeBtn = el("button", {
      type: "button",
      class: "flex size-7 items-center justify-center rounded-lg text-neutral-400 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
      html: ICONS.close,
      title: "Close editor (Esc)",
    });

    headerActions.append(saveStatusNotice, statsInfo, wrapBtn, saveBtn, closeBtn);
    header.append(fileMeta, headerActions);
    modalWindow.append(header);

    if (file.truncated) {
      const truncBanner = el("div", {
        class: "flex items-center gap-2 border-b border-amber-500/20 bg-amber-500/10 px-4 py-2 text-xs text-amber-300",
      }, [
        el("span", { class: "font-semibold", text: "Warning:" }),
        el("span", { text: "This file exceeds the editor buffer limit and is truncated. Saving will permanently overwrite the file with only this loaded portion." }),
      ]);
      modalWindow.append(truncBanner);
    }

    // Editor Area with Gutter & Textarea
    const editorBody = el("div", { class: "flex min-h-0 flex-1 bg-[#0b0d13] overflow-hidden" });

    const lineGutter = el("div", {
      class: "w-12 shrink-0 select-none overflow-hidden bg-black/25 py-3 pr-3 text-right font-mono text-[11px] text-neutral-600 border-r border-white/5",
    });

    const textarea = el("textarea", {
      class: "flex-1 overflow-auto bg-transparent p-3 font-mono text-[12px] leading-relaxed text-neutral-100 outline-none selection:bg-emerald-500/30 whitespace-pre",
      spellcheck: false,
    }) as HTMLTextAreaElement;
    textarea.value = file.content;

    editorBody.append(lineGutter, textarea);

    // Footer with line and column count
    const footer = el("div", {
      class: "flex h-7 shrink-0 items-center justify-between border-t border-white/5 bg-[#0f1219] px-4 font-mono text-[10px] text-neutral-500",
    });
    const cursorInfo = el("span", { text: "Ln 1, Col 1" });
    const lineCountInfo = el("span", { text: `${file.content.split("\n").length} lines` });
    footer.append(lineCountInfo, cursorInfo);

    modalWindow.append(editorBody, footer);
    overlay.append(modalWindow);
    document.body.append(overlay);

    // Update line numbers
    const updateLines = (): void => {
      const lines = textarea.value.split("\n").length;
      let numbers = "";
      for (let i = 1; i <= lines; i++) {
        numbers += `${i}\n`;
      }
      lineGutter.textContent = numbers;
      lineCountInfo.textContent = `${lines} lines`;
    };
    updateLines();

    // Sync scrolling between textarea and gutter
    textarea.addEventListener("scroll", () => {
      lineGutter.scrollTop = textarea.scrollTop;
    });

    // Update cursor position Ln/Col
    const updateCursor = (): void => {
      const pos = textarea.selectionStart;
      const text = textarea.value.slice(0, pos);
      const lines = text.split("\n");
      const currentLine = lines.length;
      const currentCol = lines[lines.length - 1].length + 1;
      cursorInfo.textContent = `Ln ${currentLine}, Col ${currentCol}`;
    };
    textarea.addEventListener("keyup", updateCursor);
    textarea.addEventListener("click", updateCursor);

    // Handle Tab key (indent 2 spaces) and dirty tracking
    textarea.addEventListener("keydown", (e) => {
      if (e.key === "Tab") {
        e.preventDefault();
        const start = textarea.selectionStart;
        const end = textarea.selectionEnd;
        textarea.value = textarea.value.substring(0, start) + "  " + textarea.value.substring(end);
        textarea.selectionStart = textarea.selectionEnd = start + 2;
        onDirty();
        updateLines();
      } else if ((e.ctrlKey || e.metaKey) && e.key === "s") {
        e.preventDefault();
        void performSave();
      }
    });

    textarea.addEventListener("input", () => {
      onDirty();
      updateLines();
    });

    const onDirty = (): void => {
      isDirty = textarea.value !== savedContent;
      if (isDirty) {
        dirtyBadge.classList.remove("hidden");
        saveStatusNotice.classList.add("hidden");
      } else {
        dirtyBadge.classList.add("hidden");
      }
    };

    wrapBtn.addEventListener("click", () => {
      wordWrap = !wordWrap;
      textarea.classList.toggle("whitespace-pre", !wordWrap);
      textarea.classList.toggle("whitespace-pre-wrap", wordWrap);
      wrapBtn.classList.toggle("bg-white/10", wordWrap);
    });

    const performSave = async (): Promise<void> => {
      if (file.truncated) {
        if (!window.confirm("Warning: This file was truncated when loaded! Saving will overwrite the entire file with only this truncated portion. Are you sure?")) {
          return;
        }
      }
      try {
        saveBtn.disabled = true;
        saveBtn.textContent = "Saving…";
        await filesApi.save(entry.path, textarea.value);
        savedContent = textarea.value;
        isDirty = false;
        dirtyBadge.classList.add("hidden");
        saveStatusNotice.classList.remove("hidden");
        status = `Saved ${entry.name}`;
        renderChrome();
      } catch (err) {
        showToast({ tone: "error", title: "Save Failed", message: errorText(err) });
        status = `Save failed: ${errorText(err)}`;
        renderChrome();
      } finally {
        saveBtn.disabled = false;
        saveBtn.innerHTML = `<span>Save</span> <span class="text-[10px] opacity-75 font-mono">(Ctrl+S)</span>`;
      }
    };

    saveBtn.addEventListener("click", () => void performSave());

    const closeEditor = (): void => {
      if (isDirty) {
        if (!window.confirm("You have unsaved changes. Discard them?")) return;
      }
      overlay.remove();
      document.removeEventListener("keydown", onGlobalKey);
      void load(current, false);
    };

    closeBtn.addEventListener("click", closeEditor);

    const onGlobalKey = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeEditor();
      }
    };
    document.addEventListener("keydown", onGlobalKey);

    window.setTimeout(() => textarea.focus(), 50);
  }

  // -------------------------------------------------------------------------
  // Dialogs & Modals
  // -------------------------------------------------------------------------
  function openArchiveDialog(targetPath: string): void {
    const targetName = targetPath.split("/").pop() || targetPath;
    const close = modal(
      `Compress ${targetName}`,
      el("div", { class: "flex flex-col gap-4 text-xs" }, [
        el("p", { class: "text-neutral-400", text: "Choose how to create or download this archive (.tar.zst):" }),
        el("div", { class: "flex gap-2" }, [
          el("button", {
            type: "button",
            class: "flex-1 rounded-lg bg-emerald-600 px-3 py-2 font-semibold text-white hover:bg-emerald-500 transition-colors cursor-pointer",
            text: "Create archive here",
          }, []),
          el("button", {
            type: "button",
            class: "flex-1 rounded-lg border border-white/10 bg-white/5 px-3 py-2 font-semibold text-neutral-300 hover:bg-white/10 hover:text-white transition-colors cursor-pointer",
            text: "Download as archive",
          }),
        ]),
      ]),
    );

    const buttons = document.querySelectorAll(".ss-modal-content button");
    if (buttons[0]) {
      buttons[0].addEventListener("click", async () => {
        close();
        try {
          status = "Archiving…";
          renderChrome();
          await filesApi.createArchive(targetPath);
          status = "Archive created";
          await load(current, false);
        } catch (err) {
          status = errorText(err);
          renderChrome();
        }
      });
    }
    if (buttons[1]) {
      buttons[1].addEventListener("click", () => {
        close();
        window.location.href = filesApi.archiveUrl(targetPath);
      });
    }
  }

  function modal(title: string, content: HTMLElement): () => void {
    const overlay = el("div", {
      class: "fixed inset-0 z-50 flex items-center justify-center bg-black/75 backdrop-blur-xs p-4",
    });
    const panel = el("div", {
      class: "ss-modal-content flex w-full max-w-md flex-col gap-3 rounded-2xl border border-white/10 bg-[#131722] p-5 shadow-2xl",
    });
    const close = (): void => overlay.remove();

    const topBar = el("div", { class: "flex items-center justify-between border-b border-white/10 pb-3" }, [
      el("span", { class: "text-sm font-semibold text-white", text: title }),
      el("button", {
        type: "button",
        class: "text-neutral-400 hover:text-white transition-colors cursor-pointer",
        html: ICONS.close,
      }),
    ]);
    (topBar.querySelector("button") as HTMLElement).addEventListener("click", close);

    panel.append(topBar, content);
    overlay.append(panel);
    document.body.append(overlay);

    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) close();
    });
    return close;
  }

  async function promptName(title: string, onSubmit: (val: string) => Promise<unknown>, initial = ""): Promise<void> {
    const input = el("input", {
      type: "text",
      value: initial,
      class: "w-full rounded-lg border border-white/10 bg-black/40 px-3 py-2 font-mono text-xs text-white outline-none focus:border-emerald-500",
    }) as HTMLInputElement;

    const error = el("div", { class: "min-h-4 text-xs text-rose-400" });
    const submitBtn = document.createElement("button");
    submitBtn.type = "button";
    submitBtn.className = "rounded-lg bg-emerald-600 px-4 py-2 font-semibold text-white hover:bg-emerald-500 transition-colors cursor-pointer";
    submitBtn.textContent = "Confirm";

    const close = modal(
      title,
      el("div", { class: "flex flex-col gap-3" }, [input, error, submitBtn]),
    );

    const execute = async (): Promise<void> => {
      const val = input.value.trim();
      if (!val) return;
      try {
        submitBtn.disabled = true;
        await onSubmit(val);
        close();
        await load(current, false);
      } catch (err) {
        error.textContent = errorText(err);
      } finally {
        submitBtn.disabled = false;
      }
    };

    submitBtn.addEventListener("click", () => void execute());
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void execute();
      if (e.key === "Escape") close();
    });

    window.setTimeout(() => {
      input.focus();
      input.select();
    }, 50);
  }

  function promptRename(entry: FileEntry): void {
    void promptName(
      `Rename ${entry.name}`,
      (name) => filesApi.rename(entry.path, join(parentOf(entry.path), name)),
      entry.name,
    );
  }

  function promptPermissions(entry: FileEntry): void {
    void promptName(
      `Change Mode for ${entry.name}`,
      (mode) => filesApi.chmod(entry.path, mode),
      entry.mode ?? "644",
    );
  }

  async function promptOwner(entry: FileEntry): Promise<void> {
    const accounts = await filesApi.accounts().catch(() => ({ users: [], groups: [] }));
    let user = entry.owner ?? "";
    let group = entry.group ?? "";

    const userList = document.createElement("datalist");
    userList.id = "ss-file-users";
    for (const u of accounts.users) {
      const opt = document.createElement("option");
      opt.value = u;
      userList.append(opt);
    }

    const groupList = document.createElement("datalist");
    groupList.id = "ss-file-groups";
    for (const g of accounts.groups) {
      const opt = document.createElement("option");
      opt.value = g;
      groupList.append(opt);
    }

    const userInput = el("input", {
      type: "text",
      value: user,
      placeholder: "e.g. root",
      class: "w-full rounded-lg border border-white/10 bg-black/40 px-3 py-2 text-xs text-white outline-none focus:border-emerald-500",
    }) as HTMLInputElement;
    userInput.setAttribute("list", "ss-file-users");

    const groupInput = el("input", {
      type: "text",
      value: group,
      placeholder: "e.g. stack-stack",
      class: "w-full rounded-lg border border-white/10 bg-black/40 px-3 py-2 text-xs text-white outline-none focus:border-emerald-500",
    }) as HTMLInputElement;
    groupInput.setAttribute("list", "ss-file-groups");

    const error = el("div", { class: "min-h-4 text-xs text-rose-400" });
    const applyBtn = document.createElement("button");
    applyBtn.type = "button";
    applyBtn.className = "rounded-lg bg-emerald-600 px-4 py-2 font-semibold text-white hover:bg-emerald-500 transition-colors cursor-pointer";
    applyBtn.textContent = "Apply Ownership";

    const form = el("div", { class: "flex flex-col gap-3 text-xs" }, [
      el("div", { class: "flex flex-col gap-1" }, [el("label", { class: "text-neutral-400", text: "Owner user:" }), userInput, userList]),
      el("div", { class: "flex flex-col gap-1" }, [el("label", { class: "text-neutral-400", text: "Owner group:" }), groupInput, groupList]),
      error,
      applyBtn,
    ]);

    const close = modal(`Change Owner for ${entry.name}`, form);

    applyBtn.addEventListener("click", async () => {
      user = userInput.value.trim();
      group = groupInput.value.trim();
      try {
        applyBtn.disabled = true;
        await filesApi.chown(entry.path, user || null, group || null);
        close();
        await load(current, false);
      } catch (err) {
        error.textContent = errorText(err);
      } finally {
        applyBtn.disabled = false;
      }
    });
  }

  async function removeEntry(entry: FileEntry): Promise<void> {
    const isDir = entry.kind === "dir";
    const prompt = isDir
      ? `Delete directory '${entry.name}' and all its contents?`
      : `Delete file '${entry.name}'?`;
    if (!window.confirm(prompt)) return;
    busy = true;
    status = "deleting…";
    renderChrome();
    try {
      await filesApi.remove(entry.path, isDir);
      status = "deleted";
      await load(current, false);
    } catch (err) {
      status = errorText(err);
      renderChrome();
    } finally {
      busy = false;
    }
  }

  // Initial load
  void load("");
  return { root };
}
