import { describe, it, expect } from "vitest";
import {
  sequenceMigrationSteps,
  migrationVersionPath,
  inVeToIvyWindow,
  LATEST_ANGULAR,
} from "../src/migration-engine.js";

describe("inVeToIvyWindow", () => {
  it("is true across the v9..v12 window", () => {
    expect([9, 10, 11, 12].every(inVeToIvyWindow)).toBe(true);
  });

  it("is false at v8 (pre-Ivy) and v13 (VE removed)", () => {
    expect(inVeToIvyWindow(8)).toBe(false);
    expect(inVeToIvyWindow(13)).toBe(false);
  });
});

describe("sequenceMigrationSteps: correct ordered version steps for a start version", () => {
  it("emits one ng-update step per major from current+1..latest", () => {
    const steps = sequenceMigrationSteps(17, 22);
    expect(steps.map((s) => [s.from, s.to])).toEqual([
      [17, 18],
      [18, 19],
      [19, 20],
      [20, 21],
      [21, 22],
    ]);
    expect(steps.every((s) => s.kind === "ng-update")).toBe(true);
  });

  it("prepends an in-place VE->Ivy step for a v9..v12 library", () => {
    const steps = sequenceMigrationSteps(11, 14);
    expect(steps.map((s) => `${s.kind}:${s.from}->${s.to}`)).toEqual([
      "ve-to-ivy:11->11",
      "ng-update:11->12",
      "ng-update:12->13",
      "ng-update:13->14",
    ]);
  });

  it("defaults the target to the latest Angular major", () => {
    const steps = sequenceMigrationSteps(20);
    expect(steps.at(-1)?.to).toBe(LATEST_ANGULAR);
    expect(steps.map((s) => s.to)).toEqual([21, 22]);
  });

  it("emits the deterministic official ng-update argv per step", () => {
    const [step] = sequenceMigrationSteps(20, 21);
    expect(step?.ngUpdateArgv).toEqual([
      "ng",
      "update",
      "@angular/core@21",
      "@angular/cli@21",
      "--migrate-only",
      "--from=20",
      "--to=21",
      "--allow-dirty",
    ]);
  });

  it("returns no steps when already at/above target and outside the VE window", () => {
    expect(sequenceMigrationSteps(22, 22)).toEqual([]);
    expect(sequenceMigrationSteps(23, 22)).toEqual([]);
  });

  it("still emits a VE->Ivy step when a v12 lib is otherwise at target", () => {
    const steps = sequenceMigrationSteps(12, 12);
    expect(steps).toHaveLength(1);
    expect(steps[0]?.kind).toBe("ve-to-ivy");
  });
});

describe("migrationVersionPath", () => {
  it("lists the ordered target majors touched", () => {
    expect(migrationVersionPath(9, 13)).toEqual([9, 10, 11, 12, 13]);
  });
});
