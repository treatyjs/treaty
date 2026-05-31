import { Octokit } from "octokit";
import type { RegistryManifest } from "@ngx-maintenance/registry";

/** Static configuration for a running bot instance. */
export interface BotConfig {
  /** The latest known Angular major (drives migration planning). */
  readonly latestAngular: number;
  /** GitHub App auth token (supplied out-of-band at deploy time). */
  readonly token: string;
  /** The base branch migration PRs target (e.g. `main`). */
  readonly baseBranch: string;
}

/** Runtime context shared across handlers. */
export interface BotContext {
  readonly octokit: Octokit;
  readonly config: BotConfig;
  /** The current registry manifest (the opted-in set). */
  readonly manifest: RegistryManifest;
}

/** Build a bot context from config and the current manifest. */
export function createBotContext(
  config: BotConfig,
  manifest: RegistryManifest,
): BotContext {
  return {
    octokit: new Octokit({ auth: config.token }),
    config,
    manifest,
  };
}
