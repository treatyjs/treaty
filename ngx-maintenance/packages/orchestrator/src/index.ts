/**
 * @ngx-maintenance/orchestrator
 *
 * The deterministic end-to-end orchestration that turns the four core packages
 * (github-adapter, migration-engine, staleness-detector, takeover) into the
 * runnable bot's full flow:
 *
 *   DISCOVER candidate libs
 *     -> staleness-detector decides which are stale (behind latest + > 6mo idle)
 *     -> for each stale lib: plan the chain, CLONE + MIGRATE in a workdir
 *        through the github-adapter, then IDEMPOTENTLY open a migration PR
 *        (skip when one is already open on the head branch)
 *     -> for each migration PR unmerged >= the takeover window against an
 *        unmaintained lib: invoke the takeover module to plan + create the
 *        `@ngx-maintenance/*` fork repo.
 *
 * EVERYTHING that touches the network, git, a process or the filesystem flows
 * through the injected {@link MaintenanceAdapters} boundary, so the cycle is a
 * pure function of its inputs and fully fake-testable. The scheduler is a pure
 * `tick(now)` the host cron calls — there are NO real timers in the logic, and
 * NO AI anywhere.
 */

export type { BotConfig, BotConfigInput } from "./config.js";
export { resolveConfig } from "./config.js";

export type {
  MaintenanceAdapters,
  ProductionAdaptersConfig,
} from "./adapters.js";
export {
  createProductionAdapters,
  createTakeoverAdapter,
} from "./adapters.js";

export type {
  OutstandingPr,
  MaintenanceCycleInput,
  MaintenanceCycleResult,
  LibraryOutcome,
  TakeoverOutcome,
  SkipReason,
} from "./cycle.js";
export { runMaintenanceCycle } from "./cycle.js";

export type {
  SchedulerState,
  SchedulerConfig,
  TickDecision,
} from "./scheduler.js";
export {
  tick,
  advanceState,
  initialState,
  resolveSchedulerConfig,
} from "./scheduler.js";
