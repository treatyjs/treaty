/**
 * @ngx-maintenance/github-adapter
 *
 * The single GitHub, git and process boundary for the monorepo, expressed as
 * injectable interfaces so every other package stays fake-testable.
 *
 * It owns:
 *  - the Octokit subset ({@link OctokitLike}) for pull + issue creation,
 *  - the {@link Shell} type for shelling `ng` / `npm` / `git` in a repo dir,
 *  - repo-ref parsing + matching ({@link parseRepoRef},
 *    {@link repoUrlMatchesFullName}),
 *  - the PR / issue / clone operations,
 *  - the consolidated {@link GitHubAdapter} (production: real Octokit + real
 *    child_process; tests: recording fakes).
 *
 * This consolidates the adapter logic previously split across the bot's
 * `github` and `prs` modules into one disjoint build target. NO AI / network is
 * involved in the core logic: the real GitHub/git/npm calls live behind the
 * injected {@link GitHubAdapter}.
 */

export type { OctokitLike, AssertOctokitIsLike, RepoRef } from "./octokit.js";
export {
  parseRepoRef,
  repoUrlMatchesFullName,
  sameRepo,
} from "./octokit.js";

export type {
  CommandResult,
  Shell,
  ShellInvocation,
  FakeShell,
} from "./shell.js";
export { createNodeShell, createFakeShell } from "./shell.js";

export type {
  PrLib,
  DiscoveredLib,
  MigrationPrSpec,
  OptInSuggestionSpec,
  TakeoverPlanned,
  CloneResult,
  CloneOptions,
} from "./operations.js";
export {
  migrationBranch,
  buildMigrationPr,
  buildOptInSuggestion,
  openMigrationPr,
  openOptInSuggestion,
  clone,
  parentDir,
} from "./operations.js";

export type { GitHubAdapter, AdapterConfig } from "./adapter.js";
export { createGitHubAdapter, makeAdapter } from "./adapter.js";

export type { FakeOctokit, FakeAdapter } from "./fakes.js";
export { createFakeOctokit, createFakeAdapter } from "./fakes.js";
