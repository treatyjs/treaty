import { describe, expect, it } from "vitest";
import { planChain } from "@ngx-maintenance/migration-engine";
import {
  buildMigrationPr,
  cloneAndMigrate,
  createFakeAdapter,
  createFakeOctokit,
  createFakeShell,
  createFakeWorkdirs,
  findOpenMigrationPr,
  openMigrationPrIfAbsent,
} from "../src/index.js";

const LIB = {
  npmName: "@acme/widget",
  repoUrl: "https://github.com/acme/widget.git",
};

const spec = (): ReturnType<typeof buildMigrationPr> =>
  buildMigrationPr(LIB, planChain(21, 22), "main");

describe("findOpenMigrationPr", () => {
  it("returns undefined when no open PR matches the head branch", async () => {
    const octokit = createFakeOctokit(() => []);
    expect(await findOpenMigrationPr(octokit, spec())).toBeUndefined();
    expect(octokit.pullListCalls[0]).toMatchObject({
      owner: "acme",
      repo: "widget",
      state: "open",
      head: "acme:ngx-maintenance/angular-22",
    });
  });

  it("matches a PR whose head.ref equals the migration branch", async () => {
    const octokit = createFakeOctokit(() => [
      { number: 3, head: { ref: "ngx-maintenance/angular-22" } },
    ]);
    const found = await findOpenMigrationPr(octokit, spec());
    expect(found).toMatchObject({ number: 3 });
  });

  it("ignores an open PR on a different head branch", async () => {
    const octokit = createFakeOctokit(() => [
      { number: 9, head: { ref: "some-other-branch" } },
    ]);
    expect(await findOpenMigrationPr(octokit, spec())).toBeUndefined();
  });
});

describe("openMigrationPrIfAbsent (idempotent)", () => {
  it("creates a PR when none is open", async () => {
    const octokit = createFakeOctokit(() => []);
    const outcome = await openMigrationPrIfAbsent(octokit, spec());
    expect(outcome.status).toBe("created");
    expect(octokit.pullCalls).toHaveLength(1);
  });

  it("does NOT create a PR when one is already open", async () => {
    const octokit = createFakeOctokit(() => [
      { number: 1, head: { ref: "ngx-maintenance/angular-22" } },
    ]);
    const outcome = await openMigrationPrIfAbsent(octokit, spec());
    expect(outcome.status).toBe("existing");
    expect(octokit.pullCalls).toHaveLength(0);
  });
});

describe("cloneAndMigrate", () => {
  it("clones then runs every migration step in the checkout", async () => {
    const { shell, calls } = createFakeShell();
    const result = await cloneAndMigrate(
      shell,
      LIB.repoUrl,
      "/work/widget",
      planChain(21, 22),
      { ref: "main", depth: 1 },
    );
    expect(result.ok).toBe(true);
    expect(result.clone.ok).toBe(true);
    expect(result.migration?.status).toBe("success");
    // git clone (with depth) ran in the parent, then ng update ran in the dir.
    expect(calls[0].argv.slice(0, 4)).toEqual([
      "git",
      "clone",
      "--depth",
      "1",
    ]);
    const ngUpdate = calls.find((c) => c.argv[0] === "ng");
    expect(ngUpdate?.repoDir).toBe("/work/widget");
  });

  it("does NOT attempt migration when the clone fails", async () => {
    const { shell } = createFakeShell((inv) =>
      inv.argv[1] === "clone" ? { code: 1, output: "boom" } : undefined,
    );
    const result = await cloneAndMigrate(
      shell,
      LIB.repoUrl,
      "/work/widget",
      planChain(21, 22),
    );
    expect(result.ok).toBe(false);
    expect(result.clone.ok).toBe(false);
    expect(result.migration).toBeUndefined();
  });

  it("reports failure when a migration step fails", async () => {
    const { shell } = createFakeShell((inv) =>
      inv.argv[0] === "ng" ? { code: 1, output: "schematic failed" } : undefined,
    );
    const result = await cloneAndMigrate(
      shell,
      LIB.repoUrl,
      "/work/widget",
      planChain(21, 22),
    );
    expect(result.clone.ok).toBe(true);
    expect(result.ok).toBe(false);
    expect(result.migration?.status).toBe("failed");
  });
});

describe("fake adapter + workdirs", () => {
  it("exposes recording octokit, shell and deterministic workdirs", async () => {
    const adapter = createFakeAdapter();
    const dir = await adapter.workdirs.allocate("@acme/widget");
    expect(dir).toBe("/tmp/ngx-maintenance/0-widget");
    await adapter.workdirs.dispose(dir);
    expect(adapter.workdirs.disposed).toEqual([dir]);
  });

  it("createFakeWorkdirs hands out unique sequential dirs", async () => {
    const wd = createFakeWorkdirs();
    const a = await wd.allocate("a");
    const b = await wd.allocate("b");
    expect(a).not.toBe(b);
    expect(wd.allocated).toEqual([a, b]);
  });
});
