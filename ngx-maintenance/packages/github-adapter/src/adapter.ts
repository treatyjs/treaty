import { Octokit } from "octokit";
import type { OctokitLike } from "./octokit.js";
import { createNodeShell, type Shell } from "./shell.js";
import { createNodeWorkdirs, type WorkdirProvider } from "./workdir.js";

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
  /**
   * The filesystem boundary for allocating clone working directories. The
   * orchestrator clones each library into a fresh workdir here; production uses
   * an OS temp dir, tests use a deterministic in-memory sequence.
   */
  readonly workdirs: WorkdirProvider;
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
    workdirs: createNodeWorkdirs(),
  };
}

/**
 * Assemble an adapter from already-constructed parts. Used both by the
 * production factory's callers (to swap one half) and by tests (to inject a
 * recording fake octokit + fake shell + fake workdirs), keeping the boundary
 * symmetric. The workdir provider defaults to the production OS-temp one when
 * the caller only wants to swap octokit + shell.
 */
export function makeAdapter(
  octokit: OctokitLike,
  shell: Shell,
  workdirs: WorkdirProvider = createNodeWorkdirs(),
): GitHubAdapter {
  return { octokit, shell, workdirs };
}
