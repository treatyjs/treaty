/** A supported Angular major version. The chain targets v9 through latest. */
export type AngularMajor = number;

/** One day in milliseconds. */
export const DAY_MS = 1000 * 60 * 60 * 24;

/**
 * Six months expressed in milliseconds (used for the staleness window).
 * Defined as 183 days so the boundary is deterministic and calendar-independent.
 */
export const SIX_MONTHS_MS = DAY_MS * 183;

/** A library is considered inactive after this much time with no commit. */
export const STALE_THRESHOLD_MS = SIX_MONTHS_MS;

/** The current on-disk schema version of the registry manifest. */
export const MANIFEST_VERSION = 1;

/** A single opted-in library entry in the registry manifest. */
export interface RegistryEntry {
  /** npm package name, e.g. `@scope/widget`. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
  /** The Angular major the library currently targets. */
  readonly currentAngular: AngularMajor;
  /** Whether the GitHub App is installed on the repo (enables auto-roll). */
  readonly appInstalled: boolean;
}

/** The full registry manifest (the opted-in set). */
export interface RegistryManifest {
  readonly version: number;
  readonly entries: readonly RegistryEntry[];
}

/** Minimal repository metadata used by discovery. */
export interface RepoMetadata {
  readonly repoUrl: string;
  readonly npmName: string;
  /** The Angular major this library currently peer-depends on. */
  readonly currentAngular: AngularMajor;
  /** Epoch milliseconds of the most recent commit. */
  readonly lastCommitMs: number;
}

/** A discovered library that may warrant an opt-in suggestion. */
export interface DiscoveryCandidate {
  readonly metadata: RepoMetadata;
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
