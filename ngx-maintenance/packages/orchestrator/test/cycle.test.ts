import { describe, expect, it } from "vitest";
import {
  createFakeAdapter,
  type FakeAdapter,
  type FakeAdapterOptions,
  type PullListResponder,
} from "@ngx-maintenance/github-adapter";
import { LATEST_ANGULAR } from "@ngx-maintenance/migration-engine";
import type {
  AngularMajor,
  MetadataSource,
  RepoMetadata,
} from "@ngx-maintenance/staleness-detector";
import { TWO_WEEKS_MS } from "@ngx-maintenance/takeover";
import type {
  CreateRepoResult,
  ForkInput,
  ForkResult,
  GithubAdapter as TakeoverAdapter,
  NewRepoSpec,
  PublishInput,
  PublishResult,
} from "@ngx-maintenance/takeover";
import { resolveConfig } from "../src/config.js";
import {
  runMaintenanceCycle,
  type MaintenanceAdapters,
  type OutstandingPr,
} from "../src/index.js";

const DAY_MS = 1000 * 60 * 60 * 24;
const NOW = DAY_MS * 1000; // arbitrary epoch ms well past zero

/** A deterministic, in-memory {@link MetadataSource} over a fixed candidate set. */
function fakeMetadata(
  candidates: readonly RepoMetadata[],
  latest: AngularMajor = LATEST_ANGULAR,
): MetadataSource {
  const byName = new Map(candidates.map((c) => [c.npmName, c]));
  return {
    fetchMetadata: (npmName) => Promise.resolve(byName.get(npmName)),
    fetchLatestAngularMajor: () => Promise.resolve(latest),
    listCandidates: () => Promise.resolve(candidates),
  };
}

/** A recording in-memory takeover adapter (fork/createRepo/publish). */
class FakeTakeoverAdapter implements TakeoverAdapter {
  readonly calls: string[] = [];
  readonly forks: ForkInput[] = [];
  readonly repos: NewRepoSpec[] = [];
  readonly publishes: PublishInput[] = [];

  async createRepo(spec: NewRepoSpec): Promise<CreateRepoResult> {
    this.calls.push("createRepo");
    this.repos.push(spec);
    return { repoUrl: `https://github.com/ngx-maintenance/${spec.name}` };
  }

  async fork(input: ForkInput): Promise<ForkResult> {
    this.calls.push("fork");
    this.forks.push(input);
    return { repoUrl: `https://github.com/ngx-maintenance/${input.repoName}` };
  }

  async publish(input: PublishInput): Promise<PublishResult> {
    this.calls.push("publish");
    this.publishes.push(input);
    return { name: input.forkNpmName, version: "0.0.0" };
  }
}

interface Bundle {
  readonly adapters: MaintenanceAdapters;
  readonly github: FakeAdapter;
  readonly takeover: FakeTakeoverAdapter;
}

/**
 * Assemble a fully-fake adapter bundle. `listResponder` lets a test program the
 * octokit `pulls.list` to simulate an already-open migration PR; `shellResponder`
 * lets it inject a failing clone/migration step.
 */
function bundle(
  candidates: readonly RepoMetadata[],
  options: {
    listResponder?: PullListResponder;
    shellResponder?: FakeAdapterOptions["shellResponder"];
    latest?: AngularMajor;
  } = {},
): Bundle {
  const github = createFakeAdapter({
    ...(options.listResponder ? { listResponder: options.listResponder } : {}),
    ...(options.shellResponder
      ? { shellResponder: options.shellResponder }
      : {}),
  });
  const takeover = new FakeTakeoverAdapter();
  return {
    github,
    takeover,
    adapters: {
      github,
      metadata: fakeMetadata(candidates, options.latest ?? LATEST_ANGULAR),
      takeover,
    },
  };
}

const CONFIG = resolveConfig({ baseBranch: "main" });

/** A stale (behind + >6mo idle) library at the given major. */
function staleLib(npmName: string, currentAngular: number): RepoMetadata {
  return {
    npmName,
    repoUrl: `https://github.com/acme/${npmName.replace(/^@[^/]+\//, "")}`,
    currentAngular,
    lastCommitMs: NOW - DAY_MS * 365,
  };
}

