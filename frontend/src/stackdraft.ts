/**
 * One-shot handoff from the App Store to the Containers stack editor.
 *
 * The App Store's "Deploy" button stages an image here and navigates to the
 * Containers page, whose `onShow` reads and clears it to open the Create Stack
 * editor pre-filled. A module-level slot is enough because there is exactly one
 * pending draft at a time and the navigation is immediate.
 */

export interface StackDraft {
  image: string;
}

let pending: StackDraft | null = null;

export function stageStackDraft(draft: StackDraft): void {
  pending = draft;
}

/** Read the pending draft and clear it, so it is applied at most once. */
export function takeStackDraft(): StackDraft | null {
  const draft = pending;
  pending = null;
  return draft;
}

/** A service name derived from a repository reference: `library/nginx` → `nginx`. */
export function serviceName(repo: string): string {
  const tail = repo.split("/").pop() ?? repo;
  const cleaned = tail.replace(/[^a-z0-9_-]/gi, "").toLowerCase();
  return cleaned || "app";
}

/** A minimal, valid compose document that runs one image. */
export function composeForImage(repo: string): string {
  const name = serviceName(repo);
  return `services:\n  ${name}:\n    image: ${repo}\n    restart: unless-stopped\n`;
}
