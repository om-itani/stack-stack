/**
 * Files page.
 *
 * A guarded browser over the configured roots: navigate, edit text files,
 * upload by drop, download, archive, and change permissions/ownership. Every
 * action is a single API call; the server enforces that the path is inside a
 * root, so this page never has to reason about traversal.
 */

import { filesApi } from "../api";
import type { FileEntry, FilesState } from "../api";
import { button, bytes, el, emptyNote, field, textInput } from "../ui";

export interface PageHandle {
  root: HTMLElement;
  onShow?: () => void;
}

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function formatTime(ms: number | null): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString();
}

export function createFilesPage(): PageHandle {
  const root = el("div", { class: "flex min-h-0 flex-1 flex-col gap-2 p-3" });

  let state: FilesState | null = null;
  let current = "";
  let busy = false;
  let status = "";
  let loaded = false;
  // The path whose listing failed, so the error renders inline next to the
  // breadcrumbs instead of leaving a stale table on screen.
  let loadError: string | null = null;
  /** List or grid, remembered for the session. */
  let view: "list" | "grid" = "list";
  /** Entry shown in the preview pane, if any. */
  let preview: FileEntry | null = null;

  const statusLine = el("span", { class: "tabular text-[11px] text-neutral-500" });
  const hiddenToggle = el("input", {
    type: "checkbox",
    class: "accent-neutral-500",
  }) as HTMLInputElement;
  const uploadInput = el("input", { type: "file", multiple: true, class: "hidden" }) as HTMLInputElement;

  const crumbs = el("div", { class: "flex flex-wrap items-center gap-1 text-[11px] text-neutral-400" });

  // A real progress bar for uploads: the browser knows the byte total, so there
  // is no reason to show an indefinite spinner.
  const progressTrack = el("div", {
    class: "hidden h-1 w-full overflow-hidden rounded-full bg-white/10",
  });
  const progressBar = el("div", {
    class: "h-full w-0 rounded-full bg-gradient-to-r from-emerald-500 to-teal-400",
  });
  progressTrack.append(progressBar);
  function setProgress(pct: number | null): void {
    if (pct === null) {
      progressTrack.className = "hidden h-1 w-full overflow-hidden rounded-full bg-white/10";
      progressBar.style.width = "0%";
      return;
    }
    progressTrack.className = "h-1 w-full overflow-hidden rounded-full bg-white/10";
    progressBar.style.width = `${Math.min(100, Math.max(0, pct))}%`;
  }
  const sidebar = el("div", { class: "flex w-48 shrink-0 flex-col gap-0.5 overflow-y-auto border-r border-white/5 pr-2" });
  const tableHost = el("div", { class: "min-h-0 flex-1 overflow-auto" });
  // Preview pane: hidden until a text or image file is selected.
  const previewHost = el("div", {
    class: "hidden w-80 shrink-0 overflow-auto border-l border-white/5 pl-2",
  });

  const upButton = button("up", "", () => {
    if (state?.listing.parent) void load(state.listing.parent);
  });
  const refreshButton = button("refresh", "", () => void load(current));
  const newFolder = button("new folder", "", () =>
    void promptName("New folder", (name) => filesApi.mkdir(join(current, name))),
  );
  const newFile = button("new file", "", () =>
    void promptName("New file", (name) => filesApi.createFile(join(current, name))),
  );
  const archiveButton = button("archive", "", () => {
    window.location.href = filesApi.archiveUrl(current);
  });
  const uploadButton = button("upload", "text-emerald-400 hover:border-emerald-500", () => uploadInput.click());
  const viewToggle = button("grid view", "", () => {
    view = view === "list" ? "grid" : "list";
    viewToggle.textContent = view === "list" ? "grid view" : "list view";
    render();
  });

  uploadInput.addEventListener("change", () => {
    if (uploadInput.files?.length) void uploadFiles(uploadInput.files);
    uploadInput.value = "";
  });

  hiddenToggle.addEventListener("change", () => {
    if (!state) return;
    state.config.show_hidden = hiddenToggle.checked;
    void filesApi.saveConfig(state.config).then(() => load(current));
  });

  const toolbar = el("div", { class: "flex flex-wrap items-center gap-2" }, [
    el("span", { class: "text-neutral-300", text: "files" }),
    upButton,
    refreshButton,
    newFolder,
    newFile,
    uploadButton,
    archiveButton,
    viewToggle,
    el("label", { class: "flex items-center gap-1 text-[11px] text-neutral-400" }, [hiddenToggle, "hidden"]),
    el("span", { class: "ml-auto" }, [statusLine]),
    uploadInput,
  ]);

  root.append(
    toolbar,
    crumbs,
    progressTrack,
    el("div", { class: "flex min-h-0 flex-1 gap-2" }, [sidebar, tableHost, previewHost]),
  );

  // Drag-and-drop upload onto the table area.
  tableHost.addEventListener("dragover", (event) => {
    event.preventDefault();
    tableHost.classList.add("outline", "outline-1", "outline-emerald-600");
  });
  tableHost.addEventListener("dragleave", () => tableHost.classList.remove("outline", "outline-1", "outline-emerald-600"));
  tableHost.addEventListener("drop", (event) => {
    event.preventDefault();
    tableHost.classList.remove("outline", "outline-1", "outline-emerald-600");
    if (event.dataTransfer?.files.length) void uploadFiles(event.dataTransfer.files);
  });

  function join(dir: string, name: string): string {
    return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
  }

  async function load(path: string): Promise<void> {
    if (busy) return;
    busy = true;
    status = "loading…";
    loadError = null;
    renderChrome();
    try {
      state = await filesApi.state(path);
      current = state.listing.path;
      hiddenToggle.checked = state.config.show_hidden;
      status = state.listing.truncated ? "listing truncated" : "";
    } catch (err) {
      status = errorText(err);
      // Keep the attempted path so the banner says what failed, and drop the
      // stale listing so the table cannot show a directory we are not in.
      current = path;
      loadError = errorText(err);
      state = null;
    } finally {
      busy = false;
      loaded = true;
      render();
    }
  }

  function renderChrome(): void {
    statusLine.textContent = status;
    upButton.disabled = busy || !state?.listing.parent;
  }

  /** Set the status line from a menu action, which has no other way to report. */
  function setStatus(text: string, _tone: "muted" | "error" | "ok" = "muted"): void {
    status = text;
    renderChrome();
  }

  function render(): void {
    renderChrome();
    tableHost.replaceChildren();
    sidebar.replaceChildren();
    crumbs.replaceChildren();

    if (!loaded) {
      tableHost.append(emptyNote("loading…"));
      return;
    }
    if (!state) {
      // The listing failed: name the path and the reason rather than showing a
      // bare empty directory.
      const detail = loadError ?? status ?? "the file API is unavailable";
      tableHost.append(
        el("div", {
          class: "border border-rose-800 bg-rose-950/40 px-3 py-2 text-[11px] text-rose-300",
          text: `could not list ${current}: ${detail}`,
        }),
      );
      return;
    }

    for (const node of state.roots) {
      sidebar.append(
        button(node, "text-left text-[11px] truncate", () => void load(node)),
      );
    }

    renderCrumbs(state.listing.path);
    renderEntries();
    renderPreview();
  }

  function renderCrumbs(path: string): void {
    const parts = path.split("/").filter(Boolean);
    let accumulated = "";
    const rootButton = button("/", "text-[11px]", () => void load("/"));
    crumbs.append(rootButton);
    for (const part of parts) {
      accumulated += `/${part}`;
      const target = accumulated;
      crumbs.append(el("span", { class: "text-neutral-600", text: "/" }));
      crumbs.append(button(part, "text-[11px]", () => void load(target)));
    }
  }

  function renderEntries(): void {
    const listing = state!.listing;
    if (listing.entries.length === 0) {
      tableHost.append(emptyNote("this directory is empty"));
      return;
    }
    if (view === "grid") {
      renderGrid();
    } else {
      renderTable();
    }
  }

  /** Grid view: one tile per entry with an icon, name and size. */
  function renderGrid(): void {
    const grid = el("div", {
      class: "grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4 2xl:grid-cols-6",
    });
    for (const entry of state!.listing.entries) {
      const tile = el("div", {
        class:
          "panel flex cursor-default flex-col items-center gap-1 px-2 py-3 text-center transition-colors hover:border-white/20",
      });
      tile.append(
        el("span", { class: "text-2xl leading-none", text: iconFor(entry) }),
        el("span", {
          class: `w-full truncate text-[11px] ${entry.kind === "dir" ? "text-emerald-400" : "text-neutral-200"}`,
          text: entry.name,
        }),
        el("span", {
          class: "tabular text-[10px] text-neutral-600",
          text: entry.kind === "dir" ? "folder" : bytes(entry.size),
        }),
      );
      tile.addEventListener("dblclick", () => {
        if (entry.kind === "dir") void load(entry.path);
        else void openPreview(entry);
      });
      tile.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        openContextMenu(entry, event.clientX, event.clientY);
      });
      grid.append(tile);
    }
    tableHost.append(grid);
  }

  /** A text glyph per kind; no icon font and no image requests. */
  function iconFor(entry: FileEntry): string {
    if (entry.kind === "dir") return "📁";
    const name = entry.name.toLowerCase();
    if (/\.(png|jpe?g|gif|webp|bmp|svg)$/.test(name)) return "🖼";
    if (/\.(zip|tar\.gz|tgz|tar\.zst|tar\.xz|tar\.bz2|tar|zst)$/.test(name)) return "🗜";
    if (/\.(md|txt|log|json|ya?ml|toml|ini|conf|sh|rs|ts|js|css|html)$/.test(name)) return "📄";
    return "📄";
  }

  /** Right-click menu. Positioned at the pointer and dismissed on any click. */
  function openContextMenu(entry: FileEntry, x: number, y: number): void {
    document.querySelectorAll(".ss-context-menu").forEach((node) => node.remove());

    const menu = el("div", {
      class:
        "ss-context-menu panel fixed z-50 flex min-w-40 flex-col py-1 text-[11px] shadow-lg",
    });
    menu.style.left = `${Math.min(x, window.innerWidth - 180)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - 220)}px`;

    const add = (label: string, action: () => void, tone = ""): void => {
      const item = button(label, `w-full justify-start text-left text-[11px] ${tone}`.trim(), () => {
        close();
        action();
      });
      item.style.textAlign = "left";
      menu.append(item);
    };

    const close = (): void => {
      menu.remove();
      document.removeEventListener("click", close);
      document.removeEventListener("keydown", onKey);
    };
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") close();
    };

    if (entry.kind === "dir") {
      add("Open", () => void load(entry.path));
    } else {
      add("Download", () => {
        window.location.href = filesApi.downloadUrl(entry.path);
      });
      add("Preview", () => void openPreview(entry));
      if (isText(entry.name) || isImage(entry.name)) {
        add("Edit", () => void openEditor(entry));
      }
    }
    if (entry.kind === "file" && isArchive(entry.name)) {
      add("Extract", () => void withBusy(() => filesApi.extract(entry.path)).then(() => load(current)));
    }
    add("Rename", () =>
      void promptName("Rename", (name) =>
        filesApi.rename(entry.path, join(parentOf(entry.path), name)),
      ),
    );
    add("Copy path", () => {
      void navigator.clipboard?.writeText(entry.path).then(
        () => setStatus(`copied ${entry.path}`, "ok"),
        () => setStatus("clipboard is unavailable in this browser", "error"),
      );
    });
    if (entry.kind === "file") {
      add("Permissions", () =>
        void promptName("Mode (e.g. 644)", (mode) => filesApi.chmod(entry.path, mode)),
      );
      add("Owner", () => void promptOwner(entry));
    }
    add("Delete", () => void removeEntry(entry), "text-rose-400 hover:border-rose-500");

    document.body.append(menu);
    // The click that opened the menu must not immediately dismiss it.
    window.setTimeout(() => {
      document.addEventListener("click", close);
      document.addEventListener("keydown", onKey);
    }, 0);
  }

  function isImage(name: string): boolean {
    return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(name);
  }

  function isText(name: string): boolean {
    return /\.(md|txt|log|json|ya?ml|toml|ini|conf|sh|rs|ts|js|tsx|jsx|css|html|xml|csv)$/i.test(name);
  }

  function isArchive(name: string): boolean {
    return /\.(zip|tar\.gz|tgz|tar\.zst|tzst|tar\.xz|txz|tar\.bz2|tbz2|tar)$/i.test(name);
  }

  /** Load a file into the preview pane: an image inline, text as monospace. */
  async function openPreview(entry: FileEntry): Promise<void> {
    if (entry.kind === "dir") {
      void load(entry.path);
      return;
    }
    preview = entry;
    if (isImage(entry.name)) {
      renderPreview();
      return;
    }
    if (!isText(entry.name)) {
      setStatus(`${entry.name}: no inline preview for this type`, "muted");
      preview = null;
      renderPreview();
      return;
    }
    previewHost.className = "w-80 shrink-0 overflow-auto border-l border-white/5 pl-2";
    previewHost.replaceChildren(el("div", { class: "text-[11px] text-neutral-500", text: "loading…" }));
    try {
      const file = await filesApi.read(entry.path);
      // The listing may have moved on while the read was in flight.
      if (preview?.path !== entry.path) return;
      previewHost.replaceChildren(
        el("div", { class: "mb-1 flex items-baseline gap-2" }, [
          el("span", { class: "min-w-0 flex-1 truncate text-[11px] text-neutral-200", text: entry.name }),
          button("close", "text-[10px]", clearPreview),
        ]),
        el("pre", {
          class: "tabular whitespace-pre-wrap break-words text-[10px] text-neutral-300",
          text: file.content,
        }),
      );
    } catch (err) {
      previewHost.replaceChildren(emptyNote(errorText(err)));
    }
  }

  function clearPreview(): void {
    preview = null;
    renderPreview();
  }

  function renderPreview(): void {
    if (!preview) {
      previewHost.className = "hidden";
      previewHost.replaceChildren();
      return;
    }
    previewHost.className = "w-80 shrink-0 overflow-auto border-l border-white/5 pl-2";
    const entry = preview;
    if (isImage(entry.name)) {
      previewHost.replaceChildren(
        el("div", { class: "mb-1 flex items-baseline gap-2" }, [
          el("span", { class: "min-w-0 flex-1 truncate text-[11px] text-neutral-200", text: entry.name }),
          button("close", "text-[10px]", clearPreview),
        ]),
        el("img", {
          src: filesApi.downloadUrl(entry.path),
          alt: entry.name,
          class: "max-h-96 w-full border border-white/5 object-contain",
        }),
      );
    }
  }

  function renderTable(): void {
    const listing = state!.listing;
    if (listing.entries.length === 0) {
      tableHost.append(emptyNote("this directory is empty"));
      return;
    }

    const table = el("table", { class: "w-full text-[11px]" });
    const head = el("tr", {}, [
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "name" }),
      el("th", { class: "px-2 py-1 text-right text-[10px] uppercase text-neutral-600", text: "size" }),
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "modified" }),
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "mode" }),
      el("th", { class: "px-2 py-1 text-left text-[10px] uppercase text-neutral-600", text: "owner" }),
      el("th", { class: "px-2 py-1" }),
    ]);
    table.append(el("thead", {}, [head]));

    const tbody = el("tbody");
    for (const entry of listing.entries) {
      tbody.append(row(entry));
    }
    table.append(tbody);
    tableHost.append(table);
  }

  function row(entry: FileEntry): HTMLElement {
    const nameCell = el("td", { class: "px-2 py-1" });
    const nameLabel = el("span", {
      class: entry.kind === "dir" ? "text-emerald-400" : "text-neutral-300",
      text: entry.kind === "dir" ? `${entry.name}/` : entry.name,
    });
    nameCell.append(nameLabel);
    if (entry.symlink_target) {
      nameCell.append(el("span", { class: "ml-2 text-neutral-600", text: `→ ${entry.symlink_target}` }));
    }
    nameCell.addEventListener("dblclick", () => {
      if (entry.kind === "dir") void load(entry.path);
      else void openEditor(entry);
    });

    const owner = `${entry.owner ?? "—"}:${entry.group ?? "—"}`;
    const actions = el("td", { class: "px-2 py-1 text-right whitespace-nowrap" });
    if (entry.kind !== "dir") {
      actions.append(
        el("a", { href: filesApi.downloadUrl(entry.path), class: "mr-2 text-emerald-400 underline", text: "get" }),
      );
    }
    if (entry.kind === "file") {
      actions.append(button("edit", "mr-2 text-[10px]", () => void openEditor(entry)));
    }
    if (entry.kind === "file" && /\.(zip|tar\.gz|tgz|tar\.zst|tzst|tar\.xz|txz|tar\.bz2|tar)$/i.test(entry.name)) {
      actions.append(button("extract", "mr-2 text-[10px]", () => void withBusy(() => filesApi.extract(entry.path)).then(() => load(current))));
    }
    actions.append(
      button("rename", "mr-2 text-[10px]", () =>
        void promptName("Rename", (name) =>
          filesApi.rename(entry.path, join(parentOf(entry.path), name)),
        ),
      ),
      button("perms", "mr-2 text-[10px]", () =>
        void promptName("Mode (e.g. 644)", (mode) => filesApi.chmod(entry.path, mode)),
      ),
      button("owner", "mr-2 text-[10px]", () => void promptOwner(entry)),
      button("delete", "mr-2 text-[10px] text-rose-400 hover:border-rose-500", () => void removeEntry(entry)),
    );
    // A visible affordance for the context menu, since right-click is invisible.
    const moreButton = button("···", "text-[10px]");
    moreButton.title = "more actions";
    moreButton.addEventListener("click", () => {
      const box = moreButton.getBoundingClientRect();
      openContextMenu(entry, box.left, box.bottom);
    });
    actions.append(moreButton);

    const tr = el("tr", { class: "border-b border-white/5" }, [
      nameCell,
      el("td", { class: "tabular px-2 py-1 text-right text-neutral-500", text: entry.kind === "dir" ? "—" : bytes(entry.size) }),
      el("td", { class: "px-2 py-1 text-neutral-500", text: formatTime(entry.modified_ms) }),
      el("td", { class: "tabular px-2 py-1 text-neutral-500", text: entry.mode ?? "—" }),
      el("td", { class: "px-2 py-1 text-neutral-500", text: owner }),
      actions,
    ]);
    // Right-click anywhere on the row opens the same menu as the ··· button.
    tr.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      openContextMenu(entry, event.clientX, event.clientY);
    });
    return tr;
  }

  function parentOf(path: string): string {
    const index = path.lastIndexOf("/");
    return index <= 0 ? "/" : path.slice(0, index);
  }

  async function withBusy<T>(work: () => Promise<T>): Promise<T | null> {
    busy = true;
    status = "working…";
    renderChrome();
    try {
      return await work();
    } catch (err) {
      status = errorText(err);
      return null;
    } finally {
      busy = false;
      renderChrome();
    }
  }

  async function removeEntry(entry: FileEntry): Promise<void> {
    const recursive = entry.kind === "dir";
    const warning = recursive
      ? `Delete ${entry.name} and everything inside it?`
      : `Delete ${entry.name}?`;
    if (!window.confirm(warning)) return;
    await withBusy(() => filesApi.remove(entry.path, recursive));
    await load(current);
  }

  /**
   * POST one file and report byte progress. `fetch` cannot observe upload
   * progress, so this uses `XMLHttpRequest`, which is the only API that exposes
   * `upload.onprogress` in every current browser.
   */
  function uploadOne(url: string, file: File, onProgress: (pct: number) => void): Promise<void> {
    return new Promise((resolve, reject) => {
      const request = new XMLHttpRequest();
      request.open("POST", url);
      request.withCredentials = true;
      request.upload.addEventListener("progress", (event) => {
        if (event.lengthComputable && event.total > 0) {
          onProgress((event.loaded / event.total) * 100);
        }
      });
      request.addEventListener("load", () => {
        if (request.status === 401) {
          window.dispatchEvent(new CustomEvent("stack-stack:unauthorized"));
          reject(new Error("session expired"));
          return;
        }
        if (request.status >= 200 && request.status < 300) {
          resolve();
          return;
        }
        // Surface the server's own message: "permission denied" and "no space
        // left on device" are the two the operator actually needs to read.
        let message = `upload failed (${request.status})`;
        try {
          const body = JSON.parse(request.responseText) as { error?: string };
          if (body.error) {
            message = body.error;
          }
        } catch {
          // Not JSON; keep the status-code message.
        }
        reject(new Error(message));
      });
      request.addEventListener("error", () => reject(new Error("network error during upload")));
      request.addEventListener("abort", () => reject(new Error("upload cancelled")));
      request.addEventListener("timeout", () => reject(new Error("upload timed out")));

      const form = new FormData();
      form.append("file", file, file.name);
      request.send(form);
    });
  }

  async function uploadFiles(files: FileList): Promise<void> {
    busy = true;
    const list = Array.from(files);
    try {
      for (let index = 0; index < list.length; index++) {
        const file = list[index];
        status = `uploading ${index + 1}/${list.length}: ${file.name}`;
        renderChrome();
        setProgress(0);
        // Only the bar updates per event; re-rendering the chrome on every
        // progress tick would fight the browser for no visible gain.
        await uploadOne(filesApi.uploadUrl(current), file, setProgress);
      }
      status = `uploaded ${list.length} file(s)`;
    } catch (err) {
      status = errorText(err);
    } finally {
      setProgress(null);
      busy = false;
      await load(current);
    }
  }

  // --- modals ------------------------------------------------------------

  function modal(title: string, body: HTMLElement): () => void {
    const overlay = el("div", {
      class: "fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4",
    });
    const panel = el("div", {
      class: "flex w-full max-w-lg flex-col gap-2 border border-white/5 bg-[#0e0e0e] p-3",
    });
    const close = (): void => overlay.remove();
    const bar = el("div", { class: "flex items-center gap-2" }, [
      el("span", { class: "text-neutral-300", text: title }),
      el("span", { class: "ml-auto" }),
      button("close", "text-[10px]", close),
    ]);
    panel.append(bar, body);
    overlay.append(panel);
    document.body.append(overlay);
    overlay.addEventListener("mousedown", (event) => {
      if (event.target === overlay) {
        close();
      }
    });
    return close;
  }

  async function promptName(
    title: string,
    onSubmit: (value: string) => Promise<unknown>,
  ): Promise<void> {
    let value = "";
    const error = el("div", { class: "min-h-4 text-[11px] text-rose-400" });
    const input = textInput("", (next) => (value = next), { class: "w-full" });
    const close = modal(
      title,
      el("div", { class: "flex flex-col gap-2" }, [
        field("value", input),
        error,
        button("confirm", "text-emerald-400", async () => {
          try {
            await onSubmit(value.trim());
            close();
            await load(current);
          } catch (err) {
            error.textContent = errorText(err);
          }
        }),
      ]),
    );
    input.focus();
  }

  async function promptOwner(entry: FileEntry): Promise<void> {
    const accounts = await filesApi.accounts().catch(() => ({ users: [], groups: [] }));
    let user = entry.owner ?? "";
    let group = entry.group ?? "";
    const datalist = (id: string, values: string[]): HTMLElement => {
      const list = document.createElement("datalist");
      list.id = id;
      for (const value of values) {
        const option = document.createElement("option");
        option.value = value;
        list.append(option);
      }
      return list;
    };
    const userInput = textInput(user, (value) => (user = value), { class: "w-full" });
    userInput.setAttribute("list", "file-users");
    const groupInput = textInput(group, (value) => (group = value), { class: "w-full" });
    groupInput.setAttribute("list", "file-groups");
    const error = el("div", { class: "min-h-4 text-[11px] text-rose-400" });
    const close = modal(
      "Owner",
      el("div", { class: "flex flex-col gap-2" }, [
        field("user", userInput),
        field("group", groupInput),
        datalist("file-users", accounts.users),
        datalist("file-groups", accounts.groups),
        error,
        button("apply", "text-emerald-400", async () => {
          try {
            await filesApi.chown(entry.path, user || null, group || null);
            close();
            await load(current);
          } catch (err) {
            error.textContent = errorText(err);
          }
        }),
      ]),
    );
  }

  async function openEditor(entry: FileEntry): Promise<void> {
    let file;
    try {
      file = await filesApi.read(entry.path);
    } catch (err) {
      status = errorText(err);
      renderChrome();
      return;
    }
    const area = document.createElement("textarea");
    area.className =
      "h-80 w-full border border-white/5 bg-black/40 p-2 font-mono text-[11px] text-neutral-200 outline-none";
    area.value = file.content;
    const note = el("div", {
      class: "text-[10px] text-neutral-600",
      text: `${file.mime} · ${bytes(file.size)}${file.truncated ? " · truncated for editing" : ""}`,
    });
    const error = el("div", { class: "min-h-4 text-[11px] text-rose-400" });
    const close = modal(
      entry.name,
      el("div", { class: "flex flex-col gap-2" }, [
        area,
        note,
        error,
        button("save", "text-emerald-400", async () => {
          try {
            await filesApi.save(entry.path, area.value);
            close();
            status = "saved";
            await load(current);
          } catch (err) {
            error.textContent = errorText(err);
          }
        }),
      ]),
    );
  }

  void load("");
  return { root };
}
