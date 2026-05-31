import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";

/**
 * The filesystem boundary for allocating (and disposing) clone working
 * directories. The orchestrator clones each candidate library into a fresh
 * directory obtained from {@link allocate}, runs the migration there, then
 * releases it via {@link dispose}. Keeping this behind an interface lets tests
 * inject a deterministic, in-memory sequence with NO real filesystem I/O.
 */
export interface WorkdirProvider {
  /**
   * Allocate a fresh, empty working directory for the given library slug and
   * return its absolute path. `git clone <url> <dir>` writes INTO this path, so
   * implementations create a uniquely-named directory (or its parent) that the
   * clone can populate.
   */
  allocate(slug: string): Promise<string>;
  /**
   * Release a previously-allocated working directory. Production removes the
   * tree; the fake records the disposal so a test can assert cleanup happened.
   */
  dispose(dir: string): Promise<void>;
}

/**
 * Sanitise a library slug into a filesystem-safe directory segment: any
 * character outside `[a-z0-9-]` becomes a dash. Keeps temp-dir names readable
 * and collision-resistant without leaking scope separators into the path.
 */
function safeSlug(slug: string): string {
  const cleaned = slug
    .replace(/^@[^/]+\//, "")
    .replace(/[^a-z0-9-]/gi, "-")
    .replace(/^-+|-+$/g, "")
    .toLowerCase();
  return cleaned.length > 0 ? cleaned : "lib";
}

/**
 * Production workdir provider: allocate a uniquely-suffixed directory under the
 * OS temp dir via `fs.mkdtemp`, and dispose by recursively removing it. This is
 * the only place a real temp directory is created; the orchestrator receives
 * the interface and is therefore fully fakeable.
 */
export function createNodeWorkdirs(): WorkdirProvider {
  return {
    allocate(slug) {
      return mkdtemp(`${tmpdir()}/ngx-maintenance-${safeSlug(slug)}-`);
    },
    dispose(dir) {
      return rm(dir, { recursive: true, force: true });
    },
  };
}

/** One recorded interaction with a {@link FakeWorkdirProvider}. */
export interface WorkdirEvent {
  readonly kind: "allocate" | "dispose";
  /** The slug for an allocate, or the directory for a dispose. */
  readonly value: string;
  /** The directory the allocate produced (present on `allocate` events). */
  readonly dir?: string;
}

/** A deterministic, recording {@link WorkdirProvider} for tests. */
export interface FakeWorkdirProvider extends WorkdirProvider {
  /** Every allocate/dispose, in order. */
  readonly events: readonly WorkdirEvent[];
  /** Directories handed out, in allocation order. */
  readonly allocated: readonly string[];
  /** Directories disposed, in disposal order. */
  readonly disposed: readonly string[];
}

/**
 * Build a recording fake workdir provider. Each allocation returns a stable
 * synthetic path (`/tmp/ngx-maintenance/<n>-<slug>`) so a test can assert
 * exactly which library was cloned where and that every workdir was disposed —
 * all without touching the real filesystem.
 */
export function createFakeWorkdirs(root = "/tmp/ngx-maintenance"): FakeWorkdirProvider {
  const events: WorkdirEvent[] = [];
  const allocated: string[] = [];
  const disposed: string[] = [];
  return {
    events,
    allocated,
    disposed,
    allocate(slug) {
      const dir = `${root}/${allocated.length}-${safeSlug(slug)}`;
      allocated.push(dir);
      events.push({ kind: "allocate", value: slug, dir });
      return Promise.resolve(dir);
    },
    dispose(dir) {
      disposed.push(dir);
      events.push({ kind: "dispose", value: dir });
      return Promise.resolve();
    },
  };
}
