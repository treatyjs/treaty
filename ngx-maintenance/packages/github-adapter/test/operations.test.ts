import { describe, expect, it } from "vitest";
import { planChain } from "@ngx-maintenance/migration-engine";
import {
  buildMigrationPr,
  buildOptInSuggestion,
  createFakeOctokit,
  migrationBranch,
  openMigrationPr,
  openOptInSuggestion,
} from "../src/index.js";

const LIB = {
  npmName: "@acme/widget",
  repoUrl: "https://github.com/acme/widget.git",
};

describe("migrationBranch", () => {
  it("is a stable per-major branch name", () => {
    expect(migrationBranch(22)).toBe("ngx-maintenance/angular-22");
  });
});

describe("buildMigrationPr", () => {
  it("carries the migration-engine plan steps in order", () => {
    // The migration engine sequences the correct ordered version steps; the PR
    // surfaces them. A lib at v19 -> latest v22 yields v20, v21, v22.
    const plan = planChain(19, 22);
    const spec = buildMigrationPr(LIB, plan, "main");

    expect(spec.kind).toBe("migration-pr");
    expect(spec.head).toBe("ngx-maintenance/angular-22");
    expect(spec.base).toBe("main");
    expect(spec.plan.from).toBe(19);
    expect(spec.plan.to).toBe(22);
    expect(spec.plan.steps.map((s) => s.to)).toEqual([20, 21, 22]);
    expect(spec.title).toBe("Migrate @acme/widget to Angular v22");
    expect(spec.body).toContain("v19 -> v20");
    expect(spec.body).toContain("All steps are post-Ivy.");
    expect(spec.body).toContain("No AI was used");
  });

  it("flags the VE->Ivy window for a lib pinned in v9..v12", () => {
    const plan = planChain(11, 22);
    const spec = buildMigrationPr(LIB, plan, "main");
    expect(spec.body).toContain("View-Engine -> Ivy transition");
  });
});

describe("openMigrationPr", () => {
  it("opens a PR against the parsed owner/repo via the fake octokit", async () => {
    const octokit = createFakeOctokit();
    const spec = buildMigrationPr(LIB, planChain(19, 22), "main");

    const ref = await openMigrationPr(octokit, spec);

    expect(ref).toEqual({ owner: "acme", repo: "widget" });
    expect(octokit.pullCalls).toHaveLength(1);
    expect(octokit.pullCalls[0]).toMatchObject({
      owner: "acme",
      repo: "widget",
      base: "main",
      head: "ngx-maintenance/angular-22",
      title: "Migrate @acme/widget to Angular v22",
    });
  });

  it("throws when the repo URL cannot be parsed", async () => {
    const octokit = createFakeOctokit();
    const spec = buildMigrationPr(
      { npmName: "x", repoUrl: "garbage" },
      planChain(19, 22),
      "main",
    );
    await expect(openMigrationPr(octokit, spec)).rejects.toThrow(
      /cannot parse repo/,
    );
  });
});

describe("buildOptInSuggestion / openOptInSuggestion", () => {
  const discovered = {
    npmName: "@stale/lib",
    repoUrl: "https://github.com/stale/lib",
    currentAngular: 14,
  };

  it("builds an issue plus a sample PR", () => {
    const spec = buildOptInSuggestion(discovered, planChain(14, 22), "main");
    expect(spec.kind).toBe("opt-in-suggestion");
    expect(spec.issue.title).toContain("@stale/lib");
    expect(spec.issue.body).toContain("v14");
    expect(spec.samplePr.plan.from).toBe(14);
    expect(spec.samplePr.plan.to).toBe(22);
  });

  it("opens BOTH the issue and the sample PR via octokit", async () => {
    const octokit = createFakeOctokit();
    const spec = buildOptInSuggestion(discovered, planChain(14, 22), "main");

    const ref = await openOptInSuggestion(octokit, spec);

    expect(ref).toEqual({ owner: "stale", repo: "lib" });
    expect(octokit.issueCalls).toHaveLength(1);
    expect(octokit.issueCalls[0]).toMatchObject({ owner: "stale", repo: "lib" });
    expect(octokit.pullCalls).toHaveLength(1);
    expect(octokit.pullCalls[0]).toMatchObject({
      owner: "stale",
      repo: "lib",
      head: "ngx-maintenance/angular-22",
    });
  });
});
