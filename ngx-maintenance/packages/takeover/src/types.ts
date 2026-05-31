/** Two weeks in milliseconds: the merge window before takeover is eligible. */
export const TWO_WEEKS_MS = 1000 * 60 * 60 * 24 * 14;

/** The takeover eligibility window (alias of the two-week window). */
export const TAKEOVER_WINDOW_MS = TWO_WEEKS_MS;

/** The npm scope that all taken-over forks are published under. */
export const SCOPE = "@ngx-maintenance";

/**
 * The mandatory NPM warning banner attached to every taken-over fork. It is
 * surfaced in the package README and the npm `description`, and makes the
 * compatibility-only nature of the fork explicit.
 */
export const WARNING_BANNER =
  "This is a compatibility-only ngx-maintenance fork. It tracks Angular " +
  "compatibility ONLY — not bug fixes or new features. Please migrate to a " +
  "supported alternative.";

/** A timestamp expressed either as epoch milliseconds or a `Date`. */
export type Timestamp = number | Date;

/**
 * The observable signals that decide whether a migration PR's library should be
 * taken over. Every field is a plain, deterministically-measurable value — no
 * AI, no heuristics beyond timestamp arithmetic and booleans.
 *
 * `now` may be supplied here or passed as the second argument to
 * {@link decideTakeover} / {@link shouldTakeOver}. The legacy `prOpenedMs` /
 * `lastActivityMs` fields are accepted as alternatives to `prOpenedAt` /
 * `recentActivity` so existing callers (the bot scheduler) keep working.
 */
export interface TakeoverSignals {
  /** When the migration PR was opened (epoch ms or `Date`). */
  readonly prOpenedAt?: Timestamp;
  /** The evaluation instant (epoch ms or `Date`); may be passed separately. */
  readonly now?: Timestamp;
  /** Whether the migration PR has been merged. */
  readonly prMerged: boolean;
  /** Whether the maintainer responded to the PR or discovery issue. */
  readonly maintainerResponded: boolean;
  /** Whether the repository has had recent activity (commit/comment/release). */
  readonly recentActivity?: boolean;

  /** Legacy: epoch ms when the PR was opened. Use {@link prOpenedAt}. */
  readonly prOpenedMs?: number;
  /** Legacy: epoch ms of the most recent activity. Use {@link recentActivity}. */
  readonly lastActivityMs?: number;
}

/** The deterministic, fully-explained takeover decision. */
export interface TakeoverDecision {
  /** True once the two-week window has elapsed without a merge. */
  readonly windowElapsed: boolean;
  /** True when activity + response signals indicate abandonment. */
  readonly unmaintained: boolean;
  /** True only when the window elapsed AND the library is unmaintained. */
  readonly shouldTakeover: boolean;
}

/**
 * The minimal library description {@link planTakeover} needs to emit a spec.
 * This is a structural subset of the registry's entry shape, so a
 * `RegistryEntry` (or any object carrying these fields) can be passed directly.
 */
export interface TakeoverLib {
  /** Original npm package name, e.g. `@scope/widget` or `widget`. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
}

/**
 * The spec emitted when a takeover fires. It is the complete, deterministic
 * basis for forking, publishing under the `@ngx-maintenance` scope, and
 * spinning up a NEW standalone repository (each takeover is its own repo).
 */
export interface TakeoverSpec {
  /** Original npm package name. */
  readonly sourceNpmName: string;
  /** Original source repository URL. */
  readonly sourceRepoUrl: string;
  /** The published scoped fork name, e.g. `@ngx-maintenance/widget`. */
  readonly forkNpmName: string;
  /** Alias of {@link forkNpmName}, retained for back-compat. */
  readonly scopedNpmName: string;
  /** The compatibility-only warning banner (README + npm description). */
  readonly warningBanner: string;
  /** The spec for the NEW standalone repository this takeover becomes. */
  readonly newRepo: NewRepoSpec;
  /** Alias of {@link NewRepoSpec.name}, retained for back-compat. */
  readonly newRepoName: string;
}

/** The spec for the standalone repository a takeover is spun into. */
export interface NewRepoSpec {
  /** The repository name, e.g. `ngx-maintenance-widget`. */
  readonly name: string;
  /** The default branch the migrated code lands on. */
  readonly defaultBranch: string;
  /** Whether the repository is created public (compatibility forks are). */
  readonly isPublic: boolean;
  /** The repository description (carries the compatibility-only warning). */
  readonly description: string;
}
