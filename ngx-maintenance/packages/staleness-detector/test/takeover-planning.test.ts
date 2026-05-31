import { describe, it, expect } from "vitest";
import {
  isWindowElapsed,
  isUnmaintained,
  decideTakeoverPlanning,
  shouldPlanTakeover,
} from "../src/takeover-planning.js";
import { DAY_MS, type TakeoverSignals } from "../src/types.js";

const NOW = Date.UTC(2026, 4, 29);

/** An unmaintained, unmerged PR opened `daysAgo` days before NOW. */
function signals(daysAgo: number, overrides: Partial<TakeoverSignals> = {}): TakeoverSignals {
  return {
    prOpenedAt: NOW - daysAgo * DAY_MS,
    now: NOW,
    prMerged: false,
    maintainerResponded: false,
    recentActivity: false,
    ...overrides,
  };
}

describe("isWindowElapsed: the 2-week unmerged boundary", () => {
  it("a 15-day-unmerged PR has elapsed the window", () => {
    expect(isWindowElapsed(signals(15))).toBe(true);
  });

  it("a 13-day-unmerged PR has NOT elapsed the window", () => {
    expect(isWindowElapsed(signals(13))).toBe(false);
  });

  it("is inclusive at exactly 14 days", () => {
    expect(isWindowElapsed(signals(14))).toBe(true);
  });

  it("a merged PR never elapses regardless of age", () => {
    expect(isWindowElapsed(signals(30, { prMerged: true }))).toBe(false);
  });

  it("accepts `now` as a second argument", () => {
    const s: TakeoverSignals = {
      prOpenedAt: NOW - 15 * DAY_MS,
      prMerged: false,
      maintainerResponded: false,
      recentActivity: false,
    };
    expect(isWindowElapsed(s, NOW)).toBe(true);
  });

  it("throws when no reference time is available", () => {
    const s: TakeoverSignals = {
      prOpenedAt: NOW - 15 * DAY_MS,
      prMerged: false,
      maintainerResponded: false,
      recentActivity: false,
    };
    expect(() => isWindowElapsed(s)).toThrow(TypeError);
  });
});

describe("isUnmaintained", () => {
  it("is true only when neither responded nor recently active", () => {
    expect(isUnmaintained(signals(15))).toBe(true);
  });

  it("is false when the maintainer responded", () => {
    expect(isUnmaintained(signals(15, { maintainerResponded: true }))).toBe(false);
  });

  it("is false when there is recent activity", () => {
    expect(isUnmaintained(signals(15, { recentActivity: true }))).toBe(false);
  });
});

describe("shouldPlanTakeover: 15-day triggers planning, 13-day does not", () => {
  it("triggers takeover planning at 15 days unmerged + unmaintained", () => {
    expect(shouldPlanTakeover(signals(15))).toBe(true);
  });

  it("does NOT trigger at 13 days unmerged", () => {
    expect(shouldPlanTakeover(signals(13))).toBe(false);
  });

  it("does NOT trigger at 15 days if the maintainer responded", () => {
    expect(shouldPlanTakeover(signals(15, { maintainerResponded: true }))).toBe(false);
  });

  it("does NOT trigger at 15 days if there is recent activity", () => {
    expect(shouldPlanTakeover(signals(15, { recentActivity: true }))).toBe(false);
  });

  it("does NOT trigger when the PR was merged", () => {
    expect(shouldPlanTakeover(signals(30, { prMerged: true }))).toBe(false);
  });
});

describe("decideTakeoverPlanning", () => {
  it("exposes every component of the decision at 15 days", () => {
    const decision = decideTakeoverPlanning(signals(15));
    expect(decision.windowElapsed).toBe(true);
    expect(decision.unmaintained).toBe(true);
    expect(decision.planTakeover).toBe(true);
    expect(decision.prAgeMs).toBe(15 * DAY_MS);
  });

  it("reports window not elapsed at 13 days", () => {
    const decision = decideTakeoverPlanning(signals(13));
    expect(decision.windowElapsed).toBe(false);
    expect(decision.planTakeover).toBe(false);
    expect(decision.prAgeMs).toBe(13 * DAY_MS);
  });
});
