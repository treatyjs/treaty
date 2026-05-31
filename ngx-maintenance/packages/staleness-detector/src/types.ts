/**
 * Shared types for the staleness detector.
 *
 * Everything here is plain data. The detection logic is a pure function of npm /
 * GitHub metadata (no AI, no network); the network lives behind the injected
 * {@link MetadataSource} interface so the predicates stay unit-testable with
 * fakes.
 */

/** A supported Angular major version. The chain targets v9 through latest. */
export type AngularMajor = number;

/** A timestamp expressed either as epoch milliseconds or a `Date`. */
export type Timestamp = number | Date;

/** One day in milliseconds. */
export const DAY_MS = 1000 * 60 * 60 * 24;

/**
 * Six months expressed in milliseconds (the staleness window). Defined as 183
 * days so the boundary is deterministic and calendar-independent.
 */
export const SIX_MONTHS_MS = DAY_MS * 183;

/** A library is considered inactive after this much time with no commit. */
export const STALE_THRESHOLD_MS = SIX_MONTHS_MS;

/** Two weeks in milliseconds: the merge window before takeover is eligible. */
export const TWO_WEEKS_MS = DAY_MS * 14;

/** The takeover eligibility window (alias of the two-week window). */
export const TAKEOVER_WINDOW_MS = TWO_WEEKS_MS;

/**
 * Minimal repository metadata the detector reasons over. This is the structural
 * subset of npm + GitHub data needed to decide staleness; it is a superset-safe
 * shape, so a registry `RepoMetadata` (or anything carrying these fields) can be
 * passed directly.
 */
export interface RepoMetadata {
  /** Source repository URL. */
  readonly repoUrl: string;
  /** npm package name, e.g. `@scope/widget`. */
  readonly npmName: string;
  /** The Angular major this library currently peer-depends on. */
  readonly currentAngular: AngularMajor;
  /** Epoch milliseconds of the most recent commit. */
  readonly lastCommitMs: number;
}

/**
 * A discovered library evaluated against latest Angular + the staleness window.
 * Every component of the decision is exposed so callers can explain it.
 */
export interface DiscoveryCandidate {
  readonly metadata: RepoMetadata;
  /** True when the library targets an Angular major below latest. */
  readonly behindLatest: boolean;
  /** True when the most recent commit is older than the staleness window. */
  readonly inactive: boolean;
  /** True when both behind latest AND inactive. */
  readonly stale: boolean;
  /** True when both behind latest AND inactive: worth suggesting opt-in. */
  readonly suggestOptIn: boolean;
}

/**
 * A stale library plus the deterministic reason it was flagged. Emitted by
 * {@link discoverStale} so callers can explain the decision in an issue/PR.
 */
export interface StaleFinding {
  readonly metadata: RepoMetadata;
  /** How many Angular majors behind latest the library is (>= 1 when stale). */
  readonly majorsBehind: number;
  /** Milliseconds since the most recent commit, relative to evaluation time. */
  readonly inactiveForMs: number;
  /** A human-readable, deterministic explanation (no AI). */
  readonly reason: string;
}

/**
 * The observable signals that decide whether a migration PR's library should be
 * taken over. Every field is a deterministically-measurable value — no AI, no
 * heuristics beyond timestamp arithmetic and booleans.
 */
export interface TakeoverSignals {
  /** When the migration PR was opened. */
  readonly prOpenedAt: Timestamp;
  /** The evaluation instant; may also be passed as a second argument. */
  readonly now?: Timestamp;
  /** Whether the migration PR has been merged. */
  readonly prMerged: boolean;
  /** Whether the maintainer responded to the PR or discovery issue. */
  readonly maintainerResponded: boolean;
  /** Whether the repository has had recent activity (commit/comment/release). */
  readonly recentActivity: boolean;
}

/** The deterministic, fully-explained takeover-planning decision. */
export interface TakeoverDecision {
  /** True once the two-week window has elapsed without a merge. */
  readonly windowElapsed: boolean;
  /** True when activity + response signals indicate abandonment. */
  readonly unmaintained: boolean;
  /**
   * True only when the window elapsed AND the library is unmaintained: the
   * point at which the takeover package should begin planning a fork.
   */
  readonly planTakeover: boolean;
  /** Milliseconds the PR has been open (relative to the evaluation instant). */
  readonly prAgeMs: number;
}

/**
 * The async data port the detector reads through. The real implementation is
 * the github-adapter (octokit / npm registry); tests supply a fake. This is the
 * ONLY place network access enters; everything else is pure.
 */
export interface MetadataSource {
  /**
   * Fetch the minimal metadata for a single repository, identified by its npm
   * package name. Returns `undefined` when the package is not resolvable.
   */
  fetchMetadata(npmName: string): Promise<RepoMetadata | undefined>;

  /** The latest published Angular major (e.g. from the npm registry). */
  fetchLatestAngularMajor(): Promise<AngularMajor>;

  /**
   * Enumerate candidate repositories to evaluate for discovery (the bot's
   * periodic scan). Order is preserved through detection.
   */
  listCandidates(): Promise<readonly RepoMetadata[]>;
}
