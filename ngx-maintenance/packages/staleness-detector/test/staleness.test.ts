import { describe, it, expect } from "vitest";
import {
  isInactive,
  isBehindLatest,
  isStale,
  evaluateCandidate,
  discoverStale,
  describeStale,
} from "../src/staleness.js";
import { SIX_MONTHS_MS, DAY_MS, type RepoMetadata } from "../src/types.js";

const NOW = Date.UTC(2026, 4, 29); // 2026-05-29
const LATEST = 22;
const SEVEN_MONTHS = DAY_MS * 30 * 7;

function lib(overrides: Partial<RepoMetadata> = {}): RepoMetadata {
  return {
    npmName: "@acme/widget",
    repoUrl: "https://github.com/acme/widget",
    currentAngular: 17,
    lastCommitMs: NOW - DAY_MS,
    ...overrides,
  };
}

describe("isInactive", () => {
  it("is false for a recent commit", () => {
    expect(isInactive(NOW - DAY_MS, NOW)).toBe(false);
  });

  it("is true once past the 6-month window", () => {
    expect(isInactive(NOW - (SIX_MONTHS_MS + DAY_MS), NOW)).toBe(true);
  });

  it("is exclusive at exactly the threshold", () => {
    expect(isInactive(NOW - SIX_MONTHS_MS, NOW)).toBe(false);
  });
});

describe("isBehindLatest", () => {
  it("is true when below latest", () => {
    expect(isBehindLatest(17, LATEST)).toBe(true);
  });

  it("is false when at latest", () => {
    expect(isBehindLatest(LATEST, LATEST)).toBe(false);
  });

  it("is false when ahead (pre-release adopters)", () => {
    expect(isBehindLatest(23, LATEST)).toBe(false);
  });
});

describe("isStale: a stale lib is flagged, an up-to-date lib is not", () => {
  it("flags a lib 7 months inactive AND behind latest", () => {
    const stale = lib({ currentAngular: 16, lastCommitMs: NOW - SEVEN_MONTHS });
    expect(isStale(stale, LATEST, NOW)).toBe(true);
  });

  it("does NOT flag a recently-updated current lib", () => {
    const fresh = lib({ currentAngular: LATEST, lastCommitMs: NOW - DAY_MS });
    expect(isStale(fresh, LATEST, NOW)).toBe(false);
  });

  it("does NOT flag a behind-but-actively-maintained lib", () => {
    const active = lib({ currentAngular: 16, lastCommitMs: NOW - DAY_MS });
    expect(isStale(active, LATEST, NOW)).toBe(false);
  });

  it("does NOT flag a current-but-inactive lib", () => {
    const dormant = lib({
      currentAngular: LATEST,
      lastCommitMs: NOW - SEVEN_MONTHS,
    });
    expect(isStale(dormant, LATEST, NOW)).toBe(false);
  });
});

describe("evaluateCandidate", () => {
  it("suggests opt-in only when behind AND inactive", () => {
    const candidate = evaluateCandidate(
      lib({ currentAngular: 15, lastCommitMs: NOW - SEVEN_MONTHS }),
      LATEST,
      NOW,
    );
    expect(candidate.behindLatest).toBe(true);
    expect(candidate.inactive).toBe(true);
    expect(candidate.stale).toBe(true);
    expect(candidate.suggestOptIn).toBe(true);
  });

  it("does not suggest opt-in for a healthy lib", () => {
    const candidate = evaluateCandidate(
      lib({ currentAngular: LATEST, lastCommitMs: NOW - DAY_MS }),
      LATEST,
      NOW,
    );
    expect(candidate.stale).toBe(false);
    expect(candidate.suggestOptIn).toBe(false);
  });
});

describe("describeStale", () => {
  it("computes majorsBehind and inactiveForMs deterministically", () => {
    const finding = describeStale(
      lib({ currentAngular: 19, lastCommitMs: NOW - SEVEN_MONTHS }),
      LATEST,
      NOW,
    );
    expect(finding.majorsBehind).toBe(3);
    expect(finding.inactiveForMs).toBe(SEVEN_MONTHS);
  });
});

describe("discoverStale", () => {
  it("returns only the stale libraries, preserving input order", () => {
    const libs: RepoMetadata[] = [
      lib({ npmName: "@a/stale", currentAngular: 14, lastCommitMs: NOW - SEVEN_MONTHS }),
      lib({ npmName: "@b/fresh", currentAngular: LATEST, lastCommitMs: NOW - DAY_MS }),
      lib({ npmName: "@c/behind-active", currentAngular: 18, lastCommitMs: NOW - DAY_MS }),
      lib({ npmName: "@d/stale", currentAngular: 19, lastCommitMs: NOW - SEVEN_MONTHS }),
    ];
    expect(discoverStale(libs, LATEST, NOW).map((f) => f.metadata.npmName)).toEqual([
      "@a/stale",
      "@d/stale",
    ]);
  });

  it("returns an empty set when nothing is stale", () => {
    const libs: RepoMetadata[] = [
      lib({ currentAngular: LATEST, lastCommitMs: NOW - DAY_MS }),
    ];
    expect(discoverStale(libs, LATEST, NOW)).toEqual([]);
  });

  it("produces a human-readable reason with the version gap and inactivity", () => {
    const [finding] = discoverStale(
      [lib({ currentAngular: 21, lastCommitMs: NOW - SEVEN_MONTHS })],
      LATEST,
      NOW,
    );
    expect(finding?.reason).toContain("Angular v21");
    expect(finding?.reason).toContain("1 major behind latest v22");
    expect(finding?.reason).toContain("> 6 months");
  });

  it("pluralizes the majors-behind reason correctly", () => {
    const [finding] = discoverStale(
      [lib({ currentAngular: 18, lastCommitMs: NOW - SEVEN_MONTHS })],
      LATEST,
      NOW,
    );
    expect(finding?.reason).toContain("4 majors behind");
  });
});
