import { describe, expect, it } from "vitest";
import {
  createFakeAdapter,
  type FakeAdapter,
} from "@ngx-maintenance/github-adapter";
import { LATEST_ANGULAR } from "@ngx-maintenance/migration-engine";
import type {
  MetadataSource,
  RepoMetadata,
} from "@ngx-maintenance/staleness-detector";
import type {
  CreateRepoResult,
  ForkInput,
  ForkResult,
  GithubAdapter as TakeoverAdapter,
  NewRepoSpec,
  PublishInput,
  PublishResult,
} from "@ngx-maintenance/takeover";
import {
  initialState,
  resolveSchedulerConfig,
  type MaintenanceAdapters,
} from "@ngx-maintenance/orchestrator";
import { createRunnerFrom } from "./runner.js";
import { poll } from "./poller.js";
import { parseArgs, summarize } from "./cli.js";

const DAY_MS = 1000 * 60 * 60 * 24;
const NOW = DAY_MS * 1000;

function fakeMetadata(candidates: readonly RepoMetadata[]): MetadataSource {
  return {
    fetchMetadata: (npmName) =>
      Promise.resolve(candidates.find((c) => c.npmName === npmName)),
    fetchLatestAngularMajor: () => Promise.resolve(LATEST_ANGULAR),
    listCandidates: () => Promise.resolve(candidates),
  };
}

class FakeTakeoverAdapter implements TakeoverAdapter {
  readonly calls: string[] = [];
  async createRepo(spec: NewRepoSpec): Promise<CreateRepoResult> {
    this.calls.push("createRepo");
    return { repoUrl: `https://github.com/ngx-maintenance/${spec.name}` };
  }
  async fork(input: ForkInput): Promise<ForkResult> {
    this.calls.push("fork");
    return { repoUrl: `https://github.com/ngx-maintenance/${input.repoName}` };
  }
  async publish(input: PublishInput): Promise<PublishResult> {
    this.calls.push("publish");
    return { name: input.forkNpmName, version: "0.0.0" };
  }
}

function staleLib(npmName: string, currentAngular: number): RepoMetadata {
  return {
    npmName,
    repoUrl: `https://github.com/acme/${npmName.replace(/^@[^/]+\//, "")}`,
    currentAngular,
    lastCommitMs: NOW - DAY_MS * 365,
  };
}

function makeAdapters(candidates: readonly RepoMetadata[]): {
  adapters: MaintenanceAdapters;
  github: FakeAdapter;
  takeover: FakeTakeoverAdapter;
} {
  const github = createFakeAdapter();
  const takeover = new FakeTakeoverAdapter();
  return {
    github,
    takeover,
    adapters: { github, metadata: fakeMetadata(candidates), takeover },
  };
}

describe("createRunnerFrom (injected fakes) end-to-end", () => {
  it("runs the full cycle: stale lib migrated + PR opened", async () => {
    const { adapters, github } = makeAdapters([staleLib("@acme/widget", 19)]);
    const runner = createRunnerFrom({ bot: { baseBranch: "main" } }, adapters);

    const result = await runner.run({ now: NOW });

    expect(result.prsOpened).toBe(1);
    expect(result.libraries[0].result).toBe("pr-opened");
    expect(github.octokit.pullCalls).toHaveLength(1);
  });
});

describe("poll (host-driven, pure scheduler)", () => {
  const scheduler = resolveSchedulerConfig();

  it("runs the cycle on the first poll and advances state", async () => {
    const { adapters } = makeAdapters([staleLib("@acme/widget", 19)]);
    const runner = createRunnerFrom({}, adapters);

    const first = await poll(runner, scheduler, initialState(), NOW);
    expect(first.ran).toBe(true);
    expect(first.result?.prsOpened).toBe(1);
    expect(first.state.lastRunMs).toBe(NOW);

    // A second poll within the interval does NOT re-run.
    const second = await poll(runner, scheduler, first.state, NOW + DAY_MS / 2);
    expect(second.ran).toBe(false);
    expect(second.result).toBeUndefined();
    expect(second.nextDueInMs).toBe(DAY_MS / 2);

    // Once the interval elapses it runs again.
    const third = await poll(runner, scheduler, second.state, NOW + DAY_MS);
    expect(third.ran).toBe(true);
  });

  it("carries outstanding PRs into the cycle and fires takeover at 15 days", async () => {
    const { adapters, takeover } = makeAdapters([]);
    const runner = createRunnerFrom({}, adapters);
    const state = initialState([
      {
        npmName: "@abandoned/lib",
        repoUrl: "https://github.com/abandoned/lib",
        signals: {
          prOpenedAt: NOW - DAY_MS * 15,
          prMerged: false,
          maintainerResponded: false,
          recentActivity: false,
        },
      },
    ]);

    const outcome = await poll(runner, scheduler, state, NOW);
    expect(outcome.ran).toBe(true);
    expect(outcome.result?.takeoversExecuted).toBe(1);
    expect(takeover.calls).toEqual(["createRepo", "fork", "publish"]);
  });
});

describe("CLI argument parsing + summary", () => {
  it("parses the run-cycle command with --manifest and repeated --org", () => {
    const args = parseArgs([
      "run-cycle",
      "--manifest",
      "reg.json",
      "--org",
      "acme",
      "--org",
      "widgets",
    ]);
    expect(args.command).toBe("run-cycle");
    expect(args.manifest).toBe("reg.json");
    expect(args.orgs).toEqual(["acme", "widgets"]);
  });

  it("defaults the command and manifest path", () => {
    const args = parseArgs([]);
    expect(args.command).toBe("run-cycle");
    expect(args.manifest).toBe("registry.json");
  });

  it("summarizes a cycle result deterministically", () => {
    const summary = summarize({
      now: NOW,
      prsOpened: 1,
      takeoversExecuted: 0,
      libraries: [
        {
          npmName: "@acme/widget",
          repoUrl: "https://github.com/acme/widget",
          stale: true,
          finding: undefined,
          migration: undefined,
          prHead: "ngx-maintenance/angular-22",
          result: "pr-opened",
        },
      ],
      takeovers: [],
    });
    expect(summary).toContain("1 PR(s) opened");
    expect(summary).toContain("@acme/widget: pr-opened");
  });
});
