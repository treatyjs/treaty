import {
  STALE_THRESHOLD_MS,
  DAY_MS,
  type AngularMajor,
  type DiscoveryCandidate,
  type RepoMetadata,
  type StaleFinding,
} from "./types.js";

/**
 * A library is INACTIVE when its most recent commit is older than the staleness
 * window relative to `now`. This is the time half of staleness; it does not by
 * itself imply the library is behind the latest Angular major.
 *
 * The boundary is exclusive: at exactly the threshold the library is still
 * considered active.
 */
export function isInactive(lastCommitMs: number, now: number): boolean {
  return now - lastCommitMs > STALE_THRESHOLD_MS;
}

/** A library is behind latest when it targets an Angular major below latest. */
export function isBehindLatest(
  currentAngular: AngularMajor,
  latestAngular: AngularMajor,
): boolean {
  return currentAngular < latestAngular;
}

/**
 * A library is STALE when it is BOTH behind the latest Angular major AND has had
 * no commit within the staleness window (> 6 months). This is the single
 * predicate discovery keys off of; it is a pure function of npm/GitHub metadata
 * (no AI).
 */
export function isStale(
  lib: RepoMetadata,
  latestAngular: AngularMajor,
  now: number,
): boolean {
  return (
    isBehindLatest(lib.currentAngular, latestAngular) &&
    isInactive(lib.lastCommitMs, now)
  );
}

/**
 * Evaluate a discovered repository deterministically. A candidate is suggested
 * for opt-in only when it is BOTH behind the latest Angular major AND inactive.
 */
export function evaluateCandidate(
  metadata: RepoMetadata,
  latestAngular: AngularMajor,
  now: number,
): DiscoveryCandidate {
  const behindLatest = isBehindLatest(metadata.currentAngular, latestAngular);
  const inactive = isInactive(metadata.lastCommitMs, now);
  const stale = behindLatest && inactive;
  return {
    metadata,
    behindLatest,
    inactive,
    stale,
    suggestOptIn: stale,
  };
}

/** Build the deterministic, human-readable reason a library was flagged stale. */
function staleReason(
  metadata: RepoMetadata,
  latestAngular: AngularMajor,
  majorsBehind: number,
  inactiveForMs: number,
): string {
  const inactiveDays = Math.floor(inactiveForMs / DAY_MS);
  return (
    `on Angular v${metadata.currentAngular} (${majorsBehind} major` +
    `${majorsBehind === 1 ? "" : "s"} behind latest v${latestAngular}); ` +
    `no commit in ${inactiveDays} days (> 6 months)`
  );
}

/**
 * Build the full {@link StaleFinding} for an already-known-stale library. Pure
 * arithmetic over the metadata; reused by {@link discoverStale}.
 */
export function describeStale(
  metadata: RepoMetadata,
  latestAngular: AngularMajor,
  now: number,
): StaleFinding {
  const majorsBehind = latestAngular - metadata.currentAngular;
  const inactiveForMs = now - metadata.lastCommitMs;
  return {
    metadata,
    majorsBehind,
    inactiveForMs,
    reason: staleReason(metadata, latestAngular, majorsBehind, inactiveForMs),
  };
}

/**
 * Given a set of discovered libraries, return only the STALE ones (behind latest
 * AND > 6 months inactive), each with a deterministic {@link StaleFinding.reason}.
 *
 * The result preserves input order and is a pure function of `latestAngular` and
 * `now` — no network access, no AI.
 */
export function discoverStale(
  libs: readonly RepoMetadata[],
  latestAngular: AngularMajor,
  now: number,
): readonly StaleFinding[] {
  const findings: StaleFinding[] = [];
  for (const lib of libs) {
    if (!isStale(lib, latestAngular, now)) continue;
    findings.push(describeStale(lib, latestAngular, now));
  }
  return findings;
}
