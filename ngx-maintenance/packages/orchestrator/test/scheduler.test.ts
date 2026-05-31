import { describe, expect, it } from "vitest";
import { resolveConfig } from "../src/config.js";
import {
  advanceState,
  initialState,
  resolveSchedulerConfig,
  tick,
  type OutstandingPr,
} from "../src/index.js";

const DAY_MS = 1000 * 60 * 60 * 24;
const T0 = DAY_MS * 1000;

describe("resolveConfig defaults", () => {
  it("defaults to the 6-month / 2-week windows and latest target", () => {
    const config = resolveConfig();
    expect(config.stalenessWindowMs).toBe(DAY_MS * 183);
    expect(config.takeoverWindowMs).toBe(DAY_MS * 14);
    expect(config.targetAngular).toBe(22);
    expect(config.baseBranch).toBe("main");
    expect(config.cloneDepth).toBe(1);
    expect(config.watchedOrgs).toEqual([]);
  });

  it("honors operator overrides", () => {
    const config = resolveConfig({
      watchedOrgs: ["acme"],
      takeoverWindowMs: DAY_MS * 7,
      baseBranch: "develop",
      cloneDepth: 5,
    });
    expect(config.watchedOrgs).toEqual(["acme"]);
    expect(config.takeoverWindowMs).toBe(DAY_MS * 7);
    expect(config.baseBranch).toBe("develop");
    expect(config.cloneDepth).toBe(5);
  });
});

describe("tick (pure scheduler, no real timers)", () => {
  const config = resolveSchedulerConfig();

  it("is due on the very first tick", () => {
    const decision = tick(config, initialState(), T0);
    expect(decision.due).toBe(true);
    expect(decision.input?.now).toBe(T0);
    expect(decision.nextDueInMs).toBe(0);
  });

  it("is NOT due again before the interval elapses", () => {
    let state = initialState();
    const first = tick(config, state, T0);
    expect(first.due).toBe(true);
    state = advanceState(state, T0);

    const tooSoon = tick(config, state, T0 + DAY_MS / 2);
    expect(tooSoon.due).toBe(false);
    expect(tooSoon.input).toBeUndefined();
    expect(tooSoon.nextDueInMs).toBe(DAY_MS / 2);
  });

  it("is due again once the interval elapses", () => {
    let state = advanceState(initialState(), T0);
    const decision = tick(config, state, T0 + DAY_MS);
    expect(decision.due).toBe(true);
    expect(decision.input?.now).toBe(T0 + DAY_MS);
  });

  it("carries the tracked outstanding PRs into the cycle input", () => {
    const prs: readonly OutstandingPr[] = [
      {
        npmName: "@a/b",
        repoUrl: "https://github.com/a/b",
        signals: { prOpenedAt: T0, prMerged: false, maintainerResponded: false },
      },
    ];
    const decision = tick(config, initialState(prs), T0);
    expect(decision.input?.outstandingPrs).toEqual(prs);
  });

  it("advanceState records the run time and is immutable", () => {
    const state = initialState();
    const next = advanceState(state, T0);
    expect(state.lastRunMs).toBeUndefined();
    expect(next.lastRunMs).toBe(T0);
  });
});
