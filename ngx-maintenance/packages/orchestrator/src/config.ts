import {
  SIX_MONTHS_MS,
  TWO_WEEKS_MS,
} from "@ngx-maintenance/staleness-detector";
import { LATEST_ANGULAR } from "@ngx-maintenance/migration-engine";

/**
 * Static, fully-typed configuration for a running ngx-maintenance bot. Every
 * field is a plain value; nothing here performs I/O. Thresholds default to the
 * spec's 6-month staleness window and 2-week takeover window, the base branch
 * to `main`, and the migration target to the engine's `LATEST_ANGULAR`.
 */
export interface BotConfig {
  /**
   * The GitHub organisations the bot scans for candidate libraries each cycle.
   * Discovery enumerates repositories through the metadata source; this list is
   * surfaced for the source to scope its enumeration (and for logging).
   */
  readonly watchedOrgs: readonly string[];
  /**
   * Individually watched `owner/repo` (or repo URL) entries, in addition to the
   * org scan. Lets an operator pin specific libraries without an org-wide scan.
   */
  readonly watchedRepos: readonly string[];
  /**
   * Inactivity window: a library is "stale" only when its last commit is older
   * than this (AND it is behind latest Angular). Defaults to six months.
   */
  readonly stalenessWindowMs: number;
  /**
   * Merge window before an unmerged migration PR against an unmaintained
   * library becomes takeover-eligible. Defaults to two weeks.
   */
  readonly takeoverWindowMs: number;
  /** The Angular major the migration chain targets. Defaults to latest. */
  readonly targetAngular: number;
  /** The base branch migration PRs merge into (e.g. `main`). */
  readonly baseBranch: string;
  /**
   * Shallow-clone depth for migration checkouts. A shallow clone is enough to
   * run `ng update` + verify; omit for a full clone.
   */
  readonly cloneDepth?: number;
}

/** The partial config an operator supplies; everything else is defaulted. */
export type BotConfigInput = Partial<BotConfig>;

/**
 * Resolve a complete {@link BotConfig} from a partial input, filling every
 * unset field with the spec defaults (6-month staleness, 2-week takeover,
 * latest Angular target, `main` base branch, shallow depth 1). Pure — no I/O.
 */
export function resolveConfig(input: BotConfigInput = {}): BotConfig {
  return {
    watchedOrgs: input.watchedOrgs ?? [],
    watchedRepos: input.watchedRepos ?? [],
    stalenessWindowMs: input.stalenessWindowMs ?? SIX_MONTHS_MS,
    takeoverWindowMs: input.takeoverWindowMs ?? TWO_WEEKS_MS,
    targetAngular: input.targetAngular ?? LATEST_ANGULAR,
    baseBranch: input.baseBranch ?? "main",
    ...(input.cloneDepth !== undefined ? { cloneDepth: input.cloneDepth } : { cloneDepth: 1 }),
  };
}
