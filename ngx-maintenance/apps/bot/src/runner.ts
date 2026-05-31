import { Octokit } from "octokit";
import {
  createGitHubAdapter,
  makeAdapter,
  type GitHubAdapter,
} from "@ngx-maintenance/github-adapter";
import {
  createTakeoverAdapter,
  createTreatyMigrationStep,
  resolveConfig,
  runMaintenanceCycle,
  type BotConfig,
  type BotConfigInput,
  type MaintenanceAdapters,
  type MaintenanceCycleInput,
  type MaintenanceCycleResult,
} from "@ngx-maintenance/orchestrator";
import type { RegistryManifest } from "@ngx-maintenance/registry";
import {
  createMetadataSource,
  type MetadataOctokit,
} from "./metadata-source.js";

/** Everything the runnable bot needs to drive one (or many) cycles. */
export interface RunnerConfig {
  /** GitHub App / PAT auth token (supplied out-of-band at deploy time). */
  readonly token: string;
  /** The opted-in registry manifest the metadata source enumerates. */
  readonly manifest: RegistryManifest;
  /** The latest published Angular major (resolved from npm out-of-band). */
  readonly latestAngular: number;
  /** Orchestration config (watched orgs/repos + thresholds). */
  readonly bot?: BotConfigInput;
  /**
   * Wire the OPTIONAL Treaty migration step. When `true`, the production bundle
   * adds a Treaty boundary over the GitHub adapter's shell; the cycle still only
   * runs it for libraries in `bot.treatyOptIn`. Defaults to off.
   */
  readonly enableTreaty?: boolean;
}

/** A fully-assembled runnable bot: the resolved config + boundary adapters. */
export interface Runner {
  readonly config: BotConfig;
  readonly adapters: MaintenanceAdapters;
  /** Drive one maintenance cycle at `now` with the given outstanding PRs. */
  run(input: MaintenanceCycleInput): Promise<MaintenanceCycleResult>;
}

/**
 * Assemble the PRODUCTION runnable bot. Constructs the single GitHub/git/process
 * adapter from the token, derives the takeover boundary from it, and builds the
 * registry-backed metadata source over a real Octokit. This is the only place
 * the bot news up concrete clients; the orchestration itself receives the
 * {@link MaintenanceAdapters} interface and stays fake-testable.
 */
export function createRunner(config: RunnerConfig): Runner {
  const github = createGitHubAdapter({ token: config.token });
  const metadata = createMetadataSource(
    new Octokit({ auth: config.token }) as unknown as MetadataOctokit,
    { manifest: config.manifest, latestAngular: config.latestAngular },
  );
  const adapters: MaintenanceAdapters = {
    github,
    metadata,
    takeover: createTakeoverAdapter(config.token, github),
    // The optional Treaty step shells through the same process boundary; only
    // wired when the deployment enables it.
    ...(config.enableTreaty === true
      ? { treaty: createTreatyMigrationStep(github.shell) }
      : {}),
  };
  return assemble(config, adapters);
}

/**
 * Assemble a runner from ALREADY-CONSTRUCTED adapters. Used by the production
 * factory and by tests/hosts that inject fakes (a fake GitHub adapter + fake
 * metadata source + fake takeover adapter) so the end-to-end run is
 * deterministic with no network/git/process.
 */
export function createRunnerFrom(
  config: Pick<RunnerConfig, "bot">,
  adapters: MaintenanceAdapters,
): Runner {
  return assemble(config, adapters);
}

function assemble(
  config: Pick<RunnerConfig, "bot">,
  adapters: MaintenanceAdapters,
): Runner {
  const resolved = resolveConfig(config.bot);
  return {
    config: resolved,
    adapters,
    run: (input) => runMaintenanceCycle(resolved, adapters, input),
  };
}

/**
 * Re-export the adapter assembler so a host that constructs its own octokit +
 * shell halves can build a {@link GitHubAdapter} without reaching into the
 * github-adapter package directly.
 */
export { makeAdapter, type GitHubAdapter };
