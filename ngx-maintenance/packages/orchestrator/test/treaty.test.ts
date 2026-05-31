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
import type {
  TreatyMigrationStep,
  TreatyStepInput,
  TreatyStepResult,
} from "@ngx-maintenance/treaty-support";
import { resolveConfig } from "../src/config.js";
import {
  runMaintenanceCycle,
  type MaintenanceAdapters,
} from "../src/index.js";

const DAY_MS = 1000 * 60 * 60 * 24;
const NOW = DAY_MS * 1000;

/** A deterministic in-memory metadata source over a fixed candidate set. */
function fakeMetadata(candidates: readonly RepoMetadata[]): MetadataSource {
  const byName = new Map(candidates.map((c) => [c.npmName, c]));
  return {
    fetchMetadata: (npmName) => Promise.resolve(byName.get(npmName)),
    fetchLatestAngularMajor: () => Promise.resolve(LATEST_ANGULAR),
    listCandidates: () => Promise.resolve(candidates),
  };
}

/** A no-op recording takeover adapter (not exercised by these tests). */
class FakeTakeoverAdapter implements TakeoverAdapter {
  async createRepo(spec: NewRepoSpec): Promise<CreateRepoResult> {
    return { repoUrl: `https://github.com/ngx-maintenance/${spec.name}` };
  }
  async fork(input: ForkInput): Promise<ForkResult> {
    return { repoUrl: `https://github.com/ngx-maintenance/${input.repoName}` };
  }
  async publish(input: PublishInput): Promise<PublishResult> {
    return { name: input.forkNpmName, version: "0.0.0" };
  }
}

/**
 * A recording {@link TreatyMigrationStep}. It captures every input it is asked
 * to migrate (so a test can assert it ran — or, by an empty list, that it was
 * skipped) and returns a programmable result (success by default).
 */
class FakeTreatyStep implements TreatyMigrationStep {
  readonly calls: TreatyStepInput[] = [];
  constructor(private readonly result: (input: TreatyStepInput) => TreatyStepResult =
    (input) => ({
      ok: true,
      plan: { mode: input.mode, usePackagr: true, transforms: [] },
      failedArgv: undefined,
      output: undefined,
    })) {}

  migrate(input: TreatyStepInput): Promise<TreatyStepResult> {
    this.calls.push(input);
    return Promise.resolve(this.result(input));
  }
}

interface Bundle {
  readonly adapters: MaintenanceAdapters;
  readonly github: FakeAdapter;
  readonly treaty: FakeTreatyStep | undefined;
}

/** Assemble a fake bundle, optionally wiring a recording Treaty step. */
function bundle(
  candidates: readonly RepoMetadata[],
  treaty?: FakeTreatyStep,
): Bundle {
  const github = createFakeAdapter();
  return {
    github,
    treaty,
    adapters: {
      github,
      metadata: fakeMetadata(candidates),
      takeover: new FakeTakeoverAdapter(),
      ...(treaty !== undefined ? { treaty } : {}),
    },
  };
}

/** A stale (behind latest + > 6mo idle) library at the given Angular major. */
function staleLib(npmName: string, currentAngular: number): RepoMetadata {
  return {
    npmName,
    repoUrl: `https://github.com/acme/${npmName.replace(/^@[^/]+\//, "")}`,
    currentAngular,
    lastCommitMs: NOW - DAY_MS * 365,
  };
}

