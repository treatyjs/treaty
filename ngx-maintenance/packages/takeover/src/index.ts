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
 * signals. The core emits SPECS only; the real fork, npm publish and
 * standalone-repo creation run out-of-band behind the injected
 * {@link GithubAdapter} ({@link runTakeover} gates them on the policy
 * decision), so the orchestration is unit-tested with a fake adapter.
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

export type {
  GithubAdapter,
  ForkInput,
  ForkResult,
  CreateRepoResult,
  PublishInput,
  PublishResult,
  TakeoverExecution,
  TakeoverRun,
} from "./adapter.js";
export { executeTakeover, runTakeover } from "./adapter.js";
