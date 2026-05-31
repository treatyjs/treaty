/**
 * @ngx-maintenance/takeover
 *
 * The takeover POLICY engine — pure logic, NO AI.
 *
 * {@link shouldTakeOver} fires `true` exactly when a migration PR has been
 * unmerged for two-plus weeks AND the library is clearly unmaintained (the
 * maintainer never responded and there is no recent activity). {@link
 * planTakeover} turns a library into a deterministic spec: the
 * `@ngx-maintenance/<name>` fork, the compatibility-only NPM warning banner,
 * and a NEW standalone repository (each takeover becomes its own repo; the
 * tooling stays here).
 *
 * Everything is a deterministic function of timestamps and boolean activity
 * signals.
 */

export type {
  Timestamp,
  TakeoverSignals,
  TakeoverDecision,
  TakeoverLib,
  TakeoverSpec,
  NewRepoSpec,
} from "./types.js";
export {
  TAKEOVER_WINDOW_MS,
  TWO_WEEKS_MS,
  SCOPE,
  WARNING_BANNER,
} from "./types.js";

export {
  shouldTakeOver,
  decideTakeover,
  isUnmaintained,
  isWindowElapsed,
} from "./policy.js";
export {
  planTakeover,
  buildTakeoverSpec,
  forkName,
  scopedName,
  bareName,
} from "./plan.js";
