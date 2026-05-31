import { describe, expect, it } from "vitest";
import {
  clone,
  createFakeAdapter,
  createFakeShell,
  createNodeShell,
  makeAdapter,
  parentDir,
  createFakeOctokit,
} from "../src/index.js";

describe("createFakeShell", () => {
  it("records every invocation and returns the default success result", async () => {
    const fake = createFakeShell();
    const result = await fake.shell("/repo", ["npm", "install"]);
    expect(result).toEqual({ code: 0, output: "" });
    expect(fake.calls).toEqual([{ repoDir: "/repo", argv: ["npm", "install"] }]);
  });

  it("lets a responder program a failing command", async () => {
    const fake = createFakeShell((inv) =>
      inv.argv[0] === "ng"
        ? { code: 1, output: "migration failed" }
        : undefined,
    );
    const ok = await fake.shell("/repo", ["npm", "test"]);
    const bad = await fake.shell("/repo", ["ng", "update"]);
    expect(ok.code).toBe(0);
    expect(bad).toEqual({ code: 1, output: "migration failed" });
    expect(fake.calls).toHaveLength(2);
  });
});

describe("createNodeShell", () => {
  it("resolves with an empty success result for an empty argv", async () => {
    const shell = createNodeShell();
    await expect(shell("/tmp", [])).resolves.toEqual({ code: 0, output: "" });
  });

  it("resolves (does not reject) with code 127 for a missing command", async () => {
    const shell = createNodeShell();
    const result = await shell(".", ["definitely-not-a-real-binary-xyz"]);
    expect(result.code).toBe(127);
    expect(result.output.length).toBeGreaterThan(0);
  });
});

describe("parentDir", () => {
  it("returns the parent for posix and windows paths", () => {
    expect(parentDir("/work/repos/widget")).toBe("/work/repos");
    expect(parentDir("C:\\work\\repos\\widget")).toBe("C:\\work\\repos");
    expect(parentDir("/work/repos/widget/")).toBe("/work/repos");
  });

  it("returns '.' for a bare path with no separator", () => {
    expect(parentDir("widget")).toBe(".");
  });
});

describe("clone", () => {
  it("shells git clone in the parent dir and reports success", async () => {
    const fake = createFakeShell();
    const result = await clone(
      fake.shell,
      "https://github.com/acme/widget.git",
      "/work/widget",
    );
    expect(result).toEqual({ dir: "/work/widget", ok: true, output: "" });
    expect(fake.calls).toEqual([
      {
        repoDir: "/work",
        argv: [
          "git",
          "clone",
          "https://github.com/acme/widget.git",
          "/work/widget",
        ],
      },
    ]);
  });

  it("passes depth and checks out a ref after cloning", async () => {
    const fake = createFakeShell();
    await clone(fake.shell, "url", "/work/widget", { ref: "v1", depth: 1 });
    expect(fake.calls[0].argv).toEqual([
      "git",
      "clone",
      "--depth",
      "1",
      "url",
      "/work/widget",
    ]);
    expect(fake.calls[1]).toEqual({
      repoDir: "/work/widget",
      argv: ["git", "checkout", "v1"],
    });
  });

  it("stops and reports failure when git clone exits non-zero", async () => {
    const fake = createFakeShell((inv) =>
      inv.argv[1] === "clone" ? { code: 128, output: "no such repo" } : undefined,
    );
    const result = await clone(fake.shell, "url", "/work/widget", { ref: "v1" });
    expect(result.ok).toBe(false);
    expect(result.output).toContain("no such repo");
    // Checkout must NOT run after a failed clone.
    expect(fake.calls).toHaveLength(1);
  });

  it("reports failure when the ref checkout fails", async () => {
    const fake = createFakeShell((inv) =>
      inv.argv[1] === "checkout"
        ? { code: 1, output: "unknown ref" }
        : undefined,
    );
    const result = await clone(fake.shell, "url", "/work/widget", { ref: "bad" });
    expect(result.ok).toBe(false);
    expect(result.output).toContain("unknown ref");
    expect(fake.calls).toHaveLength(2);
  });
});

describe("makeAdapter / createFakeAdapter", () => {
  it("assembles an adapter from an octokit and a shell", () => {
    const octokit = createFakeOctokit();
    const fake = createFakeShell();
    const adapter = makeAdapter(octokit, fake.shell);
    expect(adapter.octokit).toBe(octokit);
    expect(adapter.shell).toBe(fake.shell);
  });

  it("creates a fully fake adapter wiring octokit + shell together", async () => {
    const adapter = createFakeAdapter();
    await adapter.shell("/repo", ["npm", "ci"]);
    expect(adapter.fakeShell.calls).toHaveLength(1);
    expect(adapter.octokit.pullCalls).toHaveLength(0);
  });
});
