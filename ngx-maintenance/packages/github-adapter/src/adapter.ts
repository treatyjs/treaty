import { Octokit } from "octokit";
import type { OctokitLike } from "./octokit.js";
import { createNodeShell, type Shell } from "./shell.js";

/**
 * The consolidated GitHub + git + process boundary. Every other package depends
 * on this single injectable surface rather than on Octokit / child_process
 * directly, so all of their logic stays unit-testable with a fake adapter.
 */
export interface GitHubAdapter {
  /** The GitHub-API client (structural subset). */
  readonly octokit: OctokitLike;
  /** The process boundary for shelling `ng` / `npm` / `git` in a repo dir. */
  readonly shell: Shell;
}

/** Configuration for the production adapter. */
export interface AdapterConfig {
  /** GitHub App / PAT auth token (supplied out-of-band at deploy time). */
  readonly token: string;
}

/**
 * Build the PRODUCTION adapter: a real {@link Octokit} authenticated with the
 * supplied token, plus a real spawning {@link Shell}. This is the only place
 * the concrete Octokit and `child_process` are constructed; everything else
 * receives the {@link GitHubAdapter} interface and is therefore fakeable.
 */
export function createGitHubAdapter(config: AdapterConfig): GitHubAdapter {
  return {
    octokit: new Octokit({ auth: config.token }),
    shell: createNodeShell(),
  };
}

/**
 * Assemble an adapter from already-constructed parts. Used both by the
 * production factory's callers (to swap one half) and by tests (to inject a
 * recording fake octokit + fake shell), keeping the boundary symmetric.
 */
export function makeAdapter(octokit: OctokitLike, shell: Shell): GitHubAdapter {
  return { octokit, shell };
}
