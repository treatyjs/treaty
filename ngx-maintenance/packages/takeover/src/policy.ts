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
    "takeover decision requires `now` (in the signals or as an argument)",
  );
}

/** Resolve when the PR was opened from `prOpenedAt` or the legacy `prOpenedMs`. */
function resolvePrOpened(signals: TakeoverSignals): number {
  if (signals.prOpenedAt !== undefined) {
    return toMs(signals.prOpenedAt);
  }
  if (signals.prOpenedMs !== undefined) {
    return signals.prOpenedMs;
  }
  throw new TypeError("takeover decision requires `prOpenedAt`");
}

/**
 * Resolve recent-activity. Prefers the explicit `recentActivity` boolean; falls
 * back to the legacy `lastActivityMs` (recent iff within the window of `now`).
 */
function resolveRecentActivity(signals: TakeoverSignals, nowMs: number): boolean {
  if (signals.recentActivity !== undefined) {
    return signals.recentActivity;
  }
  if (signals.lastActivityMs !== undefined) {
    return nowMs - signals.lastActivityMs <= TAKEOVER_WINDOW_MS;
  }
  return false;
}

/**
 * A library is clearly unmaintained when the maintainer has NOT responded AND
 * there has been NO recent activity. Both abandonment signals must hold.
 */
export function isUnmaintained(
  signals: TakeoverSignals,
  now?: Timestamp,
): boolean {
  const nowMs = resolveNow(signals, now);
  return (
    !signals.maintainerResponded && !resolveRecentActivity(signals, nowMs)
  );
}

/**
 * True once the two-week merge window has elapsed without the PR being merged.
 * The boundary is inclusive: at exactly two weeks the window has elapsed.
 */
export function isWindowElapsed(
  signals: TakeoverSignals,
  now?: Timestamp,
): boolean {
  if (signals.prMerged) {
    return false;
  }
  const elapsed = resolveNow(signals, now) - resolvePrOpened(signals);
  return elapsed >= TAKEOVER_WINDOW_MS;
}

/**
 * Decide whether to take over a library, with every component of the decision
 * exposed. Takeover requires BOTH that the two-week window elapsed without a
 * merge AND that the library is clearly unmaintained. Purely a function of
 * timestamps and boolean signals — no AI.
 */
export function decideTakeover(
  signals: TakeoverSignals,
  now?: Timestamp,
): TakeoverDecision {
  const windowElapsed = isWindowElapsed(signals, now);
  const unmaintained = isUnmaintained(signals, now);
  return {
    windowElapsed,
    unmaintained,
    shouldTakeover: windowElapsed && unmaintained,
  };
}

/**
 * The headline policy predicate: should the migration PR's library be taken
 * over? True exactly when the PR is unmerged for two-plus weeks AND the library
 * is clearly unmaintained (no maintainer response and no recent activity).
 */
export function shouldTakeOver(
  signals: TakeoverSignals,
  now?: Timestamp,
): boolean {
  return decideTakeover(signals, now).shouldTakeover;
}
