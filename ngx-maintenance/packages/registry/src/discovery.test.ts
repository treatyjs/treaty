import { describe, it, expect } from "vitest";
import {
  isInactive,
  isBehindLatest,
  isStale,
  evaluateCandidate,
  discoverStale,
} from "./discovery.js";
import { SIX_MONTHS_MS, DAY_MS, type RepoMetadata } from "./types.js";

const NOW = Date.UTC(2026, 4, 29); // 2026-05-29, matches the fixed clock.

function lib(overrides: Partial<RepoMetadata> = {}): RepoMetadata {
  return {
    npmName: "@acme/widget",
    repoUrl: "https://github.com/acme/widget",
    currentAngular: 17,
    lastCommitMs: NOW - DAY_MS, // committed yesterday by default
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
    expect(isBehindLatest(17, 22)).toBe(true);
  });

  it("is false when at latest", () => {
    expect(isBehindLatest(22, 22)).toBe(false);
  });

  it("is false when ahead (pre-release adopters)", () => {
    expect(isBehindLatest(23, 22)).toBe(false);
  });
});

describe("isStale", () => {
  const latest = 22;

  it("flags a lib 7 months inactive AND behind latest as stale", () => {
    const sevenMonths = DAY_MS * 30 * 7;
    const stale = lib({
      currentAngular: 16,
      lastCommitMs: NOW - sevenMonths,
    });
    expect(isStale(stale, latest, NOW)).toBe(true);
  });

  it("does NOT flag a recently-updated current lib", () => {
    const fresh = lib({ currentAngular: 22, lastCommitMs: NOW - DAY_MS });
    expect(isStale(fresh, latest, NOW)).toBe(false);
  });

  it("does NOT flag a behind-but-actively-maintained lib", () => {
    const active = lib({ currentAngular: 16, lastCommitMs: NOW - DAY_MS });
    expect(isStale(active, latest, NOW)).toBe(false);
  });

  it("does NOT flag a current-but-inactive lib", () => {
    const sevenMonths = DAY_MS * 30 * 7;
    const dormantButCurrent = lib({
      currentAngular: 22,
      lastCommitMs: NOW - sevenMonths,
    });
    expect(isStale(dormantButCurrent, latest, NOW)).toBe(false);
  });
});

describe("evaluateCandidate", () => {
  const latest = 22;

  it("suggests opt-in only when behind AND inactive", () => {
    const sevenMonths = DAY_MS * 30 * 7;
    const candidate = evaluateCandidate(
      lib({ currentAngular: 15, lastCommitMs: NOW - sevenMonths }),
      latest,
      NOW,
    );
    expect(candidate.behindLatest).toBe(true);
    expect(candidate.inactive).toBe(true);
    expect(candidate.stale).toBe(true);
    expect(candidate.suggestOptIn).toBe(true);
  });

  it("does not suggest opt-in for a healthy lib", () => {
    const candidate = evaluateCandidate(
      lib({ currentAngular: 22, lastCommitMs: NOW - DAY_MS }),
      latest,
      NOW,
    );
    expect(candidate.behindLatest).toBe(false);
    expect(candidate.inactive).toBe(false);
    expect(candidate.stale).toBe(false);
    expect(candidate.suggestOptIn).toBe(false);
  });
});

describe("discoverStale", () => {
  const latest = 22;
  const sevenMonths = DAY_MS * 30 * 7;

  it("returns only the stale libraries, preserving input order", () => {
    const libs: RepoMetadata[] = [
      lib({ npmName: "@a/stale", currentAngular: 14, lastCommitMs: NOW - sevenMonths }),
      lib({ npmName: "@b/fresh", currentAngular: 22, lastCommitMs: NOW - DAY_MS }),
      lib({ npmName: "@c/behind-active", currentAngular: 18, lastCommitMs: NOW - DAY_MS }),
      lib({ npmName: "@d/stale", currentAngular: 19, lastCommitMs: NOW - sevenMonths }),
    ];
    const found = discoverStale(libs, latest, NOW);
    expect(found.map((f) => f.metadata.npmName)).toEqual([
      "@a/stale",
      "@d/stale",
    ]);
  });

  it("returns an empty set when nothing is stale", () => {
    const libs: RepoMetadata[] = [
      lib({ currentAngular: 22, lastCommitMs: NOW - DAY_MS }),
    ];
    expect(discoverStale(libs, latest, NOW)).toEqual([]);
  });

  it("computes majorsBehind and inactiveForMs deterministically", () => {
    const libs: RepoMetadata[] = [
      lib({ currentAngular: 19, lastCommitMs: NOW - sevenMonths }),
    ];
    const [finding] = discoverStale(libs, latest, NOW);
    expect(finding).toBeDefined();
    expect(finding?.majorsBehind).toBe(3);
    expect(finding?.inactiveForMs).toBe(sevenMonths);
  });

  it("produces a human-readable reason mentioning the version gap and inactivity", () => {
    const libs: RepoMetadata[] = [
      lib({ currentAngular: 21, lastCommitMs: NOW - sevenMonths }),
    ];
    const [finding] = discoverStale(libs, latest, NOW);
    expect(finding?.reason).toContain("Angular v21");
    expect(finding?.reason).toContain("1 major behind latest v22");
    expect(finding?.reason).toContain("> 6 months");
  });

  it("pluralizes the majors-behind reason correctly", () => {
    const libs: RepoMetadata[] = [
      lib({ currentAngular: 18, lastCommitMs: NOW - sevenMonths }),
    ];
    const [finding] = discoverStale(libs, latest, NOW);
    expect(finding?.reason).toContain("4 majors behind");
  });
});
