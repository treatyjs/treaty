import { describe, expect, it } from "vitest";
import {
  SCOPE,
  TWO_WEEKS_MS,
  WARNING_BANNER,
  decideTakeover,
  forkName,
  isUnmaintained,
  isWindowElapsed,
  planTakeover,
  shouldTakeOver,
  type TakeoverSignals,
} from "../src/index.js";

const PR_OPENED = Date.parse("2026-01-01T00:00:00.000Z");

/** Build signals at exactly `ageMs` after the PR opened, abandoned by default. */
function signals(
  ageMs: number,
  overrides: Partial<TakeoverSignals> = {},
): TakeoverSignals {
  return {
    prOpenedAt: PR_OPENED,
    now: PR_OPENED + ageMs,
    prMerged: false,
    maintainerResponded: false,
    recentActivity: false,
    ...overrides,
  };
}

describe("shouldTakeOver", () => {
  it("fires exactly at two weeks unmerged + unmaintained", () => {
    expect(shouldTakeOver(signals(TWO_WEEKS_MS))).toBe(true);
  });

  it("fires past two weeks unmerged + unmaintained", () => {
    expect(shouldTakeOver(signals(TWO_WEEKS_MS + 1))).toBe(true);
  });

  it("does NOT fire one millisecond before two weeks", () => {
    expect(shouldTakeOver(signals(TWO_WEEKS_MS - 1))).toBe(false);
  });

  it("does NOT fire when the PR was merged, however old", () => {
    expect(
      shouldTakeOver(signals(TWO_WEEKS_MS * 4, { prMerged: true })),
    ).toBe(false);
  });

  it("does NOT fire when the maintainer responded", () => {
    expect(
      shouldTakeOver(signals(TWO_WEEKS_MS, { maintainerResponded: true })),
    ).toBe(false);
  });

  it("does NOT fire when there is recent activity", () => {
    expect(
      shouldTakeOver(signals(TWO_WEEKS_MS, { recentActivity: true })),
    ).toBe(false);
  });

  it("accepts `Date` timestamps as well as epoch ms", () => {
    expect(
      shouldTakeOver({
        prOpenedAt: new Date(PR_OPENED),
        now: new Date(PR_OPENED + TWO_WEEKS_MS),
        prMerged: false,
        maintainerResponded: false,
        recentActivity: false,
      }),
    ).toBe(true);
  });

  it("accepts `now` passed as a separate argument", () => {
    const s: TakeoverSignals = {
      prOpenedAt: PR_OPENED,
      prMerged: false,
      maintainerResponded: false,
      recentActivity: false,
    };
    expect(shouldTakeOver(s, PR_OPENED + TWO_WEEKS_MS)).toBe(true);
    expect(shouldTakeOver(s, PR_OPENED + TWO_WEEKS_MS - 1)).toBe(false);
  });

  it("supports the legacy prOpenedMs / lastActivityMs signal shape", () => {
    const at = PR_OPENED + TWO_WEEKS_MS;
    // No recent activity (last activity is older than the window) -> takeover.
    expect(
      decideTakeover(
        {
          prOpenedMs: PR_OPENED,
          lastActivityMs: PR_OPENED - TWO_WEEKS_MS,
          prMerged: false,
          maintainerResponded: false,
        },
        at,
      ).shouldTakeover,
    ).toBe(true);
    // Recent activity within the window -> maintained -> no takeover.
    expect(
      decideTakeover(
        {
          prOpenedMs: PR_OPENED,
          lastActivityMs: at,
          prMerged: false,
          maintainerResponded: false,
        },
        at,
      ).shouldTakeover,
    ).toBe(false);
  });
});

describe("decideTakeover components", () => {
  it("reports windowElapsed and unmaintained independently", () => {
    const d = decideTakeover(signals(TWO_WEEKS_MS));
    expect(d.windowElapsed).toBe(true);
    expect(d.unmaintained).toBe(true);
    expect(d.shouldTakeover).toBe(true);
  });

  it("windowElapsed is false for a merged PR regardless of age", () => {
    expect(isWindowElapsed(signals(TWO_WEEKS_MS * 10, { prMerged: true }))).toBe(
      false,
    );
  });

  it("unmaintained requires both no response and no activity", () => {
    expect(isUnmaintained(signals(0))).toBe(true);
    expect(isUnmaintained(signals(0, { maintainerResponded: true }))).toBe(
      false,
    );
    expect(isUnmaintained(signals(0, { recentActivity: true }))).toBe(false);
  });
});

describe("planTakeover", () => {
  it("emits the @ngx-maintenance scoped fork name", () => {
    const spec = planTakeover({
      npmName: "@acme/widget",
      repoUrl: "https://github.com/acme/widget",
    });
    expect(spec.forkNpmName).toBe("@ngx-maintenance/widget");
    expect(spec.forkNpmName.startsWith(`${SCOPE}/`)).toBe(true);
    expect(spec.sourceNpmName).toBe("@acme/widget");
    expect(spec.sourceRepoUrl).toBe("https://github.com/acme/widget");
  });

  it("includes the compatibility-only warning banner", () => {
    const spec = planTakeover({ npmName: "widget", repoUrl: "url" });
    expect(spec.warningBanner).toBe(WARNING_BANNER);
    expect(spec.warningBanner).toContain("compatibility-only");
    expect(spec.warningBanner).toContain("migrate to a");
    expect(spec.newRepo.description).toBe(WARNING_BANNER);
  });

  it("emits a NEW standalone repo spec (each takeover is its own repo)", () => {
    const spec = planTakeover({ npmName: "@acme/My_Widget", repoUrl: "url" });
    expect(spec.forkNpmName).toBe("@ngx-maintenance/my-widget");
    expect(spec.newRepo.name).toBe("ngx-maintenance-my-widget");
    expect(spec.newRepo.defaultBranch).toBe("main");
    expect(spec.newRepo.isPublic).toBe(true);
  });

  it("forkName strips scope and normalises to a bare segment", () => {
    expect(forkName("@scope/Foo.Bar")).toBe("@ngx-maintenance/foo-bar");
    expect(forkName("plain")).toBe("@ngx-maintenance/plain");
  });
});
