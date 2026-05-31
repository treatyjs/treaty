import { describe, expect, it } from "vitest";
import { planMigrationChain } from "../src/planner.js";
import { runMigration, runStep } from "../src/runner.js";
import type { CommandResult, Shell } from "../src/runner.js";

/**
 * A recording fake shell. Every invocation returns success unless its argv
 * matches one of `failOn` (matched as a substring of the joined argv), in which
 * case it returns a non-zero exit. Records every argv it is handed.
 */
function fakeShell(failOn: readonly string[] = []): {
  shell: Shell;
  calls: string[][];
} {
  const calls: string[][] = [];
  const shell: Shell = async (
    _repoDir,
    argv,
  ): Promise<CommandResult> => {
    calls.push([...argv]);
    const joined = argv.join(" ");
    const hit = failOn.find((needle) => joined.includes(needle));
    return hit
      ? { code: 1, output: `simulated failure: ${joined}` }
      : { code: 0, output: `ok: ${joined}` };
  };
  return { shell, calls };
}

describe("runStep", () => {
  it("runs ng-update, peer bumps, codemods, then verify in order", async () => {
    const plan = planMigrationChain(18, 19);
    const step = plan.steps[0]!;
    const { shell, calls } = fakeShell();

    const result = await runStep("/tmp/repo", step, shell);

    expect(result.status).toBe("success");
    // First shelled command is the official ng update.
    expect(calls[0]?.slice(0, 2)).toEqual(["ng", "update"]);
    // Phases appear in the documented order.
    const phases = result.entries.map((e) => e.phase);
    expect(phases.indexOf("ng-update")).toBeLessThan(phases.indexOf("peer-bump"));
    expect(phases.indexOf("peer-bump")).toBeLessThan(phases.indexOf("codemod"));
    expect(phases.indexOf("codemod")).toBeLessThan(phases.indexOf("verify"));
    // The verify phase ran install + build + test.
    const verifyLabels = result.entries
      .filter((e) => e.phase === "verify")
      .map((e) => e.label);
    expect(verifyLabels).toEqual(["install", "build", "test"]);
  });

  it("stops at a failing ng update and does not run verify", async () => {
    const plan = planMigrationChain(16, 17);
    const step = plan.steps[0]!;
    const { shell, calls } = fakeShell(["ng update"]);

    const result = await runStep("/tmp/repo", step, shell);

    expect(result.status).toBe("failed");
    expect(result.entries).toHaveLength(1);
    expect(result.entries[0]?.phase).toBe("ng-update");
    // No verify (npm) command was ever shelled.
    expect(calls.some((c) => c.includes("test"))).toBe(false);
  });
});

describe("runMigration", () => {
  it("runs every step and reports success for a clean chain", async () => {
    const plan = planMigrationChain(16, 18);
    const { shell } = fakeShell();

    const result = await runMigration("/tmp/repo", plan, shell);

    expect(result.status).toBe("success");
    expect(result.failedAt).toBeUndefined();
    expect(result.results).toHaveLength(plan.steps.length);
    expect(result.repoDir).toBe("/tmp/repo");
    expect(result.results.every((r) => r.status === "success")).toBe(true);
  });

  it("stops at the first failing step and reports failed-at-step with logs", async () => {
    const plan = planMigrationChain(16, 19);
    // Fail the build verification only on the 17->18 step's ng update.
    const { shell } = fakeShell(["--to=18"]);

    const result = await runMigration("/tmp/repo", plan, shell);

    expect(result.status).toBe("failed");
    expect(result.failedAt?.to).toBe(18);
    // The 16->17 step ran and succeeded; the 18->19 step never ran.
    const visited = result.results.map((r) => r.step.to);
    expect(visited).toEqual([17, 18]);
    expect(result.results[0]?.status).toBe("success");
    expect(result.results[1]?.status).toBe("failed");
    // The failing step's log carries the simulated failure output.
    expect(result.results[1]?.log).toContain("simulated failure");
  });

  it("verifies each major with install + build + test", async () => {
    const plan = planMigrationChain(16, 17);
    const { shell, calls } = fakeShell();

    await runMigration("/tmp/repo", plan, shell);

    const joined = calls.map((c) => c.join(" "));
    expect(joined).toContain("npm install");
    expect(joined).toContain("npm run build");
    expect(joined).toContain("npm test");
  });
});