describe("treaty-support opt-in wiring", () => {
  it("RUNS the Treaty step for an opted-in library (after a green migration)", async () => {
    const lib = staleLib("@acme/widget", 19);
    const treaty = new FakeTreatyStep();
    const { adapters, github } = bundle([lib], treaty);
    const config = resolveConfig({
      baseBranch: "main",
      treatyOptIn: ["@acme/widget"],
      treatyMode: "enhanced",
    });

    const result = await runMaintenanceCycle(config, adapters, { now: NOW });

    // The step ran exactly once, against the cloned workdir, in enhanced mode.
    expect(treaty.calls).toHaveLength(1);
    expect(treaty.calls[0]?.npmName).toBe("@acme/widget");
    expect(treaty.calls[0]?.mode).toBe("enhanced");
    expect(treaty.calls[0]?.workdir).toBe(github.workdirs.allocated[0]);

    // It ran INSIDE the workdir scope: the allocated dir was still disposed.
    expect(github.workdirs.disposed).toEqual(github.workdirs.allocated);

    // The lib's outcome carries the Treaty result and the PR still opened.
    const outcome = result.libraries[0];
    expect(outcome?.result).toBe("pr-opened");
    expect(outcome?.treaty?.ok).toBe(true);
    expect(result.prsOpened).toBe(1);
    expect(github.octokit.pullCalls).toHaveLength(1);
  });

  it("SKIPS the Treaty step for a library that did not opt in", async () => {
    const lib = staleLib("@acme/widget", 19);
    const treaty = new FakeTreatyStep();
    // Treaty adapter present, but the library is NOT in treatyOptIn.
    const { adapters, github } = bundle([lib], treaty);
    const config = resolveConfig({ baseBranch: "main", treatyOptIn: [] });

    const result = await runMaintenanceCycle(config, adapters, { now: NOW });

    // Not opted in -> the step never ran, behaviour unchanged.
    expect(treaty.calls).toHaveLength(0);
    const outcome = result.libraries[0];
    expect(outcome?.result).toBe("pr-opened");
    expect(outcome?.treaty).toBeUndefined();
    expect(result.prsOpened).toBe(1);
    expect(github.octokit.pullCalls).toHaveLength(1);
  });

  it("does nothing Treaty-related when NO Treaty adapter is injected", async () => {
    const lib = staleLib("@acme/widget", 19);
    // Opted in by name, but the deployment wired no Treaty boundary at all.
    const { adapters } = bundle([lib], undefined);
    const config = resolveConfig({
      baseBranch: "main",
      treatyOptIn: ["@acme/widget"],
    });

    const result = await runMaintenanceCycle(config, adapters, { now: NOW });

    const outcome = result.libraries[0];
    expect(outcome?.result).toBe("pr-opened");
    expect(outcome?.treaty).toBeUndefined();
    expect(result.prsOpened).toBe(1);
  });

  it("blocks the PR and reports treaty-failed when the opt-in step fails", async () => {
    const lib = staleLib("@acme/widget", 19);
    const treaty = new FakeTreatyStep((input) => ({
      ok: false,
      plan: { mode: input.mode, usePackagr: true, transforms: [] },
      failedArgv: ["treaty-packagr", "--project", "."],
      output: "packagr failed",
    }));
    const { adapters, github } = bundle([lib], treaty);
    const config = resolveConfig({
      baseBranch: "main",
      treatyOptIn: ["@acme/widget"],
    });

    const result = await runMaintenanceCycle(config, adapters, { now: NOW });

    const outcome = result.libraries[0];
    expect(treaty.calls).toHaveLength(1);
    expect(outcome?.result).toBe("treaty-failed");
    expect(outcome?.treaty?.ok).toBe(false);
    // A failed opt-in step means NO PR opens — a human reviews.
    expect(result.prsOpened).toBe(0);
    expect(github.octokit.pullCalls).toHaveLength(0);
    // Workdir was still disposed.
    expect(github.workdirs.disposed).toEqual(github.workdirs.allocated);
  });

  it("does NOT run the Treaty step when the Angular migration fails", async () => {
    const lib = staleLib("@acme/widget", 19);
    const treaty = new FakeTreatyStep();
    const github = createFakeAdapter({
      shellResponder: (inv) =>
        inv.argv[0] === "npm" && inv.argv[2] === "build"
          ? { code: 1, output: "build failed" }
          : undefined,
    });
    const adapters: MaintenanceAdapters = {
      github,
      metadata: fakeMetadata([lib]),
      takeover: new FakeTakeoverAdapter(),
      treaty,
    };
    const config = resolveConfig({
      baseBranch: "main",
      treatyOptIn: ["@acme/widget"],
    });

    const result = await runMaintenanceCycle(config, adapters, { now: NOW });

    // Migration failed before the Treaty step could run.
    expect(treaty.calls).toHaveLength(0);
    expect(result.libraries[0]?.result).toBe("migration-failed");
    expect(result.prsOpened).toBe(0);
  });
});
