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
