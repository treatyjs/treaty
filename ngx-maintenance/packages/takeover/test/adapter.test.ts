import { describe, expect, it } from "vitest";
import {
  type CreateRepoResult,
  type ForkInput,
  type ForkResult,
  type GithubAdapter,
  type NewRepoSpec,
  type PublishInput,
  type PublishResult,
  type TakeoverLib,
  type TakeoverSignals,
  TWO_WEEKS_MS,
  executeTakeover,
  runTakeover,
} from "../src/index.js";

const PR_OPENED = Date.parse("2026-01-01T00:00:00.000Z");
const DAY_MS = 1000 * 60 * 60 * 24;

const LIB: TakeoverLib = {
  npmName: "@acme/widget",
  repoUrl: "https://github.com/acme/widget",
};

/**
 * A deterministic in-memory {@link GithubAdapter} that records every call in
 * order. No network — exactly the fake the policy/orchestration is tested with.
 */
class FakeGithubAdapter implements GithubAdapter {
  readonly calls: string[] = [];
  readonly forks: ForkInput[] = [];
  readonly repos: NewRepoSpec[] = [];
  readonly publishes: PublishInput[] = [];

  async fork(input: ForkInput): Promise<ForkResult> {
    this.calls.push("fork");
    this.forks.push(input);
    return { repoUrl: `https://github.com/ngx-maintenance/${input.repoName}` };
  }

  async createRepo(spec: NewRepoSpec): Promise<CreateRepoResult> {
    this.calls.push("createRepo");
    this.repos.push(spec);
    return { repoUrl: `https://github.com/ngx-maintenance/${spec.name}` };
  }

  async publish(input: PublishInput): Promise<PublishResult> {
    this.calls.push("publish");
    this.publishes.push(input);
    return { name: input.forkNpmName, version: "0.0.0" };
  }
}

/** Abandoned signals at exactly `ageMs` after the PR opened. */
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

describe("executeTakeover", () => {
  it("drives the adapter in repo -> fork -> publish order", async () => {
    const adapter = new FakeGithubAdapter();
    const execution = await executeTakeover(LIB, adapter);

    expect(adapter.calls).toEqual(["createRepo", "fork", "publish"]);
    expect(execution.spec.forkNpmName).toBe("@ngx-maintenance/widget");
    expect(execution.repo.repoUrl).toBe(
      "https://github.com/ngx-maintenance/ngx-maintenance-widget",
    );
    expect(execution.fork.repoUrl).toBe(
      "https://github.com/ngx-maintenance/ngx-maintenance-widget",
    );
    expect(execution.publish.name).toBe("@ngx-maintenance/widget");
  });

  it("passes the spec's banner + names straight through to the adapter", async () => {
    const adapter = new FakeGithubAdapter();
    await executeTakeover(LIB, adapter);

    expect(adapter.repos[0]?.name).toBe("ngx-maintenance-widget");
    expect(adapter.repos[0]?.description).toContain("compatibility-only");
    expect(adapter.forks[0]?.sourceRepoUrl).toBe(LIB.repoUrl);
    expect(adapter.publishes[0]?.warningBanner).toContain("compatibility-only");
  });
});

describe("runTakeover (policy-gated execution)", () => {
  it("executes through the adapter when the policy fires (>= two weeks)", async () => {
    const adapter = new FakeGithubAdapter();
    const run = await runTakeover(LIB, signals(TWO_WEEKS_MS), adapter);

    expect(run.decision.shouldTakeover).toBe(true);
    expect(run.spec?.forkNpmName).toBe("@ngx-maintenance/widget");
    expect(run.execution?.publish.name).toBe("@ngx-maintenance/widget");
    expect(adapter.calls).toEqual(["createRepo", "fork", "publish"]);
  });

  it("does NOTHING (no adapter calls, no spec) when the policy does not fire", async () => {
    const adapter = new FakeGithubAdapter();
    const run = await runTakeover(LIB, signals(TWO_WEEKS_MS - 1), adapter);

    expect(run.decision.shouldTakeover).toBe(false);
    expect(run.spec).toBeUndefined();
    expect(run.execution).toBeUndefined();
    expect(adapter.calls).toEqual([]);
  });

  it("does NOTHING when the maintainer responded, even past two weeks", async () => {
    const adapter = new FakeGithubAdapter();
    const run = await runTakeover(
      LIB,
      signals(TWO_WEEKS_MS * 3, { maintainerResponded: true }),
      adapter,
    );

    expect(run.execution).toBeUndefined();
    expect(adapter.calls).toEqual([]);
  });

  it("a 15-day-unmerged abandoned PR triggers takeover; 13-day does not", async () => {
    const fifteen = new FakeGithubAdapter();
    const fifteenRun = await runTakeover(LIB, signals(DAY_MS * 15), fifteen);
    expect(fifteenRun.decision.shouldTakeover).toBe(true);
    expect(fifteen.calls).toEqual(["createRepo", "fork", "publish"]);

    const thirteen = new FakeGithubAdapter();
    const thirteenRun = await runTakeover(LIB, signals(DAY_MS * 13), thirteen);
    expect(thirteenRun.decision.shouldTakeover).toBe(false);
    expect(thirteen.calls).toEqual([]);
  });
});
