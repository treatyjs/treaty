import { describe, expect, it } from "vitest";
import type { RegistryManifest } from "@ngx-maintenance/registry";
import { LATEST_ANGULAR } from "@ngx-maintenance/migration-engine";
import { TAKEOVER_WINDOW_MS } from "@ngx-maintenance/takeover";
import type { BotConfig } from "./context.js";
import type { BotContext } from "./context.js";
import type { OctokitLike } from "./github.js";
import { onAngularRelease, onInstallation } from "./handlers.js";
import { computeScan, type ScheduleTick } from "./scheduler.js";

/** A recording fake octokit that satisfies {@link OctokitLike}. */
interface FakeOctokit extends OctokitLike {
  readonly pullCalls: Array<Record<string, unknown>>;
  readonly issueCalls: Array<Record<string, unknown>>;
}

function makeFakeOctokit(): FakeOctokit {
  const pullCalls: Array<Record<string, unknown>> = [];
  const issueCalls: Array<Record<string, unknown>> = [];
  return {
    pullCalls,
    issueCalls,
    rest: {
      pulls: {
        create(params) {
          pullCalls.push(params);
          return Promise.resolve({ data: { number: pullCalls.length } });
        },
      },
      issues: {
        create(params) {
          issueCalls.push(params);
          return Promise.resolve({ data: { number: issueCalls.length } });
        },
      },
    },
  };
}

const CONFIG: BotConfig = {
  latestAngular: LATEST_ANGULAR,
  token: "test-token",
  baseBranch: "main",
};

function makeContext(
  manifest: RegistryManifest,
  octokit: OctokitLike,
): BotContext {
  // The handlers only use the structural OctokitLike surface; cast through
  // unknown so the recording fake can stand in for the concrete Octokit.
  return {
    octokit: octokit as unknown as BotContext["octokit"],
    config: CONFIG,
    manifest,
  };
}

describe("onInstallation", () => {
  it("marks only the installed repo's registry entries app-installed", () => {
    const manifest: RegistryManifest = {
      version: 1,
      entries: [
        {
          npmName: "@acme/widget",
          repoUrl: "https://github.com/acme/widget",
          currentAngular: 16,
          appInstalled: false,
        },
        {
          npmName: "@other/lib",
          repoUrl: "https://github.com/other/lib",
          currentAngular: 15,
          appInstalled: false,
        },
      ],
    };
    const fake = makeFakeOctokit();
    const ctx = makeContext(manifest, fake);

    const result = onInstallation(ctx, {
      action: "created",
      installation: { id: 1, account: { login: "acme" } },
      repositories: [{ full_name: "acme/widget" }],
    });

    const widget = result.manifest.entries.find(
      (e) => e.npmName === "@acme/widget",
    );
    const other = result.manifest.entries.find(
      (e) => e.npmName === "@other/lib",
    );
    expect(widget?.appInstalled).toBe(true);
    expect(other?.appInstalled).toBe(false);
    expect(result.repos).toEqual(["acme/widget"]);
  });
});

describe("onAngularRelease", () => {
  it("computes a migration PR only for behind, app-installed libraries", async () => {
    const manifest: RegistryManifest = {
      version: 1,
      entries: [
        {
          // behind + installed -> PR
          npmName: "@acme/widget",
          repoUrl: "https://github.com/acme/widget.git",
          currentAngular: 19,
          appInstalled: true,
        },
        {
          // behind but NOT installed -> no PR
          npmName: "@acme/uninstalled",
          repoUrl: "https://github.com/acme/uninstalled",
          currentAngular: 18,
          appInstalled: false,
        },
        {
          // installed but already at latest -> no PR (empty plan)
          npmName: "@acme/current",
          repoUrl: "https://github.com/acme/current",
          currentAngular: 22,
          appInstalled: true,
        },
      ],
    };
    const fake = makeFakeOctokit();
    const ctx = makeContext(manifest, fake);

    const result = await onAngularRelease(ctx, {
      action: "published",
      release: { tag_name: "v22.0.0" },
      repository: { full_name: "angular/angular" },
    });

    // Exactly one PR for the behind+installed lib.
    expect(result.prs).toHaveLength(1);
    const [pr] = result.prs;
    expect(pr.npmName).toBe("@acme/widget");
    expect(pr.plan.from).toBe(19);
    expect(pr.plan.to).toBe(22);
    expect(pr.plan.steps.map((s) => s.to)).toEqual([20, 21, 22]);
    expect(pr.head).toBe("ngx-maintenance/angular-22");
    expect(pr.base).toBe("main");
    expect(result.repos).toEqual(["https://github.com/acme/widget.git"]);

    // And the PR was actually opened against the parsed owner/repo.
    expect(fake.pullCalls).toHaveLength(1);
    expect(fake.pullCalls[0]).toMatchObject({
      owner: "acme",
      repo: "widget",
      base: "main",
      head: "ngx-maintenance/angular-22",
    });
  });

  it("opens no PRs when the release tag has no parseable major", async () => {
    const manifest: RegistryManifest = {
      version: 1,
      entries: [
        {
          npmName: "@acme/widget",
          repoUrl: "https://github.com/acme/widget",
          currentAngular: 19,
          appInstalled: true,
        },
      ],
    };
    const fake = makeFakeOctokit();
    const ctx = makeContext(manifest, fake);

    const result = await onAngularRelease(ctx, {
      action: "published",
      release: { tag_name: "not-a-version" },
      repository: { full_name: "angular/angular" },
    });

    expect(result.prs).toHaveLength(0);
    expect(fake.pullCalls).toHaveLength(0);
  });
});

