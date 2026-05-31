/**
 * @ngx-maintenance/bot
 *
 * The ngx-maintenance GitHub App. Wires the registry, migration-engine,
 * takeover policy and treaty-support packages into webhook handlers plus a
 * scheduler:
 *
 *  - installation          : a repo installs the App -> mark exactly its
 *                            registry entries app-installed (auto-roll).
 *  - @angular/core release : every new Angular major -> compute + open a
 *                            migration PR for each behind, opted-in library.
 *  - push                  : keep registry metadata fresh.
 *  - scheduled scan        : re-run discovery -> open opt-in suggestions
 *                            (issue + sample PR) for stale unregistered libs,
 *                            and evaluate the two-week takeover timer.
 *
 * Hosting and secrets are supplied out-of-band; this module is the App LOGIC.
 * NO AI is used anywhere.
 */

export type { BotContext, BotConfig } from "./context.js";
export { createBotContext } from "./context.js";

export type { OctokitLike, RepoRef } from "./github.js";
export { parseRepoRef, repoUrlMatchesFullName } from "./github.js";

export type { HandlerResult, ReleaseResult } from "./handlers.js";
export {
  onInstallation,
  onAngularRelease,
  onPush,
  releaseMajor,
} from "./handlers.js";

export type {
  MigrationPrSpec,
  OptInSuggestionSpec,
  TakeoverPlanned,
} from "./prs.js";
export {
  buildMigrationPr,
  buildOptInSuggestion,
  migrationBranch,
  openMigrationPr,
  openOptInSuggestion,
} from "./prs.js";

export type {
  ScanResult,
  ScheduleTick,
  TakeoverObservation,
} from "./scheduler.js";
export { computeScan, runScheduledScan } from "./scheduler.js";

export { createApp } from "./app.js";

// The runnable bot: the orchestration that drives the full
// discover -> migrate -> PR -> takeover flow through the github-adapter
// boundary, plus the registry-backed metadata source, the host poller and the
// CLI. The deterministic cycle itself lives in @ngx-maintenance/orchestrator.
export type {
  MetadataOctokit,
  MetadataSourceConfig,
} from "./metadata-source.js";
export { createMetadataSource } from "./metadata-source.js";

export type { RunnerConfig, Runner } from "./runner.js";
export { createRunner, createRunnerFrom, makeAdapter } from "./runner.js";

export type { PollOutcome } from "./poller.js";
export { poll } from "./poller.js";

export type { CliArgs } from "./cli.js";
export { parseArgs, summarize, main } from "./cli.js";
