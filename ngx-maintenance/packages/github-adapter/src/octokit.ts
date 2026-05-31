import type { Octokit } from "octokit";

// oxlint-disable-next-line typescript/no-explicit-any -- structural GitHub-API boundary must accept the real Octokit’s precise per-endpoint params
type OctokitParams = any;

/**
 * The structural subset of Octokit the bot depends on. Declaring it here (rather
 * than importing the concrete class everywhere) keeps every other package
 * fake-testable: any object matching this shape can be injected in unit tests,
 * while production passes a real {@link Octokit} instance.
 *
 * This is the SINGLE GitHub-API boundary for the whole monorepo. The surface is
 * intentionally minimal — only the pull and issue creation calls plus the
 * read-only repo queries the adapter operations below need.
 */
export interface OctokitLike {
  readonly rest: {
    readonly pulls: {
      create(params: OctokitParams): Promise<{ data: unknown }>;
      /**
       * List pull requests for a repository. The orchestrator queries this to
       * stay IDEMPOTENT: before opening a migration PR it checks whether one is
       * already open on the migration head branch. Returns the raw PR array;
       * callers read only the structural subset they need.
       */
      list(
        params: OctokitParams,
      ): Promise<{ data: ReadonlyArray<unknown> }>;
    };
    readonly issues: {
      create(params: OctokitParams): Promise<{ data: unknown }>;
    };
  };
}

/**
 * A real Octokit instance must structurally satisfy {@link OctokitLike}. This
 * type-level assertion is erased at runtime and exists only to keep the ambient
 * Octokit declaration and the adapter-facing surface in sync: if the ambient
 * shim drifts from {@link OctokitLike}, this stops typechecking.
 */
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
  // Owner/repo segments exclude `/`, `:` and whitespace, so an ssh remote
  // (`git@github.com:owner/repo`) and an https URL both reduce to the trailing
  // `owner/repo` pair rather than swallowing the host into the owner.
  const match =
    /(?:github\.com[/:])?([^/\s:]+)\/([^/\s:]+)$/.exec(trimmed) ?? undefined;
  if (match === undefined) return undefined;
  const owner = match[1];
  const repo = match[2];
  if (owner.length === 0 || repo.length === 0) return undefined;
  return { owner, repo };
}

/**
 * Whether a registry entry's repo URL refers to the given `owner/repo` full
 * name (as carried on installation webhooks). Comparison is on the parsed slug,
 * case-insensitively, so URL/`.git`/scheme differences do not matter.
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

/**
 * Whether two repo references (URLs or bare slugs) point at the same repository.
 * Convenience over {@link repoUrlMatchesFullName} when both sides are arbitrary
 * URL/slug forms rather than a known webhook full-name.
 */
export function sameRepo(a: string, b: string): boolean {
  return repoUrlMatchesFullName(a, b);
}