describe("computeScan (discovery + takeover timer)", () => {
  const NOW = 1_000 * 60 * 60 * 24 * 400; // arbitrary epoch ms well past zero

  it("flags a stale, unregistered, behind library for opt-in suggestion", () => {
    const manifest: RegistryManifest = { version: 1, entries: [] };
    const fake = makeFakeOctokit();
    const ctx = makeContext(manifest, fake);

    const tick: ScheduleTick = {
      now: NOW,
      discovered: [
        {
          // behind latest AND stale (last commit > 6mo ago) -> suggested
          npmName: "@stale/lib",
          repoUrl: "https://github.com/stale/lib",
          currentAngular: 14,
          lastCommitMs: NOW - 1000 * 60 * 60 * 24 * 365,
        },
        {
          // behind latest but RECENT -> not suggested
          npmName: "@fresh/lib",
          repoUrl: "https://github.com/fresh/lib",
          currentAngular: 18,
          lastCommitMs: NOW - 1000 * 60 * 60 * 24 * 3,
        },
      ],
      takeoverObservations: [],
    };

    const result = computeScan(ctx, tick);

    expect(result.suggestions).toHaveLength(1);
    const [suggestion] = result.suggestions;
    expect(suggestion.npmName).toBe("@stale/lib");
    expect(suggestion.samplePr.plan.to).toBe(LATEST_ANGULAR);
    expect(suggestion.samplePr.plan.from).toBe(14);
    expect(suggestion.issue.title).toContain("@stale/lib");
  });

  it("does not suggest a library already in the registry", () => {
    const manifest: RegistryManifest = {
      version: 1,
      entries: [
        {
          npmName: "@stale/lib",
          repoUrl: "https://github.com/stale/lib",
          currentAngular: 14,
          appInstalled: false,
        },
      ],
    };
    const fake = makeFakeOctokit();
    const ctx = makeContext(manifest, fake);

    const tick: ScheduleTick = {
      now: NOW,
      discovered: [
        {
          npmName: "@stale/lib",
          repoUrl: "https://github.com/stale/lib",
          currentAngular: 14,
          lastCommitMs: NOW - 1000 * 60 * 60 * 24 * 365,
        },
      ],
      takeoverObservations: [],
    };

    expect(computeScan(ctx, tick).suggestions).toHaveLength(0);
  });

  it("plans a takeover once the two-week window elapses on an abandoned lib", () => {
    const manifest: RegistryManifest = { version: 1, entries: [] };
    const fake = makeFakeOctokit();
    const ctx = makeContext(manifest, fake);

    const overWindow = TAKEOVER_WINDOW_MS + 1000 * 60 * 60 * 24;
    const tick: ScheduleTick = {
      now: NOW,
      discovered: [],
      takeoverObservations: [
        {
          // PR open > 2 weeks, never merged, no activity, no maintainer reply.
          npmName: "@abandoned/lib",
          repoUrl: "https://github.com/abandoned/lib",
          signals: {
            prOpenedMs: NOW - overWindow,
            prMerged: false,
            lastActivityMs: NOW - overWindow,
            maintainerResponded: false,
          },
        },
        {
          // PR merged -> no takeover.
          npmName: "@merged/lib",
          repoUrl: "https://github.com/merged/lib",
          signals: {
            prOpenedMs: NOW - overWindow,
            prMerged: true,
            lastActivityMs: NOW - overWindow,
            maintainerResponded: false,
          },
        },
      ],
    };

    const result = computeScan(ctx, tick);

    expect(result.takeovers).toHaveLength(1);
    const [takeover] = result.takeovers;
    expect(takeover.spec.sourceNpmName).toBe("@abandoned/lib");
    expect(takeover.spec.scopedNpmName).toBe("@ngx-maintenance/lib");
    expect(takeover.spec.newRepoName).toBe("ngx-maintenance-lib");
  });
});