describe("runMaintenanceCycle: discover -> migrate -> PR", () => {
  it("clones, migrates and opens ONE PR for a stale lib", async () => {
    const lib = staleLib("@acme/widget", 19);
    const { adapters, github } = bundle([lib]);

    const result = await runMaintenanceCycle(CONFIG, adapters, { now: NOW });

    const outcome = result.libraries[0];
    expect(outcome.result).toBe("pr-opened");
    expect(outcome.stale).toBe(true);
    expect(result.prsOpened).toBe(1);

    // Idempotency was probed before opening, then exactly one PR opened.
    expect(github.octokit.pullListCalls).toHaveLength(1);
    expect(github.octokit.pullCalls).toHaveLength(1);
    expect(github.octokit.pullCalls[0]).toMatchObject({
      owner: "acme",
      repo: "widget",
      base: "main",
      head: "ngx-maintenance/angular-22",
    });

    // The repo was cloned into a workdir, migrated, and the workdir disposed.
    expect(github.workdirs.allocated).toHaveLength(1);
    expect(github.workdirs.disposed).toEqual(github.workdirs.allocated);
    const cloneCall = github.fakeShell.calls.find(
      (c) => c.argv[0] === "git" && c.argv[1] === "clone",
    );
    expect(cloneCall).toBeDefined();
    const ngUpdate = github.fakeShell.calls.find(
      (c) => c.argv[0] === "ng" && c.argv[1] === "update",
    );
    expect(ngUpdate).toBeDefined();
    expect(outcome.migration?.ok).toBe(true);
  });

  it("SKIPS an up-to-date / active library (no clone, no PR)", async () => {
    // At latest already -> not behind -> not stale.
    const current: RepoMetadata = {
      npmName: "@acme/current",
      repoUrl: "https://github.com/acme/current",
      currentAngular: LATEST_ANGULAR,
      lastCommitMs: NOW - DAY_MS * 365,
    };
    // Behind but committed yesterday -> active -> not stale.
    const fresh: RepoMetadata = {
      npmName: "@acme/fresh",
      repoUrl: "https://github.com/acme/fresh",
      currentAngular: 18,
      lastCommitMs: NOW - DAY_MS,
    };
    const { adapters, github } = bundle([current, fresh]);

    const result = await runMaintenanceCycle(CONFIG, adapters, { now: NOW });

    expect(result.prsOpened).toBe(0);
    expect(result.libraries.map((l) => l.result)).toEqual([
      "not-stale",
      "not-stale",
    ]);
    expect(github.octokit.pullCalls).toHaveLength(0);
    expect(github.workdirs.allocated).toHaveLength(0);
  });

  it("does NOT re-open a PR when an open one already exists", async () => {
    const lib = staleLib("@acme/widget", 19);
    const { adapters, github } = bundle([lib], {
      // Simulate an already-open PR on the migration head branch.
      listResponder: () => [{ number: 7, head: { ref: "ngx-maintenance/angular-22" } }],
    });

    const result = await runMaintenanceCycle(CONFIG, adapters, { now: NOW });

    expect(result.libraries[0].result).toBe("pr-exists");
    expect(result.prsOpened).toBe(0);
    // Probed, but never created; and never cloned (skipped entirely).
    expect(github.octokit.pullListCalls).toHaveLength(1);
    expect(github.octokit.pullCalls).toHaveLength(0);
    expect(github.workdirs.allocated).toHaveLength(0);
  });

  it("opens NO PR when a migration step fails (human reviews)", async () => {
    const lib = staleLib("@acme/widget", 19);
    const { adapters, github } = bundle([lib], {
      // Fail the `npm run build` verify step of the first migration.
      shellResponder: (inv) =>
        inv.argv[0] === "npm" && inv.argv[2] === "build"
          ? { code: 1, output: "build failed" }
          : undefined,
    });

    const result = await runMaintenanceCycle(CONFIG, adapters, { now: NOW });

    expect(result.libraries[0].result).toBe("migration-failed");
    expect(result.prsOpened).toBe(0);
    expect(github.octokit.pullCalls).toHaveLength(0);
    // Workdir was still cleaned up even though the migration failed.
    expect(github.workdirs.disposed).toEqual(github.workdirs.allocated);
    expect(github.workdirs.allocated).toHaveLength(1);
  });

  it("opens NO PR when the clone fails", async () => {
    const lib = staleLib("@acme/widget", 19);
    const { adapters, github } = bundle([lib], {
      shellResponder: (inv) =>
        inv.argv[1] === "clone" ? { code: 1, output: "no such repo" } : undefined,
    });

    const result = await runMaintenanceCycle(CONFIG, adapters, { now: NOW });

    expect(result.libraries[0].result).toBe("clone-failed");
    expect(result.prsOpened).toBe(0);
    expect(github.octokit.pullCalls).toHaveLength(0);
    expect(github.workdirs.disposed).toEqual(github.workdirs.allocated);
  });
});

