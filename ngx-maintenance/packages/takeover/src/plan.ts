import {
  SCOPE,
  WARNING_BANNER,
  type NewRepoSpec,
  type TakeoverLib,
  type TakeoverSpec,
} from "./types.js";

/**
 * Reduce a source npm package name to a single bare, npm-safe segment: the
 * leading scope (if any) is dropped and any non-`[a-z0-9-]` character becomes a
 * dash. E.g. `@acme/My_Widget` -> `my-widget`.
 */
export function bareName(sourceNpmName: string): string {
  return sourceNpmName
    .replace(/^@[^/]+\//, "")
    .replace(/[^a-z0-9-]/gi, "-")
    .toLowerCase();
}

/**
 * Compute the scoped fork name for a source npm package, i.e.
 * `@ngx-maintenance/<name>`.
 */
export function forkName(sourceNpmName: string): string {
  return `${SCOPE}/${bareName(sourceNpmName)}`;
}

/**
 * Plan a takeover: emit the deterministic {@link TakeoverSpec} describing the
 * `@ngx-maintenance/<name>` fork, its compatibility-only warning banner, and
 * the NEW standalone repository the takeover becomes. This is pure data; the
 * actual fork/publish/repo-creation runs out-of-band against the spec.
 */
export function planTakeover(lib: TakeoverLib): TakeoverSpec {
  const bare = bareName(lib.npmName);
  const forkNpmName = `${SCOPE}/${bare}`;
  const newRepo: NewRepoSpec = {
    name: `ngx-maintenance-${bare}`,
    defaultBranch: "main",
    isPublic: true,
    description: WARNING_BANNER,
  };
  return {
    sourceNpmName: lib.npmName,
    sourceRepoUrl: lib.repoUrl,
    forkNpmName,
    scopedNpmName: forkNpmName,
    warningBanner: WARNING_BANNER,
    newRepo,
    newRepoName: newRepo.name,
  };
}

/**
 * Build a takeover spec from a source npm name + repo URL. A thin positional
 * wrapper over {@link planTakeover} for callers (the bot scheduler) that hold
 * the two values separately rather than a {@link TakeoverLib}.
 */
export function buildTakeoverSpec(
  sourceNpmName: string,
  sourceRepoUrl: string,
): TakeoverSpec {
  return planTakeover({ npmName: sourceNpmName, repoUrl: sourceRepoUrl });
}

/**
 * Deprecated alias of {@link forkName}, retained for callers that referred to
 * the scoped fork name as "scopedName".
 */
export function scopedName(sourceNpmName: string): string {
  return forkName(sourceNpmName);
}
