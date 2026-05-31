import { describe, expect, it } from "vitest";
import {
  CURATED_CODEMODS,
  codemodsForStep,
  findCodemod,
} from "../src/codemods.js";
import { planMigrationChain } from "../src/planner.js";
import type { MigrationStep } from "../src/types.js";

/** A bare step targeting `to` of the given kind, for codemod matching. */
function stepTo(to: number, kind: MigrationStep["kind"] = "ng-update"): MigrationStep {
  return {
    kind,
    from: to - 1,
    to,
    ngUpdateArgv: [],
    peerBumps: [],
    codemods: [],
    verify: [],
  };
}

describe("codemod hook registry", () => {
  it("exposes a unique id per registered hook", () => {
    const ids = CURATED_CODEMODS.map((hook) => hook.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("declares every hook with a deterministic oxc/ts-morph engine", () => {
    for (const hook of CURATED_CODEMODS) {
      expect(["oxc", "ts-morph"]).toContain(hook.engine);
    }
  });

  it("findCodemod resolves a known id and rejects an unknown one", () => {
    expect(findCodemod("ngcc-drop")?.appliesToMajor).toBe(13);
    expect(findCodemod("does-not-exist")).toBeUndefined();
  });

  it("matches hooks to a step by destination major", () => {
    const ids = codemodsForStep(stepTo(13)).map((h) => h.id);
    expect(ids).toContain("ngcc-drop");
    expect(ids).toContain("entry-components-drop");
  });

  it("honours appliesToKinds: the v9 Ivy-partial codemod is VE->Ivy-only", () => {
    expect(codemodsForStep(stepTo(9, "ng-update"))).toHaveLength(0);
    const veIds = codemodsForStep(stepTo(9, "ve-to-ivy")).map((h) => h.id);
    expect(veIds).toContain("ng-package-ivy-partial");
  });

  it("wires the VE->Ivy partial codemod into a v9 chain's leading step", () => {
    const plan = planMigrationChain(9, 22);
    expect(plan.steps[0]?.codemods).toContain("ng-package-ivy-partial");
  });
});