describe("runMaintenanceCycle: 2-week takeover timer", () => {
  function outstanding(ageMs: number): OutstandingPr {
    return {
      npmName: "@abandoned/lib",
      repoUrl: "https://github.com/abandoned/lib",
      signals: {
        prOpenedAt: NOW - ageMs,
        prMerged: false,
        maintainerResponded: false,
        recentActivity: false,
      },
    };
  }

  it("plans + creates the fork for a 15-day-unmerged abandoned PR", async () => {
    const { adapters, takeover } = bundle([]);

    const result = await runMaintenanceCycle(CONFIG, adapters, {
      now: NOW,
      outstandingPrs: [outstanding(DAY_MS * 15)],
    });

    expect(result.takeoversExecuted).toBe(1);
    const [t] = result.takeovers;
    expect(t.run.decision.shouldTakeover).toBe(true);
    expect(t.run.spec?.forkNpmName).toBe("@ngx-maintenance/lib");
    expect(t.run.execution?.repo.repoUrl).toContain("ngx-maintenance-lib");
    expect(takeover.calls).toEqual(["createRepo", "fork", "publish"]);
  });

  it("does NOT take over a 13-day-unmerged PR", async () => {
    const { adapters, takeover } = bundle([]);

    const result = await runMaintenanceCycle(CONFIG, adapters, {
      now: NOW,
      outstandingPrs: [outstanding(DAY_MS * 13)],
    });

    expect(result.takeoversExecuted).toBe(0);
    expect(result.takeovers[0].run.decision.shouldTakeover).toBe(false);
    expect(result.takeovers[0].run.execution).toBeUndefined();
    expect(takeover.calls).toEqual([]);
  });

  it("does NOT take over a merged PR even past two weeks", async () => {
    const { adapters, takeover } = bundle([]);
    const pr: OutstandingPr = {
      npmName: "@merged/lib",
      repoUrl: "https://github.com/merged/lib",
      signals: {
        prOpenedAt: NOW - TWO_WEEKS_MS * 3,
        prMerged: true,
        maintainerResponded: false,
        recentActivity: false,
      },
    };

    const result = await runMaintenanceCycle(CONFIG, adapters, {
      now: NOW,
      outstandingPrs: [pr],
    });

    expect(result.takeoversExecuted).toBe(0);
    expect(takeover.calls).toEqual([]);
  });
});

describe("runMaintenanceCycle: full mixed flow", () => {
  it("migrates the stale lib, skips the fresh one, and forks the abandoned one", async () => {
    const stale = staleLib("@acme/widget", 19);
    const fresh: RepoMetadata = {
      npmName: "@acme/fresh",
      repoUrl: "https://github.com/acme/fresh",
      currentAngular: 18,
      lastCommitMs: NOW - DAY_MS,
    };
    const { adapters, github, takeover } = bundle([stale, fresh]);

    const result = await runMaintenanceCycle(CONFIG, adapters, {
      now: NOW,
      outstandingPrs: [
        {
          npmName: "@abandoned/lib",
          repoUrl: "https://github.com/abandoned/lib",
          signals: {
            prOpenedAt: NOW - TWO_WEEKS_MS - DAY_MS,
            prMerged: false,
            maintainerResponded: false,
            recentActivity: false,
          },
        },
      ],
    });

    expect(result.prsOpened).toBe(1);
    expect(result.takeoversExecuted).toBe(1);
    expect(
      result.libraries.find((l) => l.npmName === "@acme/widget")?.result,
    ).toBe("pr-opened");
    expect(
      result.libraries.find((l) => l.npmName === "@acme/fresh")?.result,
    ).toBe("not-stale");
    expect(github.octokit.pullCalls).toHaveLength(1);
    expect(takeover.calls).toEqual(["createRepo", "fork", "publish"]);
  });
});
