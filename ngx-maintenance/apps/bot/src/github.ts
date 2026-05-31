import type { Octokit } from "octokit";

/**
 * The structural subset of Octokit the handlers depend on. Declaring it here
 * (rather than importing the concrete class everywhere) keeps the handler logic
 * testable with a fake: any object matching this shape can be injected in unit
 * tests, while production passes a real {@link Octokit} instance.
 */
export interface OctokitLike {
  readonly rest: {
    readonly pulls: {
      create(params: Record<string, unknown>): Promise<{ data: unknown }>;
    };
    readonly issues: {
      create(params: Record<string, unknown>): Promise<{ data: unknown }>;
    };
  };
}

// A real Octokit instance must structurally satisfy OctokitLike. This type-level
// assertion is erased at runtime and exists only to keep the ambient Octokit
// declaration and the handler-facing surface in sync.
export type AssertOctokitIsLike = Octokit extends OctokitLike ? true : never;

/** A parsed `owner/repo` pair. */
export interface RepoRef {
  readonly owner: string;
  readonly repo: string;
}

/**
 * Parse an `owner/repo` slug out of a repository URL or a bare `owner/repo`
 * string. Supports `https://github.com/owner/repo(.git)`, `git@github.com:...`
 * and plain `owner/repo`. Returns `undefined` when no slug can be recovered.
 */
export function parseRepoRef(repoUrlOrSlug: string): RepoRef | undefined {
  const trimmed = repoUrlOrSlug.trim().replace(/\.git$/, "");
  const match =
    /(?:github\.com[/:])?([^/\s]+)\/([^/\s]+)$/.exec(trimmed) ?? undefined;
  if (match === undefined) return undefined;
  const owner = match[1];
  const repo = match[2];
  if (owner.length === 0 || repo.length === 0) return undefined;
  return { owner, repo };
}

/**
 * Whether a registry entry's repo URL refers to the given `owner/repo`
 * full name (as carried on installation webhooks). Comparison is on the parsed
 * slug, case-insensitively, so URL/`.git`/scheme differences do not matter.
 */
export function repoUrlMatchesFullName(
  repoUrl: string,
  fullName: string,
): boolean {
  const a = parseRepoRef(repoUrl);
  const b = parseRepoRef(fullName);
  if (a === undefined || b === undefined) return false;
  return (
    a.owner.toLowerCase() === b.owner.toLowerCase() &&
    a.repo.toLowerCase() === b.repo.toLowerCase()
  );
}
