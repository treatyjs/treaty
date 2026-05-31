/**
 * @ngx-maintenance/staleness-detector
 *
 * Pure staleness and discovery predicates for the ngx-maintenance bot. Houses
 * the detection logic that previously lived inside registry discovery so it is
 * reusable independent of the manifest-storage concern.
 *
 * Three concerns, all deterministic and NO-AI:
 *  - Staleness/discovery: a library is stale when it is BOTH behind the latest
 *    Angular major AND has had no commit in more than six months.
 *  - Takeover planning: a migration PR's library enters takeover planning when
 *    it is unmerged for two-plus weeks AND clearly unmaintained.
 *  - Migration sequencing: the ordered vN steps (incl. the v9..v12 VE->Ivy
 *    transition) the migration engine should run for a given start version.
 *
 * The core is a pure function of npm / GitHub metadata. Network access enters
 * ONLY through the injected {@link MetadataSource} (the github-adapter in
 * production, a fake in tests), keeping the logic unit-testable.
 */

export type {
  AngularMajor,
  Timestamp,
  RepoMetadata,
  DiscoveryCandidate,
  StaleFinding,
  TakeoverSignals,
  TakeoverDecision,
  MetadataSource,
} from "./types.js";
export {
  DAY_MS,
  SIX_MONTHS_MS,
  STALE_THRESHOLD_MS,
  TWO_WEEKS_MS,
  TAKEOVER_WINDOW_MS,
} from "./types.js";

export {
  isInactive,
  isBehindLatest,
  isStale,
  evaluateCandidate,
  describeStale,
  discoverStale,
} from "./staleness.js";

export {
  isWindowElapsed,
  isUnmaintained,
  decideTakeoverPlanning,
  shouldPlanTakeover,
} from "./takeover-planning.js";

export type { MigrationStep, MigrationStepKind } from "./migration-engine.js";
export {
  LATEST_ANGULAR,
  IVY_TRANSITION_MAJOR,
  VE_WINDOW_END,
  inVeToIvyWindow,
  sequenceMigrationSteps,
  migrationVersionPath,
} from "./migration-engine.js";

export { StalenessDetector } from "./detector.js";
