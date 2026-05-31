import { describe, expect, it } from "vitest";
import {
  buildMigrationPr,
  createFakeAdapter,
  createGitHubAdapter,
  openMigrationPr,
} from "../src/index.js";
import { planChain } from "@ngx-maintenance/migration-engine";

describe("createGitHubAdapter (production wiring)", () => {
  it("constructs an adapter exposing an octokit + a shell", () => {
    const adapter = createGitHubAdapter({ token: "t" });
    expect(adapter.octokit).toBeDefined();
    expect(typeof adapter.shell).toBe("function");
  });
});

describe("createFakeAdapter end-to-end", () => {
  it("opens a PR through the adapter's octokit and records the shell", async () => {
    const adapter = createFakeAdapter();
    const spec = buildMigrationPr(
      { npmName: "@acme/widget", repoUrl: "acme/widget" },
      planChain(20, 22),
      "main",
    );

    await openMigrationPr(adapter.octokit, spec);
    await adapter.shell("/work/widget", ["npm", "install"]);

    expect(adapter.octokit.pullCalls).toHaveLength(1);
    expect(adapter.octokit.pullCalls[0]).toMatchObject({
      owner: "acme",
      repo: "widget",
    });
    expect(adapter.fakeShell.calls).toHaveLength(1);
  });
});
