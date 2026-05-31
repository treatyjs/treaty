import {
  evaluateCandidate,
  findEntry,
  type RepoMetadata,
} from "@ngx-maintenance/registry";
import { planChain } from "@ngx-maintenance/migration-engine";
import {
  buildTakeoverSpec,
  decideTakeover,
  type TakeoverSignals,
} from "@ngx-maintenance/takeover";
import type { BotContext } from "./context.js";
import {
  buildOptInSuggestion,
  openOptInSuggestion,
  type OptInSuggestionSpec,
  type TakeoverPlanned,
} from "./prs.js";

/** A takeover candidate observed during a scan tick. */
export interface TakeoverObservation {
  /** npm package name of the library whose migration PR is outstanding. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
  /** The timer/activity signals consulted by the takeover policy. */
  readonly signals: TakeoverSignals;
}

/** Input to a scheduled scan tick. */
export interface ScheduleTick {
  /** Current epoch ms. */
  readonly now: number;
  /** Repositories discovered via npm/GitHub queries this tick. */
  readonly discovered: readonly RepoMetadata[];
  /** Outstanding migration PRs being timed for takeover eligibility. */
  readonly takeoverObservations: readonly TakeoverObservation[];
}

/** The result of a scheduled scan. */
export interface ScanResult {
  /**
   * Opt-in suggestions for STALE, UNREGISTERED libraries behind latest: each is
   * an issue suggesting the App plus a sample migration PR.
   */
  readonly suggestions: readonly OptInSuggestionSpec[];
  /** Libraries that became takeover-eligible this tick. */
  readonly takeovers: readonly TakeoverPlanned[];
}

/**
 * Compute the scheduled scan result deterministically (no AI, no I/O):
 *
 *  - Discovery: for every discovered repo that is behind the latest Angular
 *    major AND stale AND not already in the registry, build an opt-in
 *    suggestion (issue + sample migration PR).
 *  - Takeover timer: for every outstanding migration PR, consult the takeover
 *    policy and, when the two-week window has elapsed against an unmaintained
 *    lib, emit a takeover spec for a new standalone repo.
 */
export function computeScan(ctx: BotContext, tick: ScheduleTick): ScanResult {
  const suggestions: OptInSuggestionSpec[] = [];
  for (const metadata of tick.discovered) {
    const alreadyRegistered =
      findEntry(ctx.manifest, metadata.npmName) !== undefined;
    if (alreadyRegistered) continue;
    const candidate = evaluateCandidate(
      metadata,
      ctx.config.latestAngular,
      tick.now,
    );
    if (!candidate.suggestOptIn) continue;
    const plan = planChain(metadata.currentAngular, ctx.config.latestAngular);
    suggestions.push(
      buildOptInSuggestion(metadata, plan, ctx.config.baseBranch),
    );
  }

  const takeovers: TakeoverPlanned[] = [];
  for (const observation of tick.takeoverObservations) {
    if (!decideTakeover(observation.signals, tick.now).shouldTakeover) continue;
    takeovers.push({
      kind: "takeover",
      spec: buildTakeoverSpec(observation.npmName, observation.repoUrl),
    });
  }

  return { suggestions, takeovers };
}

/**
 * Run a scheduled scan and open the suggested issues / sample PRs via octokit.
 * Returns the computed {@link ScanResult} (takeover specs are returned for the
 * out-of-band repo-creation step rather than acted on here).
 */
export async function runScheduledScan(
  ctx: BotContext,
  tick: ScheduleTick,
): Promise<ScanResult> {
  const result = computeScan(ctx, tick);
  for (const suggestion of result.suggestions) {
    // Sequential to keep deterministic ordering and avoid an API burst.
    // eslint-disable-next-line no-await-in-loop
    await openOptInSuggestion(ctx.octokit, suggestion);
  }
  return result;
}
