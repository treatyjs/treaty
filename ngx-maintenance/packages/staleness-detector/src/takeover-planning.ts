import {
  TAKEOVER_WINDOW_MS,
  type TakeoverDecision,
  type TakeoverSignals,
  type Timestamp,
} from "./types.js";

/** Normalise a {@link Timestamp} to epoch milliseconds. */
function toMs(t: Timestamp): number {
  return typeof t === "number" ? t : t.getTime();
}

/**
 * Resolve the evaluation instant: an explicit second argument wins, otherwise
 * `signals.now` is used. Throws if neither is supplied — the decision is
 * undefined without a reference time.
 */
function resolveNow(signals: TakeoverSignals, now?: Timestamp): number {
  if (now !== undefined) {
    return toMs(now);
  }
  if (signals.now !== undefined) {
    return toMs(signals.now);
  }
  throw new TypeError(
    "takeover planning requires `now` (in the signals or as an argument)",
  );
}

/**
 * True once the two-week merge window has elapsed without the PR being merged.
 * The boundary is inclusive: at exactly two weeks the window has elapsed (so a
 * 15-day-unmerged PR qualifies and a 13-day one does not). A merged PR never
 * elapses regardless of age.
 */
export function isWindowElapsed(
  signals: TakeoverSignals,
  now?: Timestamp,
): boolean {
  if (signals.prMerged) {
    return false;
  }
  const elapsed = resolveNow(signals, now) - toMs(signals.prOpenedAt);
  return elapsed >= TAKEOVER_WINDOW_MS;
}

/**
 * A library is clearly unmaintained when the maintainer has NOT responded AND
 * there has been NO recent activity. Both abandonment signals must hold.
 */
export function isUnmaintained(signals: TakeoverSignals): boolean {
  return !signals.maintainerResponded && !signals.recentActivity;
}

/**
 * Decide whether to PLAN a takeover, with every component exposed. Planning
 * fires when BOTH the two-week window elapsed without a merge AND the library is
 * clearly unmaintained. Purely a function of timestamps and booleans — no AI.
 */
export function decideTakeoverPlanning(
  signals: TakeoverSignals,
  now?: Timestamp,
): TakeoverDecision {
  const nowMs = resolveNow(signals, now);
  const windowElapsed = isWindowElapsed(signals, now);
  const unmaintained = isUnmaintained(signals);
  return {
    windowElapsed,
    unmaintained,
    planTakeover: windowElapsed && unmaintained,
    prAgeMs: nowMs - toMs(signals.prOpenedAt),
  };
}

/**
 * The headline predicate: should takeover planning begin for this PR's library?
 * True exactly when the PR is unmerged for two-plus weeks AND the library is
 * clearly unmaintained (no maintainer response and no recent activity).
 */
export function shouldPlanTakeover(
  signals: TakeoverSignals,
  now?: Timestamp,
): boolean {
  return decideTakeoverPlanning(signals, now).planTakeover;
}
