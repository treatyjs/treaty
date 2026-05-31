import { describe, expect, it } from "vitest";
import {
  inVeToIvyWindow,
  planMigrationChain,
  requiresVeToIvy,
} from "../src/planner.js";
import { LATEST_ANGULAR } from "../src/types.js";
import type { MigrationStep } from "../src/types.js";

/** The ordered (from,to) pairs of a plan's steps, for concise assertions. */
function transitions(steps: readonly MigrationStep[]): Array<[number, number]> {
  return steps.map((step) => [step.from, step.to]);
}

describe("planMigrationChain", () => {
  it("plans v9 -> latest with the leading VE->Ivy step then one step per major", () => {
    const plan = planMigrationChain(9, LATEST_ANGULAR);

    // First step is the dedicated View-Engine -> Ivy transition, in place at v9.
    expect(plan.steps[0]?.kind).toBe("ve-to-ivy");
    expect(plan.steps[0]?.from).toBe(9);
    expect(plan.steps[0]?.to).toBe(9);

    // Then exactly one ng-update step per major from 9->10 up to ->22.
    expect(transitions(plan.steps)).toEqual([
      [9, 9],
      [9, 10],
      [10, 11],
      [11, 12],
      [12, 13],
      [13, 14],
      [14, 15],
      [15, 16],
      [16, 17],
      [17, 18],
      [18, 19],
      [19, 20],
      [20, 21],
      [21, 22],
    ]);

    // The destination majors visited, in order, end at latest.
    const majors = plan.steps.slice(1).map((step) => step.to);
    expect(majors).toEqual([10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22]);
    expect(majors.at(-1)).toBe(LATEST_ANGULAR);
  });

  it("includes exactly one VE->Ivy step when starting in the v9..v12 window", () => {
    for (const current of [9, 10, 11, 12]) {
      const plan = planMigrationChain(current, LATEST_ANGULAR);
      const veSteps = plan.steps.filter((step) => step.kind === "ve-to-ivy");
      expect(veSteps).toHaveLength(1);
      expect(veSteps[0]?.from).toBe(current);
      expect(veSteps[0]?.to).toBe(current);
      // The VE->Ivy step leads the chain.
      expect(plan.steps[0]?.kind).toBe("ve-to-ivy");
    }
  });

  it("omits the VE->Ivy step once View Engine is gone (v13+)", () => {
    const plan = planMigrationChain(13, LATEST_ANGULAR);
    expect(plan.steps.every((step) => step.kind === "ng-update")).toBe(true);
    expect(transitions(plan.steps)).toEqual([
      [13, 14],
      [14, 15],
      [15, 16],
      [16, 17],
      [17, 18],
      [18, 19],
      [19, 20],
      [20, 21],
      [21, 22],
    ]);
  });

  it("plans a single-step v16 -> v17 transition correctly", () => {
    const plan = planMigrationChain(16, 17);
    expect(plan.from).toBe(16);
    expect(plan.to).toBe(17);
    expect(plan.steps).toHaveLength(1);

    const [step] = plan.steps;
    expect(step?.kind).toBe("ng-update");
    expect(step?.from).toBe(16);
    expect(step?.to).toBe(17);
    expect(step?.ngUpdateArgv).toEqual([
      "ng",
      "update",
      "@angular/core@17",
      "@angular/cli@17",
      "--migrate-only",
      "--from=16",
      "--to=17",
      "--allow-dirty",
    ]);
    // Framework peers are pinned to the destination major.
    expect(step?.peerBumps).toContainEqual({
      pkg: "@angular/core",
      range: "^17.0.0",
    });
    // Verification is install + build + test, in order.
    expect(step?.verify.map((v) => v.label)).toEqual(["install", "build", "test"]);
  });

  it("attaches the curated standalone-bootstrap codemod to the v19 step", () => {
    const plan = planMigrationChain(18, 19);
    const step = plan.steps.find((s) => s.to === 19);
    expect(step?.codemods).toContain("standalone-bootstrap");
  });

  it("returns an empty plan when already at or beyond target and out of the VE window", () => {
    expect(planMigrationChain(17, 17).steps).toHaveLength(0);
    expect(planMigrationChain(22, 22).steps).toHaveLength(0);
    expect(planMigrationChain(20, 18).steps).toHaveLength(0);
  });

  it("defaults the target to the latest Angular major", () => {
    const plan = planMigrationChain(20);
    expect(plan.to).toBe(LATEST_ANGULAR);
    expect(plan.steps.at(-1)?.to).toBe(LATEST_ANGULAR);
  });
});

describe("VE->Ivy predicates", () => {
  it("requiresVeToIvy detects the legacy v8 -> v9 boundary crossing", () => {
    expect(requiresVeToIvy(8, 9)).toBe(true);
    expect(requiresVeToIvy(8, 12)).toBe(true);
    expect(requiresVeToIvy(9, 10)).toBe(false);
  });

  it("inVeToIvyWindow is true only for v9..v12", () => {
    expect(inVeToIvyWindow(8)).toBe(false);
    expect(inVeToIvyWindow(9)).toBe(true);
    expect(inVeToIvyWindow(12)).toBe(true);
    expect(inVeToIvyWindow(13)).toBe(false);
  });
});
