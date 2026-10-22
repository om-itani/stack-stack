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

/**
 * Hardened defaults every *generated* service starts with. These are chosen to
 * be non-breaking for ordinary images:
 *
 * - `no-new-privileges` stops setuid escalation without blocking normal work;
 * - a bounded `json-file` log driver stops one chatty container filling the disk
 *   (Docker's default is unlimited).
 *
 * Deliberately absent: `read_only` and `mem_limit`. Rewriting those into
 * arbitrary images breaks databases and caches, so they are left to the operator
 * rather than guessed at.
 */
export const HARDENING_FRAGMENT = `    security_opt:
      - no-new-privileges:true
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"
`;

/** A minimal, valid, hardened compose document that runs one image. */
export function composeForImage(repo: string): string {
  const name = serviceName(repo);
  return `services:\n  ${name}:\n    image: ${repo}\n    restart: unless-stopped\n${HARDENING_FRAGMENT}`;
}
